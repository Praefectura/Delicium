"""Convert OSad's Bangboo 01 - Eous FBX in an isolated Blender CLI worker.

Run Blender with --background --factory-startup --disable-autoexec --python
<this file> -- --fbx <file> --textures <directory> --output <new .json>
--atlas <new .png> --report <new .json>. No downloaded scripts are executed.
The original FBX and PNGs are never modified. This is a single-asset adapter,
not a general FBX importer. All outputs must be new files.
"""
import argparse
import base64
import json
import math
from pathlib import Path
import struct
import sys

HEIGHT = 1.65
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_WEIGHT_ERROR = 0.0075
MAX_MEAN_WEIGHT_ERROR = 0.0005
# The source idle is a 60 Hz cycle of 59 frames (2..61) whose last frame
# repeats its first. Frame 1 is the exporter's bind pose, not an animation
# frame, so the loop is 59/60 s long rather than a rounded 1 s. The literal is
# 59/60 pre-rounded to the converter's 7-decimal wire precision, because
# `validate_payload` re-reads the encoded clip and compares it back to this
# table; the difference from the exact ratio is 3.4e-8, far below float32.
IDLE_PERIOD = 0.9833333
ACTION_DURATIONS = {"idle": IDLE_PERIOD, "walk": 0.8, "reaction": 1.0,
                    "greet": 1.6, "play": 2.2}
