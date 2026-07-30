#!/usr/bin/env python
"""Live end-to-end check of indexing, search, and the zoned-OCR verb.

Unlike the PDF/DOCX harnesses there is no second implementation to grade
against here — the index *is* ours. So the assertions are behavioural and
two-sided instead: a term that occurs in exactly one document must return
that document and not the others; a filter must exclude what it excludes;
a term that occurs nowhere must return nothing. A search that returns
everything scores as well as a correct one against a one-sided check.

Why it exists: `search` and `zone` were unreachable from the command line
until 2026-07-30 (missing from `cli::SUBCOMMANDS`, so argv fell through to
the GUI), which means they had never actually run. Everything here is a
first execution.

Requires an embedder for the ingest leg. `--model minilm` (all-MiniLM-L6-v2,
~90 MB) is the smallest supported; it is cached under CRISP_MODELS so
re-runs are offline. The ingest+search sections skip cleanly, loudly, when
no model can be obtained; the zone section never needs one.

Usage:
    CRISP_CLI=path/to/crispsorter CRISP_WORK=/tmp/idxverify \\
        python scripts/verify_index_search_live.py
"""
import json
import os
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

CLI = os.environ.get("CRISP_CLI", "target/debug/crispsorter")
WORK = Path(os.environ.get("CRISP_WORK", "/tmp/crispsorter-index-verify"))
# Model cache is deliberately *outside* WORK so a wiped work dir does not
# re-download 90 MB every run.
MODELS = Path(os.environ.get("CRISP_MODELS", "/tmp/crispsorter-model-cache"))
MODEL = os.environ.get("CRISP_MODEL", "minilm")
DATA = WORK / "data"
CORPUS = WORK / "corpus"

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  — {detail}" if detail else ""))
    return bool(ok)


class Hung:
    returncode = -1
    stdout = ""
    stderr = ("the process did not exit — a verb missing from cli::SUBCOMMANDS "
              "launches the GUI instead of running")


VERBS = ("index", "search", "zone")


