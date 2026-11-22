import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class CoordinatorTests(unittest.TestCase):
    def run_command(self, root, command, timeout=10):
        return subprocess.run([sys.executable, "-B", str(Path(__file__).with_name("bounded_run.py")),
                               "--output", str(root), "--timeout", str(timeout), "--", *command],
                              capture_output=True, text=True, timeout=80)

    def test_detached_success_has_private_receipts_and_log(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "coordinator"
            result = self.run_command(root, [sys.executable, "-c", "print('checked')"])
            self.assertEqual(result.returncode, 0, result.stderr)
            status = json.loads((root / "status.json").read_text())
            self.assertEqual(status["result"], "passed")
            self.assertEqual(status["exit_code"], 0)
            self.assertIn("checked", (root / "launcher.log").read_text())
            for name in ("owner.json", "status.json", "launcher.log"):
                self.assertEqual((root / name).stat().st_mode & 0o777, 0o600)

    def test_command_failure_is_not_reported_as_passed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "coordinator"
            result = self.run_command(root, [sys.executable, "-c", "raise SystemExit(7)"])
            self.assertNotEqual(result.returncode, 0)
            status = json.loads((root / "status.json").read_text())
            self.assertEqual(status["result"], "failed")
            self.assertEqual(status["exit_code"], 7)

    def test_timeout_reaps_owned_command(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "coordinator"
            result = self.run_command(root, [sys.executable, "-c", "import time; time.sleep(30)"], timeout=1)
            self.assertNotEqual(result.returncode, 0)
            status = json.loads((root / "status.json").read_text())
            self.assertEqual(status["error"], "coordinator deadline exceeded")
            self.assertIsInstance(status["exit_code"], int)


if __name__ == "__main__":
    unittest.main()