EXPECTED_BONES = (
    "hips", "spine_01", "spine_02", "head",
    "leftEar_01", "leftEar_02", "leftEar_03",
    "rightEar_01", "rightEar_02", "rightEar_03",
    "leftArm", "leftForearm", "leftHand",
    "rightArm", "rightForearm", "rightHand",
    "leftLeg", "leftKnee", "leftFoot", "rightLeg", "rightKnee", "rightFoot",
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def finite_vector(value, size, label):
    require(len(value) == size and all(math.isfinite(x) for x in value), label)


def walk_angles(phase):
    """Sagittal steps; the stance foot and swing foot exchange every half-cycle."""
    require(math.isfinite(phase), "Invalid walk phase")
    # Enter with neutral hip swing rather than maximum leg separation.
    phase = (phase + 0.25) % 1.0
    wave = math.sin(phase * 2 * math.pi)
    stride = math.cos(phase * 2 * math.pi)
    angles = {"hips": ("Y", 25.0), "spine_01": ("Y", 3 * wave)}
    for side, sign in (("left", 1), ("right", -1)):
        swing = max(0.0, -sign * wave)
        # A moderate crouch bridges both the bent source idle and the straight
        # reaction's return. Grounded clip endpoints alone do not guarantee
        # clearance inside their TRS/slerp crossfades.
        hip = -22 - sign * 13 * stride
        knee = 44 + 18 * swing ** 2
        angles[side + "Leg"] = ("X", hip)
        angles[side + "Knee"] = ("X", knee)
        # The source has no foot vertex weights: this terminal joint track is
        # authored consistently, but visible sole clearance comes from the knee.
        angles[side + "Foot"] = ("X", -hip - knee - 4 * swing ** 2)
        angles[side + "Arm"] = ("X", sign * 12 * stride)
        angles[side + "Ear_02"] = ("X", sign * 5 * wave)
    return angles


def top_four(influences):
    require(bool(influences), "Vertex has no deform weights")
    require(all(isinstance(i, int) and i >= 0 and math.isfinite(w) and w > 0
                for i, w in influences), "Invalid deform weight")
    require(len({i for i, _ in influences}) == len(influences), "Duplicate joint weight")
    ordered = sorted(influences, key=lambda item: (-item[1], item[0]))
    total = sum(w for _, w in ordered)
    kept = ordered[:4]
    kept_sum = sum(w for _, w in kept)
    lost = sum(w for _, w in ordered[4:]) / total
    joints = [i for i, _ in kept]
    weights = [w / kept_sum for _, w in kept]
    while len(joints) < 4:
        joints.append(joints[0])
        weights.append(0.0)
    return joints, weights, lost


def triangle_components(count, indices):
    require(len(indices) % 3 == 0, "Incomplete topology")
    adjacency = [set() for _ in range(count)]
    for start in range(0, len(indices), 3):
        a, b, c = indices[start:start + 3]
        require(all(isinstance(i, int) and 0 <= i < count for i in (a, b, c)),
                "Invalid topology index")
        for first, second in ((a, b), (b, c), (c, a)):
            adjacency[first].add(second)
            adjacency[second].add(first)
    remaining = set(range(count))
    components = []
    while remaining:
        stack = [min(remaining)]
        component = set(stack)
        remaining.difference_update(stack)
        while stack:
            new = adjacency[stack.pop()] & remaining
            remaining.difference_update(new)
            component.update(new)
            stack.extend(new)
        components.append(component)
    return components, adjacency


def blend_head_region(rows, adjacency, selected, protected, face, head, rings=2):
    """Welded source vertices share weights even across normal/UV seams."""
    require(rings >= 1 and selected and face <= selected and not selected & protected,
            "Invalid head selection")
    require(all(0 <= i < len(rows) for i in selected | protected), "Invalid head vertex")
    require(len(adjacency) == len(rows), "Head adjacency mismatch")
    require(all(all(math.isfinite(w) and w >= 0 for w in row)
                and abs(sum(row) - 1) < 1e-6 and 0 <= head < len(row) for row in rows),
            "Invalid full head weights")
    boundary = {j for i in selected for j in adjacency[i] if j not in selected | protected}
    require(bool(boundary), "Missing neck boundary")
    distances = {i: 0 for i in boundary}
    frontier = boundary
    for distance in range(1, rings):
        frontier = {j for i in frontier for j in adjacency[i]
                    if j not in selected | protected and j not in distances}
        distances.update({i: distance for i in frontier})
    adapted = [row[:] for row in rows]
    for i in sorted(selected | distances.keys()):
        amount = (rings - distances[i]) / (rings + 1) if i in distances else 1.0
        adapted[i] = [w * (1 - amount) for w in rows[i]]
        adapted[i][head] += amount
    return adapted, distances


def adapt_eous_head(name, rows, source_ids, indices):
    """Explicit OSad asset islands; never infer a hood from height alone."""
    if name not in ("body", "eyes"):
        return rows, None
    components, _ = triangle_components(len(source_ids), indices)
    sizes = sorted(map(len, components))
    head = EXPECTED_BONES.index("head")
    require(all(0 <= i < len(rows) for i in source_ids), "Invalid source vertex map")
    if name == "eyes":
        require(len(rows) == 480 and sizes == [240, 240], "Unexpected eye topology")
        adapted = [[float(i == head) for i in range(22)] for _ in rows]
        return adapted, {"mesh": name, "rigid_source_vertices": list(range(480)),
                         "transition_source_vertices": [], "protected_source_vertices": []}
    require(len(rows) == 3796 and sizes == [44, 44, 63, 63, 101, 101, 221,
                                           331, 331, 375, 375, 1026, 1097],
            "Unexpected body topology")
    shells = [c for c in components if len(c) in (1026, 1097)]
    panel = next(c for c in components if len(c) == 221)
    ears = {source_ids[i] for c in components if len(c) in (44, 63, 101) for i in c}
    face = {source_ids[i] for i in panel}
    selected = {source_ids[i] for c in shells for i in c if rows[source_ids[i]][head] > 0}
    require(sum(rows[source_ids[i]][head] > 0 for c in shells for i in c) == 855,
            "Unexpected hood weights")
    selected = (selected | face) - ears
    welded_indices = [source_ids[i] for i in indices]
    _, adjacency = triangle_components(len(rows), welded_indices)
    adapted, distances = blend_head_region(rows, adjacency, selected, ears, face, head)
    return adapted, {"mesh": name, "rule": "UV/normal islands mapped back to welded source vertices",
                     "neck_transition_rings": 2,
                     "rigid_source_vertices": sorted(selected - distances.keys()),
                     "transition_source_vertices": sorted(distances),
                     "protected_source_vertices": sorted(ears)}


def merge_similar_influences(full_weights, palettes, points):
    """Fit five influences to four using actual deformation trajectories.

    Try equivalent-joint merging first, then constrained four-support fitting
    where needed. Minimize worst sampled error, not just average error.
    """
    import numpy as np
    reduced = full_weights.copy()
    merged = 0
    for vertex, weights in enumerate(full_weights):
        active = np.flatnonzero(weights > 0)
        if len(active) <= 4:
            continue
        require(len(active) == 5, "Unexpected source influence count")
        trajectories = np.einsum("pbij,j->pbi", palettes[:, active], points[vertex])[:, :, :3]
        best = None
        for source_slot, source_joint in enumerate(active):
            for target_slot, target_joint in enumerate(active):
                if source_slot == target_slot:
                    continue
                delta = weights[source_joint] * (trajectories[:, target_slot] - trajectories[:, source_slot])
                errors = np.linalg.norm(delta, axis=1)
                score = (float(errors.max()), float(errors.mean()), int(source_joint), int(target_joint))
                if best is None or score < best:
                    best = score
        _, _, removed, retained = best
        candidate = weights.copy()
        candidate[retained] += candidate[removed]
        candidate[removed] = 0
        target = np.einsum("pbi,b->pi", trajectories, weights[active])
        if best[0] > MAX_WEIGHT_ERROR * 0.5:
            # Some source vertices combine independently moving limbs. Fit all
            # five choices of four-joint support, with sum(weights)=1 exactly.
            for dropped in range(5):
                slots = [i for i in range(5) if i != dropped]
                paths = trajectories[:, slots]
                design = (paths[:, :3] - paths[:, 3:4]).transpose(0, 2, 1).reshape(-1, 3)
                rhs = (target - paths[:, 3]).reshape(-1)
                solved = np.linalg.lstsq(design, rhs, rcond=None)[0]
                fitted = np.maximum(0, np.append(solved, 1 - solved.sum()))
                fitted /= fitted.sum()
                residual = np.einsum("pbi,b->pi", paths, fitted) - target
                # Coordinate descent on the convex worst-sample objective.
                # Each pair exchange preserves both support and unit sum.
                if np.linalg.norm(residual, axis=1).max() > MAX_WEIGHT_ERROR:
                    for unused_sweep in range(8):
                        for a in range(4):
                            for b in range(a + 1, 4):
                                direction = paths[:, a] - paths[:, b]
                                lo, hi = -fitted[a], fitted[b]
                                def objective(delta):
                                    error = residual + delta * direction
                                    return float(np.einsum("pi,pi->p", error, error).max())
                                for unused_step in range(22):
                                    left = hi - (hi - lo) * 0.61803398875
                                    right = lo + (hi - lo) * 0.61803398875
                                    if objective(left) < objective(right):
                                        hi = right
                                    else:
                                        lo = left
                                delta = (lo + hi) / 2
                                if objective(delta) < objective(0):
                                    fitted[a] += delta
                                    fitted[b] -= delta
                                    residual += delta * direction
                errors = np.linalg.norm(residual, axis=1)
                score = (float(errors.max()), float(errors.mean()), dropped, -1)
                if score < best:
                    best = score
                    candidate = np.zeros_like(weights)
                    candidate[active[slots]] = fitted
        reduced[vertex] = candidate
        merged += 1
    return reduced, merged


def validate_payload(model):
    require(model["format"] == "delicium.model" and model["format_version"] == "0.2",
            "Unexpected wire format")
    require(model["name"] == "bangboo", "Keep the existing Bangboo asset name")
    mesh = model["skeleton3d"]
    vertices, indices, joints = mesh["vertices"], mesh["indices"], mesh["joints"]
    require(0 < len(vertices) <= 50000, "Vertex limit")
    require(0 < len(indices) <= 150000 and len(indices) % 3 == 0, "Index limit")
    require(len(joints) == 22 and {j["name"] for j in joints} == set(EXPECTED_BONES),
            "Unexpected source skeleton")
    require(len(mesh["uvs"]) == len(vertices), "UV count")
    for vertex, uv in zip(vertices, mesh["uvs"]):
        finite_vector(vertex["position"], 3, "Position")
        finite_vector(vertex["normal"], 3, "Normal")
        require(sum(n * n for n in vertex["normal"]) > 0.99, "Zero or unnormalized normal")
        finite_vector(uv, 2, "UV")
        require(all(0 <= x <= 1 for x in uv), "UV outside atlas")
        require(vertex["color"] == [1, 1, 1, 1], "Opaque white vertex color")
        require(len(vertex["joints"]) == len(vertex["weights"]) == 4, "Four weight slots")
        require(all(isinstance(i, int) and 0 <= i < len(joints)
                    for i in vertex["joints"]), "Joint index")
        require(all(math.isfinite(w) and w >= 0 for w in vertex["weights"])
                and abs(sum(vertex["weights"]) - 1) < 1e-6, "Weight sum")
    for start in range(0, len(indices), 3):
        triangle = indices[start:start + 3]
        require(len(set(triangle)) == 3, "Repeated triangle index")
        require(all(isinstance(i, int) and 0 <= i < len(vertices) for i in triangle),
                "Triangle index outside mesh")
    require(mesh["texture"] == "eous-color" and len(model["images"]) == 1,
            "Exactly one embedded character atlas")
    require(mesh["material"] == {"roughness": 0.65, "metallic": 0.0,
                                  "alpha_mode": "opaque"}, "Unsupported material")
    png = base64.b64decode(model["images"][0]["png_base64"], validate=True)
    require(png[:8] == bytes((137, 80, 78, 71, 13, 10, 26, 10)), "PNG signature")
    require(struct.unpack(">II", png[16:24]) == (2048, 2048), "Atlas dimensions")
    require(model["interaction"] == {"head_joint": "head"}, "Head binding")
    clips = mesh["animations"]
    require({a["name"]: a["duration"] for a in clips} == ACTION_DURATIONS,
            "All five independent actions")
    require(model["actions"] == {name: name for name in ACTION_DURATIONS}, "Action bindings")
    names = {j["name"] for j in joints}
    for joint in joints:
        for field, size in (("translation", 3), ("rotation", 4), ("scale", 3)):
            finite_vector(joint[field], size, "Joint bind transform")
        require(joint["rotation"] == [0, 0, 0, 1] and joint["scale"] == [1, 1, 1],
                "Canonical rest joint frames")
        require(joint["parent"] is None or joint["parent"] in names, "Unknown parent")
    require(len(clips) == 5, "Exactly five clips")
    for clip in clips:
        require(len(clip["channels"]) == 22, "Every clip samples the whole skeleton")
        for channel in clip["channels"]:
            require(channel["joint"] in names, "Unknown animated joint")
            for field, size in (("translation", 3), ("rotation", 4), ("scale", 3)):
                keys = channel[field]
                require(2 <= len(keys) <= 4096, "Key limit")
                require(keys[0]["time"] == 0 and keys[-1]["time"] == clip["duration"],
                        "Clip endpoints")
                require(all(a["time"] < b["time"] for a, b in zip(keys, keys[1:])),
                        "Key time order")
                for key in keys:
                    finite_vector(key["value"], size, "Animation value")
                    if field == "rotation":
                        require(abs(sum(x * x for x in key["value"]) - 1) < 1e-5,
                                "Animation quaternion must be normalized")
                    elif field == "scale":
                        require(all(abs(x) >= 1e-6 for x in key["value"]), "Singular animated scale")
    encoded = json.dumps(model, separators=(",", ":"), allow_nan=False).encode("utf-8")
    require(len(encoded) <= MAX_FILE_BYTES, "Model exceeds the unchanged 8 MiB runtime limit")
    return encoded


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("fbx", "textures", "output", "atlas", "report"):
        parser.add_argument("--" + name, required=True, type=Path)
    argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else sys.argv[1:]
    return parser.parse_args(argv)


def convert(args):
    import bpy
    import numpy as np
    from bpy_extras import image_utils
    from mathutils import Matrix, Quaternion, Vector

    source = args.fbx.resolve(strict=True)
    texture_dir = args.textures.resolve(strict=True)
    outputs = [p.resolve() for p in (args.output, args.atlas, args.report)]
    require(len(set(outputs)) == 3, "Outputs must be distinct")
    for path in outputs:
        require(path.parent.is_dir() and not path.exists(), "Output must be a new file: " + str(path))
        require(path != source and not path.is_relative_to(texture_dir), "Do not overwrite inputs")
    require(not bpy.context.preferences.filepaths.use_scripts_auto_execute,
            "Run Blender with --disable-autoexec")
    images = {p.name.casefold(): p.resolve(strict=True) for p in texture_dir.glob("*.png")}
    require(set(images) == {"bangboocolor.png", "stoolcolor.png"}, "Expected the two supplied PNGs")
    require(all(p.parent == texture_dir for p in images.values()), "Image escaped input directory")
    original_load = image_utils.load_image

    def load_local(filename, *unused_args, **unused_kwargs):
        name = str(filename).replace(chr(92), "/").rsplit("/", 1)[-1].casefold()
        require(name in images, "Unexpected source image reference")
        return bpy.data.images.load(str(images[name]), check_existing=True)

    for obj in list(bpy.data.objects):
        bpy.data.objects.remove(obj, do_unlink=True)
    image_utils.load_image = load_local
    try:
        bpy.ops.import_scene.fbx(filepath=str(source), use_anim=True, use_image_search=False)
    finally:
        image_utils.load_image = original_load
    rigs = [o for o in bpy.data.objects if o.type == "ARMATURE"]
    require(len(rigs) == 1 and rigs[0].name == "Armature", "Unexpected armature")
    rig = rigs[0]
    bones = list(rig.data.bones)
    require([b.name for b in bones] == list(EXPECTED_BONES), "Unexpected bone order or names")
    bone_index = {b.name: i for i, b in enumerate(bones)}
    parents = [bone_index[b.parent.name] if b.parent else None for b in bones]
    require(all(p is None or p < i for i, p in enumerate(parents)), "Source hierarchy order")
    meshes = []
    excluded = []
    for obj in bpy.data.objects:
        if obj.type != "MESH":
            continue
        materials = [m.name if m else None for m in obj.data.materials]
        modifiers = list(obj.modifiers)
        if materials == ["bangboo"]:
            require(len(modifiers) == 1 and modifiers[0].type == "ARMATURE"
                    and modifiers[0].object == rig and not modifiers[0].use_deform_preserve_volume,
                    "Unsupported character modifier")
            require(len(obj.data.uv_layers) == 1, "Character must have one UV map")
            meshes.append(obj)
        else:
            require(materials == ["stool"], "Unexpected source material")
            excluded.append(obj.name)
    meshes.sort(key=lambda obj: obj.name)
    require(len(meshes) == 16 and sum(len(o.data.vertices) for o in meshes) == 9592,
            "Unexpected source character geometry")
    material = meshes[0].data.materials[0]
    shader = next(n for n in material.node_tree.nodes if n.type == "BSDF_PRINCIPLED")
    require(shader.inputs["Alpha"].default_value == 1 and not shader.inputs["Alpha"].is_linked,
            "Unsupported source alpha")
    require(shader.inputs["Metallic"].default_value == 0, "Unsupported metallic source")
    for socket in shader.inputs:
        if not socket.is_linked:
            continue
        require(len(socket.links) == 1, "Multiple source material links")
        node = socket.links[0].from_node
        if socket.name == "Base Color":
            require(node.type == "TEX_IMAGE" and node.image is not None
                    and Path(node.image.filepath).resolve() == images["bangboocolor.png"],
                    "Unsupported base color connection")
        elif socket.name == "Normal":
            require(node.type == "NORMAL_MAP" and node.space == "TANGENT"
                    and not any(s.is_linked for s in node.inputs)
                    and tuple(node.inputs["Color"].default_value) == (0.5, 0.5, 1.0, 1.0),
                    "Unsupported normal map; only the source's constant flat normal is equivalent")
        else:
            raise ValueError("Unsupported source material connection: " + socket.name)

    rig.data.pose_position = "REST"
    bpy.context.view_layer.update()
    raw_points = [o.matrix_world @ v.co for o in meshes for v in o.data.vertices]
    zmin = min(p.z for p in raw_points)
    zmax = max(p.z for p in raw_points)
    unit = HEIGHT / (zmax - zmin)
    # Proper rotation (det=+1): Blender Z-up/-Y-front -> engine Y-up/+Z-front.
    axes = Matrix(((1, 0, 0, 0), (0, 0, 1, -zmin),
                   (0, -1, 0, 0), (0, 0, 0, 1)))
    change = Matrix.Diagonal((unit, unit, unit, 1)) @ axes
    source_bind = [change @ rig.matrix_world @ b.matrix_local for b in bones]
    pivots = [m.translation.copy() for m in source_bind]
    # Rebase joint frames, not the skin: canonical rest axes make local-Y gaze
    # actually yaw. N(t)=P(t)*R^-1*T, so N(t)*T^-1 == P(t)*R^-1 exactly.
    rest_world = [Matrix.Translation(p) for p in pivots]
    rest_local = [(rest_world[p].inverted() @ rest_world[i]) if p is not None
                  else rest_world[i].copy() for i, p in enumerate(parents)]
    joints = []
    for i, bone in enumerate(bones):
        joints.append({"name": bone.name, "parent": bone.parent.name if bone.parent else None,
                       "translation": list(rest_local[i].translation),
                       "rotation": [0, 0, 0, 1], "scale": [1, 1, 1]})

    vertices, uvs, indices = [], [], []
    export_source_rows = []
    source_positions, source_weights, reduced_weights = [], [], []
    lost_weights = []
    mesh_stats = []
    original_source_weights = []
    head_adaptation = []
    for obj in meshes:
        mesh = obj.data
        mesh.calc_loop_triangles()
        matrix = change @ obj.matrix_world
        normal_matrix = matrix.to_3x3().inverted().transposed()
        weight_rows = []
        source_start = len(source_positions)
        for vertex in mesh.vertices:
            influences = []
            for group in vertex.groups:
                if group.weight > 0:
                    name = obj.vertex_groups[group.group].name
                    require(name in bone_index, "Unknown deform group: " + name)
                    influences.append((bone_index[name], group.weight))
            ids, weights, lost = top_four(influences)
            lost_weights.append(lost)
            total = sum(w for _, w in influences)
            source_weights.append([sum(w for j, w in influences if j == i) / total
                                   for i in range(22)])
            reduced_weights.append([sum(w for j, w in zip(ids, weights) if j == i)
                                    for i in range(22)])
            source_positions.append(list(matrix @ vertex.co))
            weight_rows.append((ids, weights))
        dedup = {}
        before = len(vertices)
        index_start = len(indices)
        for triangle in mesh.loop_triangles:
            require(triangle.material_index == 0, "Unexpected triangle material")
            tri_positions = [matrix @ mesh.vertices[v].co for v in triangle.vertices]
            require((tri_positions[1] - tri_positions[0]).cross(
                tri_positions[2] - tri_positions[0]).length > 1e-12, "Degenerate source triangle")
            for loop_index in triangle.loops:
                loop = mesh.loops[loop_index]
                uv = mesh.uv_layers[0].data[loop_index].uv
                require(all(-1e-6 <= c <= 1 + 1e-6 for c in uv), "Unsupported tiled UV")
                normal = (normal_matrix @ mesh.corner_normals[loop_index].vector).normalized()
                key = (loop.vertex_index, tuple(uv), tuple(normal))
                if key not in dedup:
                    ids, weights = weight_rows[loop.vertex_index]
                    dedup[key] = len(vertices)
                    export_source_rows.append(source_start + loop.vertex_index)
                    vertices.append({"position": source_positions[source_start + loop.vertex_index],
                                     "normal": list(normal), "color": [1, 1, 1, 1],
                                     "joints": ids, "weights": weights})
                    uvs.append([max(0, min(1, uv.x)), max(0, min(1, 1 - uv.y))])
                indices.append(dedup[key])
        original_rows = source_weights[source_start:]
        original_source_weights.extend([row[:] for row in original_rows])
        adapted, adaptation = adapt_eous_head(
            obj.name, original_rows,
            [i - source_start for i in export_source_rows[before:]],
            [i - before for i in indices[index_start:]])
        source_weights[source_start:] = adapted
        for local_index, row in enumerate(adapted):
            ids, weights, lost = top_four([(i, w) for i, w in enumerate(row) if w > 0])
            reduced_weights[source_start + local_index] = [
                sum(w for j, w in zip(ids, weights) if j == i) for i in range(22)]
            lost_weights[source_start + local_index] = lost
        if adaptation is not None:
            adaptation["changed_source_vertices"] = sum(a != b for a, b in zip(original_rows, adapted))
            head_adaptation.append(adaptation)
        mesh_stats.append({"name": obj.name, "source_vertices": len(mesh.vertices),
                           "export_vertices": len(vertices) - before,
                           "triangles": len(mesh.loop_triangles)})
    require(len(indices) // 3 == 18224, "Do not lose source triangles")
    points = np.array([p + [1.0] for p in source_positions], dtype=np.float64)
    full_weights = np.array(source_weights, dtype=np.float64)
    four_weights = np.array(reduced_weights, dtype=np.float64)
    error_max = 0.0
    error_sum = 0.0
    error_count = 0
    comparison_samples = 0
    decomposition_max = 0.0
    fit_palettes = []
    collect_palettes = True

    def check_weights(world):
        nonlocal error_max, error_sum, error_count, comparison_samples
        palette = np.array([list(map(list, m @ rest_world[i].inverted()))
                            for i, m in enumerate(world)], dtype=np.float64)
        if collect_palettes:
            fit_palettes.append(palette)
        moved = np.einsum("bij,vj->bvi", palette, points)[:, :, :3]
        full = np.einsum("vb,bvi->vi", full_weights, moved)
        four = np.einsum("vb,bvi->vi", four_weights, moved)
        errors = np.linalg.norm(full - four, axis=1)
        error_max = max(error_max, float(errors.max()))
        error_sum += float(errors.sum())
        error_count += len(errors)
        comparison_samples += 1
        return four

    def global_matrices(locals_):
        result = []
        for i, local in enumerate(locals_):
            parent = parents[i]
            result.append(local if parent is None else result[parent] @ local)
        return result

    def make_clip(name, samples):
        nonlocal decomposition_max
        channels = []
        previous_q = [None] * 22
        decomposed = [[] for _ in bones]
        for time, locals_ in samples:
            for i, local in enumerate(locals_):
                t, q, s = local.decompose()
                if previous_q[i] is not None and q.dot(previous_q[i]) < 0:
                    q.negate()
                previous_q[i] = q.copy()
                rebuilt = Matrix.LocRotScale(t, q, s)
                error = max(abs(local[r][c] - rebuilt[r][c]) for r in range(4) for c in range(4))
                decomposition_max = max(decomposition_max, error)
                require(error < 2e-5, "Source transform has unsupported shear")
                decomposed[i].append((time, list(t), [q.x, q.y, q.z, q.w], list(s)))
        for i, bone in enumerate(bones):
            rows = decomposed[i]
            channels.append({"joint": bone.name, "interpolation": "linear",
                             **{field: [{"time": row[0], "value": row[index]} for row in rows]
                                for field, index in (("translation", 1), ("rotation", 2), ("scale", 3))}})
        return {"name": name, "duration": ACTION_DURATIONS[name], "channels": channels}

    rig.data.pose_position = "POSE"
    action = rig.animation_data.action
    require(action.name == "Armature|Take 001|BaseLayer"
            and list(action.frame_range) == [1.0, 61.0], "Unexpected source animation")
    scene = bpy.context.scene
    require(abs(scene.render.fps / scene.render.fps_base - 60) < 1e-6, "Source animation FPS")
    # Frame 1 is the exporter's bind pose on every bone, with a zero local
    # transform on hips; it is not part of the animation. The cycle is frames
    # 2..61, and the source repeats frame 2 exactly at frame 61 (verified on
    # all 22 bones), so the loop is 59/60 s with identical endpoints. Sampling
    # frame 1 as t=0 used to put a rest pose inside the sway and, together
    # with a tail blend towards it, reversed the sway velocity once per loop.
    #
    # The bind frame is also kept out of the weight fit: it is never played,
    # and the authored clips already contribute plenty of near-rest poses.
    idle = []
    idle_ground_lifts = []
    for frame in range(1, 62):
        scene.frame_set(frame)
        world = [change @ rig.matrix_world @ rig.pose.bones[b.name].matrix
                 @ source_bind[i].inverted() @ rest_world[i] for i, b in enumerate(bones)]
        moved = check_weights(world)
        # Lift the whole pose for floor clearance; never shorten the legs or
        # change foot weights to hide the source animation's floor crossings.
        lift = max(0.0, 0.002 - float(moved[:, 1].min()))
        if frame == 1:
            continue
        idle_ground_lifts.append(lift)
        world = [Matrix.Translation((0, lift, 0)) @ m for m in world]
        local = [world[p].inverted() @ world[i] if p is not None else world[i]
                 for i, p in enumerate(parents)]
        idle.append(((frame - 2) / 60, local))
    clips = [make_clip("idle", idle)]

    def rotation(axis, degrees):
        return Matrix.Rotation(math.radians(degrees), 4, axis)

    def custom_pose(name, time):
        duration = ACTION_DURATIONS[name]
        phase = time / duration
        wave = math.sin(phase * 2 * math.pi)
        locals_ = [m.copy() for m in rest_local]

        def turn(bone, axis, angle):
            i = bone_index[bone]
            locals_[i] = locals_[i] @ rotation(axis, angle)

        def lift(amount):
            locals_[0].translation += Vector((0, amount, 0))

        if name == "walk":
            for bone, (axis, angle) in walk_angles(phase).items():
                turn(bone, axis, angle)
        elif name == "reaction":
            envelope = max(0, math.sin(math.pi * phase))
            turn("head", "X", -8 * envelope)
            turn("leftArm", "Z", 55 * envelope)
            turn("rightArm", "Z", -55 * envelope)
            turn("leftEar_01", "Z", 12 * envelope)
            turn("rightEar_01", "Z", -12 * envelope)
            lift(0.05 * envelope)
        elif name == "greet":
            envelope = min(1, time / 0.22, (duration - time) / 0.25)
            envelope = max(0, envelope)
            turn("rightArm", "Z", -(78 + 15 * math.sin(time * 4 * math.pi)) * envelope)
            turn("rightForearm", "Z", -12 * envelope)
            turn("rightHand", "Y", 15 * math.sin(time * 5 * math.pi) * envelope)
            turn("leftEar_02", "X", 8 * math.sin(time * 4 * math.pi) * envelope)
            turn("rightEar_02", "X", 8 * math.sin(time * 4 * math.pi) * envelope)
            turn("head", "Z", 3 * envelope)
        elif name == "play":
            envelope = math.sin(math.pi * phase) ** 2
            turn("spine_01", "Y", 9 * math.sin(time * 3 * math.pi) * envelope)
            turn("head", "Y", -5 * math.sin(time * 3 * math.pi) * envelope)
            turn("leftArm", "Z", (32 + 13 * math.sin(time * 4 * math.pi)) * envelope)
            turn("rightArm", "Z", -(32 - 13 * math.sin(time * 4 * math.pi)) * envelope)
            turn("leftEar_01", "Z", 11 * math.sin(time * 4 * math.pi) * envelope)
            turn("rightEar_01", "Z", -11 * math.sin(time * 4 * math.pi) * envelope)
            lift(0.055 * abs(math.sin(time * 2 * math.pi)) * envelope)
        else:
            raise ValueError("Unknown authored action")
        return locals_

    ground_lifts = {}
    for name in ("walk", "reaction", "greet", "play"):
        count = round(ACTION_DURATIONS[name] * 60)
        samples = []
        lifts = []
        for frame in range(count + 1):
            time = ACTION_DURATIONS[name] if frame == count else ACTION_DURATIONS[name] * frame / count
            locals_ = custom_pose(name, time)
            moved = check_weights(global_matrices(locals_))
            clearance = 0.0027 if name == "walk" else 0.002
            floor_offset = clearance - float(moved[:, 1].min())
            # Walk keeps a supporting sole on the floor instead of preserving
            # the old full-body hop. Other authored actions retain their lift.
            extra_lift = floor_offset if name == "walk" else max(0.0, floor_offset)
            locals_[0].translation += Vector((0, extra_lift, 0))
            lifts.append(extra_lift)
            samples.append((time, locals_))
        clips.append(make_clip(name, samples))
        ground_lifts[name] = max(lifts)
    baseline_error_max = error_max
    baseline_pose_samples = comparison_samples
    baseline_scope = "raw idle and authored poses before clip rebuild; no head drive"

    def clip_locals(clip, index):
        result = []
        for channel in clip["channels"]:
            t = Vector(channel["translation"][index]["value"])
            xyzw = channel["rotation"][index]["value"]
            q = Quaternion((xyzw[3], xyzw[0], xyzw[1], xyzw[2]))
            s = Vector(channel["scale"][index]["value"])
            result.append(Matrix.LocRotScale(t, q, s))
        return result

    # Fit and verify against the actual clips AND the runtime's +/-60 degree
    # head drive. This used to include the old idle tail keys, which were
    # blended towards the bind pose; that blend is gone, so the fit now sees
    # only poses the runtime actually plays. The four-slot fit is a minimax
    # problem with a degenerate optimum, so a handful of leg vertices land on
    # a different but equally good solution; the reported fit error bounds it.
    def fit_pose(locals_):
        check_weights(global_matrices(locals_))
        for yaw in (-60, -30, 30, 60):
            driven = [m.copy() for m in locals_]
            head = bone_index["head"]
            t, q, s = driven[head].decompose()
            q = Quaternion((0, 1, 0), math.radians(yaw)) @ q
            driven[head] = Matrix.LocRotScale(t, q, s)
            check_weights(global_matrices(driven))

    for clip in clips:
        for index in range(len(clip["channels"][0]["rotation"])):
            fit_pose(clip_locals(clip, index))
    four_weights, merged_vertices = merge_similar_influences(
        full_weights, np.array(fit_palettes), points)
    collect_palettes = False
    error_max = error_sum = 0.0
    error_count = comparison_samples = 0
    intentional_delta = full_weights - np.array(original_source_weights)
    intentional_max = intentional_sum = 0.0
    worst_fit = None
    for sample_index, palette in enumerate(fit_palettes):
        moved = np.einsum("bij,vj->bvi", palette, points)[:, :, :3]
        errors = np.linalg.norm(np.einsum("vb,bvi->vi", full_weights - four_weights, moved), axis=1)
        intentional = np.linalg.norm(np.einsum("vb,bvi->vi", intentional_delta, moved), axis=1)
        if float(errors.max()) > error_max:
            vertex_index = int(errors.argmax())
            worst_fit = {"sample": sample_index, "source_vertex": vertex_index,
                         "full_weights": full_weights[vertex_index].tolist(),
                         "four_weights": four_weights[vertex_index].tolist()}
        intentional_max = max(intentional_max, float(intentional.max()))
        intentional_sum += float(intentional.sum())
        error_max = max(error_max, float(errors.max()))
        error_sum += float(errors.sum())
        error_count += len(errors)
        comparison_samples += 1
    for vertex, source_row in zip(vertices, export_source_rows):
        active = [(i, float(w)) for i, w in enumerate(four_weights[source_row]) if w > 0]
        ids, weights, lost = top_four(active)
        require(lost == 0, "Merged vertex still exceeds four weights")
        vertex["joints"], vertex["weights"] = ids, weights
    # Recheck the floor after weight fitting; changing weights must not hide a
    # new foot intersection. Apply only a common root translation.
    for clip in clips:
        for index in range(len(clip["channels"][0]["rotation"])):
            locals_ = clip_locals(clip, index)
            world = global_matrices(locals_)
            palette = np.array([list(map(list, m @ rest_world[i].inverted()))
                                for i, m in enumerate(world)])
            moved = np.einsum("vb,bvi->vi", four_weights,
                              np.einsum("bij,vj->bvi", palette, points)[:, :, :3])
            clearance = 0.0027 if clip["name"] == "walk" else 0.002
            floor_offset = clearance - float(moved[:, 1].min())
            lift = floor_offset if clip["name"] == "walk" else max(0.0, floor_offset)
            clip["channels"][0]["translation"][index]["value"][1] += lift
    mean_error = error_sum / error_count
    print("WEIGHT_ERROR", json.dumps({"top4_max": baseline_error_max,
                                      "top4_pose_samples": baseline_pose_samples,
                                      "top4_sample_scope": baseline_scope,
                                      "merged_pose_samples": comparison_samples,
                                      "merged_max": error_max, "merged_mean": mean_error,
                                      "affected": merged_vertices,
                                      "worst_fit": worst_fit}), flush=True)
    require(error_max <= MAX_WEIGHT_ERROR and mean_error <= MAX_MEAN_WEIGHT_ERROR,
            "Four-weight reduction exceeds the predeclared deformation error budget")

    image = next((im for im in bpy.data.images if im.filepath
                  and Path(im.filepath).resolve() == images["bangboocolor.png"]), None)
    require(image is not None and tuple(image.size) == (4096, 4096), "Character atlas not loaded")
    image.scale(2048, 2048)
    image.filepath_raw = str(args.atlas.resolve())
    image.file_format = "PNG"
    image.save()
    atlas = args.atlas.read_bytes()
    model = {"format": "delicium.model", "format_version": "0.2", "name": "bangboo",
             "kind": "skeleton3d", "interaction": {"head_joint": "head"},
             "actions": {name: name for name in ACTION_DURATIONS},
             "images": [{"id": "eous-color", "png_base64": base64.b64encode(atlas).decode("ascii")}],
             "skeleton3d": {"vertices": vertices, "indices": indices, "joints": joints,
                            "animations": clips, "texture": "eous-color", "uvs": uvs,
                            "material": {"roughness": 0.65, "metallic": 0.0, "alpha_mode": "opaque"}}}
    def wire_precision(value):
        if isinstance(value, float):
            return round(value, 7)
        if isinstance(value, list):
            return [wire_precision(item) for item in value]
        if isinstance(value, dict):
            return {key: wire_precision(item) for key, item in value.items()}
        return value
    # JSON does not need Python's full float64 spelling for a float32 runtime.
    # Seven decimal places bound component rounding to 0.05 micrometres here.
    model["skeleton3d"] = wire_precision(model["skeleton3d"])
    encoded = validate_payload(model)
    report = {"source": str(source), "blender": bpy.app.version_string,
              "author": "OSad", "title": "Bangboo 01 - Eous", "license": "CC BY 4.0",
              "source_url": "https://sketchfab.com/3d-models/bangboo-01-eous-a3dbb1c10b4c4f1b97a47fa798239af9",
              "meshes": mesh_stats, "excluded_stool_meshes": excluded,
              "vertices": len(vertices), "triangles": len(indices) // 3, "joints": len(joints),
              "model_bytes": len(encoded), "atlas_bytes": len(atlas), "atlas_size": [2048, 2048],
              "rest_height": HEIGHT, "source_world_to_model": list(map(list, change)),
              "joint_frame_rebase": "canonical rest axes; N=P*R^-1*T preserves skin palette",
              "source_idle": {"frames": [2, 61], "fps": 60, "duration": IDLE_PERIOD,
                              "excluded_bind_frame": 1, "seam": "source endpoint repeated exactly",
                              "max_ground_lift": max(idle_ground_lifts)},
              "custom_actions": ground_lifts, "max_trs_reconstruction_error": decomposition_max,
              "head_adaptation": {"method": "rigid eyes/panel/hood; welded two-ring neck transition; preserve ears",
                                  "meshes": head_adaptation,
                                  "intentional_max_position_change": intentional_max,
                                  "intentional_mean_position_change": intentional_sum / error_count,
                                  "comparison": "original full weights -> intentionally adapted full weights"},
              "weight_reduction": {"comparison": "adapted full weights -> four weights (not original source)",
                                   "method": "merge equivalent trajectories; constrained four-support minimax fit",
                                   "naive_top4_max_error": baseline_error_max,
                                   "naive_top4_pose_samples": baseline_pose_samples,
                                   "naive_top4_sample_scope": baseline_scope,
                                   "head_drive_tested_degrees": [-60, -30, 0, 30, 60],
                                   "merged_vertices": merged_vertices,
                                   "source_vertices": len(source_positions),
                                   "affected_vertices": sum(w > 0 for w in lost_weights),
                                   "max_discarded_weight": max(lost_weights),
                                   "pose_samples": comparison_samples, "max_position_error": error_max,
                                   "mean_position_error": mean_error,
                                   "max_allowed": MAX_WEIGHT_ERROR, "mean_allowed": MAX_MEAN_WEIGHT_ERROR}}
    with args.output.open("xb") as handle:
        handle.write(encoded)
    with args.report.open("x", encoding="utf-8") as handle:
        json.dump(report, handle, indent=2, allow_nan=False)
    print("EOUS_CONVERSION_OK", json.dumps({k: report[k] for k in (
        "vertices", "triangles", "joints", "model_bytes", "atlas_bytes", "weight_reduction")}))
    print("MODEL", args.output.resolve())
    print("REPORT", args.report.resolve())


if __name__ == "__main__":
    convert(parse_args())
