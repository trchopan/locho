import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("verify_upgrade", ROOT / "scripts/verify-upgrade.py")
UPGRADE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPGRADE)


class UpgradeVersionTests(unittest.TestCase):
    def output(self, version, commit="a" * 40, state="clean"):
        return f"locho {version} (commit {commit}, {state})"

    def test_default_baseline_stays_stable(self):
        args = UPGRADE.parse_args(["--previous-binary", "old", "--candidate-binary", "new"])
        self.assertEqual(args.previous_version, "1.2.0")

    def test_explicit_prerelease_baseline(self):
        args = UPGRADE.parse_args(["--previous-binary", "old", "--previous-version",
                                   "1.3.0-beta.2", "--candidate-binary", "new"])
        self.assertEqual(args.previous_version, "1.3.0-beta.2")

    def test_exact_stable_and_beta_versions_pass(self):
        for version in ["1.2.0", "1.3.0-beta.2", "1.3.0-beta.3", "1.3.0"]:
            with self.subTest(version=version):
                UPGRADE.check_version(self.output(version), version)

    def test_different_version_is_not_accepted(self):
        with self.assertRaises(UPGRADE.RehearsalFailure):
            UPGRADE.check_version(self.output("1.3.0-beta.2"), "1.2.0")

    def test_dirty_and_unknown_provenance_are_rejected(self):
        for output in [self.output("1.3.0-beta.2", state="dirty"),
                       self.output("1.3.0-beta.2", commit="unknown"),
                       self.output("1.3.0-beta.2", commit="a" * 39)]:
            with self.subTest(output=output), self.assertRaises(UPGRADE.RehearsalFailure):
                UPGRADE.check_version(output, "1.3.0-beta.2")

    def test_expected_version_is_literal_not_a_regex(self):
        with self.assertRaises(UPGRADE.RehearsalFailure):
            UPGRADE.check_version(self.output("1.3.0-beta.2"), "1.3.0-beta.2|.*")

    def test_extra_output_is_not_valid_provenance(self):
        with self.assertRaises(UPGRADE.RehearsalFailure):
            UPGRADE.check_version(self.output("1.3.0-beta.2") + " extra", "1.3.0-beta.2")


if __name__ == "__main__":
    unittest.main()