def run(args, timeout=900):
    """Invoke the CLI, always scoped to the test data dir.

    Scoping is not optional: without `--data-dir` these verbs read and write
    the *real* index under Application Support, which is how the first
    version of this harness came to assert against the user's own corpus and
    report empty result sets. The verb can sit behind global flags
    (`--format json index search …`), so find it rather than assuming argv[1].
    """
    args = list(args)
    if "--data-dir" not in args:
        idx = next((i for i, a in enumerate(args) if a in VERBS), None)
        if idx is not None:
            if args[idx] == "index":
                # `--data-dir` is a global on the `index` subcommand.
                args = args[: idx + 1] + ["--data-dir", str(DATA)] + args[idx + 1 :]
            else:
                args = args + ["--data-dir", str(DATA)]
    try:
        return subprocess.run([CLI] + args, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return Hung()


def ran(name, p):
    if p.returncode != 0:
        msg = (p.stderr or p.stdout).strip()
        check(f"{name} ran", False, msg.splitlines()[-1][:220] if msg else f"exit {p.returncode}")
        return False
    return True


def jout(p):
    """Parse stdout as JSON, tolerating leading log lines."""
    s = p.stdout
    for opener in ("{", "["):
        i = s.find(opener)
        if i != -1:
            try:
                return json.loads(s[i:])
            except json.JSONDecodeError:
                continue
    return None


# ── Corpus ──────────────────────────────────────────────────────────────
# Each document owns one distinctive term so precision is checkable, and
# they differ in extension / language / frontmatter so the filters have
# something to discriminate on.
DOCS = {
    "moss.md": (
        "---\n"
        "title: Schimmelpilzgifte im Brot\n"
        "url: https://www.spiegel.de/wissenschaft/mykotoxine\n"
        "tags: [pocket-import, mykotoxin]\n"
        "year: 2021\n"
        "---\n\n"
        "# Mykotoxine\n\n"
        "Stiftung Warentest hat Schimmelpilzgifte gefunden. "
        "Das Zauberwort lautet ZINGIBER und steht nur hier.\n"
    ),
    "barth.txt": (
        "Karl Barth and the doctrine of election. The distinctive token here "
        "is QUOKKAFROST and it appears in no other document.\n"
    ),
    "umlaut.txt": (
        "Übermäßig viele Umlaute: Ärger, Öl, Übung, Straße. "
        "Ein einmaliges Wort ist WALDMEISTERBOWLE.\n"
    ),
    "plain.txt": (
        "An ordinary document about nothing in particular, mentioning "
        "election and bread so the single-hit terms above stay meaningful.\n"
    ),
}


def build_corpus():
    CORPUS.mkdir(parents=True, exist_ok=True)
    for name, body in DOCS.items():
        (CORPUS / name).write_text(body, encoding="utf-8")


def hits_of(payload):
    """Normalise either CLI shape into a list of hit dicts."""
    if payload is None:
        return []
    if isinstance(payload, list):
        return payload
    for key in ("hits", "results", "documents"):
        if isinstance(payload.get(key), list):
            return payload[key]
    return []


def names_in(hits):
    """Filenames mentioned by a result set, however the shape nests them."""
    out = set()
    for h in hits:
        blob = json.dumps(h)
        for n in DOCS:
            if n in blob:
                out.add(n)
    return out


# ── Sections ────────────────────────────────────────────────────────────
def verify_ingest():
    print("\n-- ingest --")
    DATA.mkdir(parents=True, exist_ok=True)
    MODELS.mkdir(parents=True, exist_ok=True)
    # Share one model cache across runs: the ingest handler looks in
    # <data-dir>/models, so link it rather than copying 90 MB.
    link = DATA / "models"
    if not link.exists():
        link.symlink_to(MODELS)

    t0 = time.time()
    p = run(["index", "ingest", str(CORPUS), "--model", MODEL, "--device", "cpu"])
    if not ran("index ingest", p):
        print("   (no embedder available — skipping the ingest/search sections)")
        return False
    print(f"   ingest took {time.time() - t0:.0f}s")

    p = run(["--format", "json", "index", "stats"], timeout=300)
    if ran("index stats", p):
        st = jout(p) or {}
        docs = st.get("doc_count", st.get("documents", st.get("docs")))
        check("stats: every corpus file is in the index",
              docs is not None and docs >= len(DOCS), f"{docs} docs, expected ≥ {len(DOCS)}")
        fts = st.get("fts_doc_count", st.get("fts_docs"))
        if fts is not None:
            check("stats: the FTS index has rows too", fts >= len(DOCS), f"{fts} fts docs")
    return True


def verify_index_search():
    print("\n-- index search (BM25 leg) --")
    # A term in exactly one document: both halves matter.
    p = run(["--format", "json", "index", "search", "QUOKKAFROST"], timeout=300)
    if ran("index search", p):
        hits = hits_of(jout(p))
        found = names_in(hits)
        check("search: the single-hit term finds its document",
              "barth.txt" in found, str(found))
        check("search: and does not return the others",
              found <= {"barth.txt"}, f"also matched {found - {'barth.txt'}}")

    # A term that is in no document must return nothing at all.
    p = run(["--format", "json", "index", "search", "XYZZYNOTHINGHERE"], timeout=300)
    if ran("index search (no match)", p):
        check("search: a term in no document returns no hits",
              not hits_of(jout(p)), "returned hits for a nonsense query")

    # Umlaut folding: an ASCII query should still reach a unicode body, which
    # is the FTS analyser's job.
    p = run(["--format", "json", "index", "search", "WALDMEISTERBOWLE"], timeout=300)
    if ran("index search (umlaut doc)", p):
        check("search: reaches the umlaut document",
              "umlaut.txt" in names_in(hits_of(jout(p))), "")

    # A shared term must return both documents that contain it.
    p = run(["--format", "json", "index", "search", "election"], timeout=300)
    if ran("index search (shared term)", p):
        found = names_in(hits_of(jout(p)))
        check("search: a shared term returns both documents holding it",
              {"barth.txt", "plain.txt"} <= found, str(found))


def verify_unified_search():
    print("\n-- unified `search` verb (first run ever) --")
    p = run(["--format", "json", "search", "QUOKKAFROST", "--local-only"], timeout=300)
    if ran("search --local-only", p):
        payload = jout(p)
        hits = hits_of(payload)
        check("unified: returns the expected document",
              "barth.txt" in names_in(hits), str(names_in(hits)))
        if hits:
            h = hits[0]
            check("unified: hits are badged with their source",
                  h.get("source") == "local", str(h.get("source")))
            check("unified: ids are namespaced by source",
                  str(h.get("id", "")).startswith("local:"), str(h.get("id")))
            check("unified: the RRF rank is 1-based",
                  h.get("rrf_rank") == 1, str(h.get("rrf_rank")))
            snip = h.get("snippet") or ""
            check("unified: the snippet highlights the query term",
                  "<mark>" in snip.lower(), snip[:70])

    # --ext must exclude by extension, asserted in both directions.
    p = run(["--format", "json", "search", "election", "--local-only", "--ext", "txt"], timeout=300)
    if ran("search --ext txt", p):
        found = names_in(hits_of(jout(p)))
        check("unified: --ext txt keeps the .txt documents",
              "barth.txt" in found or "plain.txt" in found, str(found))
        check("unified: --ext txt excludes the .md document",
              "moss.md" not in found, "md document survived a txt-only filter")

    p = run(["--format", "json", "search", "Schimmelpilzgifte", "--local-only",
             "--ext", "md"], timeout=300)
    if ran("search --ext md", p):
        found = names_in(hits_of(jout(p)))
        check("unified: --ext md finds the markdown document", "moss.md" in found, str(found))

    # Frontmatter-derived filters (v106 url / v107 tags).
    p = run(["--format", "json", "search", "Schimmelpilzgifte", "--local-only",
             "--url-domain", "spiegel.de"], timeout=300)
    if ran("search --url-domain", p):
        check("unified: --url-domain matches the frontmatter url",
              "moss.md" in names_in(hits_of(jout(p))),
              "url filter dropped the row whose frontmatter carries that domain")

    p = run(["--format", "json", "search", "Schimmelpilzgifte", "--local-only",
             "--url-domain", "example-not-present.invalid"], timeout=300)
    if ran("search --url-domain (no match)", p):
        check("unified: --url-domain excludes a domain the row lacks",
              not hits_of(jout(p)), "filter matched a domain that is not in the corpus")

    p = run(["--format", "json", "search", "Schimmelpilzgifte", "--local-only",
             "--tag", "pocket-import"], timeout=300)
    if ran("search --tag", p):
        check("unified: --tag matches a frontmatter tag",
              "moss.md" in names_in(hits_of(jout(p))), "")

    p = run(["--format", "json", "search", "Schimmelpilzgifte", "--local-only",
             "--tag", "no-such-tag"], timeout=300)
    if ran("search --tag (no match)", p):
        check("unified: --tag excludes a tag the row lacks", not hits_of(jout(p)), "")

    # --limit must cap the result set.
    p = run(["--format", "json", "search", "election", "--local-only", "--limit", "1"], timeout=300)
    if ran("search --limit 1", p):
        check("unified: --limit caps the result set", len(hits_of(jout(p))) <= 1,
              f"{len(hits_of(jout(p)))} hits for --limit 1")

    # An empty query is a usage error, not an empty result set.
    p = run(["search", "   ", "--local-only"], timeout=120)
    check("unified: an empty query is refused", p.returncode != 0,
          (p.stderr or "").strip()[:80])

    # --local-only and --cloud-only are mutually exclusive (clap-enforced).
    p = run(["search", "x", "--local-only", "--cloud-only"], timeout=120)
    check("unified: --local-only conflicts with --cloud-only", p.returncode != 0,
          (p.stderr or "").strip()[:80])


def verify_zone():
    """The zoned-OCR verb, end to end through templates.db."""
    print("\n-- zone (first run ever) --")
    try:
        from PIL import Image, ImageDraw
    except ImportError:
        check("zone: Pillow available to author the fixture", False, "pip install pillow")
        return

    img_path = WORK / "form.png"
    img = Image.new("RGB", (600, 400), "white")
    d = ImageDraw.Draw(img)
    # A filled box (left) and an outline-only box (right).
    d.rectangle([30, 30, 130, 130], fill="black")
    d.rectangle([330, 30, 430, 130], outline="black", width=3)
    # A text line, large and clean, for the tesseract zone.
    d.text((40, 250), "INVOICE 4711", fill="black")
    img.save(img_path)

    # `zone` opens templates.db in the data dir; create the template the way
    # the GUI would, straight into that SQLite file.
    DATA.mkdir(parents=True, exist_ok=True)
    db = DATA / "templates.db"
    con = sqlite3.connect(db)
    con.executescript("""
        CREATE TABLE IF NOT EXISTS templates (
            id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE,
            width INTEGER NOT NULL DEFAULT 0, height INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS template_zones (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            template_id INTEGER NOT NULL REFERENCES templates(id) ON DELETE CASCADE,
            label TEXT NOT NULL, x REAL NOT NULL, y REAL NOT NULL,
            w REAL NOT NULL, h REAL NOT NULL,
            zone_type TEXT NOT NULL DEFAULT 'text');
    """)
    con.execute("INSERT OR IGNORE INTO templates (name,width,height,created_at) VALUES (?,?,?,?)",
                ("consent", 600, 400, int(time.time())))
    tid = con.execute("SELECT id FROM templates WHERE name='consent'").fetchone()[0]
    con.execute("DELETE FROM template_zones WHERE template_id=?", (tid,))
    for label, x, y, w, h, kind in [
        ("agreed",   30 / 600,  30 / 400, 100 / 600, 100 / 400, "checkbox"),
        ("declined", 330 / 600, 30 / 400, 100 / 600, 100 / 400, "checkbox"),
        ("invoice",  30 / 600, 235 / 400, 300 / 600,  40 / 400, "text"),
    ]:
        con.execute("INSERT INTO template_zones (template_id,label,x,y,w,h,zone_type) "
                    "VALUES (?,?,?,?,?,?,?)", (tid, label, x, y, w, h, kind))
    con.commit()
    con.close()

    p = run(["--format", "json", "zone", str(img_path), "--template", "consent",
             "--data-dir", str(DATA)], timeout=300)
    if ran("zone", p):
        zones = {z["label"]: z for z in (jout(p) or [])}
        check("zone: every template zone comes back", set(zones) == {"agreed", "declined", "invoice"},
              str(sorted(zones)))
        check("zone: the filled checkbox reads true",
              zones.get("agreed", {}).get("text") == "true", str(zones.get("agreed")))
        check("zone: the empty checkbox reads false",
              zones.get("declined", {}).get("text") == "false", str(zones.get("declined")))
        text = (zones.get("invoice", {}).get("text") or "").strip()
        if subprocess.run(["which", "tesseract"], capture_output=True).returncode == 0:
            check("zone: the text zone OCRs its contents", "4711" in text.replace(" ", ""),
                  repr(text[:60]))
        else:
            print(f"   (no tesseract; text zone returned {text!r})")

    # An unknown template must be an error, not zero silent zones.
    p = run(["zone", str(img_path), "--template", "no-such-template",
             "--data-dir", str(DATA)], timeout=120)
    check("zone: an unknown template is refused", p.returncode != 0,
          (p.stderr or "").strip()[:90])

    # A missing image likewise.
    p = run(["zone", str(WORK / "absent.png"), "--template", "consent",
             "--data-dir", str(DATA)], timeout=120)
    check("zone: a missing image is refused", p.returncode != 0,
          (p.stderr or "").strip()[:90])


def main():
    if not Path(CLI).exists():
        print(f"CLI not found: {CLI}", file=sys.stderr)
        return 2
    WORK.mkdir(parents=True, exist_ok=True)
    build_corpus()

    # zone needs no model, so it runs regardless of the embedder.
    verify_zone()
    if verify_ingest():
        verify_index_search()
        verify_unified_search()

    print()
    passed = sum(1 for _, ok, _ in results if ok)
    print(f"{passed}/{len(results)} checks passed")
    failed = [(n, d) for n, ok, d in results if not ok]
    if failed:
        print("\nFAILED:")
        for n, d in failed:
            print(f"  - {n}" + (f"  ({d})" if d else ""))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
