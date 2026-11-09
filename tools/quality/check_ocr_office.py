#!/usr/bin/env python3
"""Generate real reviewed DOCX through production Rust modules and inspect OOXML.

This bounded synthetic qualification is separate from Word/LibreOffice rendering,
an installed-app test, schema validation, OCR accuracy, and device qualification.
All generated packages, exact inputs, warnings, logs, dependency locks, hashes,
and per-gate results remain in --output, including on a failed run.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import posixpath
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import tomllib
import xml.etree.ElementTree as ET
import zipfile

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
REL = "http://schemas.openxmlformats.org/package/2006/relationships"
NS = {"w": W, "r": R}
SOURCE_PATHS = ["src-tauri/src/ocr_document.rs", "src-tauri/src/ocr_office.rs", "src-tauri/src/ocr_docx.rs",
                "src-tauri/Cargo.toml", "Cargo.lock", "tools/quality/ocr_office_fixture.rs", "tools/quality/check_ocr_office.py"]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, data: object) -> None:
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def capture(command: list[str], cwd: Path) -> str:
    return subprocess.run(command, cwd=cwd, text=True, encoding="utf-8", errors="replace",
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=True).stdout.strip()


def wval(element: ET.Element | None, default: str = "") -> str:
    return default if element is None else element.get(f"{{{W}}}val", default)


def paragraph_text(paragraph: ET.Element) -> str:
    result = []
    for element in paragraph.iter():
        if element.tag == f"{{{W}}}t":
            result.append(element.text or "")
        elif element.tag == f"{{{W}}}tab":
            result.append("\t")
        elif element.tag == f"{{{W}}}cr" or (element.tag == f"{{{W}}}br" and element.get(f"{{{W}}}type", "textWrapping") == "textWrapping"):
            result.append("\n")
    return "".join(result)


def enabled_property(element: ET.Element | None) -> bool:
    return element is not None and wval(element, "true") not in {"0", "false", "off", "none"}


def run_marks(paragraph: ET.Element) -> list[dict[str, bool]]:
    result = []
    for run in paragraph.findall(".//w:r", NS):
        marks = {name: enabled_property(run.find(f"w:rPr/w:{tag}", NS)) for name, tag in [("bold", "b"), ("italic", "i"), ("underline", "u")]}
        result.extend([marks] * len(paragraph_text(run)))
    require(len(result) == len(paragraph_text(paragraph)), "styled paragraph has unsupported text outside runs")
    return result


def check_table(table: ET.Element, expected: dict) -> dict:
    rows = table.findall("w:tr", NS)
    grid = table.findall("w:tblGrid/w:gridCol", NS)
    require(len(rows) == expected["rows"] and len(grid) == expected["columns"], "table declared grid size changed")
    anchors: list[dict] = []
    previous: dict[int, dict] = {}
    for row_index, row in enumerate(rows):
        column = 0
        continuing = {}
        for cell in row.findall("w:tc", NS):
            span = int(wval(cell.find("w:tcPr/w:gridSpan", NS), "1"))
            require(span > 0 and column + span <= len(grid), "table cell leaves declared grid")
            paragraphs = cell.findall("w:p", NS)
            text = "\n".join(paragraph_text(paragraph) for paragraph in paragraphs)
            merge = cell.find("w:tcPr/w:vMerge", NS)
            if merge is not None and wval(merge, "continue") == "continue":
                require(column in previous, "vertical continuation has no prior anchor")
                anchor = previous[column]
                require(anchor["columnSpan"] == span and text == "", "vertical continuation changes width or duplicates literal text")
                anchor["rowSpan"] += 1
                continuing[column] = anchor
            else:
                anchor = {"row": row_index, "column": column, "rowSpan": 1, "columnSpan": span, "text": text}
                anchor["bold"] = all(enabled_property(run.find("w:rPr/w:b", NS)) for paragraph in paragraphs for run in paragraph.findall("w:r", NS) if paragraph_text(run))
                anchors.append(anchor)
                if merge is not None:
                    require(wval(merge) == "restart", "unsupported vertical merge value")
                    continuing[column] = anchor
            column += span
        require(column == len(grid), "table row has missing or extra logical cells")
        previous = continuing
    keys = ("row", "column", "rowSpan", "columnSpan", "text")
    normalized = [{key: cell[key] for key in keys} for cell in anchors]
    wanted = [{key: cell[key] for key in keys} for cell in expected["cells"]]
    require(normalized == wanted, f"table literal values or merge rectangles changed: {normalized!r}")
    for actual, wanted_cell in zip(anchors, expected["cells"]):
        if wanted_cell.get("header"):
            require(actual["bold"], "header anchor does not contain bold text runs")
    return {"rows": len(rows), "columns": len(grid), "anchors": normalized}


def inspect_docx(path: Path, expected: dict, warnings: list[str]) -> dict:
    with zipfile.ZipFile(path) as archive:
        require(archive.testzip() is None, "ZIP CRC validation failed")
        entries = archive.infolist()
        require(len({entry.filename for entry in entries}) == len(entries), "ZIP contains duplicate package names")
        files = {entry.filename: entry for entry in entries if not entry.is_dir()}
        require(sum(entry.file_size for entry in files.values()) <= 32 * 1024 * 1024, "synthetic package unexpectedly exceeds 32 MB")
        for name, entry in files.items():
            require(not PurePosixPath(name).is_absolute() and ".." not in PurePosixPath(name).parts and "\\" not in name, f"unsafe package path: {name}")
            require(not entry.flag_bits & 1, "encrypted package member")
            require("vbaproject" not in name.lower(), "unexpected macro content")
        required = {"[Content_Types].xml", "_rels/.rels", "word/document.xml", "word/styles.xml"}
        require(required <= files.keys(), "missing required Office package parts")
        parsed = {}
        for name in files:
            if name.endswith((".xml", ".rels")):
                raw = archive.read(name)
                require(b"<!DOCTYPE" not in raw.upper() and b"<!ENTITY" not in raw.upper(), "unexpected DTD/entity declaration")
                parsed[name] = ET.fromstring(raw)
        relationship_count = 0
        part_relationships = {}
        for name, tree in parsed.items():
            if not name.endswith(".rels"):
                continue
            if name == "_rels/.rels":
                owner = ""
            else:
                directory, basename = name.rsplit("/_rels/", 1)
                owner = f"{directory}/{basename[:-5]}"
            ids = {}
            for relationship in tree.findall(f"{{{REL}}}Relationship"):
                identifier, target = relationship.get("Id"), relationship.get("Target", "")
                require(bool(identifier) and identifier not in ids, "missing or duplicate relationship ID")
                require(relationship.get("TargetMode") != "External", "synthetic document unexpectedly contains an external relationship")
                resolved = posixpath.normpath(posixpath.join(posixpath.dirname(owner), target)) if not target.startswith("/") else target.lstrip("/")
                require(resolved in files, f"relationship target missing: {name} -> {target}")
                ids[identifier] = resolved
                relationship_count += 1
            part_relationships[owner] = ids
        document = parsed["word/document.xml"]
        body = document.find("w:body", NS)
        require(body is not None, "Word body missing")
        for text_node in document.findall(".//w:t", NS):
            text = text_node.text or ""
            if text and (text[0].isspace() or text[-1].isspace()):
                require(text_node.get("{http://www.w3.org/XML/1998/namespace}space") == "preserve", "leading/trailing text whitespace lacks xml:space preservation")
        paragraphs = body.findall("w:p", NS)
        actual_text = [paragraph_text(paragraph) for paragraph in paragraphs if paragraph_text(paragraph) != ""]
        require(actual_text == expected["body_paragraphs"], f"body text/order/whitespace differs: {actual_text!r}")
        all_text = "\n".join(paragraph_text(paragraph) for paragraph in document.findall(".//w:p", NS))
        for forbidden in expected["forbidden_text"]:
            require(forbidden not in all_text, f"unselected/original source text leaked: {forbidden}")
        style_elements = parsed["word/styles.xml"].findall("w:style", NS)
        style_ids = [style.get(f"{{{W}}}styleId") for style in style_elements]
        require(all(style_ids) and len(set(style_ids)) == len(style_ids), "missing or duplicate style identity")
        styles = dict(zip(style_ids, style_elements))
        for tag in ["pStyle", "rStyle"]:
            for reference in document.findall(f".//w:{tag}", NS):
                require(wval(reference) in styles, f"undefined style reference: {wval(reference)}")
        by_text = {paragraph_text(paragraph): paragraph for paragraph in paragraphs}
        for wanted in expected["paragraph_styles"]:
            paragraph = by_text[wanted["text"]]
            require(wval(paragraph.find("w:pPr/w:pStyle", NS)) == wanted["style"], "paragraph style lost")
        for wanted in expected["marks"] + expected["plain_fragments"]:
            paragraph = by_text[wanted["paragraph"]]
            start = wanted["paragraph"].index(wanted["text"])
            coverage = run_marks(paragraph)[start:start + len(wanted["text"])]
            require(coverage and all(all(item[mark] == wanted.get(mark, False) for mark in ["bold", "italic", "underline"]) for item in coverage), f"format range changed around {wanted['text']!r}")
        numbering = parsed.get("word/numbering.xml")
        num_elements = [] if numbering is None else numbering.findall("w:num", NS)
        abstract_elements = [] if numbering is None else numbering.findall("w:abstractNum", NS)
        num_ids = [item.get(f"{{{W}}}numId") for item in num_elements]
        abstract_ids = [item.get(f"{{{W}}}abstractNumId") for item in abstract_elements]
        require(all(num_ids) and len(set(num_ids)) == len(num_ids), "missing or duplicate numbering instance ID")
        require(all(abstract_ids) and len(set(abstract_ids)) == len(abstract_ids), "missing or duplicate abstract numbering ID")
        nums, abstracts = dict(zip(num_ids, num_elements)), dict(zip(abstract_ids, abstract_elements))
        for abstract in abstract_elements:
            levels = [item.get(f"{{{W}}}ilvl") for item in abstract.findall("w:lvl", NS)]
            require(all(level is not None for level in levels) and len(set(levels)) == len(levels), "missing or duplicate abstract numbering level")
        for instance in num_elements:
            levels = [item.get(f"{{{W}}}ilvl") for item in instance.findall("w:lvlOverride", NS)]
            require(all(level is not None for level in levels) and len(set(levels)) == len(levels), "missing or duplicate list override level")
        for reference in document.findall(".//w:numId", NS):
            require(wval(reference) in nums, "list refers to undefined numbering instance")
        for item in nums.values():
            require(wval(item.find("w:abstractNumId", NS)) in abstracts, "numbering instance has no abstract definition")
        for wanted in expected["lists"]:
            properties = by_text[wanted["text"]].find("w:pPr/w:numPr", NS)
            require(properties is not None, "list paragraph lost numbering")
            level = int(wval(properties.find("w:ilvl", NS), "0"))
            require(level == wanted["level"], "list nesting changed")
            instance = nums[wval(properties.find("w:numId", NS))]
            abstract = abstracts[wval(instance.find("w:abstractNumId", NS))]
            definition = next((item for item in abstract.findall("w:lvl", NS) if int(item.get(f"{{{W}}}ilvl", "-1")) == level), None)
            override = next((item for item in instance.findall("w:lvlOverride", NS) if int(item.get(f"{{{W}}}ilvl", "-1")) == level), None)
            override_definition = None if override is None else override.find("w:lvl", NS)
            effective_definition = override_definition if override_definition is not None else definition
            require(effective_definition is not None and wval(effective_definition.find("w:numFmt", NS)) == wanted["format"], "effective list number format changed")
            if "start" in wanted:
                start = wval(None if override is None else override.find("w:startOverride", NS), wval(effective_definition.find("w:start", NS), "1"))
                require(int(start) == wanted["start"], "explicit list starting number changed")
        sections = document.findall(".//w:sectPr", NS)
        for section in sections:
            size = section.find("w:pgSz", NS)
            require(size is not None and int(size.get(f"{{{W}}}w", "0")) == 12240 and int(size.get(f"{{{W}}}h", "0")) == 15840, "authored physical page dimensions changed")
        column_counts = [int((section.find("w:cols", NS).get(f"{{{W}}}num", "1") if section.find("w:cols", NS) is not None else "1")) for section in sections]
        require(column_counts == expected["column_counts"], f"logical section columns changed: {column_counts}")
        column_breaks = sum(element.get(f"{{{W}}}type") == "column" for element in document.findall(".//w:br", NS))
        page_breaks = sum(enabled_property(element) for element in document.findall(".//w:pageBreakBefore", NS)) + sum(element.get(f"{{{W}}}type") == "page" for element in document.findall(".//w:br", NS))
        require(column_breaks == expected["column_breaks"], "reading column boundary changed")
        require(page_breaks >= expected["minimum_page_breaks"], "explicit source/blank page boundary lost")
        tables = body.findall("w:tbl", NS)
        require(len(tables) == len(expected["table_grids"]), "wrong number of selected editable tables")
        table_reports = [check_table(table, wanted) for table, wanted in zip(tables, expected["table_grids"])]
        images = []
        for name in files:
            if name.startswith("word/media/"):
                raw = archive.read(name)
                require(raw[:8] == b"\x89PNG\r\n\x1a\n" and raw[12:16] == b"IHDR", "fixture figure is not a PNG")
                width, height = struct.unpack(">II", raw[16:24])
                images.append({"width": width, "height": height})
        require(images == expected["images"], f"figure count or measured pixels changed: {images}")
        embeddings = [element.get(f"{{{R}}}embed") for element in document.iter() if element.get(f"{{{R}}}embed")]
        require(len(embeddings) == len(images), "figure drawing reference count differs")
        for identifier in embeddings:
            require(identifier in part_relationships.get("word/document.xml", {}), "figure drawing has missing media relationship")
        if not expected["native_footnotes"]:
            require(not document.findall(".//w:footnoteReference", NS), "unexpected generated automatic footnote marker")
        for fragment in expected["warning_substrings"]:
            require(any(fragment in warning for warning in warnings), f"unsupported semantic warning missing: {fragment}")
        return {"status": "passed", "package_parts": len(files), "relationships": relationship_count,
                "body_paragraphs": actual_text, "tables": table_reports, "images": images,
                "section_columns": column_counts, "column_breaks": column_breaks, "page_breaks": page_breaks,
                "warnings": warnings, "limitations": ["XML checks are not Office schema validation or rendered layout qualification."]}


def lock_packages(lock: Path) -> dict[tuple[str, str], dict]:
    with lock.open("rb") as stream:
        packages = tomllib.load(stream)["package"]
    return {(package["name"], package["version"]): package for package in packages if package.get("source", "").startswith("registry+")}


def harness_manifest(repository: Path) -> str:
    packages = lock_packages(repository / "Cargo.lock")
    pins = {"docx-rs": "0.4.20", "zip": "7.2.0", "unicode-segmentation": "1.13.3", "image": "0.25.10", "quick-xml": "0.38.4"}
    for name in ["serde", "serde_json"]:
        versions = [version for package, version in packages if package == name]
        require(len(versions) == 1, f"choose the production {name} version explicitly")
        pins[name] = versions[0]
    for name, version in pins.items():
        require((name, version) in packages, f"reviewed dependency pin missing from production lock: {name} {version}")
    return f'''[package]
name = "crispsorter-office-quality"
version = "0.0.0"
edition = "2021"

[lib]
path = "lib.rs"
[[bin]]
name = "ocr-office-fixtures"
path = "main.rs"

[dependencies]
serde = {{ version = "={pins['serde']}", features = ["derive"] }}
serde_json = "={pins['serde_json']}"
unicode-segmentation = "={pins['unicode-segmentation']}"
docx-rs = "={pins['docx-rs']}"
zip = "={pins['zip']}"
quick-xml = "={pins['quick-xml']}"
image = {{ version = "={pins['image']}", default-features = false, features = ["png"] }}
'''


def execute(command: list[str], cwd: Path, log: Path, receipt: dict, timeout: int = 1500) -> str:
    print("Running " + " ".join(command), flush=True)
    started = time.monotonic()
    item = {"command": command, "working_directory": str(cwd), "log": log.name}
    receipt["commands"].append(item)
    try:
        with log.open("w", encoding="utf-8") as stream:
            completed = subprocess.run(command, cwd=cwd, stdout=stream, stderr=subprocess.STDOUT,
                                       text=True, encoding="utf-8", errors="replace", timeout=timeout)
        item.update(exit_code=completed.returncode, elapsed_seconds=round(time.monotonic() - started, 3))
        output = log.read_text(encoding="utf-8", errors="replace")
        if completed.returncode:
            print(output[-12000:], file=sys.stderr)
            raise RuntimeError(f"command failed ({completed.returncode}); see {log.name}")
        return output
    except subprocess.TimeoutExpired:
        item.update(exit_code=None, elapsed_seconds=round(time.monotonic() - started, 3), timed_out=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("target/ocr-office-qualification"), help="new or empty artifact directory")
    parser.add_argument("--target-dir", type=Path, default=Path("target/ocr-office-quality"), help="reusable Cargo build cache")
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[2]
    output = args.output.resolve()
    target = args.target_dir.resolve()
    require(not output.exists() or not any(output.iterdir()), "output directory must be new or empty; retain prior receipts separately")
    output.mkdir(parents=True, exist_ok=True)
    receipt = {"schema": "crispsorter.ocr-office-qualification.v1", "recorded_at": datetime.now(timezone.utc).isoformat(),
               "scope": "Synthetic production-module DOCX generation and independent ZIP/XML inspection",
               "tools": {"python": sys.version, "platform": platform.platform()}, "commands": [], "sources": {},
               "gates": {name: {"status": "not_run"} for name in ["production_unit_tests", "production_generation", "dependency_lock_match", "independent_package_xml", "independent_office_reader", "office_schema_validator", "installed_application", "live_ocr_corpus", "device_capture"]}}
    failed = None
    active_gate = None
    try:
        receipt["producing_checkout"] = {"head_sha": capture(["git", "rev-parse", "HEAD"], repository),
                                          "changes": capture(["git", "status", "--porcelain", "--", *SOURCE_PATHS], repository),
                                          "github_workflow_sha": os.environ.get("GITHUB_SHA"), "pull_request_head_sha": os.environ.get("OCR_PR_HEAD_SHA")}
        receipt["producing_checkout"]["dirty_sources"] = bool(receipt["producing_checkout"]["changes"])
        for path in SOURCE_PATHS:
            receipt["sources"][path] = {"sha256": sha256(repository / path), "bytes": (repository / path).stat().st_size}
        receipt["tools"]["rustc"] = capture(["rustc", "-Vv"], repository)
        receipt["tools"]["cargo"] = capture(["cargo", "-V"], repository)
        with tempfile.TemporaryDirectory(prefix="crispsorter-office-") as directory:
            harness = Path(directory)
            (harness / "Cargo.toml").write_text(harness_manifest(repository), encoding="utf-8")
            modules = {"ocr_document": "src-tauri/src/ocr_document.rs", "ocr_office": "src-tauri/src/ocr_office.rs", "ocr_docx": "src-tauri/src/ocr_docx.rs", "ocr_office_fixture": "tools/quality/ocr_office_fixture.rs"}
            (harness / "lib.rs").write_text("".join(f'#[path = {json.dumps((repository / path).as_posix(), ensure_ascii=False)}]\npub mod {name};\n' for name, path in modules.items()), encoding="utf-8")
            (harness / "main.rs").write_text('fn main() { let path = std::env::args_os().nth(1).expect("artifact output directory"); if let Err(error) = crispsorter_office_quality::ocr_office_fixture::generate(std::path::Path::new(&path)) { eprintln!("{error}"); std::process::exit(1); } }\n', encoding="utf-8")
            # Cargo prunes the unrelated application graph while retaining locked
            # registry versions/checksums. Verify the resulting subset below.
            shutil.copy2(repository / "Cargo.lock", harness / "Cargo.lock")
            for name in ["Cargo.toml", "lib.rs", "main.rs"]:
                shutil.copy2(harness / name, output / ("harness-" + name))
            base = ["cargo", "--color", "never"]
            active_gate = "production_unit_tests"
            test_log = execute(base + ["test", "--manifest-path", str(harness / "Cargo.toml"), "--target-dir", str(target), "--lib", "--", "--test-threads=1"], repository, output / "native-tests.log", receipt)
            summaries = re.findall(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored", test_log)
            require(bool(summaries), "Cargo did not report a native unit-test result")
            module_counts = {}
            for module in ["ocr_document", "ocr_office", "ocr_docx"]:
                module_counts[module] = len(re.findall(rf"^test {module}::tests::\S+ \.\.\. ok$", test_log, re.MULTILINE))
                require(module_counts[module] > 0, f"no actual {module} tests executed")
            receipt["gates"][active_gate] = {"status": "passed", "passed": sum(int(item[0]) for item in summaries), "module_counts": module_counts}
            active_gate = "dependency_lock_match"
            shutil.copy2(harness / "Cargo.lock", output / "harness-Cargo.lock")
            production = lock_packages(repository / "Cargo.lock")
            resolved = lock_packages(harness / "Cargo.lock")
            mismatches = [{"name": name, "version": version, "checksum": package.get("checksum")} for (name, version), package in resolved.items() if (name, version) not in production or any(package.get(key) != production[(name, version)].get(key) for key in ["source", "checksum"])]
            write_json(output / "dependency-lock-check.json", {"registry_package_count": len(resolved), "mismatches": mismatches, "packages": [{"name": name, "version": version, "source": package.get("source"), "checksum": package.get("checksum")} for (name, version), package in sorted(resolved.items())]})
            require(not mismatches, "harness resolved registry packages absent from production lock; inspect dependency-lock-check.json")
            receipt["gates"][active_gate] = {"status": "passed", "registry_package_count": len(resolved), "meaning": "Exact name/version/source/checksum subset; app feature graph is exercised separately by paired integration."}
            active_gate = "production_generation"
            execute(base + ["run", "--locked", "--manifest-path", str(harness / "Cargo.toml"), "--target-dir", str(target), "--bin", "ocr-office-fixtures", "--", str(output)], repository, output / "generation.log", receipt)
            fixture_manifest = json.loads((output / "fixtures.json").read_text(encoding="utf-8"))
            require(fixture_manifest.get("version") == 1 and len(fixture_manifest["fixtures"]) == 3, "fixture generation did not complete")
            receipt["gates"][active_gate] = {"status": "passed", "documents": len(fixture_manifest["fixtures"])}
            active_gate = "independent_package_xml"
            reports = []
            for fixture in fixture_manifest["fixtures"]:
                report = {"name": fixture["name"], "file": fixture["file"], **inspect_docx(output / fixture["file"], fixture["expected"], fixture["warnings"])}
                reports.append(report)
            write_json(output / "package-xml-report.json", {"validator": "Python stdlib zipfile + ElementTree; independent of docx-rs", "fixtures": reports})
            receipt["gates"][active_gate] = {"status": "passed", "documents": len(reports)}
            active_gate = None
    except Exception as error:
        failed = f"{type(error).__name__}: {error}"
        receipt["failure"] = failed
        if active_gate:
            receipt["gates"][active_gate] = {"status": "failed", "error": failed}
        print(failed, file=sys.stderr)
    finally:
        receipt["artifacts"] = {path.name: {"sha256": sha256(path), "bytes": path.stat().st_size} for path in sorted(output.iterdir()) if path.is_file() and path.name != "receipt.json"}
        receipt["finished_at"] = datetime.now(timezone.utc).isoformat()
        write_json(output / "receipt.json", receipt)
    if failed:
        return 1
    print(f"Native generation and package/XML checks passed for 3 documents. Receipt: {output / 'receipt.json'}")
    print("Independent Office rendering, schema validation, installed app, live corpus and device gates remain not_run.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
