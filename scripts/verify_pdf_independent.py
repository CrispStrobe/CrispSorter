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
    # Joined with a newline, not "": pypdf yields per-page strings that do not
    # end in one, so concatenating them fuses the last word of a page to the
    # first of the next ("survivePage") and no reader would ever produce that.
    return "\n".join((pg.extract_text() or "") for pg in pypdf.PdfReader(str(path)).pages)


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


def stream_filters(path):
    """Every stream's /Filter names, one list per stream, as pikepdf sees them."""
    import pikepdf
    out = []
    with pikepdf.open(str(path)) as pdf:
        for obj in pdf.objects:
            try:
                f = obj.get("/Filter")
            except Exception:
                continue  # not a dictionary-like object
            if f is None:
                continue
            out.append(re.findall(r"/\w+", str(f)))
    return out


def pikepdf_page_count(path):
    import pikepdf
    with pikepdf.open(str(path)) as pdf:
        return len(pdf.pages)


def raw_stream_bodies(path):
    """Every stream's bytes as stored, without decoding. [] if none."""
    import pikepdf
    out = []
    with pikepdf.open(str(path)) as pdf:
        for obj in pdf.objects:
            try:
                out.append(bytes(obj.read_raw_bytes()))
            except Exception:
                continue  # not a stream
    return out or [b""]


def make_compress_fixture():
    """A document with long, uncompressed, deflatable content streams.

    Two properties have to hold at once and neither is free:

    * **Uncompressed.** MuPDF deflates content streams on save, so a fixture
      it wrote proves nothing about stream compression — every "the streams
      are deflated" check would already hold. qpdf uncompresses them for us.
    * **Long enough to be worth deflating.** lopdf declines to deflate a
      stream unless the result is at least 19 bytes smaller, so a one-line
      page is *correctly* left alone. A short fixture would make "0 streams
      compressed" the right answer and the check meaningless.
    """
    import fitz
    doc = fitz.open()
    # One insert_text call per page, not one per line: MuPDF appends a
    # *separate* content stream per call, so 40 calls give 40 short streams
    # and nothing worth deflating. Passing the lines as a sequence puts them
    # all in one stream.
    body = [MARKER] + [f"{BODY} line {k}" for k in range(40)]
    for i in range(4):
        pg = doc.new_page()
        pg.insert_text((72, 60), [f"Page {i + 1} heading"] + body, fontsize=10)
    fat = WORK / "fat.pdf"
    doc.save(str(fat))
    doc.close()
    out = WORK / "uncompressed.pdf"
    p = subprocess.run(["qpdf", "--stream-data=uncompress", str(fat), str(out)],
                       capture_output=True, text=True)
    return out if (p.returncode in (0, 3) and out.exists()) else None


