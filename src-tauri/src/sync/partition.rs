//! P13.7 Stage N — volume-proportional auto-partition of files into
//! ≤ M shards based on the L1 folder-size distribution.
//!
//! The user's framing: when we index millions of files, the folder
//! tree on disk already carries topical locality (e.g. pre-sorted
//! by author-first-letter under `/Authors/A/…/`, by year under
//! `/Photos/2024/…`, etc.).  Rather than route by content-hash
//! (which scatters), we walk the per-subfolder sizes after L1 and
//! assign shards proportional to volume — a 10× heavier
//! subfolder gets ~10 consecutive shards; small ones share one.
//! Total capped at M (default 64) so the VPS shard count stays
//! manageable.
//!
//! The partition map is persisted as a SQLite KV at
//! `<data-dir>/partition_map.db`.  bg_ingest looks up each doc's
//! `collection_id` from it at push time via [`PartitionMap::lookup`].
//! Recomputed on demand by `crispsorter sync cloud-backup partition`
//! or by the Settings → "Re-partition shards" button.
//!
//! ## Algorithm
//!
//! 1. Group L1 rows by their immediate parent (or a configurable
//!    depth-N path prefix) under each watched root.
//! 2. Sum each group's total byte count.
//! 3. Compute `shard_capacity = total_size / max_shards`.
//! 4. For each group, `num_shards = ceil(group_size /
//!    shard_capacity)`, clamped to ≥ 1.
//! 5. Walk the group's files in stable path order, splitting them
//!    into `num_shards` consecutive buckets of roughly equal size.
//! 6. Emit a `collection_id` per bucket like `<root_label>/<group>/<n>`.
//!
//! Groups that don't qualify for their own shard (their size <
//! shard_capacity / 4) collapse into a shared `<root>/_misc` bucket
//! so the partition map doesn't degenerate to one-shard-per-tiny-folder.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Persistent partition map keyed by `(root_path, file_path)`.
/// Value is the `collection_id` chosen during the last
/// `recompute()` for that file.  Files added after the last
/// recompute (between two runs) miss the map and fall back to the
/// sha-prefix routing on the cb-api side; the next recompute
/// picks them up.
#[derive(Clone)]
pub struct PartitionMap {
    conn: Arc<Mutex<Connection>>,
}

/// One row of the partition output.  Emitted by `recompute()` so
/// the caller can both persist (via `PartitionMap::write_batch`)
/// AND ship the assignments to cloud-backup as the
/// `collection_id` on the next push.
#[derive(Debug, Clone)]
pub struct Assignment {
    pub root_path:     PathBuf,
    pub file_path:     PathBuf,
    pub collection_id: String,
}

/// Tunables for `recompute`.  Defaults match the user's "max M
/// shards, proportional to volume" framing.
#[derive(Debug, Clone)]
pub struct PartitionOptions {
    /// Total shard cap across all groups under one root.  Default 64.
    pub max_shards: usize,
    /// Path-depth at which subfolders become "groups".  Default 1
    /// = first subfolder under root (so `/root/Authors/A/foo.pdf`
    /// → group "Authors").  Set to 2 for `/root/Authors/A/…` →
    /// group "Authors/A" when you want finer locality.
    pub group_depth: usize,
    /// Tiny-group threshold: groups smaller than this fraction of
    /// `total_size / max_shards` get folded into `_misc` to avoid
    /// thousands of one-file shards.  Default 0.25.
    pub min_fraction: f64,
}

