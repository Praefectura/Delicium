"""Regression checks for fail-closed generator validation (stdlib only)."""

import contextlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "generate_bangboo", Path(__file__).with_name("generate_bangboo.py")
)
generator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(generator)


class StructuralFailureTests(unittest.TestCase):
    def test_abort_retains_the_failed_check_and_prior_results(self):
        with contextlib.redirect_stdout(io.StringIO()):
            checker = generator.run_checks({"format": "wrong"}, None, "invalid")
        self.assertEqual(len(checker.checks), 2)
        self.assertTrue(checker.checks[0]["pass"])
        self.assertEqual(checker.checks[1], {"check": "envelope/format", "pass": False})
        self.assertEqual(checker.failures, ["envelope/format: bad format"])

    def test_check_cli_rejects_structural_failure(self):
        with tempfile.TemporaryDirectory(prefix="bangboo-generator-") as folder:
            source = Path(folder) / "invalid.json"
            source.write_text('{"format":"wrong"}', encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()):
                status = generator.main(["--check", str(source)])
            self.assertEqual(status, 1)

    def test_generation_failure_does_not_write_an_asset(self):
        with tempfile.TemporaryDirectory(prefix="bangboo-generator-") as folder:
            destination = Path(folder) / "must-not-exist.json"
            with patch.object(generator, "build_document", return_value=({"format": "wrong"}, None)):
                with contextlib.redirect_stdout(io.StringIO()):
                    status = generator.main(["--output", str(destination)])
            self.assertEqual(status, 1)
            self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main()
