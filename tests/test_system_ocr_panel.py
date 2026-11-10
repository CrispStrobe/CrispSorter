"""Check that shareable OCR receipts exclude source text and arbitrary diagnostics."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools/quality"))
from system_ocr_panel import safe_diagnostics


class SummaryPrivacy(unittest.TestCase):
    def test_retains_evidence_settings_without_raw_transcription_or_paths(self):
        source = {"full_text": "private source", "stderr": "/private/page.png", "paragraph_separators": ["private"],
                  "alignment_verified": True, "language": "eng", "word_count": 4, "page_segmentation_mode": 3,
                  "confidence_calibrated": False, "future_raw_payload": {"text": "private"}}
        self.assertEqual(safe_diagnostics(source), {"alignment_verified": True, "language": "eng", "word_count": 4,
                         "page_segmentation_mode": 3, "confidence_calibrated": False})


if __name__ == "__main__":
    unittest.main()
