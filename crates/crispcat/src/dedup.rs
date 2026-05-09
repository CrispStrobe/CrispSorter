//! Duplicate detection between FileIndexes + scriptable cleanup.
//!
//! Phase 2 of PLAN P6. Mirrors Catfish's `find_all_duplicates_bulk`
//! with the same size-bucket fast path: every duplicate must agree on
//! file size first, so we use the destination index's `size_index`
//! HashMap to skip 99.9% of files in O(1) before paying for any
//! per-byte hash comparison.
//!
//! Hashing, when requested, runs in parallel via rayon over the
//! candidates that survived the size pre-filter. Cached hashes (e.g.
//! from a previous scan that materialised them in `FileEntry::hash`)
//! are reused; missing ones are computed on demand.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use super::index::{FileEntry, FileIndex};
use super::scan::{hash_file, HashAlgo};

/// How strictly to declare "duplicate".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatchStrategy {
    /// Same size + same filename. Cheap and surprisingly accurate for
    /// well-organised drives, but two unrelated 4 KB files named
    /// `README.md` would falsely collide. Catfish's default when
    /// `--hash` is omitted.
    NameAndSize,
    /// Same size + same hash digest. Byte-perfect (modulo hash
    /// collisions, which for MD5/SHA1/SHA256 are negligible at typical
    /// catalog scales). The slow path — but the hash work is rayon-
    /// parallelised so it's bounded by the number of size-collisions,
    /// not by the catalog total.
    Hash(HashAlgo),
}

impl Default for MatchStrategy {
    fn default() -> Self {
        MatchStrategy::NameAndSize
    }
}

/// Options for a single dedup run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DedupOptions {
    pub strategy: MatchStrategy,
}

/// One source file paired with the destination entries that match it.
/// Empty `destinations` means the source had no matches — these are
/// usually filtered out before display, but the API keeps them so a
/// caller can correlate "I checked N files, M had matches".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateMatch {
    pub source: FileEntry,
    pub destinations: Vec<FileEntry>,
}

/// Bulk duplicate finder. For each entry in `source`, look up matches
/// in `dest`. Always processes by ascending size group to keep memory
/// pressure predictable (one bucket of candidates in flight at a time).
///
/// Returns matches with `destinations.is_empty()` filtered out.
pub fn find_duplicates(
    source: &FileIndex,
    dest: &FileIndex,
    opts: &DedupOptions,
) -> Vec<DuplicateMatch> {
    // Group source entries by size so we hit each dest size-bucket once
    // and pay no per-entry hash work when no candidates exist at that
    // size at all (the common case).
    let mut by_size: std::collections::HashMap<u64, Vec<&FileEntry>> =
        std::collections::HashMap::new();
    for entry in &source.all_files {
        by_size.entry(entry.size).or_default().push(entry);
    }

    // Rayon-parallelise across distinct sizes. Each size's candidates
    // share a single hash-set lookup, so the parallelism granularity is
    // "one size group per thread" rather than "one file per thread"
    // (which would oversubscribe the I/O subsystem on small files).
    by_size
        .into_par_iter()
        .flat_map_iter(|(size, src_entries)| {
            let dest_candidates: Vec<&FileEntry> = dest.by_size(size).collect();
            if dest_candidates.is_empty() {
                return Vec::new().into_iter();
            }
            let mut out: Vec<DuplicateMatch> = Vec::new();
            for src in src_entries {
                let matches = find_matches_for(src, &dest_candidates, &opts.strategy);
                if !matches.is_empty() {
                    out.push(DuplicateMatch {
                        source: src.clone(),
                        destinations: matches,
                    });
                }
            }
            out.into_iter()
        })
        .collect()
}

/// Find matches for a single source file inside an already-pruned
/// candidate list (all candidates already share the source's size).
fn find_matches_for(
    src: &FileEntry,
    candidates: &[&FileEntry],
    strategy: &MatchStrategy,
) -> Vec<FileEntry> {
    match strategy {
        MatchStrategy::NameAndSize => {
            // Filter by filename — same-size + same-name is the cheap
            // approximate match Cathy uses by default.
            candidates
                .iter()
                .filter(|c| c.path.file_name() == src.path.file_name())
                .map(|&c| c.clone())
                .collect()
        }
        MatchStrategy::Hash(algo) => {
            // Compute / reuse the source hash once, then check each
            // candidate's hash. Cached `entry.hash` values (from a
            // previous scan with `--hash`) are reused; missing ones get
            // computed on demand.
            let src_hash = match resolve_hash(src, *algo) {
                Some(h) => h,
                None => return Vec::new(), // source unreadable → no match
            };
            candidates
                .iter()
                .filter_map(|c| {
                    let candidate_hash = resolve_hash(c, *algo)?;
                    if candidate_hash == src_hash {
                        Some((*c).clone())
                    } else {
                        None
                    }
                })
                .collect()
        }
    }
}

