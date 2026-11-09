"""Fail-closed control selection; these synthetic trees do not qualify a picker."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "mobile_document_picker.py"
SPEC = importlib.util.spec_from_file_location("mobile_document_picker", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
FILENAME = "crisp-ocr-provider-01234567-89ab-cdef-0123-456789abcdef.pdf"


def node(text="", *, package="com.google.android.documentsui", bounds="[10,20][210,80]", children="", **extra):
    attrs = {"text": text, "package": package, "bounds": bounds, "enabled": "true", "clickable": "true", **extra}
    return "<node " + " ".join(f'{key}="{value}"' for key, value in attrs.items()) + ">" + children + "</node>"


def inspect(nodes, **kwargs):
    return MODULE.inspect(("<hierarchy>" + nodes + "</hierarchy>").encode(), FILENAME, **kwargs)


class PickerUiTests(unittest.TestCase):
    def test_exact_fixture_uses_observed_clickable_parent_bounds(self):
        child = node(FILENAME, clickable="false", bounds="[30,30][160,60]")
        action = inspect(node(children=child))["action"]
        self.assertEqual(action["kind"], "select-fixture")
        self.assertEqual((action["x"], action["y"]), (110, 50))

    def test_duplicate_filenames_in_distinct_rows_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            inspect(node(FILENAME) + node(FILENAME, bounds="[10,100][210,180]"))

    def test_disabled_hidden_partial_and_empty_controls_are_not_selected(self):
        for attrs in ({"enabled": "false"}, {"visible-to-user": "false"}):
            self.assertIsNone(inspect(node(FILENAME, **attrs))["action"])
            self.assertIsNone(inspect(node(children=node(FILENAME), **attrs))["action"])
        self.assertIsNone(inspect(node(FILENAME + ".backup"))["action"])
        with self.assertRaisesRegex(ValueError, "empty"):
            inspect(node(FILENAME, bounds="[10,10][10,20]"))

    def test_permission_prompts_and_unknown_apps_are_never_accepted(self):
        for package in ("com.google.android.permissioncontroller", "com.example.other"):
            with self.assertRaisesRegex(ValueError, "unexpected foreground package"):
                inspect(node("Allow", package=package))

    def test_navigation_and_open_are_limited_to_visible_known_controls(self):
        downloads = node("Downloads", **{"resource-id": "android:id/title"})
        self.assertEqual(inspect(downloads)["action"]["kind"], "open-downloads")
        roots = node(**{"content-desc": "Show roots"})
        self.assertEqual(inspect(roots)["action"]["kind"], "show-roots")
        open_button = node("Open", **{"resource-id": "com.google.android.documentsui:id/action_menu_select"})
        self.assertIsNone(inspect(open_button)["action"])
        self.assertEqual(inspect(open_button, selected=True)["action"]["kind"], "confirm-selection")
        self.assertIsNone(inspect(node("Open"), selected=True)["action"])

    def test_application_return_and_malformed_hierarchies_are_distinct(self):
        self.assertFalse(inspect(node(package="com.crispstrobe.crispsorter"))["present"])
        with self.assertRaisesRegex(ValueError, "neither"):
            inspect(node(package="com.android.systemui"))
        for xml in (b'<!DOCTYPE hierarchy><hierarchy />', b'<layout />', b'x' * (MODULE.MAX_XML_BYTES + 1)):
            with self.assertRaises(ValueError):
                MODULE.inspect(xml, FILENAME)
        with self.assertRaisesRegex(ValueError, "unique fixture"):
            MODULE.inspect(b'<hierarchy />', '../../unrelated.pdf')


if __name__ == "__main__":
    unittest.main()
