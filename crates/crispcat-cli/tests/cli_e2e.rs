//! Real end-to-end tests for the `crispcat` CLI binary.
//!
//! These spawn the actual compiled binary (via `CARGO_BIN_EXE_crispcat`),
//! create real files on disk, and verify the round-trip:
//!     scan → write .caf → info → browse → find-dupes
//!
//! No network, no model download, no Tauri runtime — just the catalog
//! primitives exercised the way an end-user would actually use them.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_crispcat"))
}

/// Build a small folder tree with two duplicate-by-content files
/// and one unique file.  Returns the temp dir (kept alive by caller).
fn fixture_with_dupes() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("a.txt"),         b"alpha bravo charlie").unwrap();
    std::fs::write(root.join("b.txt"),         b"alpha bravo charlie").unwrap(); // dup of a.txt
    std::fs::write(root.join("unique.txt"),    b"distinct content here").unwrap();
    std::fs::create_dir(root.join("subdir")).unwrap();
    std::fs::write(root.join("subdir/c.txt"),  b"alpha bravo charlie").unwrap(); // also dup
    tmp
}

#[test]
fn scan_then_info_round_trips() {
    let src = fixture_with_dupes();
    let out_dir = tempfile::tempdir().unwrap();
    let caf_path = out_dir.path().join("test.caf");

    // Scan with SHA-256 for byte-level dup detection later.
    let scan = bin()
        .args(["scan", src.path().to_str().unwrap(),
               "--out", caf_path.to_str().unwrap(),
               "--hash", "sha256",
               "--format", "json"])
        .output()
        .expect("spawn crispcat scan");
    assert!(scan.status.success(),
        "crispcat scan failed: stderr={}",
        String::from_utf8_lossy(&scan.stderr));

    // The JSON output should contain the file count.
    let stdout = String::from_utf8_lossy(&scan.stdout);
    let payload: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("scan stdout must be valid JSON");
    assert_eq!(payload["files"].as_i64().unwrap(), 4, "scanned file count mismatch");
    assert_eq!(payload["complete"], true);
    assert_eq!(payload["issue_count"], 0);
    assert_eq!(payload["issues"], serde_json::json!([]));
    assert!(caf_path.exists(), ".caf file must be created");

    // Now info on the same .caf — must read back the same count.
    let info = bin()
        .args(["info", caf_path.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("spawn crispcat info");
    assert!(info.status.success(), "info failed");
    let info_json: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(info_json["file_count"].as_i64().unwrap(), 4);
    assert!(info_json["total_size_bytes"].as_i64().unwrap() > 0);
}

#[test]
fn browse_lists_files_with_filter() {
    let src = fixture_with_dupes();
    let caf = tempfile::NamedTempFile::new().unwrap();

    let scan = bin().args(["scan", src.path().to_str().unwrap(),
                           "--out", caf.path().to_str().unwrap()])
                    .output().unwrap();
    assert!(scan.status.success());

    // Browse with no filter — JSON line per file.
    let browse = bin().args(["browse", caf.path().to_str().unwrap(),
                             "--format", "json"])
                      .output().unwrap();
    let lines: Vec<_> = std::str::from_utf8(&browse.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 4, "browse should yield 4 lines");

    // Browse with a substring filter — matches only "unique.txt".
    let filtered = bin().args(["browse", caf.path().to_str().unwrap(),
                               "--filter", "unique",
                               "--format", "json"])
                        .output().unwrap();
    let lines: Vec<_> = std::str::from_utf8(&filtered.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1);
    let entry: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(entry["path"].as_str().unwrap().contains("unique.txt"));
}

#[test]
fn find_dupes_detects_byte_level_duplicates() {
    // Two folders with overlapping content.
    let src_a = tempfile::tempdir().unwrap();
    std::fs::write(src_a.path().join("doc.pdf"),  b"shared content").unwrap();
    std::fs::write(src_a.path().join("only_a.txt"), b"only in a").unwrap();

    let src_b = tempfile::tempdir().unwrap();
    std::fs::write(src_b.path().join("doc.pdf"),  b"shared content").unwrap(); // same bytes
    std::fs::write(src_b.path().join("differs.pdf"), b"different bytes").unwrap();

    let out = bin().args(["find-dupes",
                          src_a.path().to_str().unwrap(),
                          src_b.path().to_str().unwrap(),
                          "--strategy", "hash:sha256",
                          "--format", "json"])
                   .output().unwrap();
    assert!(out.status.success(),
        "find-dupes failed: {}", String::from_utf8_lossy(&out.stderr));

    let lines: Vec<_> = std::str::from_utf8(&out.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "must detect at least one duplicate");

    // The detected dup must point to the shared file.
    let m: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(m["source"].as_str().unwrap().contains("doc.pdf"));
    assert_eq!(m["destinations"].as_array().unwrap().len(), 1);
}

#[test]
fn find_dupes_name_and_size_strategy() {
    // Same name + size, different bytes → matches under name-and-size only.
    let a = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("file.txt"), b"AAAAAAAAAA").unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::write(b.path().join("file.txt"), b"BBBBBBBBBB").unwrap();

    let out = bin().args(["find-dupes",
                          a.path().to_str().unwrap(),
                          b.path().to_str().unwrap(),
                          "--strategy", "name-and-size",
                          "--format", "json"])
                   .output().unwrap();
    let dupes: Vec<_> = std::str::from_utf8(&out.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(dupes.len(), 1, "name-and-size must catch the same-name same-size pair");

    // With sha256 the same scenario should NOT match (different bytes).
    let out_sha = bin().args(["find-dupes",
                              a.path().to_str().unwrap(),
                              b.path().to_str().unwrap(),
                              "--strategy", "hash:sha256",
                              "--format", "json"])
                       .output().unwrap();
    let dupes_sha: Vec<_> = std::str::from_utf8(&out_sha.stdout).unwrap()
        .lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(dupes_sha.len(), 0, "sha256 must reject same-name different-bytes");
}

#[test]
fn unknown_subcommand_exits_with_error() {
    let out = bin().arg("notarealcommand").output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn help_succeeds() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("scan"));
    assert!(stdout.contains("info"));
    assert!(stdout.contains("browse"));
    assert!(stdout.contains("find-dupes"));
}

#[test]
fn version_flag_works() {
    let out = bin().arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("crispcat"), "expected crispcat name in version output");
}

