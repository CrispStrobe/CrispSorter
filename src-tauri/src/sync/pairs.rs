//! Persisted local-folder ↔ cloud-drive sync-pair definitions.
//!
//! This is the configuration boundary for general sync.  It deliberately
//! does not execute transfers yet: cloud-backup shard sync remains separate,
//! while the future runner can consume these stable IDs, filters, and
//! watermarks without changing the on-disk format.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncPairMode {
    ToCloud,
    ToLocal,
    TwoWay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncPair {
    pub id: String,
    pub local_root: String,
    pub drive_id: String,
    pub remote_root: String,
    pub mode: SyncPairMode,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub watermark: i64,
    pub enabled: bool,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncPlanEntry {
    pub relative_path: String,
    pub size: u64,
    pub mtime_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncRemoteEntry {
    pub relative_path: String,
    pub size: u64,
    pub mtime_unix: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncComparisonAction {
    LocalOnly,
    RemoteOnly,
    Unchanged,
    UseLocal,
    UseRemote,
    KeepBoth,
    ManualReview,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncComparisonEntry {
    pub relative_path: String,
    pub local: Option<SyncPlanEntry>,
    pub remote: Option<SyncRemoteEntry>,
    pub action: SyncComparisonAction,
}

/// Compare provider metadata with the local plan. This is intentionally
/// side-effect free; callers decide whether an action is safe to execute.
pub fn compare_plans(
    local: &[SyncPlanEntry],
    remote: &[SyncRemoteEntry],
    policy: super::conflict::ConflictPolicy,
) -> Vec<SyncComparisonEntry> {
    let mut paths: BTreeMap<String, ()> = BTreeMap::new();
    for entry in local {
        paths.entry(entry.relative_path.clone()).or_insert(());
    }
    for entry in remote {
        paths.entry(entry.relative_path.clone()).or_insert(());
    }
    let local_by_path: BTreeMap<_, _> = local.iter().map(|e| (e.relative_path.clone(), e)).collect();
    let remote_by_path: BTreeMap<_, _> = remote.iter().map(|e| (e.relative_path.clone(), e)).collect();
    paths
        .into_keys()
        .map(|path| {
            let local_entry = local_by_path.get(&path).copied().cloned();
            let remote_entry = remote_by_path.get(&path).copied().cloned();
            let action = match (&local_entry, &remote_entry) {
                (None, Some(_)) => SyncComparisonAction::RemoteOnly,
                (Some(_), None) => SyncComparisonAction::LocalOnly,
                (Some(local), Some(remote))
                    if local.size == remote.size
                        && Some(local.mtime_unix) == remote.mtime_unix =>
                {
                    SyncComparisonAction::Unchanged
                }
                (Some(local), Some(remote)) => match policy {
                    super::conflict::ConflictPolicy::LocalWins => SyncComparisonAction::UseLocal,
                    super::conflict::ConflictPolicy::RemoteWins => SyncComparisonAction::UseRemote,
                    super::conflict::ConflictPolicy::KeepBoth => SyncComparisonAction::KeepBoth,
                    super::conflict::ConflictPolicy::Manual => SyncComparisonAction::ManualReview,
                    super::conflict::ConflictPolicy::NewestWins => {
                        if local.mtime_unix >= remote.mtime_unix.unwrap_or(i64::MIN) {
                            SyncComparisonAction::UseLocal
                        } else {
                            SyncComparisonAction::UseRemote
                        }
                    }
                },
                (None, None) => unreachable!(),
            };
            SyncComparisonEntry { relative_path: path, local: local_entry, remote: remote_entry, action }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncPairRun {
    pub id: i64,
    pub pair_id: String,
    pub status: String,
    pub planned: usize,
    pub uploaded: usize,
    pub downloaded: usize,
    pub watermark: i64,
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: i64,
}

/// Build a read-only local snapshot for a pair. No provider is contacted and
/// no watermark is advanced; the eventual runner can compare this plan with
/// its remote manifest before submitting transfers.
pub fn plan_local(pair: &SyncPair) -> Result<Vec<SyncPlanEntry>> {
    let root = Path::new(&pair.local_root);
    if !root.is_dir() {
        anyhow::bail!("sync pair local root is not a directory: {}", root.display());
    }
    let mut out = Vec::new();
    visit_local(root, root, pair, &mut out)?;
    out.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(out)
}

/// Return local entries at or newer than the persisted pair watermark.
/// A watermark of zero intentionally selects every matching file on the
/// first push. Inclusive comparison avoids missing edits that occur within
/// the same second; equal-timestamp files may be safely rechecked.
pub fn plan_local_since(pair: &SyncPair) -> Result<Vec<SyncPlanEntry>> {
    Ok(plan_local(pair)?
        .into_iter()
        .filter(|entry| entry.mtime_unix >= pair.watermark)
        .collect())
}

fn visit_local(
    root: &Path,
    directory: &Path,
    pair: &SyncPair,
    out: &mut Vec<SyncPlanEntry>,
) -> Result<()> {
    for item in std::fs::read_dir(directory)? {
        let item = item?;
        let path = item.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            visit_local(root, &path, pair, out)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        if !filter_matches(&relative, &pair.include_globs, &pair.exclude_globs) {
            continue;
        }
        let mtime_unix = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(SyncPlanEntry {
            relative_path: relative,
            size: metadata.len(),
            mtime_unix,
        });
    }
    Ok(())
}

pub(crate) fn filter_matches(path: &str, includes: &[String], excludes: &[String]) -> bool {
    let included = includes.is_empty() || includes.iter().any(|p| glob_matches(p, path));
    included && !excludes.iter().any(|p| glob_matches(p, path))
}

/// Small dependency-free glob matcher for persisted sync filters. `*` matches
/// within one path segment, `**` spans segments, and `?` matches one byte.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<_> = pattern.trim_matches('/').split('/').collect();
    let path: Vec<_> = path.trim_matches('/').split('/').collect();
    glob_segments(&pattern, &path)
}

fn glob_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some(("**", rest)) => {
            glob_segments(rest, path)
                || path
                    .split_first()
                    .is_some_and(|(_, tail)| glob_segments(pattern, tail))
        }
        Some((segment, rest)) => path.split_first().is_some_and(|(head, tail)| {
            segment_matches(segment, head) && glob_segments(rest, tail)
        }),
    }
}

fn segment_matches(pattern: &str, value: &str) -> bool {
    let mut p = pattern.chars().peekable();
    let mut v = value.chars().peekable();
    while let Some(ch) = p.next() {
        match ch {
            '*' => {
                if p.peek().is_none() {
                    return true;
                }
                let rest: String = p.clone().collect();
                if segment_matches(&rest, &v.clone().collect::<String>()) {
                    return true;
                }
                while v.next().is_some() {
                    if segment_matches(&rest, &v.clone().collect::<String>()) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if v.next().is_none() {
                    return false;
                }
            }
            literal if v.next() != Some(literal) => return false,
            _ => {}
        }
    }
    v.next().is_none()
}

pub struct SyncPairStore {
    conn: Connection,
}

const MAX_RUNS_PER_PAIR: usize = 100;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sync_pairs (
    id TEXT PRIMARY KEY,
    local_root TEXT NOT NULL,
    drive_id TEXT NOT NULL,
    remote_root TEXT NOT NULL,
    mode TEXT NOT NULL,
    include_globs TEXT NOT NULL,
    exclude_globs TEXT NOT NULL,
    watermark INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 1,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sync_pair_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pair_id TEXT NOT NULL,
    status TEXT NOT NULL,
    planned INTEGER NOT NULL,
    uploaded INTEGER NOT NULL,
    downloaded INTEGER NOT NULL DEFAULT 0,
    watermark INTEGER NOT NULL,
    error TEXT,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sync_pair_runs_pair ON sync_pair_runs(pair_id, id DESC);
";

impl SyncPairStore {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let conn = Connection::open(data_dir.join("sync_pairs.db"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        let has_downloaded: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('sync_pair_runs') WHERE name = 'downloaded'",
            [],
            |row| row.get(0),
        )?;
        if has_downloaded == 0 {
            conn.execute(
                "ALTER TABLE sync_pair_runs ADD COLUMN downloaded INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        Ok(Self { conn })
    }

    pub fn upsert(&self, mut pair: SyncPair) -> Result<SyncPair> {
        pair.updated_at = now_ms();
        self.conn.execute(
            "INSERT INTO sync_pairs
             (id, local_root, drive_id, remote_root, mode, include_globs,
              exclude_globs, watermark, enabled, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
               local_root=excluded.local_root, drive_id=excluded.drive_id,
               remote_root=excluded.remote_root, mode=excluded.mode,
               include_globs=excluded.include_globs,
               exclude_globs=excluded.exclude_globs,
               watermark=excluded.watermark, enabled=excluded.enabled,
               updated_at=excluded.updated_at",
            params![
                pair.id,
                pair.local_root,
                pair.drive_id,
                pair.remote_root,
                serde_json::to_string(&pair.mode)?,
                serde_json::to_string(&pair.include_globs)?,
                serde_json::to_string(&pair.exclude_globs)?,
                pair.watermark,
                pair.enabled,
                pair.updated_at,
            ],
        )?;
        Ok(pair)
    }

    pub fn list(&self) -> Result<Vec<SyncPair>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, local_root, drive_id, remote_root, mode, include_globs,
                    exclude_globs, watermark, enabled, updated_at
             FROM sync_pairs ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            let mode: String = row.get(4)?;
            let includes: String = row.get(5)?;
            let excludes: String = row.get(6)?;
            Ok(SyncPair {
                id: row.get(0)?,
                local_root: row.get(1)?,
                drive_id: row.get(2)?,
                remote_root: row.get(3)?,
                mode: serde_json::from_str(&mode).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                include_globs: serde_json::from_str(&includes).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                exclude_globs: serde_json::from_str(&excludes).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                watermark: row.get(7)?,
                enabled: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM sync_pairs WHERE id = ?1", [id])?
            != 0)
    }

    pub fn record_run(&self, run: &SyncPairRun) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO sync_pair_runs
             (pair_id, status, planned, uploaded, downloaded, watermark, error, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run.pair_id,
                run.status,
                run.planned as i64,
                run.uploaded as i64,
                run.downloaded as i64,
                run.watermark,
                run.error,
                run.started_at,
                run.finished_at,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "DELETE FROM sync_pair_runs
             WHERE pair_id = ?1 AND id NOT IN
               (SELECT id FROM sync_pair_runs WHERE pair_id = ?1 ORDER BY id DESC LIMIT ?2)",
            params![run.pair_id, MAX_RUNS_PER_PAIR as i64],
        )?;
        Ok(id)
    }

    pub fn list_runs(&self, pair_id: &str, limit: usize) -> Result<Vec<SyncPairRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pair_id, status, planned, uploaded, downloaded, watermark, error,
                    started_at, finished_at
             FROM sync_pair_runs WHERE pair_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pair_id, limit.min(1000) as i64], |row| {
            Ok(SyncPairRun {
                id: row.get(0)?,
                pair_id: row.get(1)?,
                status: row.get(2)?,
                planned: row.get::<_, i64>(3)? as usize,
                uploaded: row.get::<_, i64>(4)? as usize,
                downloaded: row.get::<_, i64>(5)? as usize,
                watermark: row.get(6)?,
                error: row.get(7)?,
                started_at: row.get(8)?,
                finished_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> SyncPair {
        SyncPair {
            id: "pair-1".into(),
            local_root: "/tmp/docs".into(),
            drive_id: "drive-1".into(),
            remote_root: "/backup/docs".into(),
            mode: SyncPairMode::TwoWay,
            include_globs: vec!["**/*.pdf".into()],
            exclude_globs: vec!["**/.cache/**".into()],
            watermark: 42,
            enabled: true,
            updated_at: 0,
        }
    }

    #[test]
    fn pair_store_round_trips_filters_mode_and_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncPairStore::open(dir.path()).unwrap();
        let saved = store.upsert(pair()).unwrap();
        assert!(saved.updated_at > 0);
        let rows = store.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mode, SyncPairMode::TwoWay);
        assert_eq!(rows[0].include_globs, vec!["**/*.pdf"]);
        assert_eq!(rows[0].watermark, 42);
    }

    #[test]
    fn upsert_is_idempotent_and_delete_reports_presence() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncPairStore::open(dir.path()).unwrap();
        store.upsert(pair()).unwrap();
        let mut changed = pair();
        changed.enabled = false;
        store.upsert(changed).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(!store.list().unwrap()[0].enabled);
        assert!(store.delete("pair-1").unwrap());
        assert!(!store.delete("pair-1").unwrap());
    }

    #[test]
    fn glob_filters_and_local_plan_are_deterministic() {
        assert!(glob_matches("**/*.pdf", "nested/report.pdf"));
        assert!(glob_matches("docs/*.pdf", "docs/report.pdf"));
        assert!(!glob_matches("docs/*.pdf", "docs/nested/report.pdf"));
        assert!(glob_matches("**/cache/**", "a/cache/b.bin"));

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/report.pdf"), b"pdf").unwrap();
        std::fs::write(dir.path().join("skip.txt"), b"skip").unwrap();
        let mut p = pair();
        p.local_root = dir.path().to_string_lossy().into_owned();
        let plan = plan_local(&p).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].relative_path, "nested/report.pdf");
        assert_eq!(plan[0].size, 3);

        p.watermark = plan[0].mtime_unix;
        assert_eq!(plan_local_since(&p).unwrap().len(), 1);
        p.watermark -= 1;
        assert_eq!(plan_local_since(&p).unwrap().len(), 1);
    }

    #[test]
    fn run_ledger_round_trips_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncPairStore::open(dir.path()).unwrap();
        for status in ["completed", "failed"] {
            store
                .record_run(&SyncPairRun {
                    id: 0,
                    pair_id: "pair-1".into(),
                    status: status.into(),
                    planned: 2,
                    uploaded: 1,
                    downloaded: 0,
                    watermark: 9,
                    error: (status == "failed").then(|| "offline".into()),
                    started_at: 1,
                    finished_at: 2,
                })
                .unwrap();
        }
        let runs = store.list_runs("pair-1", 10).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].status, "failed");
        assert_eq!(runs[0].error.as_deref(), Some("offline"));
    }

    #[test]
    fn run_ledger_prunes_old_rows_per_pair() {
        let dir = tempfile::tempdir().unwrap();
        let store = SyncPairStore::open(dir.path()).unwrap();
        for i in 0..(MAX_RUNS_PER_PAIR + 5) {
            store
                .record_run(&SyncPairRun {
                    id: 0,
                    pair_id: "pair-1".into(),
                    status: format!("run-{i}"),
                    planned: 0,
                    uploaded: 0,
                    downloaded: 0,
                    watermark: i as i64,
                    error: None,
                    started_at: i as i64,
                    finished_at: i as i64,
                })
                .unwrap();
        }
        let runs = store.list_runs("pair-1", 1000).unwrap();
        assert_eq!(runs.len(), MAX_RUNS_PER_PAIR);
        assert_eq!(runs[0].status, "run-104");
        assert_eq!(runs.last().unwrap().status, "run-5");
    }

    #[test]
    fn compare_plans_classifies_changes_and_applies_policy() {
        let local = vec![
            SyncPlanEntry { relative_path: "same.txt".into(), size: 2, mtime_unix: 10 },
            SyncPlanEntry { relative_path: "changed.txt".into(), size: 3, mtime_unix: 20 },
            SyncPlanEntry { relative_path: "local.txt".into(), size: 1, mtime_unix: 1 },
        ];
        let remote = vec![
            SyncRemoteEntry { relative_path: "same.txt".into(), size: 2, mtime_unix: Some(10) },
            SyncRemoteEntry { relative_path: "changed.txt".into(), size: 4, mtime_unix: Some(30) },
            SyncRemoteEntry { relative_path: "remote.txt".into(), size: 5, mtime_unix: Some(1) },
        ];
        let rows = compare_plans(&local, &remote, super::conflict::ConflictPolicy::NewestWins);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].action, SyncComparisonAction::UseRemote);
        assert_eq!(rows[1].action, SyncComparisonAction::LocalOnly);
        assert_eq!(rows[2].action, SyncComparisonAction::RemoteOnly);
        assert_eq!(rows[3].action, SyncComparisonAction::Unchanged);
        let manual = compare_plans(&local, &remote, super::conflict::ConflictPolicy::Manual);
        assert_eq!(manual[0].action, SyncComparisonAction::ManualReview);
    }
}
