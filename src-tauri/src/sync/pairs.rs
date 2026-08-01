//! Persisted local-folder ↔ cloud-drive sync-pair definitions.
//!
//! This is the configuration boundary for general sync.  It deliberately
//! does not execute transfers yet: cloud-backup shard sync remains separate,
//! while the future runner can consume these stable IDs, filters, and
//! watermarks without changing the on-disk format.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
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

fn filter_matches(path: &str, includes: &[String], excludes: &[String]) -> bool {
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
";

impl SyncPairStore {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let conn = Connection::open(data_dir.join("sync_pairs.db"))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
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
    }
}
