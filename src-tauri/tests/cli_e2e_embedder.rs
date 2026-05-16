//! End-to-end CLI tests that download a real embedder model and exercise
//! the full ingest → search → delete round-trip.
//!
//! All tests are `#[ignore]` because they:
//!   * Download ~90 MB of model weights from HuggingFace on first run
//!   * Take 30+ seconds (model load is dominated by ONNX session init)
//!
//! Run explicitly with:
//!     cargo test --test cli_e2e_embedder -- --ignored --nocapture
//!
//! Or a single test:
//!     cargo test --test cli_e2e_embedder ingest_then_search -- --ignored
//!
//! These run fine in CI on a machine with internet access; gate them
//! behind the `--ignored` flag so the default `cargo test` stays fast.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_crispsorter"))
}

/// Smallest available embedder model: ~90 MB, English-only, 384-dim.
/// Plenty for round-trip CI tests; production users want bge-m3 or e5.
const TEST_MODEL: &str = "all-minilm-l6-v2";

#[test]
#[ignore]
fn init_downloads_minilm_model() {
    let data_dir = tempfile::tempdir().unwrap();
    let out = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "init", "--model", TEST_MODEL, "--device", "cpu",
               "--format", "json"])
        .output()
        .expect("spawn index init");
    assert!(out.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr));

    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["model"].as_str(), Some(TEST_MODEL));
    assert_eq!(payload["status"].as_str(), Some("ready"));

    // Models cache directory should exist and contain at least one file.
    let models = data_dir.path().join("models");
    assert!(models.exists(), "models dir not created");
    let n: usize = std::fs::read_dir(&models).unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert!(n > 0, "expected at least one downloaded artifact");
}

#[test]
#[ignore]
fn ingest_then_search_then_delete_round_trip() {
    let data_dir = tempfile::tempdir().unwrap();
    let docs    = tempfile::tempdir().unwrap();

    // Three text files with deliberately distinct content for retrieval.
    std::fs::write(docs.path().join("relativity.txt"),
        "Albert Einstein developed the theory of special relativity in 1905, \
         introducing the equivalence of mass and energy E = mc².").unwrap();
    std::fs::write(docs.path().join("evolution.txt"),
        "Charles Darwin proposed the theory of evolution by natural selection \
         in his 1859 work On the Origin of Species.").unwrap();
    std::fs::write(docs.path().join("recipe.txt"),
        "Combine flour, sugar, and butter to make shortbread cookies. \
         Bake for 20 minutes.").unwrap();

    // ── Ingest ──────────────────────────────────────────────────────────
    let ingest = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "ingest", docs.path().to_str().unwrap(),
               "--model", TEST_MODEL, "--device", "cpu",
               "--format", "text"])
        .output().expect("spawn index ingest");
    assert!(ingest.status.success(),
        "ingest failed: {}", String::from_utf8_lossy(&ingest.stderr));

    // ── Stats: must show 3 docs, ≥ 3 chunks ─────────────────────────────
    let stats = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "stats", "--format", "json"])
        .output().unwrap();
    let stats_json: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(stats_json["docs"].as_i64().unwrap(),   3, "expected 3 indexed docs");
    assert!(stats_json["chunks"].as_i64().unwrap() >= 3, "expected ≥ 3 chunks");
    assert!(stats_json["fts_docs"].as_i64().unwrap() >= 3, "FTS must have indexed all 3");

    // ── List: each filename must appear ─────────────────────────────────
    let list = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "list", "--limit", "10", "--format", "json"])
        .output().unwrap();
    let list_lines: Vec<_> = std::str::from_utf8(&list.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(list_lines.len(), 3);

    // ── BM25 search: "darwin" must find evolution.txt ──────────────────
    let search = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "search", "darwin", "--format", "json"])
        .output().expect("spawn index search");
    assert!(search.status.success());
    let hits: Vec<serde_json::Value> = std::str::from_utf8(&search.stdout).unwrap()
        .lines().filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(!hits.is_empty(), "search for 'darwin' must return at least 1 hit");
    let filenames: Vec<_> = hits.iter()
        .filter_map(|h| h["filename"].as_str())
        .collect();
    assert!(filenames.iter().any(|f| f.contains("evolution")),
        "expected evolution.txt in BM25 hits, got {filenames:?}");

    // ── BM25 search for term that's NOT in the corpus ──────────────────
    let neg = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "search", "quantum", "--format", "json"])
        .output().unwrap();
    let neg_hits: Vec<_> = std::str::from_utf8(&neg.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(neg_hits.len(), 0, "term not in corpus should yield 0 hits");

    // ── Delete one doc, stats should drop to 2 ─────────────────────────
    let first_doc_id = hits[0]["doc_id"].as_str().unwrap();
    let del = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "delete", first_doc_id])
        .output().unwrap();
    assert!(del.status.success());

    let stats2 = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "stats", "--format", "json"])
        .output().unwrap();
    let stats2_json: serde_json::Value = serde_json::from_slice(&stats2.stdout).unwrap();
    assert_eq!(stats2_json["docs"].as_i64().unwrap(), 2,
        "delete must drop doc count by 1");
}

#[test]
#[ignore]
fn export_and_inspect_cidx_round_trip() {
    let data_dir = tempfile::tempdir().unwrap();
    let docs    = tempfile::tempdir().unwrap();
    std::fs::write(docs.path().join("a.txt"), b"alpha bravo charlie delta echo").unwrap();
    std::fs::write(docs.path().join("b.txt"), b"foxtrot golf hotel india juliet").unwrap();

    // Ingest 2 docs.
    let ingest = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "ingest", docs.path().to_str().unwrap(),
               "--model", TEST_MODEL, "--device", "cpu"])
        .output().unwrap();
    assert!(ingest.status.success());

    // Export to .cidx with FTS companion.
    let cidx = data_dir.path().join("snapshot.cidx");
    let export = bin()
        .args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
               "export-cidx", cidx.to_str().unwrap(),
               "--include-fts", "--format", "json"])
        .output().unwrap();
    assert!(export.status.success(),
        "export-cidx failed: {}", String::from_utf8_lossy(&export.stderr));

    let payload: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
    assert!(payload["rows_exported"].as_i64().unwrap() >= 2);
    assert_eq!(payload["fts"].as_bool(), Some(true));

    // FTS subdir must exist.
    assert!(cidx.join("fts").is_dir(),
        "fts/ companion directory missing in .cidx");

    // Inspect the .cidx — counts must match.
    let inspect = bin()
        .args(["index", "inspect-cidx", cidx.to_str().unwrap(), "--format", "json"])
        .output().unwrap();
    assert!(inspect.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(payload["docs"].as_i64().unwrap(), 2);
    assert!(payload["chunks"].as_i64().unwrap() >= 2);
}