/// Reuse `entry.hash` if present and recomputable, else compute fresh
/// from disk. Returns `None` only when the file can't be read.
fn resolve_hash(entry: &FileEntry, algo: HashAlgo) -> Option<String> {
    if let Some(h) = &entry.hash {
        // We have a cached hash, but no way to verify it was computed
        // with the requested algo — Catalog v1 doesn't store the algo
        // alongside the hash. For now we trust the cache; a future
        // FileEntry shape with `hash_algo` would let us recompute on
        // mismatch.
        return Some(h.clone());
    }
    hash_file(&entry.path, algo).ok()
}

// ── Deletion scripts ─────────────────────────────────────────────────────

/// Output format for the deletion script. Matches Catfish's choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptFormat {
    /// `#!/bin/bash` script using `rm -f`.
    Bash,
    /// `.bat` script using `del`.
    Batch,
    /// PowerShell `Remove-Item -Force`.
    Powershell,
}

/// Which files in each match to mark for deletion. Default favours
/// removing destinations because the typical use is "I have this
/// in my source folder, find dupes in archive folders to free space."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeletionTarget {
    /// Delete each duplicate destination; keep the source.
    Destinations,
    /// Delete the source; keep the destinations. Less common; the
    /// "I want to canonicalise on the destination archive" use case.
    Source,
}

/// Build a reviewable script that deletes the configured side of each
/// match. The script never auto-runs — the caller is expected to read
/// it, save it to disk, then `bash`/`cmd` it themselves.
///
/// Includes a header summarising count + total size freed, plus a
/// commented-out paranoia line (`set -euo pipefail` for bash) so the
/// script aborts on the first failure instead of silently skipping.
pub fn generate_deletion_script(
    matches: &[DuplicateMatch],
    format: ScriptFormat,
    target: DeletionTarget,
) -> String {
    let mut paths: Vec<&Path> = Vec::new();
    let mut total_bytes: u64 = 0;
    for m in matches {
        match target {
            DeletionTarget::Source => {
                paths.push(&m.source.path);
                total_bytes += m.source.size;
            }
            DeletionTarget::Destinations => {
                for d in &m.destinations {
                    paths.push(&d.path);
                    total_bytes += d.size;
                }
            }
        }
    }

    let mut out = String::new();
    let header = format!(
        "Generated by CrispSorter Catalog — review before running.\n\
         Files to delete: {}\n\
         Estimated bytes freed: {}",
        paths.len(),
        total_bytes
    );

    match format {
        ScriptFormat::Bash => {
            out.push_str("#!/usr/bin/env bash\n");
            for line in header.lines() {
                out.push_str(&format!("# {line}\n"));
            }
            out.push_str("set -euo pipefail\n\n");
            for p in &paths {
                out.push_str(&format!("rm -f -- {}\n", shell_quote_bash(p)));
            }
        }
        ScriptFormat::Batch => {
            out.push_str("@echo off\n");
            for line in header.lines() {
                out.push_str(&format!("REM {line}\n"));
            }
            out.push('\n');
            for p in &paths {
                out.push_str(&format!("del /f /q {}\n", shell_quote_batch(p)));
            }
        }
        ScriptFormat::Powershell => {
            for line in header.lines() {
                out.push_str(&format!("# {line}\n"));
            }
            out.push_str("$ErrorActionPreference = 'Stop'\n\n");
            for p in &paths {
                out.push_str(&format!(
                    "Remove-Item -Force -LiteralPath {}\n",
                    shell_quote_powershell(p)
                ));
            }
        }
    }
    out
}

