#!/usr/bin/env python
"""Independent verification of CrispSorter's P32 PDF work.

Every assertion here is made with a tool that shares no code with what it
is checking: qpdf (C++, the library we deliberately did not link),
poppler's pdftotext, PyMuPDF (MuPDF's own parser), pypdf (pure Python) and
pikepdf. Where a claim matters — "the redacted text is really gone" — it
is checked with more than one, because a single extractor agreeing with us
could just mean we both miss the same thing.
"""
import json
import os
import re
import subprocess
import sys
import zlib
from pathlib import Path

CLI = os.environ["CRISP_CLI"]
WORK = Path(os.environ["CRISP_WORK"])
WORK.mkdir(parents=True, exist_ok=True)

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  — {detail}" if detail else ""))
    return ok


def run(args, **kw):
    """Run the CrispSorter CLI."""
    p = subprocess.run([CLI] + args, capture_output=True, text=True, **kw)
    return p


def qpdf_check(path):
    """qpdf --check: structural validation by an independent implementation."""
    p = subprocess.run(["qpdf", "--check", str(path)], capture_output=True, text=True)
    # qpdf exits 3 for warnings, 0 for clean; 2 means it could not read it.
    return p.returncode in (0, 3), (p.stdout + p.stderr).strip()


def text_poppler(path):
    p = subprocess.run(["pdftotext", str(path), "-"], capture_output=True, text=True)
    return p.stdout


def text_mupdf(path):
    import fitz
    doc = fitz.open(str(path))
    out = "".join(page.get_text() for page in doc)
    doc.close()
    return out


def text_pypdf(path):
    import pypdf
    r = pypdf.PdfReader(str(path))
    return "".join((pg.extract_text() or "") for pg in r.pages)


def page_content(path, page_index):
    """The decompressed content stream of one page.

    This is the check a black rectangle fails: an extractor might not
    report covered text, but the operators are still in the stream.
    """
    import pikepdf
    with pikepdf.open(str(path)) as pdf:
        contents = pdf.pages[page_index].obj.get("/Contents")
        if contents is None:
            return b""
        # /Contents is a stream or an array of streams; the array form is
        # what our own appends produce, so both must be handled.
        try:
            return bytes(contents.read_bytes())
        except Exception:
            return b"".join(bytes(c.read_bytes()) for c in contents)


def page_text_mupdf(path, page_index):
    import fitz
    doc = fitz.open(str(path))
    t = doc[page_index].get_text()
    doc.close()
    return t


def page_text_pypdf(path, page_index):
    import pypdf
    return pypdf.PdfReader(str(path)).pages[page_index].extract_text() or ""


# ── Fixture ────────────────────────────────────────────────────────────
SRC = Path(os.environ.get("CRISP_SAMPLE", "")).resolve()


def make_fixture():
    """A multi-page PDF with known text, built by an independent tool so
    the input is not something our own writer produced."""
    import fitz
    doc = fitz.open()
    for i in range(4):
        page = doc.new_page()
        page.insert_text((72, 700), f"Page {i + 1} heading", fontsize=18)
        page.insert_text((72, 660), "CONFIDENTIAL-MARKER-42", fontsize=12)
        page.insert_text((72, 640), "ordinary body text that must survive", fontsize=12)
    out = WORK / "fixture.pdf"
    doc.save(str(out))
    doc.close()
    return out


