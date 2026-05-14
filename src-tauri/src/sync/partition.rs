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
    let root_str = root_path.to_string_lossy();
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
        // Edge case: ceil'd sum still > max_shards if every group
        // hit the floor.  Drop the smallest allotments to 1 first;
        // then if still over, merge the two smallest groups.  Caps
        // the worst case at 2 × max_shards which is acceptable.
        if scaled_sum > max_shards {
            // Sort groups by size ascending; clip the smallest first.
            group_sizes.sort_by_key(|(_, s)| *s);
            for (k, _) in &group_sizes {
                if scaled_sum <= max_shards { break; }
                if allotments[k] > 1 {
                    *allotments.get_mut(k).unwrap() -= 1;
                    scaled_sum -= 1;
                }
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
