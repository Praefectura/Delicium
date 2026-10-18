"""Calibrate Eous reaction entry/return without re-exporting its mesh.

Use Python -I. Input is the preserved v18 JSON, outputs must be new files.
Only hips/leg tracks near the reaction endpoints may change. Runtime action
playback, the central gesture, and all non-reaction asset data stay unchanged.
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


def slerp(a, b, alpha):
    a = a / np.linalg.norm(a, axis=-1, keepdims=True)
    b = b / np.linalg.norm(b, axis=-1, keepdims=True)
    dots = (a * b).sum(axis=-1)
    b = np.where((dots < 0)[:, None], -b, b)
    dots = np.abs(dots).clip(0, 1)
    angle = np.arccos(dots)
    denominator = np.maximum(np.sin(angle), 1e-12)
    result = (np.sin((1 - alpha) * angle)[:, None] * a
              + np.sin(alpha * angle)[:, None] * b) / denominator[:, None]
    result = np.where((dots > .9995)[:, None], a * (1 - alpha) + b * alpha, result)
    return result / np.linalg.norm(result, axis=-1, keepdims=True)


def blend(a, b, alpha):
    return (a[0] * (1 - alpha) + b[0] * alpha,
            slerp(a[1], b[1], alpha),
            a[2] * (1 - alpha) + b[2] * alpha)


def multiply(a, b):
    av, bv = a[:3], b[:3]
    return np.append(a[3] * bv + b[3] * av + np.cross(av, bv),
                     a[3] * b[3] - np.dot(av, bv))


class Skin:
    def __init__(self, model):
        self.mesh = model["skeleton3d"]
        self.names = [joint["name"] for joint in self.mesh["joints"]]
        lookup = {name: i for i, name in enumerate(self.names)}
        self.parents = [lookup.get(joint["parent"]) for joint in self.mesh["joints"]]
        require(all(parent is None or parent < i for i, parent in enumerate(self.parents)),
                "Joints must be parent-before-child")
        self.points = np.array([vertex["position"] + [1.] for vertex in self.mesh["vertices"]])
        self.ids = np.array([vertex["joints"] for vertex in self.mesh["vertices"]])
        self.weights = np.array([vertex["weights"] for vertex in self.mesh["vertices"]])
        rest = tuple(np.array([joint[field] for joint in self.mesh["joints"]])
                     for field in ("translation", "rotation", "scale"))
        self.inverse_rest = np.linalg.inv(self.worlds(rest))

    def worlds(self, pose):
        translation, rotation, scale = pose
        rotation = rotation / np.linalg.norm(rotation, axis=-1, keepdims=True)
        x, y, z, w = rotation.T
        local = np.tile(np.eye(4), (len(self.names), 1, 1))
        local[:, :3, :3] = np.array([
            [1-2*(y*y+z*z), 2*(x*y-z*w), 2*(x*z+y*w)],
            [2*(x*y+z*w), 1-2*(x*x+z*z), 2*(y*z-x*w)],
            [2*(x*z-y*w), 2*(y*z+x*w), 1-2*(x*x+y*y)],
        ]).transpose(2, 0, 1) * scale[:, None, :]
        local[:, :3, 3] = translation
        result = []
        for i, parent in enumerate(self.parents):
            result.append(local[i] if parent is None else result[parent] @ local[i])
        return np.array(result)

    def palette(self, pose):
        return self.worlds(pose) @ self.inverse_rest

    def points_at(self, pose):
        moved = np.einsum("vkij,vj->vki", self.palette(pose)[self.ids], self.points)
        return np.einsum("vk,vki->vi", self.weights, moved)[:, :3]

    def rows(self, clip):
        channels = {channel["joint"]: channel for channel in clip["channels"]}
        times = [key["time"] for key in channels[self.names[0]]["translation"]]
        require(all([key["time"] for key in channels[name][field]] == times
                    for name in self.names for field in ("translation", "rotation", "scale")),
                "Expected common authored key times")
        poses = [tuple(np.array([channels[name][field][i]["value"] for name in self.names])
                       for field in ("translation", "rotation", "scale"))
                 for i in range(len(times))]
        return np.array(times), poses


def sample(rows, time):
    times, poses = rows
    time = min(max(time, times[0]), times[-1])
    index = min(int(np.searchsorted(times, time, side="right")) - 1, len(times) - 2)
    alpha = (time - times[index]) / (times[index + 1] - times[index])
    return blend(poses[index], poses[index + 1], alpha)


def unchanged_data(model):
    result = copy.deepcopy(model)
    result["skeleton3d"]["animations"] = [clip for clip in result["skeleton3d"]["animations"]
                                            if clip["name"] != "reaction"]
    return result


def validate_fit(skin, rows, cache_path, return_poses=()):
    with np.load(cache_path, allow_pickle=False) as cache:
        require(list(cache["joint_names"]) == skin.names, "Fit-cache joint order")
        require(cache["points"].shape == skin.points.shape
                and np.allclose(cache["points"], skin.points, atol=1e-6, rtol=0),
                "Fit-cache source position alignment")
        full = cache["full_weights"].copy()
        four = cache["four_weights"].copy()
        mapping = cache["export_source_rows"].copy()
        source_count = int(cache["source_vertex_count"])
    dense = np.zeros_like(full)
    for i in range(len(skin.points)):
        np.add.at(dense[i], skin.ids[i], skin.weights[i])
    require(np.allclose(dense, four, atol=1e-12, rtol=0), "Fit-cache exported weights")
    source_ids, first = np.unique(mapping, return_index=True)
    require(np.array_equal(source_ids, np.arange(source_count)), "Incomplete source fit domain")
    difference = (full - four)[first]
    affected = np.flatnonzero(np.any(np.abs(difference) > 1e-12, axis=1))
    points = skin.points[first][affected]
    difference = difference[affected]
    maximum = total = 0.
    samples = 0
    head = skin.names.index("head")
    poses = [sample(rows, step / 120) for step in range(121)] + list(return_poses)
    for base in poses:
        for degrees in (0, -60, -30, 30, 60):
            pose = tuple(part.copy() for part in base)
            angle = math.radians(degrees) / 2
            pose[1][head] = multiply(np.array([0., math.sin(angle), 0., math.cos(angle)]),
                                     pose[1][head])
            moved = np.einsum("bij,vj->bvi", skin.palette(pose), points)[:, :, :3]
            errors = np.linalg.norm(np.einsum("vb,bvi->vi", difference, moved), axis=1)
            maximum = max(maximum, float(errors.max(initial=0)))
            total += float(errors.sum())
            samples += 1
    mean = total / (source_count * samples)
    require(maximum <= .0075 and mean <= .0005, "New-pose full/four fit budget exceeded")
    return {"maximum": maximum, "mean": mean, "samples": samples,
            "source_vertices": source_count,
            "domain": "new reaction at 120Hz and offline return probes, five head yaws"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("input", "output", "report"):
        parser.add_argument("--" + flag, type=Path, required=True)
    parser.add_argument("--weights", type=Path)
    parser.add_argument("--entry-key", type=int, default=1)
    parser.add_argument("--tail-seconds", type=float, default=.15)
    args = parser.parse_args()
    require(args.output.resolve() != args.input.resolve(), "Never overwrite the input")
    require(not args.output.exists() and not args.report.exists(), "Outputs must be new")
    require(args.output.resolve() != args.report.resolve(), "Distinct outputs required")
    require(args.report.resolve() != args.input.resolve(), "Never overwrite the input")
    require(args.entry_key in (0, 1, 2) and 0 < args.tail_seconds <= .3, "Calibration bounds")
    source = args.input.read_bytes()
    model = json.loads(source)
    spec = importlib.util.spec_from_file_location("eous_converter", Path(__file__).with_name("convert_eous.py"))
    converter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(converter)
    converter.validate_payload(model)
    original = copy.deepcopy(model)
    skin = Skin(model)
    require(skin.names[0] == "hips" and model["skeleton3d"]["joints"][0]["parent"] is None,
            "Ground correction requires hips as the root joint")
    clips = {clip["name"]: clip for clip in model["skeleton3d"]["animations"]}
    reaction = clips["reaction"]
    original_rows = skin.rows(reaction)
    endpoint = sample(skin.rows(clips["idle"]), args.entry_key / 60)
    changed_names = {"hips", "leftLeg", "leftKnee", "leftFoot", "rightLeg", "rightKnee", "rightFoot"}
    changed_ids = [i for i, name in enumerate(skin.names) if name in changed_names]
    channels = {channel["joint"]: channel for channel in reaction["channels"]}
    lifts = []
    for index, time in enumerate(original_rows[0]):
        distance = min(time, reaction["duration"] - time)
        progress = min(max(distance / args.tail_seconds, 0.), 1.)
        weight = 1 - progress * progress * (3 - 2 * progress)
        if weight == 0:
            continue
        pose = tuple(part.copy() for part in original_rows[1][index])
        target = blend(pose, endpoint, weight)
        for part, values in zip(pose, target):
            part[changed_ids] = values[changed_ids]
        lift = max(0., .002 - float(skin.points_at(pose)[:, 1].min()))
        pose[0][0, 1] += lift
        lifts.append(lift)
        for joint in changed_ids:
            for field, values in zip(("translation", "rotation", "scale"), pose):
                channels[skin.names[joint]][field][index]["value"] = [round(float(v), 7) for v in values[joint]]
    for name in changed_names:
        for field in ("translation", "rotation", "scale"):
            channels[name][field][-1]["value"] = channels[name][field][0]["value"].copy()
    converter.validate_payload(model)
    require(unchanged_data(model) == unchanged_data(original), "Non-reaction data changed")
    original_reaction = next(clip for clip in original["skeleton3d"]["animations"] if clip["name"] == "reaction")
    for channel in reaction["channels"]:
        before = next(c for c in original_reaction["channels"] if c["joint"] == channel["joint"])
        if channel["joint"] not in changed_names:
            require(channel == before, "Unrelated reaction track changed")
        for field in ("translation", "rotation", "scale"):
            for key, old in zip(channel[field], before[field]):
                if args.tail_seconds <= key["time"] <= 1 - args.tail_seconds:
                    require(key == old, "Central reaction gesture changed")
    rows = skin.rows(reaction)
    minimum = min(float(skin.points_at(sample(rows, step / 120))[:, 1].min()) for step in range(121))
    require(minimum >= -.0003, "Reaction interpolation clearance failed")
    idle_rows = skin.rows(clips["idle"])
    # Offline probes bracket the final fixed-step source, not an exact runtime replay.
    # The sustained-idle Rust regression checks the actual f32 clock and display poses.
    returns = []
    for source_time in (58 / 60, 59 / 60, 1 - 1e-6):
        source_pose = sample(rows, source_time)
        committed = [blend(source_pose, sample(idle_rows, step / 60), step / 9) for step in range(10)]
        returns.extend(committed)
        returns.extend(blend(a, b, .5) for a, b in zip(committed, committed[1:]))
    return_minimum = min(float(skin.points_at(pose)[:, 1].min()) for pose in returns)
    require(return_minimum >= -.0003, "Reaction-to-idle margin failed")
    fit = validate_fit(skin, rows, args.weights, returns) if args.weights else None
    output = json.dumps(model, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()
    require(len(output) <= converter.MAX_FILE_BYTES, "Asset size budget")
    report = {"source_sha256": hashlib.sha256(source).hexdigest(),
              "output_sha256": hashlib.sha256(output).hexdigest(),
              "entry_key": args.entry_key, "tail_seconds": args.tail_seconds,
              "changed_joints": sorted(changed_names), "max_offline_ground_lift": max(lifts, default=0),
              "reaction_floor_120hz": minimum, "offline_return_and_half_step_floor": return_minimum,
              "offline_return_source_times": [58 / 60, 59 / 60, 1 - 1e-6],
              "unchanged_data_sha256": digest(unchanged_data(model)), "new_pose_fit": fit,
              "preserved_sections": {name: digest(model["skeleton3d"][name])
                                     for name in ("vertices", "indices", "uvs", "joints", "material")},
              "preserved_images_sha256": digest(model["images"]),
              "preserved_clips": {name: digest(clip) for name, clip in clips.items() if name != "reaction"}}
    with args.output.open("xb") as target:
        target.write(output)
    with args.report.open("x", encoding="utf-8") as target:
        json.dump(report, target, indent=2, allow_nan=False)
        target.write("\n")
    print(json.dumps(report, allow_nan=False))


if __name__ == "__main__":
    main()