def main():
    fixture = make_fixture()
    ok, msg = qpdf_check(fixture)
    check("fixture is structurally valid (qpdf --check)", ok, msg.splitlines()[0] if msg else "")

    # Sanity: all three extractors see the marker in the input.
    for fn, nm in ((text_poppler, "poppler"), (text_mupdf, "mupdf"), (text_pypdf, "pypdf")):
        check(f"fixture marker visible to {nm}", "CONFIDENTIAL-MARKER-42" in fn(fixture))

    # ── 1. Page operations ─────────────────────────────────────────────
    out = WORK / "removed.pdf"
    p = run(["pdf", "remove", str(fixture), "--pages", "2", "--out", str(out)])
    if p.returncode != 0:
        # Subcommand naming differs; discover it.
        check("pdf remove ran", False, p.stderr.strip()[:200])
    else:
        ok, msg = qpdf_check(out)
        check("remove-pages output valid (qpdf)", ok, msg.splitlines()[0] if msg else "")
        import fitz
        d = fitz.open(str(out))
        check("remove-pages dropped exactly one page (mupdf)", d.page_count == 3,
              f"page_count={d.page_count}")
        d.close()

    # ── 2. Redaction — the claim that matters most ─────────────────────
    red = WORK / "redacted.pdf"
    # Ask MuPDF where the marker actually is, in PDF (y-up) coordinates.
    # Hard-coding a guess is how the first run of this harness ended up
    # redacting empty space and reporting success.
    import fitz
    _d = fitz.open(str(fixture))
    _pg = _d[0]
    _h = _pg.rect.height
    _hits = _pg.search_for("CONFIDENTIAL-MARKER-42")
    assert _hits, "fixture marker not found by MuPDF"
    _r = _hits[0]
    rect_arg = f"1,{_r.x0 - 2:.1f},{_h - _r.y1 - 2:.1f},{_r.width + 4:.1f},{_r.height + 4:.1f}"
    _d.close()
    print(f"      (redacting {rect_arg})")
    p = run(["pdf", "redact-regions", str(fixture), "--rect", rect_arg, "--out", str(red)])
    if p.returncode != 0:
        check("pdf redact-regions ran", False, p.stderr.strip()[:300])
    else:
        ok, msg = qpdf_check(red)
        check("redacted output valid (qpdf)", ok, msg.splitlines()[0] if msg else "")

        # Only page 1 was redacted; the fixture repeats the marker on
        # every page, so this must be checked per page.
        check("redacted text gone from page 1 (MuPDF)",
              "CONFIDENTIAL-MARKER-42" not in page_text_mupdf(red, 0),
              repr(page_text_mupdf(red, 0)[:70]))
        check("redacted text gone from page 1 (pypdf)",
              "CONFIDENTIAL-MARKER-42" not in page_text_pypdf(red, 0))
        check("redacted text absent from page 1's content stream",
              b"CONFIDENTIAL-MARKER-42" not in page_content(red, 0),
              "the check a black rectangle would fail")

        # And the marker must survive where it was not redacted, or we
        # have removed too much.
        check("marker still present on page 2 (not over-removed)",
              "CONFIDENTIAL-MARKER-42" in page_text_mupdf(red, 1))
        check("neighbouring text on page 1 survived",
              "ordinary body text" in page_text_mupdf(red, 0),
              repr(page_text_mupdf(red, 0)[:70]))
        check("other pages untouched", "Page 2 heading" in page_text_mupdf(red, 1))

    # ── 3. AES-256 encryption ──────────────────────────────────────────
    enc = WORK / "encrypted.pdf"
    p = run(["pdf", "encrypt", str(fixture), "--owner-password", "ownerpw",
             "--user-password", "userpw", "--out", str(enc)])
    if p.returncode != 0:
        check("pdf encrypt ran", False, p.stderr.strip()[:300])
    else:
        import pikepdf
        # /V and /R identify the security handler. AESV3 is V=5, R=6.
        data = enc.read_bytes()
        v5 = re.search(rb"/V\s+5", data) is not None
        r6 = re.search(rb"/R\s+6", data) is not None
        aesv3 = b"AESV3" in data
        check("encrypted with V5 (AES-256 handler)", v5)
        check("revision R6", r6)
        check("AESV3 crypt filter present", aesv3)
        check("no RC4 artefacts", b"/V 2" not in data)

        try:
            with pikepdf.open(str(enc), password="userpw") as pdf:
                enc_info = pdf.encryption
                check("pikepdf opens it with the user password", True,
                      f"{enc_info.encryption_method if hasattr(enc_info,'encryption_method') else ''}")
                check("pikepdf reports 256-bit key",
                      getattr(enc_info, "R", None) == 6 or "256" in str(enc_info),
                      str(enc_info)[:120])
        except Exception as e:
            check("pikepdf opens it with the user password", False, str(e)[:200])

        try:
            with pikepdf.open(str(enc)) as pdf:
                check("opening without a password is refused", False, "opened unencrypted!")
        except pikepdf.PasswordError:
            check("opening without a password is refused", True)
        except Exception as e:
            check("opening without a password is refused", "assword" in str(e), str(e)[:120])

    # ── 4. Text substitution ───────────────────────────────────────────
    sub = WORK / "substituted.pdf"
    p = run(["pdf", "substitute-text", str(fixture),
             "--find", "ordinary", "--replace", "SWAPPED", "--out", str(sub)])
    if p.returncode != 0:
        check("pdf substitute-text ran", False, p.stderr.strip()[:300])
    else:
        ok, msg = qpdf_check(sub)
        check("substituted output valid (qpdf)", ok, msg.splitlines()[0] if msg else "")
        t = text_mupdf(sub)
        check("replacement present (MuPDF)", "SWAPPED" in t)
        check("original string gone (MuPDF)", "ordinary" not in t)
        check("replacement present (poppler)", "SWAPPED" in text_poppler(sub))

    # ── 5. Annotations: written by MuPDF, read by us ───────────────────
    import fitz
    ann_src = WORK / "annotated.pdf"
    d = fitz.open(str(fixture))
    pg = d[0]
    a = pg.add_highlight_annot(fitz.Rect(70, 130, 300, 150))
    a.set_info(content="a highlight from MuPDF", title="Tester")
    a.update()
    n = pg.add_text_annot(fitz.Point(400, 200), "a sticky note")
    n.set_info(title="Tester")
    n.update()
    d.save(str(ann_src))
    d.close()

    p = run(["--format", "json", "pdf", "annotations", str(ann_src)])
    if p.returncode != 0:
        check("pdf annotations ran", False, p.stderr.strip()[:300])
    else:
        try:
            got = json.loads(p.stdout)
        except Exception as e:
            got = []
            check("annotations output is json", False, str(e)[:120] + p.stdout[:120])
        kinds = {g["ann_type"] for g in got}
        check("reads annotations another tool wrote", len(got) >= 2, f"{len(got)} found")
        check("highlight recognised", "highlight" in kinds, str(kinds))
        check("sticky note recognised", "note" in kinds, str(kinds))
        texts = " ".join(g.get("text") or "" for g in got)
        check("annotation contents round-tripped", "highlight from MuPDF" in texts,
              texts[:80])
        check("author read from /T", any(g.get("author") == "Tester" for g in got))

    # ── 6. AcroForm: built by an independent tool, read by us ──────────
    import pikepdf
    form_src = WORK / "form.pdf"
    # pypdf can author a simple text field.
    try:
        import pypdf
        from pypdf.generic import NameObject, TextStringObject
        w = pypdf.PdfWriter()
        w.append(str(fixture))
        w.add_page(pypdf.PdfReader(str(fixture)).pages[0])
        # pypdf's form authoring API varies; fall back to skipping.
        raise NotImplementedError
    except Exception:
        # Build the form with fitz instead — it has a first-class widget API.
        d = fitz.open(str(fixture))
        pg = d[0]
        wdg = fitz.Widget()
        wdg.field_name = "fullName"
        wdg.field_type = fitz.PDF_WIDGET_TYPE_TEXT
        wdg.rect = fitz.Rect(72, 300, 300, 320)
        wdg.field_value = "Ada Lovelace"
        pg.add_widget(wdg)
        d.save(str(form_src))
        d.close()

    p = run(["--format", "json", "pdf", "form-fields", str(form_src)])
    if p.returncode != 0:
        check("pdf form-fields ran", False, p.stderr.strip()[:300])
    else:
        try:
            fields = json.loads(p.stdout)
        except Exception as e:
            fields = []
            check("form-fields output is json", False, str(e)[:120])
        names = {f["name"] for f in fields}
        check("reads a form another tool built", "fullName" in names, str(names))
        check("reads the field value",
              any(f.get("value") == "Ada Lovelace" for f in fields),
              str([f.get("value") for f in fields]))

    flat = WORK / "flattened.pdf"
    p = run(["pdf", "flatten-form", str(form_src), "--out", str(flat)])
    if p.returncode != 0:
        check("pdf flatten-form ran", False, p.stderr.strip()[:300])
    else:
        ok, msg = qpdf_check(flat)
        check("flattened output valid (qpdf)", ok, msg.splitlines()[0] if msg else "")
        d = fitz.open(str(flat))
        widgets = sum(1 for pg in d for _ in pg.widgets())
        d.close()
        check("flatten removed the interactive fields (MuPDF)", widgets == 0,
              f"{widgets} widgets remain")
        check("flattened value is now page text", "Ada Lovelace" in text_mupdf(flat))

    # ── 7. Annotation export ───────────────────────────────────────────
    md = WORK / "annots.md"
    p = run(["pdf", "export-annotations", str(ann_src), "--to", "markdown", "--out", str(md)])
    if p.returncode == 0 and md.exists():
        body = md.read_text()
        check("markdown export contains the annotation text",
              "highlight from MuPDF" in body, body[:80].replace("\n", " "))
    else:
        check("pdf export-annotations ran", False, p.stderr.strip()[:200])

    # ── Summary ────────────────────────────────────────────────────────
    print()
    passed = sum(1 for _, ok, _ in results if ok)
    print(f"{passed}/{len(results)} independent checks passed")
    failed = [n for n, ok, _ in results if not ok]
    if failed:
        print("FAILED:")
        for n in failed:
            print("  -", n)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
