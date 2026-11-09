"""Cargo config preservation for the mobile runtime handoff; no native build."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import tomllib
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "prepare_ocr_cargo_env.py"
SPEC = importlib.util.spec_from_file_location("prepare_ocr_cargo_env", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class CargoEnvironmentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.project = self.root / "src-tauri"
        self.project.mkdir()
        (self.project / "Cargo.toml").write_text('[package]\nname = "test"\nversion = "0.0.0"\n')
        self.runtime = self.root / 'runtime "quoted" é 📄'
        self.runtime.mkdir()
        # Placeholder files test config selection only, not binary validity.
        (self.runtime / "libcrispembed.a").write_bytes(b"placeholder")
        self.config = self.project / ".cargo" / "config.toml"

    def write_config(self, text: str) -> None:
        self.config.parent.mkdir(exist_ok=True)
        self.config.write_text(text, encoding="utf-8")

    def test_creates_real_toml_with_absolute_forced_path_and_is_idempotent(self) -> None:
        self.assertEqual(MODULE.prepare(self.project, self.runtime), self.config)
        original = self.config.read_bytes()
        parsed = tomllib.loads(original.decode("utf-8"))
        self.assertEqual(parsed, {"env": {MODULE.VARIABLE: {"value": str(self.runtime), "force": True}}})
        MODULE.prepare(self.project, self.runtime)
        self.assertEqual(self.config.read_bytes(), original)

    def test_preserves_comments_targets_existing_env_and_mode(self) -> None:
        original = (
            '# Retain this comment and formatting.\n'
            '[target.aarch64-linux-android]\n'
            'rustflags = ["-Clink-arg=-Wl,-z,max-page-size=16384"]\n'
            '[env]\n'
            'KEEP = { value = "unchanged", force = true }\n'
            '[build]\n'
            'jobs = 2\n'
        )
        self.write_config(original)
        self.config.chmod(0o640)
        MODULE.prepare(self.project, self.runtime)
        written = self.config.read_text(encoding="utf-8")
        self.assertTrue(written.startswith(original))
        result = tomllib.loads(written)
        self.assertEqual(result["env"].pop(MODULE.VARIABLE), {"value": str(self.runtime), "force": True})
        self.assertEqual(result, tomllib.loads(original))
        self.assertEqual(self.config.stat().st_mode & 0o777, 0o640)

    def test_conflicting_values_are_unchanged(self) -> None:
        cases = (
            '[env]\nCRISPEMBED_SYS_LIB_DIR = "/other"\n',
            '[env.CRISPEMBED_SYS_LIB_DIR]\nvalue = "/other"\nforce = true\n',
            f'[env.CRISPEMBED_SYS_LIB_DIR]\nvalue = {MODULE.json.dumps(str(self.runtime), ensure_ascii=False)}\nforce = false\n',
            f'[env.CRISPEMBED_SYS_LIB_DIR]\nvalue = {MODULE.json.dumps(str(self.runtime), ensure_ascii=False)}\n',
        )
        for original in cases:
            with self.subTest(original=original):
                self.write_config(original)
                with self.assertRaisesRegex(ValueError, "conflicts"):
                    MODULE.prepare(self.project, self.runtime)
                self.assertEqual(self.config.read_text(encoding="utf-8"), original)

    def test_legacy_active_config_is_preserved_without_shadow_file(self) -> None:
        self.write_config('[net]\noffline = true\n')
        legacy = self.config.with_name("config")
        self.config.rename(legacy)
        self.assertEqual(MODULE.prepare(self.project, self.runtime), legacy)
        self.assertFalse(self.config.exists())
        self.assertTrue(tomllib.loads(legacy.read_text())["net"]["offline"])

    def test_dual_config_files_are_refused_without_changes(self) -> None:
        self.write_config("# current\n")
        legacy = self.config.with_name("config")
        legacy.write_text("# legacy\n")
        with self.assertRaisesRegex(ValueError, "both"):
            MODULE.prepare(self.project, self.runtime)
        self.assertEqual(self.config.read_text(), "# current\n")
        self.assertEqual(legacy.read_text(), "# legacy\n")

    def test_invalid_and_closed_inline_toml_are_not_rewritten(self) -> None:
        for original in ('[invalid\n', 'env = { KEEP = "unchanged" }\n', 'env = "invalid type"\n'):
            with self.subTest(original=original):
                self.write_config(original)
                with self.assertRaises(ValueError):
                    MODULE.prepare(self.project, self.runtime)
                self.assertEqual(self.config.read_text(), original)

    def test_missing_runtime_or_relative_path_does_not_create_config(self) -> None:
        (self.runtime / "libcrispembed.a").unlink()
        with self.assertRaisesRegex(ValueError, "must contain"):
            MODULE.prepare(self.project, self.runtime)
        with self.assertRaisesRegex(ValueError, "absolute"):
            MODULE.prepare(self.project, Path("relative/runtime"))
        self.assertFalse(self.config.parent.exists())

    def test_android_shared_runtime_is_accepted(self) -> None:
        (self.runtime / "libcrispembed.a").unlink()
        (self.runtime / "libcrispembed.so").write_bytes(b"placeholder")
        MODULE.prepare(self.project, self.runtime)
        self.assertEqual(tomllib.loads(self.config.read_text())["env"][MODULE.VARIABLE]["value"], str(self.runtime))

    def test_symlink_config_is_not_followed(self) -> None:
        self.config.parent.mkdir()
        outside = self.root / "external.toml"
        outside.write_text("# untouched\n")
        self.config.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "symlink"):
            MODULE.prepare(self.project, self.runtime)
        self.assertEqual(outside.read_text(), "# untouched\n")
        self.assertTrue(self.config.is_symlink())


if __name__ == "__main__":
    unittest.main()
