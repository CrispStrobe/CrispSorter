"""Keep native decoder qualification tied to the same immutable input bytes."""

import base64
import hashlib
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "tests/fixtures/mobile-documents"
SWIFT = ROOT / "crates/tauri-plugin-mobile-provider/ios/Tests/Fixtures"


class MobileDocumentFixtures(unittest.TestCase):
    def test_declared_binary_inputs_match_their_pinned_hashes_and_formats(self):
        manifest = json.loads((FIXTURES / "manifest.json").read_text())
        entries = [manifest, *manifest["images"], *manifest["pdf_variants"]]
        self.assertEqual(len(entries), 6)
        for entry in entries:
            with self.subTest(filename=entry["filename"]):
                filename = entry.get("fixture", entry["filename"])
                encoded = (FIXTURES / filename).read_bytes()
                data = base64.b64decode(encoded.strip(), validate=True) if filename.endswith(".b64") else encoded
                self.assertEqual(len(data), entry["bytes"])
                self.assertEqual(hashlib.sha256(data).hexdigest(), entry["sha256"])
                if entry["filename"].endswith(".pdf"):
                    self.assertTrue(data.startswith(b"%PDF-"))
                    self.assertIn(b"%%EOF", data[-30:])
                    self.assertEqual(b"/Encrypt" in data, entry.get("password_protected", False))
                elif entry["filename"].endswith(".avif"):
                    self.assertEqual(data[4:12], b"ftypavif")
                elif entry["filename"].endswith(".heic"):
                    self.assertEqual(data[4:12], b"ftypheic")
                    self.assertIn(b"hvcC", data)
                    self.assertIn(b"ispe", data)
                elif entry["filename"].endswith(".jpg"):
                    self.assertTrue(data.startswith(b"\xff\xd8"))
                    self.assertTrue(data.endswith(b"\xff\xd9"))
                    self.assertIn(b"Exif\0\0", data)

    def test_swift_resources_match_the_android_and_rust_canonical_fixtures(self):
        canonical = {path.name for path in FIXTURES.iterdir() if path.is_file()}
        bundled = {path.name for path in SWIFT.iterdir() if path.is_file()}
        self.assertEqual(canonical, bundled)
        for filename in canonical:
            with self.subTest(filename=filename):
                self.assertEqual((FIXTURES / filename).read_bytes(), (SWIFT / filename).read_bytes())


if __name__ == "__main__":
    unittest.main()
