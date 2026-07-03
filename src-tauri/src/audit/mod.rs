//! Append-only audit trail (P25.2).
//!
//! Records every significant user action (search, open, export, delete,
//! ingest, status change) in a WAL-mode SQLite database.  The log is
//! append-only — no UPDATE or DELETE on existing rows.  Designed for
//! ISO 27001 / GDPR compliance in enterprise deployments.

pub mod tauri_commands;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS audit_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ts         INTEGER NOT NULL,              -- epoch millis
    action     TEXT    NOT NULL,              -- search, open, export, delete, ingest, status_change, ...
    doc_id     TEXT,                          -- nullable (e.g. search has no single doc_id)
    detail     TEXT    NOT NULL DEFAULT '',   -- JSON or human-readable context
    user_agent TEXT    NOT NULL DEFAULT 'gui' -- 'gui', 'cli', 'api'
);
CREATE INDEX IF NOT EXISTS idx_audit_ts     ON audit_log(ts);
CREATE INDEX IF NOT EXISTS idx_audit_action ON audit_log(action);
CREATE INDEX IF NOT EXISTS idx_audit_doc_id ON audit_log(doc_id) WHERE doc_id IS NOT NULL;
";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: i64,
    pub action: String,
    pub doc_id: Option<String>,
    pub detail: String,
    pub user_agent: String,
}

#[derive(Clone)]
pub struct AuditLog {
    conn: Arc<Mutex<Connection>>,
}