def verify_compression(fx):
    print("\n-- compression (P32.6) --")
    src = make_compress_fixture()
    if not check("compress: qpdf produced an uncompressed input", src is not None):
        return
    if not check("compress: that input really has no filtered streams (pikepdf)",
                 not any(fs for fs in stream_filters(src)),
                 "without this every deflation check below passes trivially"):
        return
    longest = max(len(s) for s in raw_stream_bodies(src))
    if not check("compress: its streams are long enough to be worth deflating",
                 longest > 2000,
                 f"longest stream is {longest} B; lopdf declines to deflate a stream that "
                 "would not shrink by 19 bytes, so a short fixture would make "
                 "'0 compressed' the correct answer"):
        return

    o = WORK / "compressed.pdf"
    p = run(["--format", "json", "pdf", "compress", str(src), "--out", str(o)])
    if ran("compress", p):
        rep = json.loads(p.stdout)
        check("compress: valid (qpdf)", qpdf_ok(o))
        check("compress: page count unchanged (MuPDF)",
              page_count(o) == page_count(src),
              f"{page_count(src)} → {page_count(o)}")
        check("compress: smaller on disk (os.stat, not our report)",
              o.stat().st_size < src.stat().st_size,
              f"{src.stat().st_size} → {o.stat().st_size}")
        check("compress: reported size is the real size",
              rep["bytes_after"] == o.stat().st_size,
              f"reported {rep['bytes_after']}, actual {o.stat().st_size}")
        check("compress: it reports the streams it deflated",
              rep["streams_compressed"] >= page_count(src),
              f"{rep['streams_compressed']} for {page_count(src)} pages")
        # Content preservation, from two readers that share no code with us.
        check("compress: text intact (MuPDF)",
              MARKER in all_text(o) and BODY in all_text(o))
        check("compress: text intact (poppler)", MARKER in text_poppler(o))
        check("compress: text identical to the input (poppler, word-for-word)",
              text_poppler(o).split() == text_poppler(src).split())
        # Object streams only pay off if the packed objects stay reachable —
        # which is exactly what broke once (8 pages → 0).
        raw = o.read_bytes()
        check("compress: object streams were written", b"/ObjStm" in raw)
        check("compress: xref stream accompanies them", b"/XRef" in raw,
              "packed objects are unreachable from a classic xref table")
        check("compress: pikepdf reaches every page through them",
              pikepdf_page_count(o) == page_count(src))
        flat = [f for fs in stream_filters(o) for f in fs]
        check("compress: streams that were raw are now deflated (pikepdf)",
              flat.count("/FlateDecode") >= page_count(src), str(flat[:6]))

    # Opting out must actually opt out. This variant also lets the reported
    # count be compared exactly: without object streams the writer invents no
    # streams of its own, so pikepdf's tally and ours must agree.
    plain = WORK / "compressed-classic.pdf"
    p3 = run(["--format", "json", "pdf", "compress", str(src), "--out", str(plain),
              "--no-object-streams"])
    if ran("compress --no-object-streams", p3):
        raw = plain.read_bytes()
        check("compress: --no-object-streams writes none", b"/ObjStm" not in raw)
        check("compress: --no-object-streams still valid (qpdf)", qpdf_ok(plain))
        check("compress: --no-object-streams keeps the text (MuPDF)",
              MARKER in all_text(plain))
        deflated = sum(1 for fs in stream_filters(plain) if "/FlateDecode" in fs)
        reported = json.loads(p3.stdout)["streams_compressed"]
        check("compress: the reported count is what pikepdf finds deflated",
              reported == deflated, f"reported {reported}, pikepdf sees {deflated}")

    # Honesty on a document with nothing to gain: the original fixture's
    # streams are a line or two long, so lopdf rightly declines to deflate
    # them — and the report must say 0, not one per stream it merely tried.
    short = WORK / "uncompressed-short.pdf"
    q = subprocess.run(["qpdf", "--stream-data=uncompress", str(fx), str(short)],
                       capture_output=True, text=True)
    if q.returncode in (0, 3) and short.exists():
        so = WORK / "short-compressed.pdf"
        p4 = run(["--format", "json", "pdf", "compress", str(short), "--out", str(so),
                  "--no-object-streams"])
        if ran("compress (nothing to gain)", p4):
            reported = json.loads(p4.stdout)["streams_compressed"]
            deflated = sum(1 for fs in stream_filters(so) if "/FlateDecode" in fs)
            check("compress: streams too short to deflate are reported as untouched",
                  reported == deflated == 0, f"reported {reported}, pikepdf sees {deflated}")
            check("compress: and the text is still there (poppler)",
                  MARKER in text_poppler(so))

    nostream = WORK / "compressed-nostream.pdf"
    p2 = run(["--format", "json", "pdf", "compress", str(src), "--out", str(nostream),
              "--no-stream-compression", "--no-object-streams"])
    if ran("compress --no-stream-compression", p2):
        check("compress: --no-stream-compression deflates nothing",
              json.loads(p2.stdout)["streams_compressed"] == 0)
        check("compress: --no-stream-compression leaves the streams raw (pikepdf)",
              not any("/FlateDecode" in fs for fs in stream_filters(nostream)))

    # Running it twice must not deflate anything twice: a /Filter array with
    # two FlateDecode entries is data compressed on top of itself.
    twice = WORK / "compressed-twice.pdf"
    if o.exists() and ran("compress (second pass)",
                          run(["pdf", "compress", str(o), "--out", str(twice)])):
        doubled = [fs for fs in stream_filters(twice) if fs.count("/FlateDecode") > 1]
        check("compress: nothing is deflated twice (pikepdf)", not doubled, str(doubled[:2]))
        check("compress: second pass keeps every page (MuPDF)",
              page_count(twice) == page_count(src))
        check("compress: second pass keeps the text (poppler)",
              MARKER in text_poppler(twice))


