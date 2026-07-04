//! Zoned OCR template store (P26.4).
//!
//! User-defined extraction templates: named rectangles on a reference
//! page.  Each zone has a label ("invoice_number", "total_amount") and
//! normalised coordinates (0.0–1.0 fraction of page width/height) so
//! the same template works across DPI variants.
//!
//! Storage: WAL-mode SQLite `templates.db` (same pattern as audit,
//! retention, annotations).

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS templates (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL UNIQUE,
    width      INTEGER NOT NULL DEFAULT 0,  -- reference page width (px)
    height     INTEGER NOT NULL DEFAULT 0,  -- reference page height (px)
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS template_zones (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    template_id INTEGER NOT NULL REFERENCES templates(id) ON DELETE CASCADE,
    label       TEXT    NOT NULL,
    x           REAL    NOT NULL,  -- normalised 0.0–1.0
    y           REAL    NOT NULL,
    w           REAL    NOT NULL,
    h           REAL    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tz_template ON template_zones(template_id);
";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    pub id: i64,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub id: i64,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub zones: Vec<Zone>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateSummary {
    pub id: i64,
    pub name: String,
    pub zone_count: usize,
}

#[derive(Clone)]
pub struct TemplateStore {
    conn: Arc<Mutex<Connection>>,
}

impl TemplateStore {
    pub fn open_or_create(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("templates.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening templates DB {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA).context("creating templates schema")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Create a new template. Returns its id.
    pub fn create_template(&self, name: &str, width: u32, height: u32) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let ts = now_ms();
        conn.execute(
            "INSERT INTO templates (name, width, height, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, width, height, ts],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Add a zone to a template. Returns the zone id.
    pub fn add_zone(
        &self,
        template_id: i64,
        label: &str,
        x: f64, y: f64, w: f64, h: f64,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO template_zones (template_id, label, x, y, w, h) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![template_id, label, x, y, w, h],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get a template with all its zones.
    pub fn get_template(&self, id: i64) -> Result<Option<Template>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, width, height FROM templates WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, u32>(3)?,
            ))
        })?;
        let Some(Ok((tid, name, width, height))) = rows.next() else {
            return Ok(None);
        };

        let zones = self.get_zones_locked(&conn, tid)?;
        Ok(Some(Template { id: tid, name, width, height, zones }))
    }

    /// Get a template by name.
    pub fn get_template_by_name(&self, name: &str) -> Result<Option<Template>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, width, height FROM templates WHERE name = ?1"
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
                row.get::<_, u32>(3)?,
            ))
        })?;
        let Some(Ok((tid, name, width, height))) = rows.next() else {
            return Ok(None);
        };

        let zones = self.get_zones_locked(&conn, tid)?;
        Ok(Some(Template { id: tid, name, width, height, zones }))
    }

    fn get_zones_locked(&self, conn: &Connection, template_id: i64) -> Result<Vec<Zone>> {
        let mut stmt = conn.prepare(
            "SELECT id, label, x, y, w, h FROM template_zones WHERE template_id = ?1 ORDER BY id"
        )?;
        let zones = stmt.query_map(params![template_id], |row| {
            Ok(Zone {
                id: row.get(0)?,
                label: row.get(1)?,
                x: row.get(2)?,
                y: row.get(3)?,
                w: row.get(4)?,
                h: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(zones)
    }

    /// List all templates with zone counts.
    pub fn list_templates(&self) -> Result<Vec<TemplateSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT t.id, t.name, COUNT(z.id) FROM templates t \
             LEFT JOIN template_zones z ON z.template_id = t.id \
             GROUP BY t.id ORDER BY t.name"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TemplateSummary {
                id: row.get(0)?,
                name: row.get(1)?,
                zone_count: row.get::<_, i64>(2)? as usize,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Delete a template and all its zones (cascade).
    pub fn delete_template(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // Foreign key cascade deletes zones
        conn.execute("DELETE FROM templates WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Delete a single zone.
    pub fn delete_zone(&self, zone_id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM template_zones WHERE id = ?1", params![zone_id])?;
        Ok(())
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
    fn create_and_get_template() {
        let dir = TempDir::new().unwrap();
        let store = TemplateStore::open_or_create(dir.path()).unwrap();
        let id = store.create_template("Invoice A", 2480, 3508).unwrap();
        store.add_zone(id, "invoice_number", 0.1, 0.05, 0.3, 0.04).unwrap();
        store.add_zone(id, "total_amount", 0.6, 0.8, 0.3, 0.04).unwrap();

        let t = store.get_template(id).unwrap().unwrap();
        assert_eq!(t.name, "Invoice A");
        assert_eq!(t.width, 2480);
        assert_eq!(t.zones.len(), 2);
        assert_eq!(t.zones[0].label, "invoice_number");
        assert_eq!(t.zones[1].label, "total_amount");
    }

    #[test]
    fn list_templates_with_counts() {
        let dir = TempDir::new().unwrap();
        let store = TemplateStore::open_or_create(dir.path()).unwrap();
        let id1 = store.create_template("A", 100, 100).unwrap();
        let id2 = store.create_template("B", 100, 100).unwrap();
        store.add_zone(id1, "f1", 0.0, 0.0, 0.5, 0.5).unwrap();
        store.add_zone(id1, "f2", 0.5, 0.5, 0.5, 0.5).unwrap();
        // B has no zones

        let list = store.list_templates().unwrap();
        assert_eq!(list.len(), 2);
        let a = list.iter().find(|t| t.name == "A").unwrap();
        let b = list.iter().find(|t| t.name == "B").unwrap();
        assert_eq!(a.zone_count, 2);
        assert_eq!(b.zone_count, 0);
    }

    #[test]
    fn delete_template_cascades_zones() {
        let dir = TempDir::new().unwrap();
        let store = TemplateStore::open_or_create(dir.path()).unwrap();
        let id = store.create_template("X", 100, 100).unwrap();
        store.add_zone(id, "z1", 0.0, 0.0, 1.0, 1.0).unwrap();
        store.delete_template(id).unwrap();
        assert!(store.get_template(id).unwrap().is_none());
        assert!(store.list_templates().unwrap().is_empty());
    }

    #[test]
    fn duplicate_name_fails() {
        let dir = TempDir::new().unwrap();
        let store = TemplateStore::open_or_create(dir.path()).unwrap();
        store.create_template("Dup", 100, 100).unwrap();
        assert!(store.create_template("Dup", 100, 100).is_err());
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let dir = TempDir::new().unwrap();
        let store = TemplateStore::open_or_create(dir.path()).unwrap();
        assert!(store.get_template(999).unwrap().is_none());
        assert!(store.get_template_by_name("nope").unwrap().is_none());
    }

    #[test]
    fn delete_single_zone() {
        let dir = TempDir::new().unwrap();
        let store = TemplateStore::open_or_create(dir.path()).unwrap();
        let tid = store.create_template("T", 100, 100).unwrap();
        let z1 = store.add_zone(tid, "a", 0.0, 0.0, 0.5, 0.5).unwrap();
        store.add_zone(tid, "b", 0.5, 0.5, 0.5, 0.5).unwrap();
        store.delete_zone(z1).unwrap();
        let t = store.get_template(tid).unwrap().unwrap();
        assert_eq!(t.zones.len(), 1);
        assert_eq!(t.zones[0].label, "b");
    }

    #[test]
    fn normalised_coords_stored_exactly() {
        let dir = TempDir::new().unwrap();
        let store = TemplateStore::open_or_create(dir.path()).unwrap();
        let tid = store.create_template("T", 100, 100).unwrap();
        store.add_zone(tid, "precise", 0.123456, 0.654321, 0.111111, 0.222222).unwrap();
        let t = store.get_template(tid).unwrap().unwrap();
        let z = &t.zones[0];
        assert!((z.x - 0.123456).abs() < 1e-10);
        assert!((z.y - 0.654321).abs() < 1e-10);
        assert!((z.w - 0.111111).abs() < 1e-10);
        assert!((z.h - 0.222222).abs() < 1e-10);
    }

    #[test]
    fn reopen_preserves_data() {
        let dir = TempDir::new().unwrap();
        {
            let store = TemplateStore::open_or_create(dir.path()).unwrap();
            let tid = store.create_template("Persist", 200, 300).unwrap();
            store.add_zone(tid, "field", 0.1, 0.2, 0.3, 0.4).unwrap();
        }
        let store = TemplateStore::open_or_create(dir.path()).unwrap();
        let list = store.list_templates().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Persist");
        assert_eq!(list[0].zone_count, 1);
    }
}
