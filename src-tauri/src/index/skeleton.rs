/// Stage W — Skeleton local index.
///
/// When `IndexConfig.local_skeleton_only = true`, bg_ingest skips
/// LanceDB, FTS, and the embedder entirely and only writes two tiny
/// SQLite KV tables:
///
/// * `author_index`     — (author TEXT PK, doc_count INT, last_seen_at INT)
/// * `parent_dir_index` — (parent_dir TEXT PK, doc_count INT, last_seen_at INT)
///
/// Both tables let the search UI show instant local hints while the
/// real document results are fetched from the remote VPS cb-api.
///
/// The DB file lives at `<data_dir>/skeleton_index.db`.  All mutations
/// go through an internal `rusqlite::Connection` protected by a
/// `std::sync::Mutex`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

/// A compact KV index of (author, parent_dir) → doc_count.
#[derive(Clone)]
pub struct SkeletonIndex {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SkeletonHit {
    pub name: String,
    pub doc_count: i64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SkeletonSearchResult {
    pub authors: Vec<SkeletonHit>,
    pub parent_dirs: Vec<SkeletonHit>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SkeletonStats {
    pub author_count: i64,
    pub parent_dir_count: i64,
    pub total_docs: i64,
}

impl SkeletonIndex {
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("skeleton_index.db");
        let conn = Connection::open(&path)
            .with_context(|| format!("open skeleton DB {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS author_index (
                author       TEXT PRIMARY KEY,
                doc_count    INTEGER NOT NULL DEFAULT 0,
                last_seen_at INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS parent_dir_index (
                parent_dir   TEXT PRIMARY KEY,
                doc_count    INTEGER NOT NULL DEFAULT 0,
                last_seen_at INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Increment the doc_count for *author* by 1.  No-op when author is
    /// empty.
    pub fn upsert_author(&self, author: &str) -> Result<()> {
        let author = author.trim();
        if author.is_empty() {
            return Ok(());
        }
        let now = Self::now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO author_index (author, doc_count, last_seen_at)
             VALUES (?1, 1, ?2)
             ON CONFLICT(author) DO UPDATE SET
               doc_count    = doc_count + 1,
               last_seen_at = ?2",
            params![author, now],
        )?;
        Ok(())
    }

    /// Increment the doc_count for *parent_dir* by 1.  No-op when dir is
    /// empty.
    pub fn upsert_parent_dir(&self, parent_dir: &str) -> Result<()> {
        let dir = parent_dir.trim();
        if dir.is_empty() {
            return Ok(());
        }
        let now = Self::now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO parent_dir_index (parent_dir, doc_count, last_seen_at)
             VALUES (?1, 1, ?2)
             ON CONFLICT(parent_dir) DO UPDATE SET
               doc_count    = doc_count + 1,
               last_seen_at = ?2",
            params![dir, now],
        )?;
        Ok(())
    }

    /// Return authors whose name contains *query* (case-insensitive), ordered
    /// by doc_count desc.
    pub fn search_authors(&self, query: &str, limit: usize) -> Result<Vec<SkeletonHit>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = conn.prepare(
            "SELECT author, doc_count FROM author_index
             WHERE lower(author) LIKE ?1
             ORDER BY doc_count DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok(SkeletonHit {
                name: row.get(0)?,
                doc_count: row.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// Return parent_dirs whose path contains *query* (case-insensitive),
    /// ordered by doc_count desc.
    pub fn search_parent_dirs(&self, query: &str, limit: usize) -> Result<Vec<SkeletonHit>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = conn.prepare(
            "SELECT parent_dir, doc_count FROM parent_dir_index
             WHERE lower(parent_dir) LIKE ?1
             ORDER BY doc_count DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok(SkeletonHit {
                name: row.get(0)?,
                doc_count: row.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    /// Aggregate stats for the status endpoint.
    pub fn stats(&self) -> Result<SkeletonStats> {
        let conn = self.conn.lock().unwrap();
        let author_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM author_index", [], |r| r.get(0))?;
        let parent_dir_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM parent_dir_index", [], |r| r.get(0))?;
        let total_docs: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(doc_count), 0) FROM author_index",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(SkeletonStats {
            author_count,
            parent_dir_count,
            total_docs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_index() -> (SkeletonIndex, TempDir) {
        let dir = TempDir::new().unwrap();
        let idx = SkeletonIndex::open_or_create(dir.path()).unwrap();
        (idx, dir)
    }

    #[test]
    fn upsert_author_increments() {
        let (idx, _dir) = make_index();
        idx.upsert_author("Kant").unwrap();
        idx.upsert_author("Kant").unwrap();
        idx.upsert_author("Hegel").unwrap();
        let hits = idx.search_authors("k", 10).unwrap();
        assert_eq!(hits[0].name, "Kant");
        assert_eq!(hits[0].doc_count, 2);
    }

    #[test]
    fn upsert_parent_dir_increments() {
        let (idx, _dir) = make_index();
        idx.upsert_parent_dir("/docs/kant").unwrap();
        idx.upsert_parent_dir("/docs/kant").unwrap();
        let hits = idx.search_parent_dirs("kant", 10).unwrap();
        assert_eq!(hits[0].doc_count, 2);
    }

    #[test]
    fn empty_author_is_noop() {
        let (idx, _dir) = make_index();
        idx.upsert_author("").unwrap();
        idx.upsert_author("   ").unwrap();
        let hits = idx.search_authors("", 10).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_is_case_insensitive() {
        let (idx, _dir) = make_index();
        idx.upsert_author("Nietzsche").unwrap();
        let hits = idx.search_authors("nietzsche", 10).unwrap();
        assert_eq!(hits.len(), 1);
        let hits2 = idx.search_authors("NIETZSCHE", 10).unwrap();
        assert_eq!(hits2.len(), 1);
    }

    #[test]
    fn search_returns_empty_for_no_match() {
        let (idx, _dir) = make_index();
        idx.upsert_author("Kant").unwrap();
        let hits = idx.search_authors("xyz_no_match", 10).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn stats_reflect_upserts() {
        let (idx, _dir) = make_index();
        idx.upsert_author("A").unwrap();
        idx.upsert_author("B").unwrap();
        idx.upsert_author("A").unwrap();
        idx.upsert_parent_dir("/x").unwrap();
        let s = idx.stats().unwrap();
        assert_eq!(s.author_count, 2);
        assert_eq!(s.parent_dir_count, 1);
        assert_eq!(s.total_docs, 3); // A=2, B=1
    }

    #[test]
    fn open_idempotent() {
        let dir = TempDir::new().unwrap();
        {
            let idx = SkeletonIndex::open_or_create(dir.path()).unwrap();
            idx.upsert_author("Kafka").unwrap();
        }
        // Reopen: data persists.
        let idx2 = SkeletonIndex::open_or_create(dir.path()).unwrap();
        let hits = idx2.search_authors("kafka", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_count, 1);
    }
}
