"""Re-author the Eous walk clip from convert_eous.walk_angles without Blender.

Use Python -I. Input is the shipped JSON, outputs must be new files. Only the
walk clip changes; the same gait function feeds the Blender converter, so a
future re-export reproduces these tracks. Every other clip, the mesh, the
skeleton and the atlas are compared by digest and must stay byte-identical.
"""
import argparse
import copy
import hashlib
import importlib.util
import json
import math
from pathlib import Path

import numpy as np


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"),
                                    allow_nan=False).encode()).hexdigest()


def load_tools():
    """Import the converter (gait) and the reaction calibrator (rig helpers)."""
    here = Path(__file__).parent
    modules = {}
    for name in ("convert_eous", "recalibrate_eous_reaction"):
        spec = importlib.util.spec_from_file_location(name, here / (name + ".py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        modules[name] = module
    return modules["convert_eous"], modules["recalibrate_eous_reaction"]


def axis_rotation(axis, degrees):
    angle = math.radians(degrees)
    c, s = math.cos(angle), math.sin(angle)
    matrix = np.eye(4)
    if axis == "X":
        matrix[1, 1], matrix[1, 2], matrix[2, 1], matrix[2, 2] = c, -s, s, c
    elif axis == "Y":
        matrix[0, 0], matrix[0, 2], matrix[2, 0], matrix[2, 2] = c, s, -s, c
    elif axis == "Z":
        matrix[0, 0], matrix[0, 1], matrix[1, 0], matrix[1, 1] = c, -s, s, c
    else:
        raise ValueError("Unknown rotation axis")
    return matrix


def rest_locals(joints):
    result = []
    for joint in joints:
        matrix = np.eye(4)
        x, y, z, w = joint["rotation"]
        matrix[:3, :3] = np.array([
            [1-2*(y*y+z*z), 2*(x*y-z*w), 2*(x*z+y*w)],
            [2*(x*y+z*w), 1-2*(x*x+z*z), 2*(y*z-x*w)],
            [2*(x*z-y*w), 2*(y*z+x*w), 1-2*(x*x+y*y)]]) * np.array(joint["scale"])
        matrix[:3, 3] = joint["translation"]
        result.append(matrix)
    return result


def worlds(locals_, parents):
    result = []
    for i, parent in enumerate(parents):
        result.append(locals_[i] if parent is None else result[parent] @ locals_[i])
    return np.array(result)


def decompose(matrix):
    """Column-vector quaternion with w >= 0, matching glam's xyzw order."""
    t = matrix[:3, 3].copy()
    scale = np.linalg.norm(matrix[:3, :3], axis=0)
    r = matrix[:3, :3] / scale
    trace = r[0, 0] + r[1, 1] + r[2, 2]
    if trace > 0:
        s = math.sqrt(trace + 1.0) * 2
        q = [(r[2, 1]-r[1, 2])/s, (r[0, 2]-r[2, 0])/s,
             (r[1, 0]-r[0, 1])/s, 0.25*s]
    elif r[0, 0] > r[1, 1] and r[0, 0] > r[2, 2]:
        s = math.sqrt(1.0+r[0, 0]-r[1, 1]-r[2, 2])*2
        q = [0.25*s, (r[0, 1]+r[1, 0])/s,
             (r[0, 2]+r[2, 0])/s, (r[2, 1]-r[1, 2])/s]
    elif r[1, 1] > r[2, 2]:
        s = math.sqrt(1.0+r[1, 1]-r[0, 0]-r[2, 2])*2
        q = [(r[0, 1]+r[1, 0])/s, 0.25*s,
             (r[1, 2]+r[2, 1])/s, (r[0, 2]-r[2, 0])/s]
    else:
        s = math.sqrt(1.0+r[2, 2]-r[0, 0]-r[1, 1])*2
        q = [(r[0, 2]+r[2, 0])/s, (r[1, 2]+r[2, 1])/s,
             0.25*s, (r[1, 0]-r[0, 1])/s]
    q = np.array(q, dtype=np.float64)
    return t, q / np.linalg.norm(q), scale


def unchanged_data(model):
    result = copy.deepcopy(model)
    result["skeleton3d"]["animations"] = [clip for clip in result["skeleton3d"]["animations"]
                                         if clip["name"] != "walk"]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("input", "output", "report"):
        parser.add_argument("--" + flag, type=Path, required=True)
    args = parser.parse_args()
    require(args.output.resolve() != args.input.resolve(), "Never overwrite the input")
    require(args.report.resolve() != args.input.resolve(), "Never overwrite the input")
    require(args.output.resolve() != args.report.resolve(), "Distinct outputs required")
    require(not args.output.exists() and not args.report.exists(), "Outputs must be new")
    converter, calib = load_tools()
    source = args.input.read_bytes()
    model = json.loads(source)
    converter.validate_payload(model)
    original = copy.deepcopy(model)
    skin = calib.Skin(model)
    require(skin.names[0] == "hips" and model["skeleton3d"]["joints"][0]["parent"] is None,
            "Walk grounding requires hips as the root joint")
    clips = {clip["name"]: clip for clip in model["skeleton3d"]["animations"]}
    walk = clips["walk"]
    count = round(walk["duration"] * 60)
    require(count == 48, "Unexpected walk key count")
    times = [walk["channels"][0]["translation"][i]["time"] for i in range(count + 1)]
    # The wire format stores key times at 7 decimals, so 1/60 reads 0.0166667.
    require(all(abs(times[i] - round(i / 60, 7)) < 1e-9 for i in range(len(times))),
            "Expected 60 Hz authored keys")
    require(all(len(channel["translation"]) == count + 1
                and [key["time"] for key in channel["rotation"]] == times
                and [key["time"] for key in channel["scale"]] == times
                for channel in walk["channels"]), "Common key times required")

    joints = model["skeleton3d"]["joints"]
    rest = rest_locals(joints)
    index = {name: i for i, name in enumerate(skin.names)}
    # Same clearance the converter uses for the walk clip.
    clearance = 0.0027
    previous_q = [None] * len(skin.names)
    rows = [[] for _ in skin.names]
    grounds = []
    for frame in range(count + 1):
        time = walk["duration"] if frame == count else walk["duration"] * frame / count
        phase = time / walk["duration"]
        locals_ = [matrix.copy() for matrix in rest]
        for bone, turns in converter.walk_angles(phase).items():
            for axis, degrees in turns:
                locals_[index[bone]] = locals_[index[bone]] @ axis_rotation(axis, degrees)
        for bone, amount in converter.walk_lifts(phase).items():
            locals_[index[bone]][1, 3] += amount
        parts = [decompose(matrix) for matrix in locals_]
        pose = (np.array([part[0] for part in parts]),
                np.array([part[1] for part in parts]),
                np.array([part[2] for part in parts]))
        offset = clearance - float(skin.points_at(pose)[:, 1].min())
        pose[0][0, 1] += offset
        grounds.append(offset)
        for i in range(len(skin.names)):
            q = pose[1][i]
            if previous_q[i] is not None and np.dot(q, previous_q[i]) < 0:
                q = -q
            previous_q[i] = q
            rows[i].append((pose[0][i], q, pose[2][i]))

    channel_by_joint = {channel["joint"]: channel for channel in walk["channels"]}
    expected = {"hips", "spine_01", "head", "leftArm", "rightArm", "leftEar_02", "rightEar_02",
                "leftLeg", "rightLeg", "leftKnee", "rightKnee", "leftFoot", "rightFoot"}
    for i, name in enumerate(skin.names):
        channel = channel_by_joint[name]
        for frame in range(count + 1):
            t, q, s = rows[i][frame]
            channel["translation"][frame]["value"] = [round(float(v), 7) for v in t]
            channel["rotation"][frame]["value"] = [round(float(v), 7) for v in q]
            channel["scale"][frame]["value"] = [round(float(v), 7) for v in s]
    # The loop seams must be exact after rounding.
    for name in skin.names:
        channel = channel_by_joint[name]
        for field in ("translation", "rotation", "scale"):
            require(channel[field][0]["value"] == channel[field][-1]["value"],
                    "Rounded loop seam for " + name + " " + field)

    changed = {name for name in skin.names
               if any(channel_by_joint[name][field][frame]["value"]
                      != next(c for c in next(clip for clip in original["skeleton3d"]["animations"]
                                              if clip["name"] == "walk")["channels"]
                              if c["joint"] == name)[field][frame]["value"]
                      for field in ("translation", "rotation", "scale")
                      for frame in range(count + 1))}
    require(changed == expected, "Walk changed joints: " + ",".join(sorted(changed ^ expected)))

    converter.validate_payload(model)
    require(unchanged_data(model) == unchanged_data(original),
            "Non-walk data changed")
    for name, clip in clips.items():
        if name != "walk":
            before = next(c for c in original["skeleton3d"]["animations"] if c["name"] == name)
            require(digest(clip) == digest(before), "Unrelated clip changed: " + name)

    rows_after = skin.rows(walk)
    minimum = min(float(skin.points_at(calib.sample(rows_after, step / 120))[:, 1].min())
                  for step in range(round(walk["duration"] * 120) + 1))
    require(minimum >= -0.0003, "Walk interpolation clearance failed")

    idle_rows = skin.rows(clips["idle"])
    entry = skin.rows(walk)
    # Runtime crossfades hold the frozen source and blend towards the live clip
    # at 1/60 s steps; probe both directions over the entry second of idle.
    steps = 9
    idle_early = [calib.sample(idle_rows, step / 60) for step in range(steps + 1)]
    into_walk = 1e9
    for source_time in (0, 1 / 60, .1, .2):
        start = calib.sample(idle_rows, source_time)
        for frame in range(steps + 1):
            pose = calib.blend(start, calib.sample(entry, 0), frame / steps)
            into_walk = min(into_walk, float(skin.points_at(pose)[:, 1].min()))
    out_of_walk = 1e9
    for frame in range(count + 1):
        walk_pose = calib.sample(entry, walk["duration"] * frame / count)
        for step in range(steps + 1):
            pose = calib.blend(walk_pose, idle_early[step], step / steps)
            out_of_walk = min(out_of_walk, float(skin.points_at(pose)[:, 1].min()))
    require(into_walk >= -0.0005, "Idle-to-walk crossfade floor: %r" % into_walk)
    require(out_of_walk >= -0.0005, "Walk-to-idle crossfade floor: %r" % out_of_walk)

    output = json.dumps(model, separators=(",", ":"), ensure_ascii=False,
                        allow_nan=False).encode()
    require(len(output) <= converter.MAX_FILE_BYTES, "Asset size budget")
    report = {"source_sha256": hashlib.sha256(source).hexdigest(),
              "output_sha256": hashlib.sha256(output).hexdigest(),
              "changed_joints": sorted(changed),
              "walk_ground_lifts": {"min": min(grounds), "max": max(grounds)},
              "walk_floor_120hz": minimum,
              "crossfade_floor": {"idle_to_walk": into_walk, "walk_to_idle": out_of_walk},
              "crossfade_scope": {"idle_entry_times": [0, 1 / 60, .1, .2],
                                  "idle_return_poses": "first 0.15 s of idle",
                                  "steps": steps},
              "unchanged_data_sha256": digest(unchanged_data(model)),
              "preserved_sections": {name: digest(model["skeleton3d"][name])
                                     for name in ("vertices", "indices", "uvs", "joints", "material")},
              "preserved_images_sha256": digest(model["images"]),
              "preserved_clips": {name: digest(clip) for name, clip in clips.items()
                                  if name != "walk"}}
    with args.output.open("xb") as target:
        target.write(output)
    with args.report.open("x", encoding="utf-8") as target:
        json.dump(report, target, indent=2, allow_nan=False)
        target.write("\n")
    print(json.dumps(report, allow_nan=False))


if __name__ == "__main__":
    main()
