//! P13/B1 — settings for the CrispLens Tier 2 backend.
//!
//! What lives WHERE:
//!
//! * `tauri-plugin-store` (`crisplens.settings.json`):
//!     * `backend`: which tier — `"local"` (Tier 1) or `"crisplens"`
//!       (Tier 2).
//!     * `url`: CrispLens base URL (no trailing slash).
//!     * `thumbnail_size_px`, `phash_threshold`: UI tuning.
//!   Everything in here is NON-SECRET.  Backups + cloud-sync include
//!   this file — that's fine.
//!
//! * OS-native secret store (`crate::images::crisplens::secret`):
//!     * The session cookie value, keyed by URL.
//!   Per the spec's risk register, the credential MUST stay out of
//!   the JSON store.  This module never reads or writes secrets;
//!   it only carries the URL / mode flags.
//!
//! Persistence format is JSON via `serde_json`; we don't pull in
//! the Tauri store plugin's Rust API directly because:
//!   1. The plugin is mainly a JS-side wrapper around a JSON file.
//!   2. Going through the file directly keeps the CLI happy — the
//!      CLI runs outside the Tauri runtime and can't talk to the
//!      JS plugin.
//!   3. The store lives at the same `data_dir` resolution the CLI
//!      already does (`cli::mod::resolve_data_dir`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which backend the Images vertical talks to.  Stored verbatim in
/// the settings JSON so the frontend's dropdown can round-trip it
/// without case-mangling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesBackend {
    /// Tier 1 — local-only.  Default for every fresh install.
    Local,
    /// Tier 2 — HTTP client against CrispLens (v2 or v4).
    CrispLens,
}

impl Default for ImagesBackend {
    fn default() -> Self {
        ImagesBackend::Local
    }
}

/// Settings payload.  Everything `#[serde(default)]` so older
/// settings JSON migrates transparently when a field is added.
///
/// All fields except `backend` and `url` have sane defaults that
/// match what the A1-A4 Tier 1 code uses, so even a never-touched
/// settings file produces the same behaviour the user already gets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImagesSettings {
    pub backend: ImagesBackend,
    /// CrispLens base URL (e.g. `"https://crisplens.example.com"`).
    /// Empty string means "not configured" — the UI treats this as
    /// equivalent to `backend == Local` regardless of the dropdown.
    pub url: String,
    /// Default 256.  Tier 1 thumbnail generator's `--size`.
    pub thumbnail_size_px: u32,
    /// Default 8.  Tier 1 near-dup grouping threshold.
    pub phash_threshold: u32,
}

impl Default for ImagesSettings {
    fn default() -> Self {
        ImagesSettings {
            backend: ImagesBackend::default(),
            url: String::new(),
            thumbnail_size_px: 256,
            phash_threshold: 8,
        }
    }
}

impl ImagesSettings {
    /// Returns `true` when Tier 2 is configured and addressable.
    /// Doesn't ping anything — the auto-degradation monitor (slice
    /// B4) is what decides whether Tier 2 is currently reachable.
    pub fn tier2_enabled(&self) -> bool {
        matches!(self.backend, ImagesBackend::CrispLens) && !self.url.trim().is_empty()
    }

    /// Normalised URL — trailing slash stripped, surrounding whitespace
    /// trimmed.  The HTTP client (B2+) concatenates relative paths
    /// against this, so a stable shape matters.
    pub fn normalised_url(&self) -> &str {
        let s = self.url.trim();
        s.strip_suffix('/').unwrap_or(s)
    }
}

/// Filesystem location of the settings JSON inside the app data dir.
pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("crisplens.settings.json")
}

/// Read settings from disk.  Returns the default settings if the
/// file doesn't exist yet (fresh install) — callers don't need to
/// branch on first-launch.
pub fn load(data_dir: &Path) -> ImagesSettings {
    let path = settings_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => ImagesSettings::default(),
    }
}

/// Persist settings to disk.  Creates the data dir if needed.
pub fn save(data_dir: &Path, settings: &ImagesSettings) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(data_dir), json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_settings_are_tier1_with_sane_defaults() {
        let s = ImagesSettings::default();
        assert_eq!(s.backend, ImagesBackend::Local);
        assert!(s.url.is_empty());
        assert_eq!(s.thumbnail_size_px, 256);
        assert_eq!(s.phash_threshold, 8);
        assert!(!s.tier2_enabled(), "default settings should NOT enable Tier 2");
    }

    #[test]
    fn tier2_enabled_requires_both_backend_and_nonempty_url() {
        // Just selecting CrispLens isn't enough — URL is the
        // addressability signal.  Mirrors the spec's "When
        // `bilderBackend = CrispLens` but the URL is empty or
        // unreachable, the UI degrades to Tier 1 silently."
        let mut s = ImagesSettings::default();
        s.backend = ImagesBackend::CrispLens;
        assert!(!s.tier2_enabled(), "no URL — not enabled");
        s.url = "   ".to_owned(); // whitespace only
        assert!(!s.tier2_enabled(), "whitespace-only URL — not enabled");
        s.url = "https://crisplens.example.com".to_owned();
        assert!(s.tier2_enabled());
    }

    #[test]
    fn normalised_url_strips_trailing_slash_and_whitespace() {
        let s = ImagesSettings {
            url: "  https://x.example/  ".to_owned(),
            ..Default::default()
        };
        assert_eq!(s.normalised_url(), "https://x.example");

        let s2 = ImagesSettings {
            url: "https://x.example".to_owned(),
            ..Default::default()
        };
        assert_eq!(s2.normalised_url(), "https://x.example");
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = TempDir::new().unwrap();
        let s = ImagesSettings {
            backend: ImagesBackend::CrispLens,
            url: "https://crisplens.example.com".to_owned(),
            thumbnail_size_px: 512,
            phash_threshold: 12,
        };
        save(tmp.path(), &s).unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded, s);
    }

    #[test]
    fn load_returns_default_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded, ImagesSettings::default());
    }

    #[test]
    fn load_returns_default_when_file_is_garbage() {
        let tmp = TempDir::new().unwrap();
        let p = settings_path(tmp.path());
        std::fs::write(&p, "{ this is not valid json").unwrap();
        // Garbage on disk shouldn't crash the app — silently fall
        // back to default settings.  The next save will overwrite.
        let loaded = load(tmp.path());
        assert_eq!(loaded, ImagesSettings::default());
    }

    #[test]
    fn settings_json_uses_camelcase_field_names() {
        // Frontend bindings live on the Svelte side and expect
        // camelCase.  Pin here so a future #[serde(rename_all = ...)]
        // typo can't silently break the UI form.
        let s = ImagesSettings {
            backend: ImagesBackend::CrispLens,
            url: "https://x".into(),
            thumbnail_size_px: 256,
            phash_threshold: 8,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"thumbnailSizePx\":256"), "got {json}");
        assert!(json.contains("\"phashThreshold\":8"),   "got {json}");
        assert!(json.contains("\"backend\":\"crisplens\""), "got {json}");
    }

    #[test]
    fn settings_json_migrates_when_fields_added() {
        // An older settings file with only `backend` + `url` MUST
        // still load — defaults fill the rest.  Pins the
        // forward-compat invariant.
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            settings_path(tmp.path()),
            r#"{"backend":"local","url":""}"#,
        ).unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded.backend, ImagesBackend::Local);
        assert_eq!(loaded.thumbnail_size_px, 256);
        assert_eq!(loaded.phash_threshold, 8);
    }
}