#[test]
fn scan_skips_max_size_files() {
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("small.txt"), b"x").unwrap();
    std::fs::write(src.path().join("big.txt"), vec![b'A'; 10_000]).unwrap();

    let caf: PathBuf = tempfile::NamedTempFile::new().unwrap().path().to_owned();
    let out = bin().args(["scan", src.path().to_str().unwrap(),
                          "--out", caf.to_str().unwrap(),
                          "--max-size", "100",
                          "--format", "json"])
                   .output().unwrap();
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["files"].as_i64().unwrap(), 1, "10 KB file should be excluded");
    assert_eq!(payload["excluded_by_size"], 1);
    assert_eq!(payload["complete"], true);
}

#[test]
fn invalid_root_and_unknown_hash_leave_existing_output_untouched() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let caf = out.path().join("existing.caf");
    std::fs::write(&caf, b"existing catalog bytes").unwrap();
    let missing = src.path().join("missing");
    let file_root = src.path().join("file");
    std::fs::write(&file_root, b"file").unwrap();
    for root in [&missing, &file_root] {
        let result = bin().args(["scan", root.to_str().unwrap(), "--out", caf.to_str().unwrap()]).output().unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert_eq!(std::fs::read(&caf).unwrap(), b"existing catalog bytes");
    }
    let result = bin().args(["scan", src.path().to_str().unwrap(), "--out", caf.to_str().unwrap(), "--hash", "sha512"]).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unknown hash algorithm"));
    assert_eq!(std::fs::read(&caf).unwrap(), b"existing catalog bytes");
}

#[cfg(unix)]
#[test]
fn partial_scan_requires_opt_in_and_returns_its_diagnostics() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let path = src.path().join("pre-epoch");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(7).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(
        std::time::UNIX_EPOCH - std::time::Duration::from_secs(1))).unwrap();
    let caf = out.path().join("existing.caf");
    std::fs::write(&caf, b"keep previous catalog").unwrap();

    let strict = bin().args(["scan", src.path().to_str().unwrap(), "--out", caf.to_str().unwrap()]).output().unwrap();
    assert!(!strict.status.success());
    assert!(String::from_utf8_lossy(&strict.stderr).contains("incomplete scan"));
    assert_eq!(std::fs::read(&caf).unwrap(), b"keep previous catalog");

    let partial = bin().args(["scan", src.path().to_str().unwrap(), "--out", caf.to_str().unwrap(), "--allow-partial"]).output().unwrap();
    assert!(partial.status.success(), "{}", String::from_utf8_lossy(&partial.stderr));
    let report: serde_json::Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(report["complete"], false);
    assert_eq!(report["files"], 1);
    assert_eq!(report["total_size_bytes"], 7);
    assert_eq!(report["issue_count"], 1);
    assert_eq!(report["issues"][0]["kind"], "modified_time");
    assert_eq!(report["issues"][0]["path"], path.to_str().unwrap());
    assert_eq!(report["suppressed_issues"], 0);
    let loaded = crispcat::caf::read_file(&caf).unwrap();
    assert_eq!(loaded.total_size(), 7);
}

#[test]
fn zero_byte_files_remain_zero_after_scan_and_browse() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("empty"), b"").unwrap();
    let caf = out.path().join("zero.caf");
    let scan = bin().args(["scan", src.path().to_str().unwrap(), "--out", caf.to_str().unwrap()]).output().unwrap();
    assert!(scan.status.success());
    let browse = bin().args(["browse", caf.to_str().unwrap()]).output().unwrap();
    assert!(browse.status.success());
    let entry: serde_json::Value = serde_json::from_slice(&browse.stdout).unwrap();
    assert_eq!(entry["size"], 0);
}
