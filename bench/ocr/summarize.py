#!/usr/bin/env python3
"""Leaderboard for one OCR benchmark run (bench/ocr/runner.py results).

Pulls every unit JSON of results/<run_id>/ from the store (hf:<repo> or dir:<path>), and
writes results/<run_id>/summary/{leaderboard.md, leaderboard.csv, by_tag.csv} locally and
(unless --no-upload) back to the store in one commit.

Per (device, engine): units, ok, failures by status; mean CER / CER without spaces / WER /
word recall over successful units AND a failure-penalised mean (failed unit = CER 1, word
recall 0: the CrispSorter panel rule "do not average only successful cases"); mean primary
metric (word recall on receipts, CER without spaces elsewhere); median and mean seconds per
page; peak tree RSS; peak GPU process memory; model pull / warm-up seconds.

usage: summarize.py --run-id ID [--store hf:cstr/crispsorter-ocr-bench] [--out DIR] [--no-upload]
"""
from __future__ import annotations

import argparse
import csv
import io
import json
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import runner  # noqa: E402

METRICS = ("cer", "cer_nospace", "wer", "word_recall")
PENALTY = dict(cer=1.0, cer_nospace=1.0, wer=1.0, word_recall=0.0)


def _mean(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.fmean(xs), 4) if xs else None


def _median(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 2) if xs else None


def _max(xs):
    xs = [x for x in xs if x is not None]
    return round(max(xs), 1) if xs else None


def load_run(store, run_id, cache_dir):
    """Return {units, engines, sessions, skipped} record lists for a run."""
    root = store.fetch_prefix(f"results/{run_id}", Path(cache_dir))
    out = dict(units=[], engines=[], sessions=[], skipped=[])
    base = root / "results" / run_id
    for kind in out:
        for f in sorted((base / kind).rglob("*.json")) if (base / kind).is_dir() else []:
            try:
                out[kind].append(json.loads(f.read_text()))
            except (OSError, json.JSONDecodeError):
                print("unreadable", f, file=sys.stderr)
    return out


def aggregate(units, engines=(), skipped=()):
    """Rows per (device, engine_id) and per (device, engine_id, tag)."""
    groups = {}
    for u in units:
        groups.setdefault((u["device"], u["engine_id"]), []).append(u)
    done_keys = {u["unit"] for u in units}
    skips = {}
    for s in skipped:
        if s.get("unit") not in done_keys:
            skips[(s["device"], s["engine_id"])] = skips.get((s["device"], s["engine_id"]), 0) + 1
    prep = {}
    for e in engines:
        prep.setdefault((e["device"], e["engine_id"]), []).append(e)
    rows, tag_rows = [], []
    for (dev, eid), us in sorted(groups.items()):
        ok = [u for u in us if u["status"] == "ok"]
        fails = {}
        for u in us:
            if u["status"] != "ok":
                fails[u["status"]] = fails.get(u["status"], 0) + 1
        row = dict(device=dev, engine_id=eid, engine=us[0].get("engine"), units=len(us), ok=len(ok),
                   failed=len(us) - len(ok), failures=";".join(f"{k}={v}" for k, v in sorted(fails.items())),
                   skipped_pending=skips.get((dev, eid), 0))
        for m in METRICS:
            row[f"mean_{m}"] = _mean([u["metrics"][m] for u in ok])
            row[f"penalised_{m}"] = _mean([(u["metrics"] or {}).get(m, PENALTY[m]) if u["status"] == "ok" else PENALTY[m] for u in us])
        row["mean_primary"] = _mean([u["primary_value"] for u in ok])
        row["receipt_word_recall"] = _mean([u["metrics"]["word_recall"] for u in ok if "receipt" in u["input"]["tags"]])
        row["nonreceipt_cer_nospace"] = _mean([u["metrics"]["cer_nospace"] for u in ok if "receipt" not in u["input"]["tags"]])
        walls = [u["timing"]["wall_seconds"] for u in ok]
        row["median_s_per_page"] = _median(walls)
        row["mean_s_per_page"] = _mean(walls)
        row["peak_rss_mb"] = _max([(u.get("memory") or {}).get("peak_rss_tree_mb") or (u.get("memory") or {}).get("max_rss_single_mb") for u in us])
        row["median_rss_mb"] = _median([(u.get("memory") or {}).get("peak_rss_tree_mb") for u in ok])
        row["peak_gpu_process_mb"] = _max([(u.get("gpu") or {}).get("peak_process_mb") for u in us])
        row["peak_gpu_delta_mb"] = _max([(u.get("gpu") or {}).get("peak_delta_mb") for u in us])
        row["gpu_unused_units"] = sum(1 for u in ok if u.get("gpu_note")) if dev == "gpu" else None
        ps = prep.get((dev, eid), [])
        row["pull_seconds"] = _median([(p.get("pull") or {}).get("seconds") for p in ps])
        row["warmup_seconds"] = _median([(p.get("warmup") or {}).get("wall_seconds") for p in ps])
        row["model_mb"] = _max([(p.get("model_bytes_added") or 0) / 2**20 for p in ps]) if ps else None
        binaries = sorted({(u.get("binary") or {}).get("git_sha") or "?" for u in us})
        row["binary_git_sha"] = ",".join(b[:12] for b in binaries)
        rows.append(row)
        tags = sorted({t for u in us for t in u["input"]["tags"]})
        for t in tags:
            tu = [u for u in us if t in u["input"]["tags"]]
            tok = [u for u in tu if u["status"] == "ok"]
            tag_rows.append(dict(device=dev, engine_id=eid, tag=t, units=len(tu), ok=len(tok),
                                 mean_cer=_mean([u["metrics"]["cer"] for u in tok]),
                                 mean_cer_nospace=_mean([u["metrics"]["cer_nospace"] for u in tok]),
                                 mean_wer=_mean([u["metrics"]["wer"] for u in tok]),
                                 mean_word_recall=_mean([u["metrics"]["word_recall"] for u in tok]),
                                 mean_primary=_mean([u["primary_value"] for u in tok])))
    return rows, tag_rows