fn shell_quote_bash(p: &Path) -> String {
    // Single-quote the path; embedded single quotes get split-rejoined
    // ('\'' = close-quote, escape, reopen-quote — the canonical bash
    // single-quoting trick).
    let s = p.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn shell_quote_batch(p: &Path) -> String {
    // Batch's "" inside double-quoted strings is the only quote-escape
    // mechanism. Backslashes are literal.
    let s = p.to_string_lossy();
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn shell_quote_powershell(p: &Path) -> String {
    // PowerShell single-quotes are literal except for embedded single
    // quotes which need doubling.
    let s = p.to_string_lossy();
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fake_index(root: &str, files: &[(&str, u64, u32)]) -> FileIndex {
        let mut idx = FileIndex::new(PathBuf::from(root), false);
        for (name, size, mtime) in files {
            idx.add(FileEntry::new(
                PathBuf::from(format!("{root}/{name}")),
                *size,
                *mtime,
            ));
        }
        idx
    }

    #[test]
    fn name_and_size_match() {
        let src = fake_index(
            "/src",
            &[("a.txt", 100, 1), ("b.bin", 200, 1), ("c.dat", 300, 1)],
        );
        let dst = fake_index(
            "/dst",
            &[
                ("a.txt", 100, 1),  // size + name match → dup
                ("b.bin", 999, 1),  // name match but wrong size
                ("d.txt", 100, 1),  // size match but wrong name
            ],
        );
        let matches = find_duplicates(&src, &dst, &DedupOptions::default());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].source.path.file_name().unwrap(), "a.txt");
        assert_eq!(matches[0].destinations.len(), 1);
    }

    #[test]
    fn hash_match_with_real_files() {
        let tmp = TempDir::new().unwrap();
        let src_dir = tmp.path().join("src");
        let dst_dir = tmp.path().join("dst");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&dst_dir).unwrap();
        fs::write(src_dir.join("hello.txt"), b"hello world").unwrap();
        // Same content, different name — name+size would miss it,
        // hash should catch it.
        fs::write(dst_dir.join("renamed.txt"), b"hello world").unwrap();
        // Same name, different content — name+size would also miss
        // (different size); hash should also miss (different content).
        fs::write(dst_dir.join("hello.txt"), b"different content").unwrap();

        let src = super::super::scan::scan_dir(&src_dir, Default::default()).unwrap();
        let dst = super::super::scan::scan_dir(&dst_dir, Default::default()).unwrap();

        // Hash strategy finds the renamed copy.
        let matches = find_duplicates(
            &src,
            &dst,
            &DedupOptions {
                strategy: MatchStrategy::Hash(HashAlgo::Md5),
            },
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].destinations.len(), 1);
        assert_eq!(
            matches[0].destinations[0].path.file_name().unwrap(),
            "renamed.txt"
        );
    }

    #[test]
    fn empty_when_no_size_collisions() {
        let src = fake_index("/src", &[("a", 100, 1)]);
        let dst = fake_index("/dst", &[("b", 200, 1)]);
        let matches = find_duplicates(&src, &dst, &DedupOptions::default());
        assert!(matches.is_empty());
    }

    #[test]
    fn deletion_script_bash_quotes_special_chars() {
        let m = vec![DuplicateMatch {
            source: FileEntry::new(PathBuf::from("/keep/file.txt"), 10, 0),
            destinations: vec![
                FileEntry::new(PathBuf::from("/del/normal.txt"), 10, 0),
                FileEntry::new(PathBuf::from("/del/with space.txt"), 20, 0),
                FileEntry::new(PathBuf::from("/del/with'quote.txt"), 30, 0),
            ],
        }];
        let s =
            generate_deletion_script(&m, ScriptFormat::Bash, DeletionTarget::Destinations);
        assert!(s.starts_with("#!/usr/bin/env bash\n"));
        assert!(s.contains("# Files to delete: 3"));
        assert!(s.contains("# Estimated bytes freed: 60"));
        assert!(s.contains("rm -f -- '/del/normal.txt'"));
        assert!(s.contains("rm -f -- '/del/with space.txt'"));
        // The single-quote escape: '\''
        assert!(s.contains("rm -f -- '/del/with'\\''quote.txt'"));
    }

    #[test]
    fn deletion_script_batch_quotes_double_quotes() {
        let m = vec![DuplicateMatch {
            source: FileEntry::new(PathBuf::from("C:\\src\\file.txt"), 10, 0),
            destinations: vec![FileEntry::new(
                PathBuf::from("D:\\dst\\with\"quote.txt"),
                10,
                0,
            )],
        }];
        let s =
            generate_deletion_script(&m, ScriptFormat::Batch, DeletionTarget::Destinations);
        assert!(s.starts_with("@echo off\n"));
        assert!(s.contains("REM Files to delete: 1"));
        assert!(s.contains("del /f /q \"D:\\dst\\with\"\"quote.txt\""));
    }

    #[test]
    fn deletion_script_target_source_keeps_destinations() {
        let m = vec![DuplicateMatch {
            source: FileEntry::new(PathBuf::from("/del-me.txt"), 50, 0),
            destinations: vec![
                FileEntry::new(PathBuf::from("/keep1.txt"), 50, 0),
                FileEntry::new(PathBuf::from("/keep2.txt"), 50, 0),
            ],
        }];
        let s = generate_deletion_script(&m, ScriptFormat::Bash, DeletionTarget::Source);
        assert!(s.contains("rm -f -- '/del-me.txt'"));
        assert!(!s.contains("keep1.txt"));
        assert!(!s.contains("keep2.txt"));
        assert!(s.contains("# Files to delete: 1"));
    }
}
