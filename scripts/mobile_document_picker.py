#!/usr/bin/env python3
"""Choose one observed control in the dedicated Android DocumentsUI fixture run.

This helper never controls a device. It parses a freshly captured UI Automator
hierarchy and returns an exact, unambiguous visible control and its bounds. The
caller records the hierarchy/screenshot before tapping. Unknown packages,
permission prompts, ambiguous files and unexpected controls fail closed.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import xml.etree.ElementTree as ET


DOCUMENT_PACKAGES = frozenset({"com.android.documentsui", "com.google.android.documentsui"})
PASSIVE_PACKAGES = frozenset({"com.android.systemui", "com.crispstrobe.crispsorter"})
MAX_XML_BYTES = 2 * 1024 * 1024


def inspect(xml: bytes, filename: str, *, selected: bool = False) -> dict:
    if len(xml) > MAX_XML_BYTES or b"<!DOCTYPE" in xml.upper() or b"<!ENTITY" in xml.upper():
        raise ValueError("unsafe or oversized UI hierarchy")
    if not re.fullmatch(r"crisp-ocr-provider-[0-9a-f-]{36}\.pdf", filename):
        raise ValueError("expected the unique fixture PDF filename")
    root = ET.fromstring(xml)
    if root.tag != "hierarchy":
        raise ValueError("expected a UI Automator hierarchy")
    nodes = list(root.iter("node"))
    if len(nodes) > 10_000:
        raise ValueError("UI hierarchy contains too many controls")
    packages = {node.get("package", "") for node in nodes} - {""}
    unknown = packages - DOCUMENT_PACKAGES - PASSIVE_PACKAGES
    if unknown:
        raise ValueError("unexpected foreground package: " + ", ".join(sorted(unknown)))
    present = packages & DOCUMENT_PACKAGES
    if len(present) > 1:
        raise ValueError("multiple document picker packages are visible")
    result = {"present": bool(present), "packages": sorted(packages), "action": None}
    if not present:
        if "com.crispstrobe.crispsorter" not in packages:
            raise ValueError("neither the application nor DocumentsUI is visible")
        return result
    package = next(iter(present))
    parents = {child: parent for parent in root.iter() for child in parent}

    def clickable(node: ET.Element) -> ET.Element | None:
        selected = None
        while node.tag == "node" and node.get("package") == package:
            if node.get("enabled") != "true" or node.get("visible-to-user", "true") != "true":
                return None
            if selected is None and node.get("clickable") == "true":
                selected = node
            node = parents.get(node, root)
        return selected

    def select(kind: str, matches: list[ET.Element]) -> dict | None:
        targets = {}
        for node in matches:
            target = clickable(node)
            if target is None:
                continue
            bounds = target.get("bounds", "")
            match = re.fullmatch(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", bounds)
            if not match:
                raise ValueError("picker control has invalid observed bounds")
            left, top, right, bottom = map(int, match.groups())
            if not (0 <= left < right <= 16_384 and 0 <= top < bottom <= 16_384):
                raise ValueError("picker control has empty or oversized observed bounds")
            targets[bounds] = {
                "kind": kind, "package": package, "text": node.get("text", ""),
                "description": node.get("content-desc", ""),
                "resource_id": target.get("resource-id", ""),
                "bounds": [left, top, right, bottom],
                "x": (left + right) // 2, "y": (top + bottom) // 2,
            }
        if len(targets) > 1:
            raise ValueError("ambiguous observed picker controls for " + kind)
        return next(iter(targets.values()), None)

    visible = [node for node in nodes if node.get("package") == package]
    if selected:
        # A single click normally returns immediately. Some system picker
        # variants require the explicit Open button after a selected file.
        result["action"] = select("confirm-selection", [node for node in visible
            if node.get("text", "").lower() == "open"
            and node.get("resource-id", "").split(":id/")[-1] in {"action_menu_select", "button1"}])
        return result
    result["action"] = select("select-fixture", [node for node in visible if node.get("text") == filename])
    if result["action"] is not None:
        return result
    result["action"] = select("open-downloads", [node for node in visible
        if node.get("text") == "Downloads"
        and node.get("resource-id", "").split(":id/")[-1] in {"title", "text1"}])
    if result["action"] is not None:
        return result
    result["action"] = select("show-roots", [node for node in visible
        if node.get("content-desc") in {"Show roots", "Open navigation drawer"}])
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--xml", type=Path, required=True)
    parser.add_argument("--filename", required=True)
    parser.add_argument("--selected", action="store_true")
    args = parser.parse_args()
    try:
        if args.xml.is_symlink() or args.xml.stat().st_size > MAX_XML_BYTES:
            raise ValueError("unsafe or oversized hierarchy file")
        print(json.dumps(inspect(args.xml.read_bytes(), args.filename, selected=args.selected)))
    except (OSError, ValueError, ET.ParseError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