impl AuditLog {
    /// Open or create the audit log at `<data_dir>/audit.db`.
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("audit.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening audit DB {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA).context("creating audit schema")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Append an audit entry.  This is the hot path — keep it fast.
    pub fn log(
        &self,
        action: &str,
        doc_id: Option<&str>,
        detail: &str,
        user_agent: &str,
    ) -> Result<()> {
        let ts = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO audit_log (ts, action, doc_id, detail, user_agent) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ts, action, doc_id, detail, user_agent],
        )?;
        Ok(())
    }

    /// Query the log with optional filters.
    pub fn query(
        &self,
        since: Option<i64>,
        action: Option<&str>,
        doc_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<AuditEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut conditions = vec!["1=1".to_string()];
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ts) = since {
            conditions.push(format!("ts >= ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(ts));
        }
        if let Some(a) = action {
            conditions.push(format!("action = ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(a.to_string()));
        }
        if let Some(d) = doc_id {
            conditions.push(format!("doc_id = ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(d.to_string()));
        }

        let sql = format!(
            "SELECT id, ts, action, doc_id, detail, user_agent FROM audit_log WHERE {} ORDER BY ts DESC LIMIT {} OFFSET {}",
            conditions.join(" AND "),
            limit,
            offset,
        );

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            Ok(AuditEntry {
                id: row.get(0)?,
                ts: row.get(1)?,
                action: row.get(2)?,
                doc_id: row.get(3)?,
                detail: row.get(4)?,
                user_agent: row.get(5)?,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// Count total entries (optionally filtered by action).
    pub fn count(&self, action: Option<&str>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = if let Some(a) = action {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = ?1",
                params![a],
                |row| row.get(0),
            )?
        } else {
            conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))?
        };
        Ok(count as usize)
    }

    /// Summary: count of entries per action type.
    pub fn action_summary(&self) -> Result<Vec<(String, usize)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT action, COUNT(*) FROM audit_log GROUP BY action ORDER BY COUNT(*) DESC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn log_and_query() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        log.log("search", None, "query=hello", "gui").unwrap();
        log.log("open", Some("doc1"), "/path/to/file.pdf", "gui").unwrap();
        log.log("delete", Some("doc2"), "", "cli").unwrap();

        let all = log.query(None, None, None, 100, 0).unwrap();
        assert_eq!(all.len(), 3);
        // Most recent first
        assert_eq!(all[0].action, "delete");

        let searches = log.query(None, Some("search"), None, 100, 0).unwrap();
        assert_eq!(searches.len(), 1);

        let doc1 = log.query(None, None, Some("doc1"), 100, 0).unwrap();
        assert_eq!(doc1.len(), 1);
        assert_eq!(doc1[0].action, "open");
    }

    #[test]
    fn count_and_summary() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        log.log("search", None, "a", "gui").unwrap();
        log.log("search", None, "b", "gui").unwrap();
        log.log("open", Some("x"), "", "gui").unwrap();

        assert_eq!(log.count(None).unwrap(), 3);
        assert_eq!(log.count(Some("search")).unwrap(), 2);

        let summary = log.action_summary().unwrap();
        assert_eq!(summary[0], ("search".to_string(), 2));
        assert_eq!(summary[1], ("open".to_string(), 1));
    }

    #[test]
    fn query_with_since_filter() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        log.log("old", None, "", "gui").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let ts = super::now_ms();
        log.log("new", None, "", "gui").unwrap();
        let recent = log.query(Some(ts), None, None, 100, 0).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].action, "new");
    }

    #[test]
    fn query_combined_filters() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        log.log("search", Some("d1"), "q=a", "gui").unwrap();
        log.log("search", Some("d2"), "q=b", "cli").unwrap();
        log.log("open", Some("d1"), "", "gui").unwrap();
        let results = log.query(None, Some("search"), Some("d1"), 100, 0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].detail, "q=a");
    }

    #[test]
    fn query_pagination() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        for i in 0..10 {
            log.log("action", None, &format!("{i}"), "gui").unwrap();
        }
        let page1 = log.query(None, None, None, 3, 0).unwrap();
        let page2 = log.query(None, None, None, 3, 3).unwrap();
        assert_eq!(page1.len(), 3);
        assert_eq!(page2.len(), 3);
        assert_ne!(page1[0].id, page2[0].id);
    }

    #[test]
    fn user_agent_variants() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        log.log("a", None, "", "gui").unwrap();
        log.log("b", None, "", "cli").unwrap();
        log.log("c", None, "", "api").unwrap();
        let all = log.query(None, None, None, 100, 0).unwrap();
        let agents: Vec<&str> = all.iter().map(|e| e.user_agent.as_str()).collect();
        assert!(agents.contains(&"gui"));
        assert!(agents.contains(&"cli"));
        assert!(agents.contains(&"api"));
    }

    #[test]
    fn empty_log_counts() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        assert_eq!(log.count(None).unwrap(), 0);
        assert!(log.action_summary().unwrap().is_empty());
        assert!(log.query(None, None, None, 100, 0).unwrap().is_empty());
    }

    #[test]
    fn concurrent_writes() {
        use std::sync::Arc;
        let dir = TempDir::new().unwrap();
        let log = Arc::new(AuditLog::open_or_create(dir.path()).unwrap());
        let mut handles = Vec::new();
        for i in 0..10 {
            let l = log.clone();
            handles.push(std::thread::spawn(move || {
                l.log("action", None, &format!("detail-{i}"), "test").unwrap();
            }));
        }
        for h in handles { h.join().unwrap(); }
        assert_eq!(log.count(None).unwrap(), 10);
    }

    #[test]
    fn special_chars_in_detail() {
        let dir = TempDir::new().unwrap();
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        log.log("search", None, "query='hello \"world\"' & <tag>", "gui").unwrap();
        let entries = log.query(None, None, None, 1, 0).unwrap();
        assert!(entries[0].detail.contains("\"world\""));
        assert!(entries[0].detail.contains("<tag>"));
    }

    #[test]
    fn reopen_preserves_data() {
        let dir = TempDir::new().unwrap();
        {
            let log = AuditLog::open_or_create(dir.path()).unwrap();
            log.log("a", None, "1", "gui").unwrap();
            log.log("b", None, "2", "cli").unwrap();
        }
        // Reopen
        let log = AuditLog::open_or_create(dir.path()).unwrap();
        assert_eq!(log.count(None).unwrap(), 2);
    }
}
