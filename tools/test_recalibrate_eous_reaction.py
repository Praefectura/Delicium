"""Pure numerical and preservation checks for the clip-only calibration tool."""
import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import numpy as np

spec = importlib.util.spec_from_file_location(
    "reaction_calibration", Path(__file__).with_name("recalibrate_eous_reaction.py"))
calibration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(calibration)


class ReactionCalibrationTests(unittest.TestCase):
    def test_slerp_handles_identity_and_antipodal_quaternions(self):
        identity = np.array([[0., 0., 0., 1.]])
        for alpha in (0., .25, .5, 1.):
            np.testing.assert_allclose(calibration.slerp(identity, -identity, alpha), identity)
        turn = np.array([[0., np.sin(.3), 0., np.cos(.3)]])
        halfway = calibration.slerp(identity, turn, .5)
        np.testing.assert_allclose(halfway, [[0., np.sin(.15), 0., np.cos(.15)]])
        np.testing.assert_allclose(np.linalg.norm(halfway, axis=1), 1.)

    def test_sampling_nonuniform_keys_clamps_and_interpolates(self):
        def pose(x):
            return (np.array([[x, 0., 0.]]), np.array([[0., 0., 0., 1.]]), np.ones((1, 3)))
        rows = (np.array([0., .2, 1.]), [pose(0.), pose(2.), pose(10.)])
        self.assertEqual(calibration.sample(rows, -.1)[0][0, 0], 0.)
        self.assertEqual(calibration.sample(rows, 2.)[0][0, 0], 10.)
        self.assertAlmostEqual(calibration.sample(rows, .6)[0][0, 0], 6.)

    def test_skin_rest_is_identity_and_parent_translation_reaches_child(self):
        joints = [{"name": "hips", "parent": None, "translation": [0., .3, 0.],
                   "rotation": [0., 0., 0., 1.], "scale": [1., 1., 1.]},
                  {"name": "head", "parent": "hips", "translation": [0., .5, 0.],
                   "rotation": [0., 0., 0., 1.], "scale": [1., 1., 1.]}]
        model = {"skeleton3d": {"joints": joints, "vertices": [
            {"position": [.1, .8, 0.], "joints": [1, 0, 0, 0], "weights": [1., 0., 0., 0.]}]}}
        skin = calibration.Skin(model)
        pose = tuple(np.array([joint[field] for joint in joints])
                     for field in ("translation", "rotation", "scale"))
        np.testing.assert_allclose(skin.points_at(pose), [[.1, .8, 0.]])
        pose[0][0, 1] += .07
        np.testing.assert_allclose(skin.points_at(pose), [[.1, .87, 0.]])

    def test_preservation_excludes_only_reaction_not_other_asset_data(self):
        model = {"images": ["texture"], "skeleton3d": {"vertices": [[1, 2, 3]],
                 "animations": [{"name": "idle", "value": 1},
                                {"name": "reaction", "value": 2}]}}
        changed = copy.deepcopy(model)
        changed["skeleton3d"]["animations"][1]["value"] = 3
        self.assertEqual(calibration.unchanged_data(model), calibration.unchanged_data(changed))
        changed["images"] = ["other"]
        self.assertNotEqual(calibration.unchanged_data(model), calibration.unchanged_data(changed))
        self.assertEqual(model["skeleton3d"]["animations"][1]["value"], 2)

    def test_fit_checks_return_poses_and_uses_the_unique_source_vertex_domain(self):
        joints = [{"name": "hips", "parent": None, "translation": [0., 0., 0.],
                   "rotation": [0., 0., 0., 1.], "scale": [1., 1., 1.]},
                  {"name": "head", "parent": "hips", "translation": [0., .5, 0.],
                   "rotation": [0., 0., 0., 1.], "scale": [1., 1., 1.]}]
        vertex = {"position": [0., .5, 0.], "joints": [1, 0, 0, 0], "weights": [1., 0., 0., 0.]}
        root = {"position": [.1, 0., 0.], "joints": [0, 0, 0, 0], "weights": [1., 0., 0., 0.]}
        skin = calibration.Skin({"skeleton3d": {"joints": joints, "vertices": [vertex, vertex, root]}})
        pose = tuple(np.array([joint[field] for joint in joints])
                     for field in ("translation", "rotation", "scale"))
        rows = (np.array([0., 1.]), [pose, pose])
        returning = tuple(part.copy() for part in pose)
        returning[0][1, 1] += .01
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory) / "weights.npz"
            np.savez(cache, points=skin.points, joint_names=np.array(skin.names),
                     full_weights=np.array([[.5, .5], [.5, .5], [1., 0.]]),
                     four_weights=np.array([[0., 1.], [0., 1.], [1., 0.]]),
                     export_source_rows=np.array([0, 0, 1]), source_vertex_count=np.array(2))
            result = calibration.validate_fit(skin, rows, cache, [returning])
            self.assertEqual(result["samples"], 610)
            self.assertAlmostEqual(result["maximum"], .005)
            self.assertAlmostEqual(result["mean"], .005 / (2 * 122))
            returning[0][1, 1] += .1
            with self.assertRaisesRegex(ValueError, "fit budget exceeded"):
                calibration.validate_fit(skin, rows, cache, [returning])

    def test_existing_output_is_rejected_before_input_is_read(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "existing.json"
            output.write_text("retain", encoding="utf-8")
            args = ["calibration", "--input", str(Path(directory) / "absent.json"),
                    "--output", str(output), "--report", str(Path(directory) / "report.json")]
            with patch("sys.argv", args), self.assertRaisesRegex(ValueError, "Outputs must be new"):
                calibration.main()
            self.assertEqual(output.read_text(encoding="utf-8"), "retain")


if __name__ == "__main__":
    unittest.main()
