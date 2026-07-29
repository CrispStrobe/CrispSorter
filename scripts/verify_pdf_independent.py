#!/usr/bin/env python
"""Independent verification of CrispSorter's PDF and annotation work.

Every assertion is made with a tool that shares no code with what it is
checking: qpdf (the C++ library we deliberately did not link), poppler,
MuPDF, pypdf, pikepdf, and sqlite3 for the annotation store. Fixtures are
authored by MuPDF rather than by us, so we are never both writing and
grading our own homework.

Two lessons are baked in, after an earlier version of this script produced
false passes:

* Never hard-code a coordinate. MuPDF's insert_text is y-down from the top
  and pages are not always Letter; a guessed rectangle redacted empty
  space, and every "the text is gone" check then held trivially. Positions
  are looked up at run time.
* A check that cannot fail is worse than none. Where a claim matters it is
  asserted from both sides — text gone *here* and still present *there* —
  so over-removal and under-removal both fail.

Usage:
    CRISP_CLI=path/to/crispsorter CRISP_WORK=/tmp/verify \\
        python scripts/verify_pdf_independent.py
"""
import json
import os
import re
import sqlite3
import subprocess
import sys
from pathlib import Path

CLI = os.environ.get("CRISP_CLI", "target/debug/crispsorter")
WORK = Path(os.environ.get("CRISP_WORK", "/tmp/crispsorter-verify"))
WORK.mkdir(parents=True, exist_ok=True)
DATA = WORK / "store"

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  — {detail}" if detail else ""))
    return bool(ok)


def run(args):
    return subprocess.run([CLI] + args, capture_output=True, text=True)


def ran(name, p):
    """Assert the CLI call itself succeeded, surfacing stderr when not."""
    if p.returncode != 0:
        msg = (p.stderr or p.stdout).strip()
        check(f"{name} ran", False, msg.splitlines()[-1][:200] if msg else f"exit {p.returncode}")
        return False
    return True


# ── Independent readers ────────────────────────────────────────────────
def qpdf_ok(path):
    p = subprocess.run(["qpdf", "--check", str(path)], capture_output=True, text=True)
    return p.returncode in (0, 3)


def mupdf_doc(path):
    import fitz
    return fitz.open(str(path))


def page_text(path, i):
    d = mupdf_doc(path)
    t = d[i].get_text()
    d.close()
    return t


def all_text(path):
    d = mupdf_doc(path)
    t = "".join(pg.get_text() for pg in d)
    d.close()
    return t


def text_poppler(path):
    return subprocess.run(["pdftotext", str(path), "-"], capture_output=True, text=True).stdout


def text_pypdf(path):
    import pypdf
    return "".join((pg.extract_text() or "") for pg in pypdf.PdfReader(str(path)).pages)


def page_count(path):
    d = mupdf_doc(path)
    n = d.page_count
    d.close()
    return n


def page_content(path, i):
    """Decompressed content stream(s) of one page."""
    import pikepdf
    with pikepdf.open(str(path)) as pdf:
        c = pdf.pages[i].obj.get("/Contents")
        if c is None:
            return b""
        try:
            return bytes(c.read_bytes())
        except Exception:
            return b"".join(bytes(x.read_bytes()) for x in c)


def locate(path, needle, page=0):
    """Where `needle` sits in PDF (y-up) coordinates. Looked up, never guessed."""
    d = mupdf_doc(path)
    pg = d[page]
    h = pg.rect.height
    hits = pg.search_for(needle)
    d.close()
    if not hits:
        return None
    r = hits[0]
    return (r.x0, h - r.y1, r.width, r.height)


def rect_arg(page1, box, pad=2.0):
    x, y, w, h = box
    return f"{page1},{x - pad:.1f},{y - pad:.1f},{w + 2 * pad:.1f},{h + 2 * pad:.1f}"


MARKER = "CONFIDENTIAL-MARKER-42"
BODY = "ordinary body text that must survive"