REGION_BODY = (
    "The quick brown fox jumps over the lazy dog and then keeps running "
    "across the page until the words have to wrap several times over."
)


def region_lines(path, box, page=0):
    """Lines MuPDF finds inside `box` (PDF y-up coords), via a clip."""
    import fitz
    d = mupdf_doc(path)
    pg = d[page]
    h = pg.rect.height
    x, y, w, hh = box
    clip = fitz.Rect(x - 1, h - (y + hh) - 1, x + w + 1, h - y + 1)
    txt = pg.get_text("text", clip=clip)
    d.close()
    return [ln for ln in txt.splitlines() if ln.strip()]


def word_rects(path, needle, page=0):
    """Every hit for `needle`, in PDF y-up coords."""
    d = mupdf_doc(path)
    pg = d[page]
    h = pg.rect.height
    hits = [(r.x0, h - r.y1, r.x1, h - r.y0) for r in pg.search_for(needle)]
    d.close()
    return hits


def verify_text_region(fx):
    print("\n-- text regions (P32.9) --")
    # A box in empty space on page 1, well clear of the fixture's own text
    # (which sits at y-up ≈ 130–220 on an A4 page). Never guess a coordinate
    # that has to be *empty* either: the clip below would otherwise pick up
    # the fixture's lines and the line-count check would pass by accident.
    box = (72.0, 400.0, 220.0, 120.0)
    rect = f"1,{box[0]},{box[1]},{box[2]},{box[3]}"
    check("region: the target box starts empty (MuPDF)", not region_lines(fx, box),
          "a box that already holds text makes every count below meaningless")
    o = WORK / "region.pdf"
    p = run(["--format", "json", "pdf", "text-region", str(fx), "--rect", rect,
             "--text", REGION_BODY, "--font", "helvetica", "--size", "10",
             "--align", "left", "--out", str(o)])
    if ran("text-region", p):
        rep = json.loads(p.stdout)
        check("region: valid (qpdf)", qpdf_ok(o))
        check("region: text is on the page (MuPDF)", "quick brown fox" in page_text(o, 0))
        check("region: text is on the page (poppler)", "quick brown fox" in text_poppler(o))
        check("region: the fixture's own text survived", MARKER in page_text(o, 0))
        check("region: no overflow for a box this size", not rep["overflow"], str(rep))

        lines = region_lines(o, box)
        check("region: MuPDF counts the lines the layout reported",
              len(lines) == rep["lines"], f"MuPDF {len(lines)} vs reported {rep['lines']}")
        check("region: it did wrap (more than one line)", len(lines) > 1, str(len(lines)))

        # The whole point of the width tables: nothing may sit outside the box.
        x0, y0, w, h = box
        outside = []
        for word in ("quick", "brown", "running", "several", "wrap"):
            for (wx0, wy0, wx1, wy1) in word_rects(o, word):
                if wx0 < x0 - 1 or wx1 > x0 + w + 1 or wy0 < y0 - 1 or wy1 > y0 + h + 1:
                    outside.append((word, round(wx0, 1), round(wx1, 1)))
        check("region: every word MuPDF finds is inside the box", not outside, str(outside[:3]))

    # Overflow must be reported and *not* drawn.
    tiny = (300.0, 500.0, 120.0, 22.0)
    check("region: the overflow box starts empty (MuPDF)", not region_lines(fx, tiny))
    o2 = WORK / "region-overflow.pdf"
    p = run(["--format", "json", "pdf", "text-region", str(fx),
             "--rect", f"1,{tiny[0]},{tiny[1]},{tiny[2]},{tiny[3]}",
             "--text", REGION_BODY, "--size", "10", "--out", str(o2)])
    if ran("text-region (overflow)", p):
        rep = json.loads(p.stdout)
        check("region: overflow reported", rep["overflow"] and rep["lines_dropped"] > 0, str(rep))
        check("region: overflow is not drawn (MuPDF finds no tail words)",
              "several times over" not in page_text(o2, 0).replace("\n", " "))
        check("region: only the fitting lines were drawn",
              len(region_lines(o2, tiny)) == rep["lines"],
              f"MuPDF {len(region_lines(o2, tiny))} vs reported {rep['lines']}")

    # Alignment is geometry, so a reader can check it.
    xs = {}
    for align in ("left", "right"):
        oa = WORK / f"region-{align}.pdf"
        if ran(f"text-region --align {align}",
               run(["pdf", "text-region", str(fx), "--rect", rect, "--text", "short line",
                    "--align", align, "--size", "10", "--out", str(oa)])):
            hits = word_rects(oa, "short")
            xs[align] = hits[0][0] if hits else None
    if xs.get("left") is not None and xs.get("right") is not None:
        check("region: right-aligned text starts further right (MuPDF)",
              xs["right"] > xs["left"] + 10, f"{xs['left']:.1f} vs {xs['right']:.1f}")

    # Latin-1 accents: trap 4 seen from the outside. If the width tables were
    # keyed by the AFM's Adobe-Standard codes again, these would be reported
    # unsupported and never drawn.
    o3 = WORK / "region-accents.pdf"
    p = run(["--format", "json", "pdf", "text-region", str(fx), "--rect", rect,
             "--text", "Übermäßig café naïve", "--size", "12", "--out", str(o3)])
    if ran("text-region (accents)", p):
        rep = json.loads(p.stdout)
        check("region: accents are not reported unsupported",
              rep["unsupported_chars"] == [], str(rep["unsupported_chars"]))
        got = page_text(o3, 0)
        check("region: MuPDF reads the accents back", "Übermäßig" in got,
              repr([ln for ln in got.splitlines() if "berm" in ln][:1]))
        check("region: poppler reads the accents back", "café" in text_poppler(o3))

    # A character the face has no glyph for is reported, not silently dropped.
    o4 = WORK / "region-cjk.pdf"
    p = run(["--format", "json", "pdf", "text-region", str(fx), "--rect", rect,
             "--text", "hello 日本語 world", "--size", "12", "--out", str(o4)])
    if ran("text-region (unsupported)", p):
        rep = json.loads(p.stdout)
        check("region: unsupported characters are reported",
              "日" in rep["unsupported_chars"], str(rep["unsupported_chars"]))
        check("region: and are absent from the page (MuPDF)", "日" not in page_text(o4, 0))
        check("region: the renderable rest is still drawn", "hello" in page_text(o4, 0))

    # --border draws a real stroked rectangle, which MuPDF can enumerate.
    o5 = WORK / "region-border.pdf"
    if ran("text-region --border",
           run(["pdf", "text-region", str(fx), "--rect", rect, "--text", "boxed",
                "--border", "--out", str(o5)])):
        d = mupdf_doc(o5)
        pg = d[0]
        ph = pg.rect.height
        near = []
        for dr in pg.get_drawings():
            r = dr["rect"]
            if (abs(r.x0 - box[0]) < 2 and abs(r.width - box[2]) < 2
                    and abs((ph - r.y1) - box[1]) < 2 and abs(r.height - box[3]) < 2):
                near.append(dr.get("type"))
        d.close()
        check("region: --border strokes the box (MuPDF get_drawings)",
              any(t in ("s", "fs") for t in near), str(near[:3]))

    # A face with no width table must be refused, not silently substituted.
    bad = run(["pdf", "text-region", str(fx), "--rect", rect, "--text", "x",
               "--font", "comic-sans", "--out", str(WORK / "nope.pdf")])
    check("region: an unknown --font is refused", bad.returncode != 0,
          (bad.stderr or "").strip()[:80])


def verify_text_extraction(fx):
    print("\n-- text extraction (pdf text) --")
    p = run(["--format", "json", "pdf", "text", str(fx)])
    if ran("pdf text", p):
        got = json.loads(p.stdout)["text"]
        check("text: marker extracted", MARKER in got)
        check("text: body extracted", BODY in got)
        check("text: every word poppler finds we find too",
              set(text_poppler(fx).split()) <= set(got.split()),
              str(sorted(set(text_poppler(fx).split()) - set(got.split()))[:5]))
        check("text: every word pypdf finds we find too",
              set(text_pypdf(fx).split()) <= set(got.split()),
              str(sorted(set(text_pypdf(fx).split()) - set(got.split()))[:5]))

        o = WORK / "extracted.txt"
        if ran("pdf text --out", run(["pdf", "text", str(fx), "--out", str(o)])):
            check("text: --out writes the same text",
                  o.read_text(encoding="utf-8") == got,
                  f"{len(o.read_text(encoding='utf-8'))} vs {len(got)} chars")


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
    verify_compression(fx)
    verify_text_region(fx)
    verify_text_extraction(fx)
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
