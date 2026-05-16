//! Real CLI smoke tests for the main `crispsorter` binary (built as `tauri-app`).
//!
//! All tests in this file are `#[ignore]`'d — they require building the
//! full Tauri-app binary (~10 GB of artifacts; LanceDB + Tantivy + ort +
//! mistralrs + ASR are heavy compile units). The default `cargo test` run
//! skips them so contributor laptops aren't constantly rebuilding the kitchen sink.
//!
//! Run with:
//!     cargo test --test cli_smoke -- --ignored
//!
//! Or, with build artifacts kept off the boot disk:
//!     CARGO_TARGET_DIR=/path/with/30gb/free \
//!     cargo test --test cli_smoke -- --ignored
//!
//! Tests here exercise subcommands that DON'T need a downloaded embedder
//! model — version, doctor, catalog, batch, completion, manpage. The
//! companion file `cli_e2e_embedder.rs` covers the heavier
//! init→ingest→search→delete pipeline and is also `#[ignore]`'d.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_crispsorter"))
}

#[test]
#[ignore]
fn version_prints_json_with_name_and_version() {
    let out = bin().args(["version", "--format", "json"])
                   .output().expect("spawn version");
    assert!(out.status.success(),
        "version failed: {}", String::from_utf8_lossy(&out.stderr));
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("must be valid JSON");
    assert_eq!(payload["name"].as_str(), Some("crispsorter"));
    assert!(payload["version"].as_str().is_some());
    assert!(payload["target"].as_str().is_some());
    assert!(payload["arch"].as_str().is_some());
}

#[test]
#[ignore]
fn doctor_reports_engine_availability() {
    let out = bin().args(["doctor", "--format", "json"]).output().unwrap();
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for key in ["tesseract_installed", "ocrs_models_available",
                "paddle_ocr_available", "pdf_extract_compiled_in",
                "embedder_model_cached"]
    {
        assert!(payload[key].is_boolean(), "doctor missing bool field {key}");
    }
    if !cfg!(feature = "paddle-ocr") {
        assert_eq!(payload["paddle_ocr_available"].as_bool(), Some(false));
    }
}

#[test]
#[ignore]
fn help_lists_subcommands() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for sub in &["version", "doctor", "catalog", "index", "batch",
                 "chat", "completion", "manpage"]
    {
        assert!(stdout.contains(sub), "--help missing subcommand: {sub}");
    }
}

#[test]
#[ignore]
fn completion_emits_zsh_script() {
    let out = bin().args(["completion", "zsh"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("#compdef"));
    assert!(stdout.contains("crispsorter"));
}

#[test]
#[ignore]
fn manpage_writes_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin().args(["manpage", "--out", dir.path().to_str().unwrap()])
                   .output().unwrap();
    assert!(out.status.success());
    let manpage = dir.path().join("crispsorter.1");
    assert!(manpage.exists());
    let content = std::fs::read_to_string(&manpage).unwrap();
    assert!(content.contains("crispsorter"));
    assert!(content.contains(".SH NAME"));
}

fn make_dupe_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("paper1.pdf"), b"fake-pdf-content-1").unwrap();
    std::fs::write(tmp.path().join("paper2.pdf"), b"fake-pdf-content-2").unwrap();
    std::fs::write(tmp.path().join("dup.pdf"),    b"fake-pdf-content-1").unwrap();
    tmp
}

#[test]
#[ignore]
fn catalog_scan_writes_caf_and_browse_reads_back() {
    let src = make_dupe_fixture();
    let tmp_out = tempfile::tempdir().unwrap();
    let caf = tmp_out.path().join("test.caf");
    let scan = bin().args(["catalog", "scan", src.path().to_str().unwrap(),
                           "--out", caf.to_str().unwrap(),
                           "--hash", "sha256", "--format", "json"])
                    .output().unwrap();
    assert!(scan.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&scan.stdout).unwrap();
    assert_eq!(payload["files"].as_i64().unwrap(), 3);
    assert!(caf.exists());

    let browse = bin().args(["catalog", "browse", caf.to_str().unwrap(),
                             "--format", "json"])
                      .output().unwrap();
    assert!(browse.status.success());
    let lines: Vec<_> = std::str::from_utf8(&browse.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3);
}