def make_fixture(pages=4):
    import fitz
    doc = fitz.open()
    for i in range(pages):
        pg = doc.new_page()
        pg.insert_text((72, 700), f"Page {i + 1} heading", fontsize=18)
        pg.insert_text((72, 660), MARKER, fontsize=12)
        pg.insert_text((72, 640), BODY, fontsize=12)
    out = WORK / "fixture.pdf"
    doc.save(str(out))
    doc.close()
    return out


def store_rows(table):
    db = DATA / "annotations.db"
    if not db.exists():
        return []
    con = sqlite3.connect(str(db))
    try:
        return con.execute(f"SELECT * FROM {table}").fetchall()
    except sqlite3.Error:
        return []
    finally:
        con.close()


# ── Sections ───────────────────────────────────────────────────────────
def verify_page_ops(fx):
    print("\n-- page operations --")
    o = WORK / "removed.pdf"
    if ran("remove", run(["pdf", "remove", str(fx), "--pages", "2", "--out", str(o)])):
        check("remove: valid (qpdf)", qpdf_ok(o))
        check("remove: one page fewer (MuPDF)", page_count(o) == 3, f"n={page_count(o)}")
        check("remove: dropped the right page", "Page 2 heading" not in all_text(o))

    o = WORK / "extracted.pdf"
    if ran("extract", run(["pdf", "extract", str(fx), "--pages", "1,3", "--out", str(o)])):
        check("extract: valid (qpdf)", qpdf_ok(o))
        check("extract: two pages", page_count(o) == 2, f"n={page_count(o)}")
        t = all_text(o)
        check("extract: kept 1 and 3, not 2",
              "Page 1 heading" in t and "Page 3 heading" in t and "Page 2 heading" not in t)

    o = WORK / "reordered.pdf"
    if ran("reorder", run(["pdf", "reorder", str(fx), "--order", "4,3,2,1", "--out", str(o)])):
        check("reorder: valid (qpdf)", qpdf_ok(o))
        check("reorder: first page is now the old page 4",
              "Page 4 heading" in page_text(o, 0), repr(page_text(o, 0)[:28]))

    o = WORK / "rotated.pdf"
    if ran("rotate", run(["pdf", "rotate", str(fx), "--pages", "1",
                          "--degrees", "90", "--out", str(o)])):
        d = mupdf_doc(o)
        rot = d[0].rotation
        d.close()
        check("rotate: MuPDF reports 90°", rot == 90, f"rotation={rot}")

    o = WORK / "merged.pdf"
    if ran("merge", run(["pdf", "merge", str(fx), str(fx), "--out", str(o)])):
        check("merge: valid (qpdf)", qpdf_ok(o))
        check("merge: page count doubled", page_count(o) == 8, f"n={page_count(o)}")

    outdir = WORK / "split"
    outdir.mkdir(exist_ok=True)
    if ran("split", run(["pdf", "split", str(fx), "--pages", "1-2,3-4",
                         "--out-dir", str(outdir)])):
        parts = sorted(outdir.glob("*.pdf"))
        check("split: produced two files", len(parts) == 2, str([p.name for p in parts]))
        if len(parts) == 2:
            check("split: each part valid with 2 pages",
                  all(qpdf_ok(p) and page_count(p) == 2 for p in parts))

    o = WORK / "numbered.pdf"
    if ran("number", run(["pdf", "number", str(fx), "--out", str(o)])):
        check("page numbers: valid (qpdf)", qpdf_ok(o))
        check("page numbers: '3' appears on page 3", "3" in page_text(o, 2))

    o = WORK / "watermarked.pdf"
    if ran("watermark", run(["pdf", "watermark", str(fx), "--text", "DRAFTWM",
                             "--out", str(o)])):
        check("watermark: text present (MuPDF)", "DRAFTWM" in all_text(o))

    o = WORK / "blank.pdf"
    if ran("insert-blank", run(["pdf", "insert-blank", str(fx),
                                "--at", "1", "--out", str(o)])):
        check("insert-blank: one page more", page_count(o) == 5, f"n={page_count(o)}")

    o = WORK / "cropped.pdf"
    if ran("crop", run(["pdf", "crop", str(fx), "--pages", "1",
                        "--rect", "50,50,300,400", "--out", str(o)])):
        d = mupdf_doc(o)
        w = d[0].rect.width
        d.close()
        check("crop: MuPDF sees the narrower page", abs(w - 300) < 2, f"width={w:.1f}")

    o = WORK / "meta.pdf"
    if ran("metadata", run(["pdf", "metadata", str(fx), "--title", "Verified Title",
                            "--author", "Independent Tool", "--out", str(o)])):
        import pikepdf
        with pikepdf.open(str(o)) as pdf:
            info = {str(k): str(v) for k, v in (pdf.docinfo or {}).items()}
        check("metadata: title readable by pikepdf",
              info.get("/Title") == "Verified Title", str(info)[:90])
        check("metadata: author readable by pikepdf",
              info.get("/Author") == "Independent Tool")


