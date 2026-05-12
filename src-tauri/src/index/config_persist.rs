//! On-disk persistence for [`crate::index::IndexConfig`] (P13.5
//! follow-up).
//!
//! Mirrors the CrispLens settings pattern at
//! [`crate::images::crisplens::settings`] — JSON-on-disk in the
//! app data dir, default-on-load when the file is missing, best-
//! effort on save (errors logged but not propagated to UI).
//!
//! Loaded in the Tauri setup hook so the first `index_get_config`
//! call returns the persisted shape before the user touches
//! Settings.  Saved in `index_set_config` after the in-memory
//! `AppState.index.config` update.

use crate::index::IndexConfig;
use std::path::{Path, PathBuf};

/// Filesystem location of the index-config JSON.  Sits next to
/// `crisplens.settings.json`, `crisp_jobs.db`, and the migration
/// ledger — all are admin metadata that lives at the data-dir
/// root rather than inside `lance/` or `fts/`.
pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("index_config.json")
}

/// Read the config from disk.  Returns `IndexConfig::default()` when:
/// - the file doesn't exist (fresh install — every existing
///   user before this commit lands counts);
/// - the file is unreadable / corrupt (caller still gets a working
///   default rather than a startup panic).
///
/// Either case is intentionally silent — UI shouldn't show a
/// scary "config load failed" toast on first launch.  A loaded
/// IndexConfig is the only positive evidence; a default one is
/// the no-news state.
pub fn load(data_dir: &Path) -> IndexConfig {
    let path = settings_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => IndexConfig::default(),
    }
}

/// Persist the config to disk.  Creates the data dir if missing.
/// Errors propagate so the `index_set_config` Tauri command can
/// log them — but it doesn't fail the command, because the in-
/// memory state is already updated and the user's next call will
/// see the change.
pub fn save(data_dir: &Path, config: &IndexConfig) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(data_dir), json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh tempdir → load returns Default::default(); no panic.
    #[test]
    fn load_missing_returns_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = load(tmp.path());
        // Default config has enabled=false, mode=Local, no translate_to.
        assert!(!cfg.enabled);
        assert!(cfg.translate_to.is_none());
    }

    /// Save → load round-trip preserves the new translate_to field.
    /// Drift guard: a future serde rename / case change on the
    /// IndexConfig fields breaks this immediately.
    #[test]
    fn save_then_load_round_trips_translate_to() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = IndexConfig::default();
        cfg.translate_to = Some("en".to_owned());
        cfg.enabled = true;
        save(tmp.path(), &cfg).expect("save");
        let loaded = load(tmp.path());
        assert_eq!(loaded.translate_to.as_deref(), Some("en"));
        assert!(loaded.enabled);
    }

    /// Corrupt JSON on disk falls back to default rather than
    /// crashing the setup hook.  Matters because users with old
    /// indices that have schema-incompatible config files
    /// shouldn't get locked out — they can re-set in Settings.
    #[test]
    fn load_corrupt_file_returns_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(settings_path(tmp.path()), b"{not valid json").unwrap();
        let cfg = load(tmp.path());
        // Falls back to Default — the corrupted file is implicitly
        // overwritten on the next save.
        assert!(!cfg.enabled);
    }

    /// Save creates the data dir even if it didn't exist.
    /// First-launch case: the user's app-data dir might not be
    /// pre-created on Linux.
    #[test]
    fn save_creates_data_dir_when_missing() {
        let parent = tempfile::TempDir::new().unwrap();
        let dd = parent.path().join("not-yet-created");
        let cfg = IndexConfig::default();
        save(&dd, &cfg).expect("save creates dir");
        assert!(dd.exists());
        assert!(settings_path(&dd).exists());
    }
}
