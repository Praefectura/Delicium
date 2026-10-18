"""Offline contract and failure tests; no Blender, network, or desktop required."""
import base64
import copy
import importlib.util
import json
import math
from pathlib import Path
import struct
import unittest
import zlib

SPEC = importlib.util.spec_from_file_location("convert_eous", Path(__file__).with_name("convert_eous.py"))
CONVERTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONVERTER)


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def fixture():
    png = (bytes((137, 80, 78, 71, 13, 10, 26, 10))
           + chunk(b"IHDR", struct.pack(">IIBBBBB", 2048, 2048, 8, 2, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(bytes(2048 * (2048 * 3 + 1))))
           + chunk(b"IEND", b""))
    joints = [{"name": name, "parent": None, "translation": [0, 0, 0],
               "rotation": [0, 0, 0, 1], "scale": [1, 1, 1]}
              for name in CONVERTER.EXPECTED_BONES]
    clips = []
    for name, duration in CONVERTER.ACTION_DURATIONS.items():
        channels = []
        for joint in joints:
            channels.append({"joint": joint["name"], "interpolation": "linear",
                             **{field: [{"time": time, "value": joint[field][:]}
                                        for time in (0, duration)]
                                for field in ("translation", "rotation", "scale")}})
        clips.append({"name": name, "duration": duration, "channels": channels})
    return {"format": "delicium.model", "format_version": "0.2", "name": "bangboo",
            "kind": "skeleton3d", "interaction": {"head_joint": "head"},
            "actions": {name: name for name in CONVERTER.ACTION_DURATIONS},
            "images": [{"id": "eous-color", "png_base64": base64.b64encode(png).decode("ascii")}],
            "skeleton3d": {"vertices": [{"position": p, "normal": [0, 0, 1],
                                            "color": [1, 1, 1, 1], "joints": [0, 0, 0, 0],
                                            "weights": [1, 0, 0, 0]}
                                           for p in ([0, 0, 0], [1, 0, 0], [0, 1, 0])],
                           "indices": [0, 1, 2], "uvs": [[0, 0], [1, 0], [0, 1]],
                           "joints": joints, "animations": clips, "texture": "eous-color",
                           "material": {"roughness": 0.65, "metallic": 0.0, "alpha_mode": "opaque"}}}


class ConversionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.valid = fixture()

    def test_walk_is_sagittal_and_exchanges_stance_each_half_cycle(self):
        for step in range(241):
            phase = step / 240
            a = CONVERTER.walk_angles(phase)
            b = CONVERTER.walk_angles(phase + 0.5)
            self.assertEqual(a["hips"], ("Y", 25.0))
            for suffix in ("Leg", "Knee", "Foot", "Arm", "Ear_02"):
                self.assertEqual(a["left" + suffix][0], "X")
                self.assertAlmostEqual(a["left" + suffix][1], b["right" + suffix][1])
            self.assertAlmostEqual(a["leftLeg"][1] + a["rightLeg"][1], -44)
            for side in ("left", "right"):
                self.assertGreaterEqual(a[side + "Knee"][1], 44)
                self.assertLessEqual(a[side + "Knee"][1], 62)
            self.assertAlmostEqual(min(a["leftKnee"][1], a["rightKnee"][1]), 44)
        self.assertEqual(CONVERTER.walk_angles(0), CONVERTER.walk_angles(1))
        self.assertEqual(CONVERTER.walk_angles(0)["leftKnee"][1], 44)
        self.assertEqual(CONVERTER.walk_angles(0)["rightKnee"][1], 62)
        self.assertEqual(CONVERTER.walk_angles(0.5)["leftKnee"][1], 62)
        self.assertAlmostEqual(CONVERTER.walk_angles(0)["leftLeg"][1], -22)
        self.assertAlmostEqual(CONVERTER.walk_angles(0)["rightLeg"][1], -22)

    def test_walk_is_finite_smooth_and_rejects_nonfinite_phase(self):
        previous = CONVERTER.walk_angles(-1 / 10000)
        for step in range(10001):
            current = CONVERTER.walk_angles(step / 10000)
            for bone, (axis, angle) in current.items():
                self.assertTrue(math.isfinite(angle))
                self.assertLess(abs(angle - previous[bone][1]), 0.05)
            previous = current
        for phase in (float("nan"), float("inf"), -float("inf")):
            with self.assertRaisesRegex(ValueError, "walk phase"):
                CONVERTER.walk_angles(phase)

    def test_valid_contract_encodes(self):
        result = CONVERTER.validate_payload(self.valid)
        self.assertEqual(json.loads(result)["name"], "bangboo")

    def test_top_four_is_deterministic_and_reports_loss(self):
        ids, weights, lost = CONVERTER.top_four([(4, 0.1), (0, 0.4), (3, 0.1), (2, 0.2), (1, 0.2)])
        self.assertEqual(ids, [0, 1, 2, 3])
        self.assertAlmostEqual(sum(weights), 1)
        self.assertAlmostEqual(lost, 0.1)

    def test_invalid_influences_are_rejected(self):
        for influences in ([], [(0, -1)], [(0, float("nan"))], [(0, 1), (0, 1)]):
            with self.subTest(influences=influences), self.assertRaises(ValueError):
                CONVERTER.top_four(influences)

    def test_merging_equivalent_joint_trajectories_preserves_deformation(self):
        import numpy as np
        full = np.array([[0.4, 0.2, 0.15, 0.15, 0.1]])
        palettes = np.tile(np.eye(4), (2, 5, 1, 1))
        palettes[1, :, 0, 3] = [0, 1, 2, 3, 0]
        points = np.array([[0, 0, 0, 1]])
        reduced, merged = CONVERTER.merge_similar_influences(full, palettes, points)
        self.assertEqual(merged, 1)
        self.assertEqual(np.count_nonzero(reduced), 4)
        self.assertAlmostEqual(float(reduced.sum()), 1)
        moved = np.einsum("pbij,vj->pvbi", palettes, points)
        error = np.einsum("vb,pvbi->pvi", full - reduced, moved)
        np.testing.assert_allclose(error, 0, atol=1e-12)

    def test_eyes_move_as_one_head_without_ear_or_spine_blending(self):
        head = CONVERTER.EXPECTED_BONES.index("head")
        rows = [[0.0] * 22 for _ in range(480)]
        for row in rows:
            row[2], row[head], row[4] = 0.7, 0.1, 0.2
        original = copy.deepcopy(rows)
        indices = [v for base in (0, 240) for i in range(1, 239)
                   for v in (base, base + i, base + i + 1)]
        adapted, report = CONVERTER.adapt_eous_head("eyes", rows, list(range(480)), indices)
        self.assertEqual(rows, original)
        self.assertEqual(len(report["rigid_source_vertices"]), 480)
        for row in adapted:
            self.assertEqual(row[head], 1)
            self.assertEqual(sum(row), 1)
        with self.assertRaisesRegex(ValueError, "eye topology"):
            CONVERTER.adapt_eous_head("eyes", rows, list(range(480)), indices[:-3])

    def test_neck_blends_by_welded_topology_and_preserves_ears_and_torso(self):
        head = CONVERTER.EXPECTED_BONES.index("head")
        rows = [[0.0] * 22 for _ in range(8)]
        for row in rows:
            row[2] = 1
        for i in (6, 7):
            rows[i][2], rows[i][4] = 0, 1
        adjacency = [{1}, {0, 2}, {1, 3}, {2, 4}, {3, 5}, {4, 6}, {5, 7}, {6}]
        original = copy.deepcopy(rows)
        adapted, distances = CONVERTER.blend_head_region(
            rows, adjacency, {3, 4, 5}, {6, 7}, {4, 5}, head)
        self.assertEqual(rows, original)
        self.assertEqual(distances, {2: 0, 1: 1})
        self.assertAlmostEqual(adapted[1][head], 1 / 3)
        self.assertAlmostEqual(adapted[2][head], 2 / 3)
        self.assertEqual(adapted[3][head], 1)
        self.assertEqual(adapted[4][head], 1)
        self.assertEqual(adapted[5][head], 1)
        for i in (0, 6, 7):
            self.assertEqual(adapted[i], rows[i])
        for row in adapted:
            self.assertAlmostEqual(sum(row), 1)
            self.assertTrue(all(w >= 0 for w in row))

    def test_unknown_body_and_invalid_head_selections_fail_closed(self):
        with self.assertRaisesRegex(ValueError, "body topology"):
            CONVERTER.adapt_eous_head("body", [[1.0] + [0.0] * 21] * 3,
                                      [0, 1, 2], [0, 1, 2])
        rows = [[1.0, 0.0], [1.0, 0.0]]
        for selected, protected, face in (({0}, {0}, set()), ({0}, set(), {1}),
                                           (set(), set(), set())):
            with self.subTest(selected=selected), self.assertRaises(ValueError):
                CONVERTER.blend_head_region(rows, [{1}, {0}], selected, protected, face, 1)
        with self.assertRaisesRegex(ValueError, "full head weights"):
            CONVERTER.blend_head_region([[float("nan"), 0], [1, 0]],
                                        [{1}, {0}], {0}, set(), set(), 1)
        untouched, report = CONVERTER.adapt_eous_head("handkerchief", rows, [], [])
        self.assertIs(untouched, rows)
        self.assertIsNone(report)

    def test_topology_components_are_deterministic_and_reject_bad_indices(self):
        components, adjacency = CONVERTER.triangle_components(7, [0, 1, 2, 3, 4, 5])
        self.assertEqual(components, [{0, 1, 2}, {3, 4, 5}, {6}])
        self.assertEqual(adjacency[0], {1, 2})
        for indices in ([0, 1], [0, 1, 7], [0, 1, -1]):
            with self.subTest(indices=indices), self.assertRaises(ValueError):
                CONVERTER.triangle_components(7, indices)

    def test_invalid_payloads_are_rejected_before_serialization(self):
        def index(model):
            model["skeleton3d"]["indices"][0] = 100

        def weights(model):
            model["skeleton3d"]["vertices"][0]["weights"][0] = 0.5

        def joint(model):
            model["skeleton3d"]["vertices"][0]["joints"][0] = 22

        def uv(model):
            model["skeleton3d"]["uvs"][0][0] = 1.1

        def normal(model):
            model["skeleton3d"]["vertices"][0]["normal"] = [0, 0, 0]

        def alpha(model):
            model["skeleton3d"]["material"]["alpha_mode"] = "blend"

        def nan(model):
            model["skeleton3d"]["vertices"][0]["position"][0] = float("nan")

        def scale(model):
            model["skeleton3d"]["animations"][0]["channels"][0]["scale"][0]["value"][0] = 0

        def missing_action(model):
            del model["actions"]["greet"]

        for mutate in (index, weights, joint, uv, normal, alpha, nan, scale, missing_action):
            with self.subTest(case=mutate.__name__):
                model = copy.deepcopy(self.valid)
                mutate(model)
                with self.assertRaises(ValueError):
                    CONVERTER.validate_payload(model)

    def test_runtime_file_limit_is_not_relaxed(self):
        model = copy.deepcopy(self.valid)
        model["name"] = "bangboo"
        model["padding"] = "x" * CONVERTER.MAX_FILE_BYTES
        with self.assertRaisesRegex(ValueError, "8 MiB"):
            CONVERTER.validate_payload(model)


if __name__ == "__main__":
    unittest.main()