def to_csv(rows):
    if not rows:
        return ""
    buf = io.StringIO()
    w = csv.DictWriter(buf, fieldnames=list(rows[0].keys()))
    w.writeheader(); w.writerows(rows)
    return buf.getvalue()


def fmt(x, nd=3):
    if x is None:
        return "-"
    if isinstance(x, float):
        return f"{x:.{nd}f}"
    return str(x)


def to_markdown(run_id, rows, tag_rows, sessions):
    L = [f"# CrispSorter OCR benchmark: `{run_id}`", ""]
    if sessions:
        L.append("Sessions:")
        seen = set()
        for s in sessions:
            if s.get("session") in seen or "finished_at" not in s:
                continue
            seen.add(s.get("session"))
            h = s.get("host") or {}
            gpus = ", ".join(f"{g.get('name')} sm_{str(g.get('compute_cap', '')).replace('.', '')}" for g in h.get("gpus") or [])
            bi = s.get("build_info") or {}
            L.append(f"- `{s['session']}` {s.get('device')} shard {s.get('shard')}: {h.get('cpu_model', '?')} x{h.get('cpu_count')}, "
                     f"{h.get('mem_total_gb')} GB{(' ; ' + gpus) if gpus else ''}; binary `{(bi.get('git_sha') or '?')[:12]}` "
                     f"{bi.get('variant', '')}; {s.get('stop_reason')} after {fmt((s.get('elapsed_seconds') or 0) / 3600, 2)} h")
        L.append("")
    L += ["Metrics: CrispEmbed `score.py` normalisation (vendored, sha256-pinned). *mean* = over successful units; "
          "*pen.* = failure-penalised (failed unit counts CER 1 / recall 0). Primary = word recall on receipts "
          "(partial ground truth), CER without spaces elsewhere. Lower CER/WER is better, higher recall is better.", ""]
    for dev in sorted({r["device"] for r in rows}):
        rs = sorted([r for r in rows if r["device"] == dev],
                    key=lambda r: (r["penalised_cer_nospace"] if r["penalised_cer_nospace"] is not None else 9))
        L += [f"## {dev.upper()}", "",
              "| engine | ok/units | failures | CER | CER-ns | WER | recall | pen. CER-ns | receipt recall | s/page (med) | "
              "peak RSS MB | GPU MB | pull s | warm-up s |",
              "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
        for r in rs:
            L.append(f"| {r['engine_id']} | {r['ok']}/{r['units']}"
                     f"{(' +' + str(r['skipped_pending']) + ' skipped') if r['skipped_pending'] else ''} | {r['failures'] or '-'} | "
                     f"{fmt(r['mean_cer'])} | {fmt(r['mean_cer_nospace'])} | {fmt(r['mean_wer'])} | {fmt(r['mean_word_recall'])} | "
                     f"{fmt(r['penalised_cer_nospace'])} | {fmt(r['receipt_word_recall'])} | {fmt(r['median_s_per_page'], 1)} | "
                     f"{fmt(r['peak_rss_mb'], 0)} | {fmt(r['peak_gpu_process_mb'], 0)} | {fmt(r['pull_seconds'], 0)} | "
                     f"{fmt(r['warmup_seconds'], 0)} |")
        L.append("")
        tags = sorted({t["tag"] for t in tag_rows if t["device"] == dev})
        if tags:
            L += [f"### {dev.upper()}: mean primary metric by tag (ok units)", "",
                  "| engine | " + " | ".join(tags) + " |", "|---|" + "---|" * len(tags)]
            for r in rs:
                cells = []
                for t in tags:
                    tr = next((x for x in tag_rows if x["device"] == dev and x["engine_id"] == r["engine_id"] and x["tag"] == t), None)
                    cells.append(f"{fmt(tr['mean_primary'])} ({tr['ok']}/{tr['units']})" if tr else "-")
                L.append(f"| {r['engine_id']} | " + " | ".join(cells) + " |")
            L.append("")
    return "\n".join(L) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--store", default=f"hf:{runner.DEFAULT_REPO}")
    ap.add_argument("--hf-token-file")
    ap.add_argument("--out", default="ocr-bench-summary")
    ap.add_argument("--no-upload", action="store_true")
    a = ap.parse_args(argv)
    runner.safe_name(a.run_id)
    store = runner.open_store(a.store, token=runner.read_token(a.hf_token_file)) if not a.store.startswith("hf:") \
        else runner.HFStore(a.store[3:], token=runner.read_token(a.hf_token_file), create=False)
    out = Path(a.out)
    data = load_run(store, a.run_id, out / "cache")
    rows, tag_rows = aggregate(data["units"], data["engines"], data["skipped"])
    md = to_markdown(a.run_id, rows, tag_rows, data["sessions"])
    files = {"leaderboard.md": md, "leaderboard.csv": to_csv(rows), "by_tag.csv": to_csv(tag_rows)}
    (out / "summary").mkdir(parents=True, exist_ok=True)
    for name, text in files.items():
        (out / "summary" / name).write_text(text)
    print(md)
    print(f"{len(data['units'])} unit(s), {len(rows)} engine row(s); written to {out / 'summary'}", file=sys.stderr)
    if not a.no_upload:
        store.put_many([(f"results/{a.run_id}/summary/{n}", t.encode()) for n, t in files.items()],
                       f"summary for {a.run_id}: {len(data['units'])} units")
        print(f"uploaded to {store.name} results/{a.run_id}/summary/", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