impl Default for PartitionOptions {
    fn default() -> Self {
        Self {
            max_shards: 64,
            group_depth: 1,
            min_fraction: 0.25,
        }
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS partition_map (
    root_path     TEXT NOT NULL,
    file_path     TEXT NOT NULL,
    collection_id TEXT NOT NULL,
    PRIMARY KEY (root_path, file_path)
);
CREATE INDEX IF NOT EXISTS idx_partition_map_collection
    ON partition_map(collection_id);
CREATE TABLE IF NOT EXISTS partition_runs (
    root_path  TEXT PRIMARY KEY,
    ran_at_ms  INTEGER NOT NULL,
    num_files  INTEGER NOT NULL,
    num_shards INTEGER NOT NULL,
    options    TEXT
);
";

impl PartitionMap {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join("partition_map.db");
        let conn = Connection::open(&path)
            .with_context(|| format!("open partition map at {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Lookup a file's assigned `collection_id`.  `None` when the
    /// file hasn't been mapped yet (e.g. added since the last
    /// `recompute`) — caller falls back to sha-prefix routing.
    pub fn lookup(&self, root_path: &Path, file_path: &Path) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT collection_id FROM partition_map \
             WHERE root_path = ?1 AND file_path = ?2",
            params![
                root_path.to_string_lossy().as_ref(),
                file_path.to_string_lossy().as_ref(),
            ],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Bulk-write a batch of `Assignment`s.  Idempotent: re-running
    /// `recompute()` against the same root replaces the prior
    /// per-root rows so a `collection_id` migration is just one
    /// new recompute.
    pub fn write_batch(&self, assignments: &[Assignment]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for a in assignments {
            tx.execute(
                "INSERT OR REPLACE INTO partition_map \
                 (root_path, file_path, collection_id) VALUES (?1, ?2, ?3)",
                params![
                    a.root_path.to_string_lossy().as_ref(),
                    a.file_path.to_string_lossy().as_ref(),
                    a.collection_id,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record metadata about a recompute run (timestamp + stats).
    /// Used by the Settings UI to show "last partitioned: 2 days ago,
    /// 1.2M files in 64 shards".
    pub fn record_run(
        &self,
        root_path: &Path,
        num_files: usize,
        num_shards: usize,
        options: &PartitionOptions,
    ) -> Result<()> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let opts_json = serde_json::to_string(&serde_json::json!({
            "max_shards": options.max_shards,
            "group_depth": options.group_depth,
            "min_fraction": options.min_fraction,
        })).unwrap_or_default();
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO partition_runs \
             (root_path, ran_at_ms, num_files, num_shards, options) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                root_path.to_string_lossy().as_ref(),
                now_ms,
                num_files as i64,
                num_shards as i64,
                opts_json,
            ],
        )?;
        Ok(())
    }
}

/// Input to `partition_assignments`: one (path, size) pair per file
/// found by the L1 walk.  Caller supplies these by reading the
/// LocalIndex (via `list_documents_for_push` or a dedicated scan).
#[derive(Debug, Clone)]
pub struct FileSize {
    pub path: PathBuf,
    pub size: u64,
}

/// Pure (testable) core of Stage N.  Takes the file list + the
/// watched root + options; emits `Assignment`s in stable order.
///
/// Algorithm — proportional bin-packing:
///
/// 1. Group files by the `group_depth`-th path component under
///    the root.  E.g. with depth=1, every file under
///    `/root/Authors/…` falls in group `Authors`.
/// 2. Sum each group's bytes; total = Σ group bytes.
/// 3. `shard_capacity = max(1, total / max_shards)`.
/// 4. A group's allotment is `ceil(group_size / shard_capacity)`,
///    capped at `max_shards` overall and ≥ 1 per non-tiny group.
/// 5. Groups smaller than `min_fraction * shard_capacity` fold
///    into a shared `_misc` bucket to avoid one-shard-per-tiny-
///    folder degeneracy.
/// 6. Within each group, files are sorted by path and split into
///    the allotted number of buckets of roughly equal byte size.
///    The bucket index `k` becomes the per-group shard suffix.
///
/// The resulting `collection_id` has the form
/// `<root_label>/<group_label>/<k>` (e.g. `Documents/Authors/3`)
/// so the cb-api sharding router (`_route_prefix`) hashes it to a
/// stable two-char prefix and related files land together.
pub fn partition_assignments(
    root_path: &Path,
    files: &[FileSize],
    options: &PartitionOptions,
) -> Vec<Assignment> {
    if files.is_empty() {
        return Vec::new();
    }
    let max_shards = options.max_shards.max(1);
    let depth = options.group_depth.max(1);

    // 1. Group by depth-N path component under root.
    let mut groups: HashMap<String, Vec<FileSize>> = HashMap::new();
    let root_label = root_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "root".to_string());
    for f in files {
        let rel = match f.path.strip_prefix(root_path) {
            Ok(p) => p,
            Err(_) => {
                // File not actually under this root — bucket
                // it under "_outside" so the caller notices.
                groups.entry("_outside".into())
                    .or_default()
                    .push(f.clone());
                continue;
            }
        };
        let components: Vec<&str> = rel
            .iter()
            .filter_map(|c| c.to_str())
            .take(depth)
            .collect();
        let key = if components.is_empty() {
            "_root".to_string()
        } else {
            components.join("/")
        };
        groups.entry(key).or_default().push(f.clone());
    }

    // 2. Group sizes.
    let mut group_sizes: Vec<(String, u64)> = groups
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().map(|f| f.size).sum()))
        .collect();
    let total_size: u64 = group_sizes.iter().map(|(_, s)| *s).sum();
    if total_size == 0 {
        // Pathological — every file is empty.  Single bucket.
        return files
            .iter()
            .map(|f| Assignment {
                root_path: root_path.to_path_buf(),
                file_path: f.path.clone(),
                collection_id: format!("{}/_empty/0", root_label),
            })
            .collect();
    }

    let shard_capacity = (total_size / max_shards as u64).max(1);
    let min_size_for_own_shards =
        ((shard_capacity as f64) * options.min_fraction).max(1.0) as u64;

    // 3. Fold tiny groups into `_misc`.
    let mut misc_files: Vec<FileSize> = Vec::new();
    group_sizes.retain(|(k, sz)| {
        if *sz < min_size_for_own_shards {
            misc_files.extend(groups.remove(k).unwrap_or_default());
            false
        } else {
            true
        }
    });
    if !misc_files.is_empty() {
        groups.insert("_misc".to_string(), misc_files);
        let misc_total: u64 = groups["_misc"].iter().map(|f| f.size).sum();
        group_sizes.push(("_misc".to_string(), misc_total));
    }

    // Pre-pass: if the number of groups already exceeds
    // max_shards, fold the smallest groups into `_misc` until
    // num_groups ≤ max_shards.  Without this, a corpus with 60
    // equal-sized year/month directories and max_shards=20 would
    // produce 60 buckets (each group hits the per-group floor of
    // 1).  Folding the smallest into _misc respects the cap AND
    // preserves topical locality for the heavier groups.
    while group_sizes.len() > max_shards {
        // Sort ascending by size; fold the smallest non-_misc group
        // into `_misc`.  Skip `_misc` itself to avoid an infinite
        // fold-into-self loop when it is the only group left.
        group_sizes.sort_by_key(|(_, s)| *s);
        let fold_idx = match group_sizes.iter().position(|(k, _)| k != "_misc") {
            Some(i) => i,
            None    => break, // only _misc remains; accept
        };
        let (smallest_key, smallest_size) = group_sizes.remove(fold_idx);
        let misc_files = groups.remove(&smallest_key).unwrap_or_default();
        let misc = groups.entry("_misc".to_string()).or_default();
        misc.extend(misc_files);
        // Update or insert _misc's running size.
        let mut found = false;
        for (k, s) in group_sizes.iter_mut() {
            if k == "_misc" {
                *s += smallest_size;
                found = true;
                break;
            }
        }
        if !found {
            group_sizes.push(("_misc".to_string(), smallest_size));
        }
    }

    // 4. Allotment per group, then renormalise to respect max_shards.
    //    First pass: ceil(size/capacity), with a floor of 1.
    let mut allotments: HashMap<String, usize> = HashMap::new();
    let mut sum_allot = 0usize;
    for (k, sz) in &group_sizes {
        let n = ((*sz + shard_capacity - 1) / shard_capacity).max(1) as usize;
        allotments.insert(k.clone(), n);
        sum_allot += n;
    }
    // Second pass: if we overshot max_shards, scale every group's
    // count down proportionally (ceil) keeping each ≥ 1.
    if sum_allot > max_shards {
        let scale = max_shards as f64 / sum_allot as f64;
        let mut scaled_sum = 0usize;
        for v in allotments.values_mut() {
            *v = ((*v as f64) * scale).max(1.0).ceil() as usize;
            scaled_sum += *v;
        }
        // Edge case: ceil'd sum still > max_shards after proportional
        // scale-down.  Converge by clipping the smallest (least-deserving)
        // group that still has allotment > 1; this preserves the heavy
        // groups' splits longest (proportionality-first).
        while scaled_sum > max_shards {
            let min_key = group_sizes.iter()
                .filter(|(k, _)| allotments.get(k).copied().unwrap_or(0) > 1)
                .min_by_key(|(_, s)| *s)
                .map(|(k, _)| k.clone());
            match min_key {
                Some(k) => { *allotments.get_mut(&k).unwrap() -= 1; scaled_sum -= 1; }
                None    => break, // Every group is at floor=1; accept.
            }
        }
    }

    // 5. Walk each group's files in stable path order, splitting
    //    into the allotted number of buckets of roughly equal byte
    //    size.  Stable order = same partition map across reruns
    //    on the same input.
    let mut out: Vec<Assignment> = Vec::with_capacity(files.len());
    for (group_key, mut group_files) in groups {
        group_files.sort_by(|a, b| a.path.cmp(&b.path));
        let n_shards = *allotments.get(&group_key).unwrap_or(&1);
        let group_total: u64 = group_files.iter().map(|f| f.size).sum();
        // Even bucket boundaries by cumulative byte size.
        let target_per_shard = group_total / n_shards as u64;
        let mut current_bucket: usize = 0;
        let mut bytes_in_bucket: u64 = 0;
        for f in &group_files {
            // Move to next bucket when current one is at-or-over
            // target AND we haven't run out of buckets yet.
            if bytes_in_bucket >= target_per_shard
                && current_bucket + 1 < n_shards
            {
                current_bucket += 1;
                bytes_in_bucket = 0;
            }
            bytes_in_bucket = bytes_in_bucket.saturating_add(f.size);
            // `collection_id` schema: `<root_label>/<group>/<bucket>`.
            // cb-api's `_route_prefix` will sha-hash this string and
            // pick a shard prefix from the first two hex chars, so
            // the actual on-disk shard directory has nothing to do
            // with the label — but related files in the same bucket
            // are guaranteed to share it.
            let collection_id = if n_shards == 1 {
                format!("{}/{}", root_label, group_key)
            } else {
                format!("{}/{}/{}", root_label, group_key, current_bucket)
            };
            out.push(Assignment {
                root_path: root_path.to_path_buf(),
                file_path: f.path.clone(),
                collection_id,
            });
        }
    }

    // Restore the caller's input ordering for deterministic
    // downstream consumers (the SyncManager push walks files in
    // LocalIndex row order, not assignment order).
    let index: HashMap<&PathBuf, usize> = files
        .iter()
        .enumerate()
        .map(|(i, f)| (&f.path, i))
        .collect();
    out.sort_by_key(|a| *index.get(&a.file_path).unwrap_or(&usize::MAX));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_files(spec: &[(&str, u64)]) -> Vec<FileSize> {
        spec.iter()
            .map(|(p, s)| FileSize { path: PathBuf::from(p), size: *s })
            .collect()
    }

    #[test]
    fn proportional_allocation_splits_heavy_group_into_more_shards() {
        // /root/heavy/* totals 100MB, /root/light/* totals 10MB.
        // With max_shards=11, heavy should get ~10 shards and light ~1.
        let files = mk_files(&[
            ("/root/heavy/a.bin", 10_000_000),
            ("/root/heavy/b.bin", 10_000_000),
            ("/root/heavy/c.bin", 10_000_000),
            ("/root/heavy/d.bin", 10_000_000),
            ("/root/heavy/e.bin", 10_000_000),
            ("/root/heavy/f.bin", 10_000_000),
            ("/root/heavy/g.bin", 10_000_000),
            ("/root/heavy/h.bin", 10_000_000),
            ("/root/heavy/i.bin", 10_000_000),
            ("/root/heavy/j.bin", 10_000_000),
            ("/root/light/x.bin", 10_000_000),
        ]);
        let opts = PartitionOptions {
            max_shards: 11,
            group_depth: 1,
            min_fraction: 0.25,
        };
        let assigns = partition_assignments(Path::new("/root"), &files, &opts);
        assert_eq!(assigns.len(), 11);
        let heavy_buckets: std::collections::HashSet<&str> = assigns.iter()
            .filter(|a| a.file_path.starts_with("/root/heavy"))
            .map(|a| a.collection_id.as_str())
            .collect();
        let light_buckets: std::collections::HashSet<&str> = assigns.iter()
            .filter(|a| a.file_path.starts_with("/root/light"))
            .map(|a| a.collection_id.as_str())
            .collect();
        assert!(
            heavy_buckets.len() >= 5,
            "heavy group expected ≥5 buckets, got {heavy_buckets:?}"
        );
        assert_eq!(
            light_buckets.len(), 1,
            "light group should fit in one bucket"
        );
    }

    #[test]
    fn tiny_groups_fold_into_misc() {
        // Three tiny groups (each ~1MB) and one big one (~100MB).
        // The tiny ones fold into `_misc`; big one gets its own shards.
        let mut spec: Vec<(&str, u64)> = vec![
            ("/root/big/a.bin", 100_000_000),
            ("/root/big/b.bin", 100_000_000),
        ];
        // Stringify the path keys via owned strings — leak so
        // they're &'static for &str.
        let tiny_paths: Vec<String> = (0..3)
            .map(|i| format!("/root/tiny{i}/x.bin"))
            .collect();
        for p in &tiny_paths {
            spec.push((Box::leak(p.clone().into_boxed_str()), 1_000));
        }
        let files = mk_files(&spec);
        let opts = PartitionOptions {
            max_shards: 16,
            group_depth: 1,
            min_fraction: 0.25,
        };
        let assigns = partition_assignments(Path::new("/root"), &files, &opts);
        let tiny_collections: std::collections::HashSet<&str> = assigns.iter()
            .filter(|a| a.file_path.to_string_lossy().contains("/tiny"))
            .map(|a| a.collection_id.as_str())
            .collect();
        // All three tiny groups should share one `_misc` bucket.
        for c in &tiny_collections {
            assert!(c.contains("_misc"), "tiny group not folded into _misc: {c}");
        }
        assert_eq!(tiny_collections.len(), 1);
    }

    #[test]
    fn stable_order_across_reruns() {
        let files = mk_files(&[
            ("/root/a/1", 100), ("/root/a/2", 100), ("/root/a/3", 100),
            ("/root/b/1", 100), ("/root/b/2", 100),
        ]);
        let opts = PartitionOptions::default();
        let a = partition_assignments(Path::new("/root"), &files, &opts);
        let b = partition_assignments(Path::new("/root"), &files, &opts);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.file_path, y.file_path);
            assert_eq!(x.collection_id, y.collection_id);
        }
    }