def verify_redaction(fx):
    print("\n-- redaction (P32.7) --")
    box = locate(fx, MARKER)
    if not check("marker located by MuPDF", box is not None, str(box)):
        return
    o = WORK / "redacted.pdf"
    if not ran("redact-regions", run(["pdf", "redact-regions", str(fx),
                                      "--rect", rect_arg(1, box), "--out", str(o)])):
        return
    check("redaction: valid (qpdf)", qpdf_ok(o))
    check("redaction: gone from page 1 (MuPDF)", MARKER not in page_text(o, 0),
          repr(page_text(o, 0)[:55]))
    import pypdf
    check("redaction: gone from page 1 (pypdf)",
          MARKER not in (pypdf.PdfReader(str(o)).pages[0].extract_text() or ""))
    check("redaction: gone from page 1's content stream",
          MARKER.encode() not in page_content(o, 0),
          "the check a black-rectangle overlay would fail")
    check("redaction: still present on page 2 (not over-removed)",
          MARKER in page_text(o, 1))
    check("redaction: neighbouring text on page 1 survived", BODY in page_text(o, 0))

    # The visual-only command must behave the opposite way, or its warning
    # is a lie.
    o2 = WORK / "blackedout.pdf"
    if ran("redact (visual)", run(["pdf", "redact", str(fx),
                                   "--patterns", MARKER, "--out", str(o2)])):
        check("black-out: text deliberately still recoverable",
              MARKER in page_text(o2, 0),
              "documented visual-only; this failing would mean the warning is wrong")


