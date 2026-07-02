//! Retention policies (P25.3).
//!
//! Per-folder or per-tag rules that automatically archive or delete
//! documents after a configured time period.  Rules are stored in a
//! WAL-mode SQLite database.  A background worker (daily check) applies
//! the rules by updating `doc_status` in the LanceDB index.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS retention_rules (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    name               TEXT    NOT NULL,
    -- Target: a folder prefix (e.g. '/home/user/Contracts/') or a tag.
    match_type         TEXT    NOT NULL DEFAULT 'folder', -- 'folder' or 'tag'
    match_value        TEXT    NOT NULL,
    -- Days after indexing to change status.
    archive_after_days INTEGER,          -- set doc_status = 'archived'
    delete_after_days  INTEGER,          -- remove from index entirely
    enabled            INTEGER NOT NULL DEFAULT 1,
    created_at         INTEGER NOT NULL
);
";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionRule {
    pub id: i64,
    pub name: String,
    pub match_type: String,
    pub match_value: String,
    pub archive_after_days: Option<i64>,
    pub delete_after_days: Option<i64>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionAction {
    pub doc_id: String,
    pub action: String, // "archive" or "delete"
    pub rule_name: String,
}

#[derive(Clone)]
pub struct RetentionStore {
    conn: Arc<Mutex<Connection>>,
}

impl RetentionStore {
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("retention.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening retention DB {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA).context("creating retention schema")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn add_rule(
        &self,
        name: &str,
        match_type: &str,
        match_value: &str,
        archive_after_days: Option<i64>,
        delete_after_days: Option<i64>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = now_ms();
        conn.execute(
            "INSERT INTO retention_rules (name, match_type, match_value, archive_after_days, delete_after_days, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![name, match_type, match_value, archive_after_days, delete_after_days, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_rules(&self) -> Result<Vec<RetentionRule>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, match_type, match_value, archive_after_days, delete_after_days, enabled FROM retention_rules ORDER BY id"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RetentionRule {
                id: row.get(0)?,
                name: row.get(1)?,
                match_type: row.get(2)?,
                match_value: row.get(3)?,
                archive_after_days: row.get(4)?,
                delete_after_days: row.get(5)?,
                enabled: row.get::<_, i64>(6)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn delete_rule(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM retention_rules WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_rule_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE retention_rules SET enabled = ?1 WHERE id = ?2",
            params![enabled as i64, id],
        )?;
        Ok(())
    }

    /// Evaluate all enabled rules and return a list of actions to take.
    /// The caller must apply these actions against the LanceDB index
    /// (set doc_status or delete).
    ///
    /// `docs` should be an iterator of (doc_id, location_uri, tags, indexed_at_ms).
    pub fn evaluate_rules(
        &self,
        docs: &[(String, String, Vec<String>, i64)],
    ) -> Result<Vec<RetentionAction>> {
        let rules = self.list_rules()?;
        let now = now_ms();
        let day_ms: i64 = 86_400_000;
        let mut actions = Vec::new();

        for (doc_id, location_uri, tags, indexed_at) in docs {
            let age_days = (now - indexed_at) / day_ms;

            for rule in &rules {
                if !rule.enabled { continue; }

                let matches = match rule.match_type.as_str() {
                    "folder" => location_uri.contains(&rule.match_value),
                    "tag" => tags.iter().any(|t| t == &rule.match_value),
                    _ => false,
                };
                if !matches { continue; }

                // Delete takes priority over archive
                if let Some(del_days) = rule.delete_after_days {
                    if age_days >= del_days {
                        actions.push(RetentionAction {
                            doc_id: doc_id.clone(),
                            action: "delete".into(),
                            rule_name: rule.name.clone(),
                        });
                        break; // One action per doc
                    }
                }
                if let Some(arc_days) = rule.archive_after_days {
                    if age_days >= arc_days {
                        actions.push(RetentionAction {
                            doc_id: doc_id.clone(),
                            action: "archive".into(),
                            rule_name: rule.name.clone(),
                        });
                        break;
                    }
                }
            }
        }
        Ok(actions)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Tauri commands ─────────────────────────────────────────────────────

pub mod tauri_commands {
    use super::*;
    use tauri::State;
    use crate::AppState;

    async fn get_store(state: &State<'_, AppState>) -> Result<RetentionStore, String> {
        let data_dir = state.data_dir.lock().await;
        let dir = data_dir.as_ref().ok_or("App data dir not set")?;
        RetentionStore::open_or_create(dir).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn retention_add_rule(
        state: State<'_, AppState>,
        name: String,
        match_type: String,
        match_value: String,
        archive_after_days: Option<i64>,
        delete_after_days: Option<i64>,
    ) -> Result<i64, String> {
        let store = get_store(&state).await?;
        store.add_rule(&name, &match_type, &match_value, archive_after_days, delete_after_days)
            .map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn retention_list_rules(
        state: State<'_, AppState>,
    ) -> Result<Vec<RetentionRule>, String> {
        let store = get_store(&state).await?;
        store.list_rules().map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn retention_delete_rule(
        state: State<'_, AppState>,
        id: i64,
    ) -> Result<(), String> {
        let store = get_store(&state).await?;
        store.delete_rule(id).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn retention_set_enabled(
        state: State<'_, AppState>,
        id: i64,
        enabled: bool,
    ) -> Result<(), String> {
        let store = get_store(&state).await?;
        store.set_rule_enabled(id, enabled).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rule_crud() {
        let dir = TempDir::new().unwrap();
        let store = RetentionStore::open_or_create(dir.path()).unwrap();

        let id = store.add_rule("Archive old", "folder", "/archive/", Some(90), None).unwrap();
        assert!(id > 0);

        let rules = store.list_rules().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "Archive old");
        assert!(rules[0].enabled);

        store.set_rule_enabled(id, false).unwrap();
        let rules = store.list_rules().unwrap();
        assert!(!rules[0].enabled);

        store.delete_rule(id).unwrap();
        assert_eq!(store.list_rules().unwrap().len(), 0);
    }

    #[test]
    fn evaluate_archive() {
        let dir = TempDir::new().unwrap();
        let store = RetentionStore::open_or_create(dir.path()).unwrap();
        store.add_rule("Archive contracts", "folder", "/contracts/", Some(30), None).unwrap();

        let now = now_ms();
        let old = now - 31 * 86_400_000; // 31 days ago
        let recent = now - 5 * 86_400_000; // 5 days ago

        let docs = vec![
            ("d1".into(), "/contracts/deal.pdf".into(), vec![], old),
            ("d2".into(), "/contracts/new.pdf".into(), vec![], recent),
            ("d3".into(), "/other/doc.pdf".into(), vec![], old),
        ];
        let actions = store.evaluate_rules(&docs).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].doc_id, "d1");
        assert_eq!(actions[0].action, "archive");
    }

    #[test]
    fn evaluate_delete_by_tag() {
        let dir = TempDir::new().unwrap();
        let store = RetentionStore::open_or_create(dir.path()).unwrap();
        store.add_rule("Delete temp", "tag", "temporary", None, Some(7)).unwrap();

        let now = now_ms();
        let old = now - 8 * 86_400_000;

        let docs = vec![
            ("d1".into(), "/path".into(), vec!["temporary".into()], old),
            ("d2".into(), "/path".into(), vec!["important".into()], old),
        ];
        let actions = store.evaluate_rules(&docs).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].doc_id, "d1");
        assert_eq!(actions[0].action, "delete");
    }
}