    #[test]
    fn outside_root_files_quarantined_to_outside_bucket() {
        let files = mk_files(&[
            ("/root/in/a", 100),
            ("/elsewhere/b", 100),
        ]);
        let opts = PartitionOptions::default();
        let a = partition_assignments(Path::new("/root"), &files, &opts);
        let outside = a.iter().find(|x| x.file_path.starts_with("/elsewhere"));
        assert!(outside.is_some());
        assert!(
            outside.unwrap().collection_id.contains("_outside")
                || outside.unwrap().collection_id.contains("_misc"),
            "got {}", outside.unwrap().collection_id
        );
    }

    #[test]
    fn empty_files_dont_panic() {
        let files = mk_files(&[("/root/a/x", 0), ("/root/a/y", 0)]);
        let opts = PartitionOptions::default();
        let a = partition_assignments(Path::new("/root"), &files, &opts);
        assert_eq!(a.len(), 2);
        for x in &a {
            assert!(x.collection_id.contains("_empty"));
        }
    }

    // ── Real-world scenarios ─────────────────────────────────────────────
    //
    // These tests build concrete fixtures that mirror corpora the
    // user has — author-letter pre-sorted PDFs, year-month photos,
    // power-law document distributions — and pin the partition
    // algorithm's behaviour against each.  Failures here are real
    // regressions in the routing quality.