def verify_crypto(fx):
    print("\n-- encryption (AES-256) --")
    import pikepdf
    enc = WORK / "encrypted.pdf"
    if not ran("encrypt", run(["pdf", "encrypt", str(fx), "--owner-password", "ownerpw",
                               "--user-password", "userpw", "--out", str(enc)])):
        return
    data = enc.read_bytes()
    check("AES: /V 5", re.search(rb"/V\s+5", data) is not None)
    check("AES: /R 6", re.search(rb"/R\s+6", data) is not None)
    check("AES: AESV3 crypt filter", b"AESV3" in data)
    try:
        with pikepdf.open(str(enc), password="userpw") as pdf:
            info = pdf.encryption
        check("AES: pikepdf opens with the user password", True)
        check("AES: pikepdf reports aesv3", "aesv3" in str(info).lower(), str(info)[:85])
    except Exception as e:
        check("AES: pikepdf opens with the user password", False, str(e)[:140])
    try:
        with pikepdf.open(str(enc)):
            check("AES: refuses to open without a password", False, "opened!")
    except Exception as e:
        check("AES: refuses to open without a password", "assword" in str(e), str(e)[:70])

    p = run(["pdf", "is-encrypted", str(enc)])
    check("is-encrypted agrees", p.returncode == 0 and "not" not in p.stdout, p.stdout.strip())

    # Non-empty user password: the library cannot supply one at load
    # time, so this must fail loudly rather than write a corrupt file.
    dec = WORK / "decrypted.pdf"
    p = run(["pdf", "decrypt", str(enc), "--password", "userpw", "--out", str(dec)])
    check("decrypt: refuses a non-empty user password rather than corrupting",
          p.returncode != 0, (p.stderr or "").strip().splitlines()[-1][:90] if p.stderr else "")
    check("decrypt: wrote no file when it could not succeed", not dec.exists())

    # Owner-password-only is the case the library *can* handle, and it
    # must round-trip properly.
    enc2 = WORK / "encrypted_owner_only.pdf"
    if ran("encrypt (owner only)", run(["pdf", "encrypt", str(fx),
                                        "--owner-password", "ownerpw",
                                        "--out", str(enc2)])):
        dec2 = WORK / "decrypted_owner_only.pdf"
        if ran("decrypt (owner only)", run(["pdf", "decrypt", str(enc2),
                                            "--password", "", "--out", str(dec2)])):
            check("decrypt: valid (qpdf)", qpdf_ok(dec2))
            try:
                with pikepdf.open(str(dec2)) as d:
                    check("decrypt: opens with no password", True)
                    check("decrypt: encryption really removed",
                          d.encryption is None or not d.is_encrypted,
                          str(d.is_encrypted))
            except Exception as e:
                check("decrypt: opens with no password", False, str(e)[:90])
            check("decrypt: round-trip preserved the text", MARKER in all_text(dec2))

    rc4 = WORK / "rc4.pdf"
    if ran("encrypt --legacy-rc4", run(["pdf", "encrypt", str(fx), "--owner-password", "o",
                                        "--out", str(rc4), "--legacy-rc4"])):
        check("legacy: RC4 path is not AESV3", b"AESV3" not in rc4.read_bytes())


def verify_text_edit(fx):
    print("\n-- text editing (P32.8) --")
    o = WORK / "substituted.pdf"
    if ran("substitute-text", run(["pdf", "substitute-text", str(fx), "--find", "ordinary",
                                   "--replace", "SWAPPED", "--out", str(o)])):
        check("substitute: valid (qpdf)", qpdf_ok(o))
        check("substitute: new text present (MuPDF)", "SWAPPED" in all_text(o))
        check("substitute: old text gone (MuPDF)", "ordinary" not in all_text(o))
        check("substitute: new text present (poppler)", "SWAPPED" in text_poppler(o))
        check("substitute: rest of the line intact",
              "body text that must survive" in all_text(o))

    box = locate(fx, MARKER)
    o = WORK / "overprinted.pdf"
    if box and ran("overprint", run(["pdf", "overprint", str(fx), "--rect", rect_arg(1, box),
                                     "--text", "OVERPRINTED", "--out", str(o)])):
        check("overprint: valid (qpdf)", qpdf_ok(o))
        check("overprint: new text present (MuPDF)", "OVERPRINTED" in page_text(o, 0))
        check("overprint: old text deliberately still there", MARKER in page_text(o, 0),
              "tier 1 covers rather than removes, and says so")


