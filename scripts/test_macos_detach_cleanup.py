import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import replay_macos_package_validation as replay


class DetachCleanupTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.mount = self.root / "mount"
        self.mount.mkdir()

    def simulated_detach(self, exit_code, stderr, stdout=""):
        original = replay.command

        def command(exe, args, env, output, label, timeout):
            self.assertEqual(exe, Path("/usr/bin/hdiutil"))
            self.assertEqual(args, ["detach", str(self.mount)])
            self.assertEqual(timeout, 30)
            self.assertNotIn("GH_TOKEN", env)
            self.assertNotIn("HTTPS_PROXY", env)
            code = "import sys;sys.stdout.write(sys.argv[1]);sys.stderr.write(sys.argv[2]);sys.exit(int(sys.argv[3]))"
            return original(
                sys.executable,
                ["-c", code, stdout, stderr, str(exit_code)],
                env,
                output,
                label,
                timeout=2,
            )

        return command

    def test_normal_detach_requires_exit_zero_complete_evidence_and_mount_absence(self):
        with patch.object(Path, "is_mount", side_effect=[True, False]), patch.object(
            replay,
            "command",
            side_effect=self.simulated_detach(0, "", '"disk42" ejected.\n'),
        ) as command:
            result = replay.cleanup_mount(self.mount, self.root)
        self.assertEqual(result["status"], "detached")
        command.assert_called_once()
        self.assertFalse(result["mount_present_after"])
        self.assertEqual(
            (self.root / "dmg-detach.stdout.txt").read_text(), '"disk42" ejected.\n'
        )

    def test_busy_failure_retains_bounded_redacted_stderr_without_force_or_retry(self):
        stderr = (
            'hdiutil: detach failed - Resource busy\n{"password":"cleanup-secret"}\n'
        )
        with patch.object(Path, "is_mount", side_effect=[True, True]), patch.object(
            replay,
            "command",
            side_effect=self.simulated_detach(16, stderr, "partial detach\n"),
        ) as command:
            result = replay.cleanup_mount(self.mount, self.root)
        self.assertEqual(result["status"], "failed")
        command.assert_called_once()
        diagnostic = (self.root / "dmg-detach.stderr.txt").read_text()
        self.assertIn("Resource busy", diagnostic)
        self.assertNotIn("cleanup-secret", diagnostic)
        saved = json.loads((self.root / "dmg-cleanup.json").read_text())
        self.assertEqual(saved, result)
        self.assertEqual(saved["command"]["exit_code"], 16)

    def test_exit_zero_cannot_hide_a_mount_that_remains_present(self):
        with patch.object(Path, "is_mount", side_effect=[True, True]), patch.object(
            replay, "command", side_effect=self.simulated_detach(0, "")
        ):
            result = replay.cleanup_mount(self.mount, self.root)
        self.assertEqual(result["status"], "failed")

    def test_unowned_symlink_or_inactive_mount_is_never_detached(self):
        other = self.root / "other"
        other.mkdir()
        with patch.object(replay, "command") as command:
            self.assertEqual(replay.cleanup_mount(other, self.root)["status"], "failed")
            with patch.object(Path, "is_mount", return_value=False):
                result = replay.cleanup_mount(self.mount, self.root)
            self.assertFalse(result["attempted"])
            self.assertEqual(result["status"], "failed")
            self.mount.rmdir()
            self.mount.symlink_to(other)
            self.assertEqual(
                replay.cleanup_mount(self.mount, self.root)["status"], "failed"
            )
            command.assert_not_called()

    def test_cleanup_error_cannot_erase_original_probe_failure_or_observation(self):
        result = {
            "status": "failed",
            "error": "original doctor mismatch",
            "commands": [{"label": "retained-retry"}],
            "original_ocr_results": {"original": False},
            "retry_ocr_results": {"retry": True},
        }
        with patch.object(
            replay, "cleanup_mount", side_effect=OSError("recording failed")
        ):
            replay.finalize_cleanup(result, self.mount, self.root)
        saved = json.loads((self.root / "observation.json").read_text())
        self.assertEqual(saved["error"], "original doctor mismatch")
        self.assertEqual(saved["retry_ocr_results"], {"retry": True})
        self.assertEqual(saved["status"], "failed")
        self.assertTrue(saved["detach_failed"])
        with patch.object(replay, "cleanup_mount", return_value={"status": "detached"}):
            replay.finalize_cleanup(result, self.mount, self.root)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["error"], "original doctor mismatch")

    def test_timeout_and_pipe_failure_cannot_qualify_cleanup(self):
        for flag in [
            "timed_out",
            "output_pipe_failure",
            "diagnostics_rejected",
            "output_truncated",
        ]:
            record = {"exit_code": 0, flag: True}
            with patch.object(
                Path, "is_mount", side_effect=[True, False]
            ), patch.object(replay, "command", return_value=(record, {})):
                result = replay.cleanup_mount(self.mount, self.root)
            self.assertEqual(result["status"], "failed")
            # Each iteration receives a fresh private cleanup HOME.
            (self.root / "cleanup-home").rmdir()


if __name__ == "__main__":
    unittest.main()