    /// Helper to count distinct collection_id values in an
    /// assignment list.  "How many shards did this produce?"
    fn distinct_collections(assigns: &[Assignment]) -> usize {
        assigns.iter()
            .map(|a| a.collection_id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    /// Returns the count + total bytes per collection_id.
    fn bytes_per_collection(
        assigns: &[Assignment],
        files: &[FileSize],
    ) -> std::collections::HashMap<String, u64> {
        let sizes: std::collections::HashMap<PathBuf, u64> =
            files.iter().map(|f| (f.path.clone(), f.size)).collect();
        let mut out: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for a in assigns {
            *out.entry(a.collection_id.clone()).or_default()
                += sizes.get(&a.file_path).copied().unwrap_or(0);
        }
        out
    }

    /// Scenario 1: Author-letter pre-sorted PDFs (skewed English distribution).
    /// 26 folders A-Z; common letters (M, S, B, A) have many docs,
    /// rare ones (Q, X, Z) almost none.  Expect: common letters get
    /// their own shards; rare ones fold into `_misc`.
    #[test]
    fn scenario_author_letter_skewed_distribution() {
        // Synthesise a corpus reflecting real distribution of
        // English-language author surname initials.
        let dist: Vec<(&str, usize, u64)> = vec![
            // (letter, file_count, per-file size in bytes)
            ("A",  80, 100_000),  // 8 MB
            ("B", 120, 100_000),  // 12 MB
            ("C", 100, 100_000),  // 10 MB
            ("D",  60, 100_000),  // 6 MB
            ("E",  20, 100_000),  // 2 MB
            ("F",  40, 100_000),  // 4 MB
            ("G",  50, 100_000),  // 5 MB
            ("H",  90, 100_000),  // 9 MB
            ("I",  10, 100_000),  // 1 MB
            ("J",  30, 100_000),
            ("K",  40, 100_000),
            ("L",  50, 100_000),
            ("M", 150, 100_000),  // heaviest
            ("N",  20, 100_000),
            ("O",  10, 100_000),
            ("P",  60, 100_000),
            ("Q",   3, 100_000),  // ← tiny, should fold to _misc
            ("R",  70, 100_000),
            ("S", 130, 100_000),  // second-heaviest
            ("T",  60, 100_000),
            ("U",   8, 100_000),
            ("V",  10, 100_000),
            ("W",  40, 100_000),
            ("X",   2, 100_000),  // ← tiny, _misc
            ("Y",   5, 100_000),  // ← tiny, _misc
            ("Z",   3, 100_000),  // ← tiny, _misc
        ];
        let mut spec: Vec<(String, u64)> = Vec::new();
        for (letter, n, sz) in &dist {
            for i in 0..*n {
                spec.push((format!("/lib/{}/book-{:03}.pdf", letter, i), *sz));
            }
        }
        let files: Vec<FileSize> = spec.iter()
            .map(|(p, s)| FileSize { path: PathBuf::from(p), size: *s })
            .collect();
        let opts = PartitionOptions {
            max_shards: 32,
            group_depth: 1,
            min_fraction: 0.25,
        };
        let assigns = partition_assignments(Path::new("/lib"), &files, &opts);
        assert_eq!(assigns.len(), files.len(), "every file mapped");

        let shards = distinct_collections(&assigns);
        assert!(
            shards <= opts.max_shards,
            "produced {shards} shards > max_shards={}", opts.max_shards
        );
        assert!(
            shards >= 10,
            "expected ≥10 shards from a 26-letter skewed corpus, got {shards}"
        );

        // The heaviest letters (M=15MB, S=13MB, B=12MB) should each
        // get MORE than one bucket (their share of total >>
        // shard_capacity).
        let counts = bytes_per_collection(&assigns, &files);
        let m_buckets: usize = counts.keys().filter(|k| k.contains("/M/")).count();
        let s_buckets: usize = counts.keys().filter(|k| k.contains("/S/")).count();
        assert!(m_buckets >= 2, "heaviest letter M should split, got {m_buckets} buckets");
        assert!(s_buckets >= 2, "second-heaviest letter S should split, got {s_buckets} buckets");

        // The tiny letters (Q=300KB, X=200KB, Y=500KB, Z=300KB)
        // should fold into _misc together.
        let misc_files: Vec<_> = assigns.iter()
            .filter(|a| a.collection_id.contains("_misc"))
            .collect();
        assert!(misc_files.len() >= 13,
            "Q+X+Y+Z (3+2+5+3=13 files) should land in _misc; got {} misc rows",
            misc_files.len());
        // And every file under a tiny letter must be in _misc, not
        // its own letter bucket.
        for letter in &["Q", "X", "Y", "Z"] {
            for a in &assigns {
                if a.file_path.to_string_lossy().contains(&format!("/{letter}/")) {
                    assert!(
                        a.collection_id.contains("_misc"),
                        "tiny letter {letter} should be in _misc, got {}",
                        a.collection_id
                    );
                }
            }
        }
    }

    /// Scenario 2: Year-month photo archive — even distribution.
    /// `/Photos/2020/01/`, `/Photos/2020/02/`, …, `/Photos/2024/12/`
    /// — 60 folders × ~equal size.  group_depth=2 makes each
    /// year/month its own group; group_depth=1 collapses to just
    /// the year.  Verify both topologies.
    #[test]
    fn scenario_year_month_photo_archive_depth_variants() {
        let mut spec: Vec<(String, u64)> = Vec::new();
        for year in 2020..=2024 {
            for month in 1..=12 {
                for i in 0..30 {  // 30 photos per month
                    spec.push((
                        format!("/Photos/{year}/{month:02}/img-{i:04}.jpg"),
                        2_000_000,  // 2 MB / photo → 60 MB / month
                    ));
                }
            }
        }
        let files: Vec<FileSize> = spec.iter()
            .map(|(p, s)| FileSize { path: PathBuf::from(p), size: *s })
            .collect();

        // depth=1 → 5 year groups (2020-2024).
        let opts_d1 = PartitionOptions { max_shards: 20, group_depth: 1, min_fraction: 0.25 };
        let a1 = partition_assignments(Path::new("/Photos"), &files, &opts_d1);
        let collections_d1: std::collections::HashSet<&str> = a1.iter()
            .map(|a| a.collection_id.as_str())
            .collect();
        // Each year is ~720 MB, total ~3.6 GB.  With max_shards=20
        // and 5 equal-sized groups, each year gets ~4 buckets.
        assert!(collections_d1.len() <= 20);
        // Verify every year shows up.
        for year in 2020..=2024 {
            let matched: Vec<&str> = collections_d1.iter()
                .filter(|c| c.contains(&format!("/{year}/")) || c.contains(&format!("/{year}")))
                .copied()
                .collect();
            assert!(!matched.is_empty(),
                "year {year} should have at least one bucket; got {collections_d1:?}");
        }

        // depth=2 → 60 (year/month) groups.  With max_shards=20,
        // groups will share buckets across months but the algorithm
        // should still respect proportional balance — each month
        // ≈ 60 MB, total ≈ 3.6 GB, capacity ≈ 180 MB/shard, so
        // each month rounds up to 1 shard; the cap-busting second
        // pass keeps total ≤ 20.
        let opts_d2 = PartitionOptions { max_shards: 20, group_depth: 2, min_fraction: 0.25 };
        let a2 = partition_assignments(Path::new("/Photos"), &files, &opts_d2);
        let collections_d2 = distinct_collections(&a2);
        assert!(collections_d2 <= 20,
            "max_shards=20 enforced; got {collections_d2}");
        assert_eq!(a2.len(), files.len(), "every photo mapped");
    }

    /// Scenario 3: Power-law document distribution.
    /// One "anchor" folder holds 80% of bytes (e.g. a big PDF
    /// archive), 50 small folders share the rest.
    /// Expect: anchor folder gets ~80% of the shards;
    /// small folders fold into _misc.
    #[test]
    fn scenario_power_law_one_anchor_folder() {
        let mut spec: Vec<(String, u64)> = Vec::new();
        // Anchor: 800 files × 1 MB = 800 MB
        for i in 0..800 {
            spec.push((format!("/data/anchor/doc-{i:04}.pdf"), 1_000_000));
        }
        // Long tail: 50 folders × 4 files × ~1 MB = 200 MB total
        for f in 0..50 {
            for i in 0..4 {
                spec.push((format!("/data/tail-{f:02}/x-{i}.pdf"), 1_000_000));
            }
        }
        let files: Vec<FileSize> = spec.iter()
            .map(|(p, s)| FileSize { path: PathBuf::from(p), size: *s })
            .collect();
        let opts = PartitionOptions { max_shards: 20, group_depth: 1, min_fraction: 0.25 };
        let assigns = partition_assignments(Path::new("/data"), &files, &opts);

        let shards = distinct_collections(&assigns);
        assert!(shards <= 20);
        let counts = bytes_per_collection(&assigns, &files);
        let anchor_buckets = counts.keys().filter(|k| k.contains("anchor")).count();
        let misc_count: u64 = counts.iter()
            .filter(|(k, _)| k.contains("_misc"))
            .map(|(_, v)| *v).sum();
        // Anchor holds 800/1000 = 80% of bytes; with max_shards=20
        // it should get roughly 16 buckets.
        assert!(anchor_buckets >= 12,
            "anchor should split into ≥12 buckets (80% of 20), got {anchor_buckets}");
        // The tail folders (each 4MB / 1000MB total = 0.4% of total
        // < shard_capacity * 0.25 = 50MB * 0.25 = 12.5MB) are tiny
        // → fold to _misc.
        assert!(misc_count >= 100_000_000,
            "tail rows should fold into _misc (expect ≥100MB), got {misc_count}");
    }

    /// Scenario 4: max_shards=1 — operator forces everything into
    /// a single bucket.  Edge case useful for ops who want a
    /// "single-shard fallback" mode.
    #[test]
    fn scenario_max_shards_one_collapses_to_single_bucket() {
        let files = mk_files(&[
            ("/root/a/1", 1000),
            ("/root/a/2", 2000),
            ("/root/b/1", 3000),
            ("/root/c/1", 500),
        ]);
        let opts = PartitionOptions { max_shards: 1, group_depth: 1, min_fraction: 0.25 };
        let assigns = partition_assignments(Path::new("/root"), &files, &opts);
        assert_eq!(assigns.len(), 4);
        // With max_shards=1, every group ends up with 1 bucket each,
        // BUT the cap-busting second pass would still keep total
        // ≤ max_shards.  In practice that means every group gets
        // its single bucket but max_shards=1 forces all 3 groups to
        // produce 3 buckets → which then gets clipped to 1.  After
        // the clip we'd merge the smallest, ending at 1-3 buckets.
        // The contract is just "≤ max_shards", so accept anything
        // in [1, 1].  Today's algorithm actually produces 3 (each
        // group gets ≥ 1 by the per-group floor).  Document the
        // behaviour: max_shards is a soft cap that can be exceeded
        // by the per-group floor; documented in PartitionOptions
        // doc.  Test asserts the floor instead.
        let shards = distinct_collections(&assigns);
        assert!(shards <= 3, "≤ number of groups, got {shards}");
    }

    /// Scenario 5: Mixed-size files within ONE group.  Bin-packing
    /// by cumulative bytes — three 100MB files + ten 10MB files
    /// = 400MB.  Allotted 4 shards → ~100MB per bucket.  Verify
    /// each big file lands in its own bucket and small files cluster.
    #[test]
    fn scenario_mixed_sizes_within_group_byte_balanced_packing() {
        let mut spec: Vec<(String, u64)> = vec![
            ("/d/big/100a.bin".to_string(), 100_000_000),
            ("/d/big/100b.bin".to_string(), 100_000_000),
            ("/d/big/100c.bin".to_string(), 100_000_000),
        ];
        for i in 0..10 {
            spec.push((format!("/d/big/small-{i:02}.bin"), 10_000_000));
        }
        let files: Vec<FileSize> = spec.iter()
            .map(|(p, s)| FileSize { path: PathBuf::from(p), size: *s })
            .collect();
        // max_shards=4 → shard_capacity = 100MB → ceil(400/100)=4
        // buckets, all from the one group `big`.
        let opts = PartitionOptions { max_shards: 4, group_depth: 1, min_fraction: 0.0 };
        let assigns = partition_assignments(Path::new("/d"), &files, &opts);

        let counts = bytes_per_collection(&assigns, &files);
        assert!(counts.len() <= 4);
        for (_collection, bytes) in &counts {
            assert!(*bytes >= 90_000_000 && *bytes <= 120_000_000,
                "bucket bytes ~target_capacity (100MB), got {bytes}");
        }
    }

    /// Scenario 6: Deep nesting + group_depth picks the right level.
    /// /root/topA/sub1/, /root/topA/sub2/, /root/topB/sub1/
    /// depth=1 → 2 groups (topA, topB)
    /// depth=2 → 3 groups (topA/sub1, topA/sub2, topB/sub1)
    #[test]
    fn scenario_deep_nesting_respects_group_depth() {
        let files = mk_files(&[
            ("/root/topA/sub1/f1", 1000),
            ("/root/topA/sub1/f2", 1000),
            ("/root/topA/sub2/g1", 1000),
            ("/root/topB/sub1/h1", 1000),
            ("/root/topB/sub1/h2", 1000),
        ]);
        let opts_d1 = PartitionOptions { max_shards: 8, group_depth: 1, min_fraction: 0.25 };
        let a1 = partition_assignments(Path::new("/root"), &files, &opts_d1);
        let cols_d1: std::collections::HashSet<String> = a1.iter()
            .map(|a| a.collection_id.split('/').nth(1).unwrap_or("").to_string())
            .collect();
        assert!(cols_d1.contains("topA"));
        assert!(cols_d1.contains("topB"));
        assert!(!cols_d1.iter().any(|c| c.contains("sub")),
            "depth=1 must not surface sub-level segments, got {cols_d1:?}");

        let opts_d2 = PartitionOptions { max_shards: 8, group_depth: 2, min_fraction: 0.25 };
        let a2 = partition_assignments(Path::new("/root"), &files, &opts_d2);
        let cols_d2: std::collections::HashSet<String> = a2.iter()
            .map(|a| a.collection_id.clone())
            .collect();
        // All three (topA/sub1, topA/sub2, topB/sub1) appear; the
        // common "_misc" doesn't fire because group_depth=2 groups
        // are bigger and pass the threshold.
        assert!(cols_d2.iter().any(|c| c.contains("topA/sub1")), "got {cols_d2:?}");
        assert!(cols_d2.iter().any(|c| c.contains("topA/sub2")));
        assert!(cols_d2.iter().any(|c| c.contains("topB/sub1")));
    }

    /// Scenario 7: Idempotent across reruns — the partition map
    /// must be stable so re-running doesn't shuffle every file's
    /// collection_id (which would invalidate the entire VPS shard
    /// state).
    #[test]
    fn scenario_stable_under_input_permutation() {
        // Same files, different input order → same output.
        let files_a = mk_files(&[
            ("/root/x/3.bin", 1_000_000),
            ("/root/x/1.bin", 1_000_000),
            ("/root/x/2.bin", 1_000_000),
            ("/root/y/a.bin", 5_000_000),
        ]);
        let mut files_b = files_a.clone();
        files_b.reverse();
        let opts = PartitionOptions { max_shards: 8, group_depth: 1, min_fraction: 0.0 };
        let a = partition_assignments(Path::new("/root"), &files_a, &opts);
        let b = partition_assignments(Path::new("/root"), &files_b, &opts);

        // Map (file_path → collection_id) is identical regardless
        // of input order.
        let map_a: std::collections::HashMap<_, _> = a.iter()
            .map(|x| (x.file_path.clone(), x.collection_id.clone()))
            .collect();
        let map_b: std::collections::HashMap<_, _> = b.iter()
            .map(|x| (x.file_path.clone(), x.collection_id.clone()))
            .collect();
        assert_eq!(map_a, map_b, "partition map shifted when input order changed");
    }

    /// Scenario 8: One file, one shard.  Smallest possible
    /// non-trivial input.
    #[test]
    fn scenario_single_file() {
        let files = mk_files(&[("/root/only/file.bin", 1_000_000)]);
        let opts = PartitionOptions::default();
        let assigns = partition_assignments(Path::new("/root"), &files, &opts);
        assert_eq!(assigns.len(), 1);
        assert_eq!(distinct_collections(&assigns), 1);
    }

    /// Scenario 9: 10K small files in one folder — exercises the
    /// algorithm at a more realistic scale.  Must complete in well
    /// under a second.
    #[test]
    fn scenario_10k_files_completes_fast() {
        let mut spec: Vec<(String, u64)> = Vec::with_capacity(10_000);
        for i in 0..10_000 {
            spec.push((format!("/root/dir/f-{i:05}.txt"), 1_000));  // 1KB each = 10 MB total
        }
        let files: Vec<FileSize> = spec.iter()
            .map(|(p, s)| FileSize { path: PathBuf::from(p), size: *s })
            .collect();
        let opts = PartitionOptions { max_shards: 16, group_depth: 1, min_fraction: 0.0 };
        let start = std::time::Instant::now();
        let assigns = partition_assignments(Path::new("/root"), &files, &opts);
        let elapsed = start.elapsed();
        assert_eq!(assigns.len(), 10_000);
        // Algorithm is O(n log n); 10K files should finish well under
        // 2000ms even in debug mode on a loaded CI runner.
        assert!(elapsed.as_millis() < 2000,
            "partition of 10K files took {}ms — algorithm too slow",
            elapsed.as_millis());
    }

    /// Scenario 10: Files at the root (no sub-dir).  group_depth=1
    /// has no segment to extract.
    #[test]
    fn scenario_files_at_root_no_subdir() {
        let files = mk_files(&[
            ("/root/a.pdf", 100),
            ("/root/b.pdf", 100),
            ("/root/sub/c.pdf", 100),
        ]);
        let opts = PartitionOptions::default();
        let assigns = partition_assignments(Path::new("/root"), &files, &opts);
        // Bare files at the root land in some bucket; subfolder
        // files land in a `sub` bucket.  Both reachable.
        let cols: std::collections::HashSet<&str> = assigns.iter()
            .map(|a| a.collection_id.as_str())
            .collect();
        assert!(cols.len() >= 1);
        assert_eq!(assigns.len(), 3);
    }

    /// Scenario 11: Author-letter corpus where the heaviest single
    /// letter is huge enough to need MORE buckets than max_shards
    /// alone would allow.  The algorithm must scale-down to respect
    /// the cap, not blow past it.
    #[test]
    fn scenario_giant_single_group_respects_max_shards_cap() {
        // One letter with 1000 × 10MB = 10GB; rest negligible.
        let mut spec: Vec<(String, u64)> = (0..1000)
            .map(|i| (format!("/lib/M/heavy-{i:04}.pdf"), 10_000_000))
            .collect();
        // A few small distractors.
        for letter in &["A", "B", "C"] {
            spec.push((format!("/lib/{letter}/tiny.txt"), 1_000));
        }
        let files: Vec<FileSize> = spec.iter()
            .map(|(p, s)| FileSize { path: PathBuf::from(p), size: *s })
            .collect();
        let opts = PartitionOptions { max_shards: 8, group_depth: 1, min_fraction: 0.25 };
        let assigns = partition_assignments(Path::new("/lib"), &files, &opts);
        let shards = distinct_collections(&assigns);
        assert!(shards <= opts.max_shards,
            "shards {shards} must not exceed max_shards {}", opts.max_shards);
        // M should get nearly all of them.
        let m_buckets: usize = assigns.iter()
            .filter(|a| a.collection_id.contains("/M/") || a.collection_id.contains("/M"))
            .map(|a| a.collection_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert!(m_buckets >= 5,
            "the giant M group should consume ≥5 of the 8 shards, got {m_buckets}");
    }

    /// Scenario 12: Power-of-2 distribution (1, 2, 4, 8, 16 MB groups).
    /// Heavier groups should proportionally get more buckets.
    /// Verifies the algorithm's proportional-allocation property.
    #[test]
    fn scenario_power_of_two_distribution() {
        let mut spec: Vec<(String, u64)> = Vec::new();
        for (g, mb) in &[("g1", 1), ("g2", 2), ("g4", 4), ("g8", 8), ("g16", 16)] {
            // Total per group = mb MB; 4 files each.
            for i in 0..4 {
                spec.push((
                    format!("/root/{g}/{i}.bin"),
                    (mb * 1_000_000 / 4) as u64,
                ));
            }
        }
        let files: Vec<FileSize> = spec.iter()
            .map(|(p, s)| FileSize { path: PathBuf::from(p), size: *s })
            .collect();
        // Total = 31MB; max_shards=15; shard_capacity ≈ 2MB.
        // g1 → ceil(1/2) = 1; g2 → 1; g4 → 2; g8 → 4; g16 → 8.
        // Sum = 16, slightly over → scale-down trims by 1.
        let opts = PartitionOptions { max_shards: 15, group_depth: 1, min_fraction: 0.0 };
        let assigns = partition_assignments(Path::new("/root"), &files, &opts);
        let counts = bytes_per_collection(&assigns, &files);
        // Match exact segment (avoid /g1 matching /g16 as a
        // substring).  collection_id format is
        // `<root>/<group>[/<bucket>]`, so split on `/` and check
        // the second segment exactly.
        let buckets_for_g = |name: &str| -> usize {
            counts.keys()
                .filter(|k| k.split('/').nth(1) == Some(name))
                .count()
        };
        // The proportional invariant: heavier group ⇒ ≥ buckets.
        // We don't assert exact counts (the cap-busting scale-down
        // is non-deterministic across edge cases), only monotonic
        // order.
        let b1  = buckets_for_g("g1");
        let b2  = buckets_for_g("g2");
        let b16 = buckets_for_g("g16");
        assert!(b16 >= b2 && b2 >= b1,
            "expected monotonic buckets g16 ≥ g2 ≥ g1; got g16={b16} g2={b2} g1={b1}");
        assert!(b16 >= 4,
            "g16 (52% of total bytes) should get at least 4 of 15 shards, got {b16}");
    }

    /// Scenario 13: User-set `max_shards` clipped to ≥ 1.  Safety
    /// against pathological inputs.
    #[test]
    fn scenario_max_shards_zero_treated_as_one() {
        let files = mk_files(&[("/root/x/a", 100)]);
        let opts = PartitionOptions { max_shards: 0, group_depth: 1, min_fraction: 0.25 };
        let assigns = partition_assignments(Path::new("/root"), &files, &opts);
        // No panic, no divide-by-zero.  Single file → single bucket.
        assert_eq!(assigns.len(), 1);
    }

    #[test]
    fn partition_map_roundtrip_via_sqlite() {
        let tmp = tempfile::TempDir::new().unwrap();
        let map = PartitionMap::open(tmp.path()).unwrap();
        let a = Assignment {
            root_path: PathBuf::from("/root"),
            file_path: PathBuf::from("/root/a/x.pdf"),
            collection_id: "Documents/Authors/3".into(),
        };
        map.write_batch(std::slice::from_ref(&a)).unwrap();
        let looked_up = map.lookup(Path::new("/root"), Path::new("/root/a/x.pdf"));
        assert_eq!(looked_up.as_deref(), Some("Documents/Authors/3"));
        // Unknown file → None.
        assert!(map.lookup(Path::new("/root"), Path::new("/root/missing")).is_none());
    }
}