def verify_forms(fx):
    print("\n-- AcroForms (P32.5) --")
    import fitz, pypdf
    src = WORK / "form.pdf"
    d = mupdf_doc(fx)
    pg = d[0]
    w = fitz.Widget()
    w.field_name = "fullName"
    w.field_type = fitz.PDF_WIDGET_TYPE_TEXT
    w.rect = fitz.Rect(72, 300, 300, 320)
    w.field_value = "Ada Lovelace"
    pg.add_widget(w)
    d.save(str(src))
    d.close()

    p = run(["--format", "json", "pdf", "form-fields", str(src)])
    if ran("form-fields", p):
        fields = json.loads(p.stdout)
        names = {f["name"] for f in fields}
        check("forms: reads a form MuPDF authored", "fullName" in names, str(names))
        check("forms: reads the value", any(f.get("value") == "Ada Lovelace" for f in fields))

    filled = WORK / "filled.pdf"
    if ran("fill-form", run(["pdf", "fill-form", str(src), "--set", "fullName=Grace Hopper",
                             "--out", str(filled)])):
        check("fill: valid (qpdf)", qpdf_ok(filled))
        got = pypdf.PdfReader(str(filled)).get_fields() or {}
        val = next((v.get("/V") for k, v in got.items() if "fullName" in str(k)), None)
        check("fill: pypdf reads the new value", str(val) == "Grace Hopper", str(val))
        d = mupdf_doc(filled)
        vals = [wd.field_value for pg in d for wd in pg.widgets()]
        d.close()
        check("fill: MuPDF reads the new value", "Grace Hopper" in vals, str(vals))

        flat = WORK / "flat.pdf"
        if ran("flatten-form", run(["pdf", "flatten-form", str(filled), "--out", str(flat)])):
            check("flatten: valid (qpdf)", qpdf_ok(flat))
            d = mupdf_doc(flat)
            n = sum(1 for pg in d for _ in pg.widgets())
            d.close()
            check("flatten: no interactive fields remain (MuPDF)", n == 0, f"{n} left")
            check("flatten: value is now page text", "Grace Hopper" in all_text(flat))
            check("flatten: pypdf sees no form",
                  not (pypdf.PdfReader(str(flat)).get_fields() or {}))


def verify_annotations(fx):
    print("\n-- annotations (P32.3) --")
    import fitz
    src = WORK / "annotated.pdf"
    d = mupdf_doc(fx)
    pg = d[0]
    a = pg.add_highlight_annot(fitz.Rect(70, 130, 300, 150))
    a.set_info(content="a highlight from MuPDF", title="Tester")
    a.update()
    n = pg.add_text_annot(fitz.Point(400, 200), "a sticky note")
    n.set_info(title="Tester")
    n.update()
    d.save(str(src))
    d.close()

    p = run(["--format", "json", "pdf", "annotations", str(src)])
    if ran("annotations", p):
        got = json.loads(p.stdout)
        kinds = {g["ann_type"] for g in got}
        check("annots: reads what MuPDF wrote", len(got) >= 2, str(len(got)))
        check("annots: highlight recognised", "highlight" in kinds, str(kinds))
        check("annots: note recognised", "note" in kinds, str(kinds))
        check("annots: contents preserved",
              any("highlight from MuPDF" in (g.get("text") or "") for g in got))
        check("annots: author read from /T", any(g.get("author") == "Tester" for g in got))

    for fmt, ext in (("markdown", "md"), ("csv", "csv"), ("json", "json")):
        o = WORK / f"annots.{ext}"
        if ran(f"export-annotations {fmt}",
               run(["pdf", "export-annotations", str(src), "--to", fmt, "--out", str(o)])):
            check(f"export {fmt}: contains the annotation",
                  "highlight from MuPDF" in o.read_text())

    DATA.mkdir(parents=True, exist_ok=True)
    if ran("import-annotations", run(["pdf", "import-annotations", str(src),
                                      "--doc-id", "verify-doc", "--data-dir", str(DATA)])):
        rows = store_rows("annotations")
        check("store: rows landed in SQLite (sqlite3)", len(rows) >= 2, f"{len(rows)} rows")
        check("store: content is in the row",
              any("highlight from MuPDF" in str(r) for r in rows))
        before = len(rows)
        run(["pdf", "import-annotations", str(src), "--doc-id", "verify-doc",
             "--data-dir", str(DATA)])
        after = len(store_rows("annotations"))
        check("store: re-import adds nothing (sqlite3)", after == before, f"{before} → {after}")

    stamped = WORK / "stamped.pdf"
    if ran("stamp-annotations", run(["pdf", "stamp-annotations", str(fx), "--doc-id",
                                     "verify-doc", "--out", str(stamped),
                                     "--data-dir", str(DATA)])):
        check("stamp: valid (qpdf)", qpdf_ok(stamped))
        d = mupdf_doc(stamped)
        infos = [an.info.get("content", "") for pg in d for an in pg.annots()]
        d.close()
        check("stamp: MuPDF sees annotations in a file that had none",
              len(infos) >= 2, str(len(infos)))
        check("stamp: contents survived the round trip",
              any("highlight from MuPDF" in c for c in infos), str(infos)[:80])