#[test]
#[ignore]
fn catalog_find_dupes_detects_content_duplicates() {
    let src = make_dupe_fixture();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(dst.path().join("copy.pdf"), b"fake-pdf-content-1").unwrap();
    let out = bin().args(["catalog", "find-dupes",
                          src.path().to_str().unwrap(),
                          dst.path().to_str().unwrap(),
                          "--strategy", "hash:sha256", "--format", "json"])
                   .output().unwrap();
    assert!(out.status.success());
    let lines: Vec<_> = std::str::from_utf8(&out.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "expected at least one duplicate match");
}

#[test]
#[ignore]
fn batch_add_then_list_round_trips() {
    let data_dir = tempfile::tempdir().unwrap();
    let files_dir = tempfile::tempdir().unwrap();
    std::fs::write(files_dir.path().join("doc1.pdf"), b"x").unwrap();
    std::fs::write(files_dir.path().join("doc2.pdf"), b"y").unwrap();

    let add = bin().args(["batch", "--data-dir", data_dir.path().to_str().unwrap(),
                          "add", files_dir.path().to_str().unwrap(),
                          "--format", "json"])
                   .output().unwrap();
    assert!(add.status.success());
    let added: serde_json::Value = serde_json::from_slice(&add.stdout).unwrap();
    let job_id = added["job_id"].as_str().unwrap().to_owned();
    assert_eq!(added["files_added"].as_i64().unwrap(), 2);

    let list = bin().args(["batch", "--data-dir", data_dir.path().to_str().unwrap(),
                           "list", "--job-id", &job_id, "--format", "json"])
                    .output().unwrap();
    assert!(list.status.success());
    let lines: Vec<_> = std::str::from_utf8(&list.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);
}

#[test]
#[ignore]
fn batch_apply_dry_run_executes_no_moves() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source.pdf");
    let dst = dir.path().join("Sorted/Author/2024/source.pdf");
    std::fs::write(&src, b"keep me").unwrap();
    let plan = serde_json::json!({
        "mode": "move",
        "items": [{ "src": src.to_str().unwrap(), "dst": dst.to_str().unwrap() }],
    });
    let plan_path = dir.path().join("plan.json");
    std::fs::write(&plan_path, serde_json::to_string(&plan).unwrap()).unwrap();
    let apply = bin().args(["batch", "apply", plan_path.to_str().unwrap(),
                            "--dry-run", "--format", "text"])
                     .output().unwrap();
    assert!(apply.status.success());
    assert!(src.exists());
    assert!(!dst.exists());
}

#[test]
#[ignore]
fn batch_apply_real_move_executes_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source.pdf");
    let dst = dir.path().join("Sorted/Author/2024/source.pdf");
    std::fs::write(&src, b"move me").unwrap();
    let plan = serde_json::json!({
        "mode": "move",
        "items": [{ "src": src.to_str().unwrap(), "dst": dst.to_str().unwrap() }],
    });
    let plan_path = dir.path().join("plan.json");
    std::fs::write(&plan_path, serde_json::to_string(&plan).unwrap()).unwrap();
    let apply = bin().args(["batch", "apply", plan_path.to_str().unwrap(),
                            "--format", "text"])
                     .output().unwrap();
    assert!(apply.status.success());
    assert!(!src.exists());
    assert!(dst.exists());
    assert_eq!(std::fs::read(&dst).unwrap(), b"move me");
}

#[test]
#[ignore]
fn index_list_failed_on_empty_index_succeeds() {
    let data_dir = tempfile::tempdir().unwrap();
    let out = bin().args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
                          "list-failed", "--format", "json"])
                   .output().unwrap();
    assert!(out.status.success(),
        "list-failed on empty DB should succeed: {}",
        String::from_utf8_lossy(&out.stderr));
    let lines: Vec<_> = std::str::from_utf8(&out.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 0);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("0 failed"));
}

#[test]
#[ignore]
fn index_stats_on_empty_returns_zero_counts() {
    let data_dir = tempfile::tempdir().unwrap();
    let out = bin().args(["index", "--data-dir", data_dir.path().to_str().unwrap(),
                          "stats", "--format", "json"])
                   .output().unwrap();
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["docs"].as_i64().unwrap(),       0);
    assert_eq!(payload["chunks"].as_i64().unwrap(),     0);
    assert_eq!(payload["fts_docs"].as_i64().unwrap(),   0);
}