def verify_kindle(fx):
    print("\n-- Kindle clippings (P32.4) --")
    clip = WORK / "My Clippings.txt"
    clip.write_text(
        "Thinking, Fast and Slow (Daniel Kahneman)\n"
        "- Your Highlight on page 12 | Location 234-236 | Added on Monday, 1 January 2024 12:00:00\n"
        "\n"
        f"{BODY}\n"
        "==========\n"
        "Another Book (Someone Else)\n"
        "- Your Highlight on page 3 | Location 10-12 | Added on Tuesday, 2 January 2024 12:00:00\n"
        "\n"
        "text from a different book entirely\n"
        "==========\n",
        encoding="utf-8",
    )
    p = run(["--format", "json", "pdf", "kindle-books", str(clip)])
    if ran("kindle-books", p):
        books = json.loads(p.stdout)
        check("kindle: lists both books", len(books) == 2, str(books))

    p = run(["--format", "json", "pdf", "kindle-import", str(clip), "--doc-id", "kindle-doc",
             "--title", "Thinking", "--document", str(fx), "--data-dir", str(DATA)])
    if ran("kindle-import", p):
        summary = json.loads(p.stdout)
        check("kindle: title filter kept one book", summary["deduped"] == 1, str(summary)[:110])
        check("kindle: imported it", summary["imported"] == 1)
        check("kindle: located the passage in the document", summary["matched"] == 1,
              f"matched={summary['matched']}")

        rows = [r for r in store_rows("highlights") if "kindle-doc" in str(r)]
        check("kindle: highlight row in SQLite (sqlite3)", len(rows) == 1, str(len(rows)))
        if rows:
            check("kindle: stored offsets are non-zero (anchored)",
                  any(isinstance(v, int) and v > 0 for v in rows[0][3:5]), str(rows[0][:6]))
        before = len(rows)
        run(["--format", "json", "pdf", "kindle-import", str(clip), "--doc-id", "kindle-doc",
             "--title", "Thinking", "--document", str(fx), "--data-dir", str(DATA)])
        after = len([r for r in store_rows("highlights") if "kindle-doc" in str(r)])
        check("kindle: re-import adds nothing (sqlite3)", after == before, f"{before} → {after}")


def verify_sanitise():
    print("\n-- sanitise --")
    src = WORK / "meta.pdf"
    if not src.exists():
        return
    o = WORK / "sanitised.pdf"
    if ran("sanitise", run(["pdf", "sanitise", str(src), "--out", str(o)])):
        check("sanitise: valid (qpdf)", qpdf_ok(o))
        import pikepdf
        with pikepdf.open(str(o)) as pdf:
            info = {str(k): str(v) for k, v in (pdf.docinfo or {}).items()}
        check("sanitise: title stripped (pikepdf)", info.get("/Title") in (None, ""),
              str(info)[:85])
        check("sanitise: page text untouched", MARKER in all_text(o))


def main():
    if not Path(CLI).exists():
        print(f"CLI not found: {CLI}", file=sys.stderr)
        return 2
    fx = make_fixture()
    check("fixture valid (qpdf --check)", qpdf_ok(fx))
    for fn, nm in ((text_poppler, "poppler"), (all_text, "MuPDF"), (text_pypdf, "pypdf")):
        check(f"fixture marker visible to {nm}", MARKER in fn(fx))

    verify_page_ops(fx)
    verify_redaction(fx)
    verify_crypto(fx)
    verify_text_edit(fx)
    verify_forms(fx)
    verify_annotations(fx)
    verify_kindle(fx)
    verify_sanitise()

    print()
    passed = sum(1 for _, ok, _ in results if ok)
    print(f"{passed}/{len(results)} independent checks passed")
    failed = [(n, d) for n, ok, d in results if not ok]
    if failed:
        print("\nFAILED:")
        for n, d in failed:
            print(f"  - {n}" + (f"  ({d})" if d else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
