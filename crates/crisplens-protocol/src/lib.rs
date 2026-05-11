//! Wire types shared between CrispSorter (Tier 2 client) and the
//! sibling CrispLens server.
//!
//! ## Source of truth (NOT the spec)
//!
//! `docs/P13_Bilder_integration.md` sketched a protocol shape based
//! on an *intended* OAuth2/JWT flow and an *intended* `{ items,
//! total, page, page_size }` envelope.  The real CrispLens HTTP
//! surface — both `routers/*.py` (v2, FastAPI) and
//! `electron-app-v4/server/routes/*.js` (v4, Express) — diverges in
//! material ways:
//!
//! * Auth is **httpOnly session cookies**, not bearer JWT.  Login
//!   returns `{ ok, username, role, token? }`.  v2 echoes the cookie
//!   value in the body (`token`); v4 only sets `Set-Cookie`.
//! * Image rows use `filepath` / `file_size` (snake_case throughout)
//!   and include a rich set of fields the spec didn't anticipate
//!   (`ai_description`, `ai_scene_type`, `ai_tags`, `star_rating`,
//!   etc.).  Neither version returns `sha256`, `phash`, `gps_lat/lon`
//!   at the list endpoint.
//! * v4 wraps lists in `{ images: [...], total }`.  v2 returns a bare
//!   `[...]` array.  The HTTP-client adapter (slice B2+) normalises
//!   v2 into the v4 envelope before deserialising into these types.
//! * v4 health: `{ ok, version, backend }`.  v2 health:
//!   `{ ok, model_ready, nc_license_accepted, thumb_cache }`.
//!   Common shape: `{ ok }`.
//!
//! This crate models the **actual** v4 wire shape with serde aliases
//! and permissive defaults for v2 quirks.  The unit tests pin both
//! v2-flavoured and v4-flavoured JSON payloads against the same Rust
//! types, so any future v5 schema drift surfaces here as a failing
//! deserialise rather than as silent UI bugs.
//!
//! ## What this crate covers (slice B1 scope)
//!
//! * Auth: `LoginRequest`, `LoginResponse`, `LogoutResponse`, `MeResponse`.
//! * Health: `HealthResponse`.
//! * Image list response envelope: `Image`, `ImagesListResponse`.
//! * Generic error body: `ErrorResponse`.
//!
//! ## Deferred to later slices
//!
//! Face / Person / SearchHit shapes — each is verified against the
//! live route code when its consumer slice (B2 semantic search, B3
//! faces subtab) lands, so we don't lock in a wrong wire shape we'd
//! later have to migrate.
//!
//! Keep this crate **pure-Rust + serde-only**: no async, no HTTP, no
//! Tauri.  The HTTP client itself (with Tauri-runtime concerns +
//! cookie persistence + authentication retry) lives in
//! `src-tauri/src/images/crisplens/` and depends on this crate.

use serde::{Deserialize, Serialize};

// ── Authentication ───────────────────────────────────────────────────────

/// `POST /api/auth/login` request body — matches both v2's
/// `LoginRequest(BaseModel)` and v4's destructure of `req.body`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// `POST /api/auth/login` success response.  The cookie is set via
/// `Set-Cookie: session=<value>` by the server in both versions —
/// what differs is whether the cookie value also appears in the JSON
/// body:
///
/// * v2 (FastAPI): `{ ok, username, role, token }` — `token` is the
///   cookie value.
/// * v4 (Express): `{ ok, username, role }` — cookie-only; the
///   client must read `Set-Cookie`.
///
/// Both shapes deserialise into this type cleanly.  The HTTP-client
/// wrapper falls back to the `Set-Cookie` header when `token` is
/// `None`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoginResponse {
    pub ok:       bool,
    pub username: String,
    pub role:     String,
    /// Session-cookie value, only present in v2.  When `None`, the
    /// client reads the `Set-Cookie` header from the HTTP response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token:    Option<String>,
}

/// `POST /api/auth/logout` body.  Both versions return `{ ok: true }`
/// with the cookie cleared via `Set-Cookie: session=; Max-Age=0`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogoutResponse {
    pub ok: bool,
}

/// `GET /api/auth/me` response — used on app launch to validate a
/// stored session cookie.
///
/// * v2: `{ username, role, is_active }`
/// * v4: `{ username, role }`
///
/// `is_active` defaults to `true` when absent so v4 payloads
/// deserialise cleanly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeResponse {
    pub username:  String,
    pub role:      String,
    #[serde(default = "default_active")]
    pub is_active: bool,
}

fn default_active() -> bool {
    true
}

// ── Health ───────────────────────────────────────────────────────────────

/// `GET /api/health` response.  Both versions emit `ok: bool` as the
/// authoritative liveness signal; everything else is permissive.
///
/// * v4: `{ ok, version, backend }`
/// * v2: `{ ok, model_ready, nc_license_accepted, thumb_cache }`
///
/// We surface `version` + `backend` (the v4 fields) for the
/// degradation banner copy, plus `model_ready` (v2-only) so the UI
/// can show "warming up" when CrispLens is starting cold.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthResponse {
    pub ok: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
    /// v2-only signal — `Some(false)` means the engine is still
    /// loading models.  `None` on v4 (always ready).
    #[serde(default)]
    pub model_ready: Option<bool>,
}

// ── Images ───────────────────────────────────────────────────────────────

/// One image row as CrispLens returns it from `GET /api/images` and
/// related endpoints.  Field names match v4's `rowToApi`; v2's
/// `image_ops.browse_images_filtered` returns an overlapping subset
/// (no `format`, no `description`, no `flag` — only `color_flag`).
///
/// `#[serde(default)]` everywhere, plus `#[serde(alias = ...)]` for
/// v2-flavoured field names.  A v2 payload's `star_rating` lands in
/// `rating`; v4's `color_flag` and the JS-side alias `flag` both
/// land in `flag`.
///
/// Notably absent because neither backend returns them at the list
/// endpoint: `sha256`, `phash`, `gps_lat`, `gps_lon`, raw `exif`
/// blob.  CrispSorter cross-references its local rows by filename
/// + dimensions + `taken_at` until the backends grow these fields
/// (or until B5 promotes a separate `/api/images/{id}/details`
/// hit-per-image call).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Image {
    pub id:       i64,
    pub filename: String,
    /// Server-local path on the CrispLens host.  v4: `filepath`;
    /// v2: same.  `server_path` is a v2-compat alias.
    pub filepath: String,
    /// Pre-cloud-sync local path on the user's original machine.
    /// `None` on items that were uploaded straight to the server.
    #[serde(default)]
    pub local_path: Option<String>,

    #[serde(default)]
    pub file_size: Option<i64>,
    #[serde(default)]
    pub width:     Option<i32>,
    #[serde(default)]
    pub height:    Option<i32>,
    /// v4 only.  v2 doesn't surface this column at the list endpoint.
    #[serde(default)]
    pub format:    Option<String>,

    /// ISO-8601 from EXIF DateTimeOriginal (no timezone — local time
    /// at the camera).
    #[serde(default)]
    pub taken_at:     Option<String>,
    pub created_at:   String,
    #[serde(default)]
    pub processed_at: Option<String>,

    /// AI-derived fields.  Both v2 and v4 include them; null on rows
    /// that haven't gone through the LLM scene/tag pipeline yet.
    #[serde(default)]
    pub ai_description: Option<String>,
    #[serde(default)]
    pub ai_scene_type:  Option<String>,
    /// Pre-split tag list.  Both v2 and v4 emit two fields: a raw
    /// `ai_tags` (comma-joined String, or `null`) AND a parsed
    /// `ai_tags_list` (`Vec<String>`).  We bind to the parsed list
    /// only — the raw string version is dropped silently by serde's
    /// unknown-field handling, which is what we want (no need to
    /// split it client-side when the server already did).
    #[serde(default, rename = "ai_tags_list")]
    pub ai_tags: Vec<String>,

    /// 0..=5 star rating.  v4 emits both `rating` AND `star_rating`
    /// (v2-compat alias); we bind only to `rating`.  v2 raw payloads
    /// emit `star_rating` exclusively — the HTTP-client adapter
    /// (slice B2) renames it to `rating` before passing to serde.
    /// Serde's `alias` attribute doesn't work here because both
    /// fields are present in v4 payloads and serde rejects them as
    /// duplicates.
    #[serde(default)]
    pub rating: Option<i32>,
    /// Colour flag (e.g. "red" / "yellow" / "green").  Same v2/v4
    /// duplication story as `rating` — bind to v4's canonical
    /// `flag`, leave the rename to the HTTP adapter.
    #[serde(default)]
    pub flag: Option<String>,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub face_count: Option<i32>,

    /// Permission model — `"private"` | `"shared"` | `"public"`.
    /// v4 default is `"shared"`.
    #[serde(default)]
    pub visibility: Option<String>,
}

/// `GET /api/images` response envelope.
///
/// v4 emits this shape directly: `{ images: [...], total }`.  v2
/// returns a bare `[...]` array — the HTTP-client adapter (slice B2)
/// is expected to wrap it before deserialise so callers see one
/// uniform shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImagesListResponse {
    pub images: Vec<Image>,
    #[serde(default)]
    pub total:  Option<i64>,
}

// ── Watchfolders (B5) ────────────────────────────────────────────────────

/// One row from CrispLens's `watch_folders` SQLite table.  Both v2
/// and v4 return objects from `SELECT * FROM watch_folders` directly,
/// and that bypasses the usual serialisation contract: booleans come
/// through as SQLite ints (`recursive: 1`, `auto_scan: 0`), and
/// `scan_interval_hours` ends up as a float (`24.0`) because v2's
/// migration column is `REAL`.  Verified live against
/// `https://<crisplens-host>`.
///
/// Rather than push the int-vs-bool / int-vs-float / unit
/// (`scan_interval` seconds vs `scan_interval_hours` hours) noise
/// into the HTTP-client adapter, we use `serde_json::Value` for the
/// version-skewed fields.  CrispSorter only branches on `path` for
/// the slice-B5 cross-reference feature; the rest is informational.
/// If a future slice needs typed access to `recursive` etc., promote
/// them then — the adapter will know which backend it's talking to
/// by then.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchFolder {
    #[serde(default)]
    pub id:   Option<i64>,
    pub path: String,
    #[serde(default)]
    pub recursive: Option<serde_json::Value>,
    #[serde(default)]
    pub auto_scan: Option<serde_json::Value>,
    /// v4 emits `scan_interval` (seconds); v2 emits
    /// `scan_interval_hours` (hours, often as a float).  Aliased so
    /// either backend's raw row drops into the same field; type is
    /// `Value` because v2 uses REAL and v4 uses INTEGER.
    #[serde(default, alias = "scan_interval_hours")]
    pub scan_interval: Option<serde_json::Value>,
    #[serde(default)]
    pub enabled: Option<serde_json::Value>,
}

impl WatchFolder {
    /// `recursive` flag normalised to a `bool` regardless of wire
    /// shape (v2/v4 emit SQLite ints; future Rust port might emit
    /// proper booleans).  Returns `None` when the field is absent.
    pub fn recursive_bool(&self) -> Option<bool> {
        coerce_bool(self.recursive.as_ref())
    }

    pub fn auto_scan_bool(&self) -> Option<bool> {
        coerce_bool(self.auto_scan.as_ref())
    }

    pub fn enabled_bool(&self) -> Option<bool> {
        coerce_bool(self.enabled.as_ref())
    }
}

fn coerce_bool(v: Option<&serde_json::Value>) -> Option<bool> {
    let v = v?;
    if let Some(b) = v.as_bool() {
        return Some(b);
    }
    if let Some(i) = v.as_i64() {
        return Some(i != 0);
    }
    if let Some(f) = v.as_f64() {
        return Some(f != 0.0);
    }
    None
}

// ── Errors ───────────────────────────────────────────────────────────────

/// FastAPI / Express error body.  Both versions return `{ detail }`
/// for 4xx + 5xx — surfaced verbatim by the UI so the user sees the
/// server's reason rather than a generic "request failed".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorResponse {
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(value: &T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
    {
        let json = serde_json::to_string(value).expect("serialise");
        let parsed: T = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(value, &parsed, "round-trip mismatch via JSON: {json}");
    }

    // ── Auth ────────────────────────────────────────────────────────────

    #[test]
    fn login_request_serialises_snake_case_fields() {
        // Both v2 (Pydantic BaseModel) and v4 (req.body destructure)
        // expect literal `username` / `password`.  A future
        // accidental `#[serde(rename_all = "camelCase")]` retrofit
        // would silently break login — pin it here.
        let req = LoginRequest {
            username: "alice".into(),
            password: "s3cret".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"username\":\"alice\""), "got {json}");
        assert!(json.contains("\"password\":\"s3cret\""), "got {json}");
    }

    #[test]
    fn login_response_v2_payload_parses() {
        // From the live `routers/auth.py:99` return statement.
        let json = r#"{
            "ok": true,
            "username": "alice",
            "role": "admin",
            "token": "f0e1d2c3b4a5"
        }"#;
        let r: LoginResponse = serde_json::from_str(json).unwrap();
        assert!(r.ok);
        assert_eq!(r.username, "alice");
        assert_eq!(r.role, "admin");
        assert_eq!(r.token.as_deref(), Some("f0e1d2c3b4a5"));
    }

    #[test]
    fn login_response_v4_payload_parses_without_token() {
        // From `electron-app-v4/server/auth.js:158` — cookie-only;
        // no `token` field in the body.  Client falls back to
        // `Set-Cookie` for the actual session value.
        let json = r#"{"ok": true, "username": "alice", "role": "admin"}"#;
        let r: LoginResponse = serde_json::from_str(json).unwrap();
        assert!(r.ok);
        assert!(r.token.is_none());
    }

    #[test]
    fn logout_response_parses_canonical_payload() {
        let json = r#"{"ok": true}"#;
        let r: LogoutResponse = serde_json::from_str(json).unwrap();
        assert!(r.ok);
        round_trip(&r);
    }

    #[test]
    fn me_response_v2_payload_parses() {
        // From `routers/auth.py:195` — includes is_active.
        let json = r#"{"username": "alice", "role": "admin", "is_active": true}"#;
        let r: MeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.username, "alice");
        assert!(r.is_active);
    }

    #[test]
    fn me_response_v4_payload_parses_with_default_active() {
        // From `electron-app-v4/server/auth.js:168` — no is_active
        // field.  Default to true so the client treats a logged-in
        // v4 session as active.
        let json = r#"{"username": "alice", "role": "admin"}"#;
        let r: MeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.username, "alice");
        assert!(r.is_active, "default is_active should be true on v4");
    }

    // ── Health ──────────────────────────────────────────────────────────

    #[test]
    fn health_response_v4_payload_parses() {
        // From `electron-app-v4/server/routes/misc.js:64`.
        let json = r#"{"ok": true, "version": "4.0.0", "backend": "node-js"}"#;
        let r: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(r.ok);
        assert_eq!(r.version.as_deref(), Some("4.0.0"));
        assert_eq!(r.backend.as_deref(), Some("node-js"));
        assert!(r.model_ready.is_none());
    }

    #[test]
    fn health_response_v2_payload_parses() {
        // From `fastapi_app.py:591` — superset of fields, including
        // `model_ready` we DO surface for the warming-up banner.
        let json = r#"{
            "ok": true,
            "model_ready": false,
            "nc_license_accepted": true,
            "thumb_cache": {"hits": 0}
        }"#;
        let r: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(r.ok);
        assert_eq!(r.model_ready, Some(false));
        // Unknown fields (nc_license_accepted / thumb_cache) are
        // dropped silently by serde — that's intentional, the UI
        // doesn't need them.
        assert!(r.version.is_none());
    }

    #[test]
    fn health_response_minimal_payload_parses() {
        // A degenerate case worth pinning: the only field a future
        // CrispLens MUST keep is `ok`.  Pin that contract.
        let r: HealthResponse =
            serde_json::from_str(r#"{"ok": false}"#).unwrap();
        assert!(!r.ok);
    }

    // ── Image / ImagesListResponse ──────────────────────────────────────

    #[test]
    fn image_v4_row_parses_with_full_field_set() {
        // From `electron-app-v4/server/routes/images.js:54` rowToApi.
        let json = r#"{
            "id": 42,
            "filename": "sunset.jpg",
            "filepath": "/var/lib/crisplens/photos/sunset.jpg",
            "server_path": "/var/lib/crisplens/photos/sunset.jpg",
            "origin_path": "/Users/alice/Pictures/sunset.jpg",
            "local_path": "/Users/alice/Pictures/sunset.jpg",
            "file_size": 1234567,
            "width": 4032,
            "height": 3024,
            "format": "jpeg",
            "taken_at": "2024-03-15T14:22:09",
            "created_at": "2024-03-15T14:30:00Z",
            "processed_at": "2024-03-15T14:30:15Z",
            "face_count": 2,
            "ai_description": "A sunset over a lake",
            "ai_scene_type": "outdoor",
            "ai_tags": "sunset,lake,nature",
            "ai_tags_list": ["sunset", "lake", "nature"],
            "rating": 4,
            "star_rating": 4,
            "flag": "green",
            "color_flag": "green",
            "description": "Trip to Essen",
            "creator": "Alice",
            "copyright": "(c) Alice 2024",
            "visibility": "shared",
            "faces": [],
            "people": []
        }"#;
        let img: Image = serde_json::from_str(json).unwrap();
        assert_eq!(img.id, 42);
        assert_eq!(img.filepath, "/var/lib/crisplens/photos/sunset.jpg");
        assert_eq!(img.local_path.as_deref(), Some("/Users/alice/Pictures/sunset.jpg"));
        assert_eq!(img.file_size, Some(1_234_567));
        assert_eq!(img.format.as_deref(), Some("jpeg"));
        assert_eq!(img.rating, Some(4));
        assert_eq!(img.flag.as_deref(), Some("green"));
        // ai_tags_list alias landed in ai_tags.
        assert_eq!(img.ai_tags, vec!["sunset", "lake", "nature"]);
    }

    #[test]
    fn image_v2_raw_row_loses_rating_and_flag_without_adapter() {
        // A RAW v2 payload (from `image_ops.py:924` SELECT) carries
        // `star_rating` + `color_flag` — NOT the v4-canonical names.
        // Without the HTTP-client adapter's rename step, those
        // fields are silently dropped: rating/flag end up None.
        // This test pins the contract so any future regression that
        // promotes them to first-class fields here fails loudly.
        let json = r#"{
            "id": 7,
            "filename": "lake.jpeg",
            "filepath": "/server/lake.jpeg",
            "created_at": "2024-01-01T00:00:00Z",
            "ai_tags_list": ["lake"],
            "star_rating": 3,
            "color_flag": "yellow"
        }"#;
        let img: Image = serde_json::from_str(json).unwrap();
        assert_eq!(img.id, 7);
        assert!(img.rating.is_none(), "raw v2 payload must NOT populate rating — adapter is responsible");
        assert!(img.flag.is_none(),   "raw v2 payload must NOT populate flag   — adapter is responsible");
        assert_eq!(img.ai_tags, vec!["lake"]);
    }

    #[test]
    fn image_v2_payload_adapter_renamed_to_v4_names_parses_fully() {
        // The HTTP-client adapter (slice B2) is expected to rename
        // v2 fields to v4 names before passing the payload to
        // serde.  Pin the post-rename shape here so adapter authors
        // know what to produce.
        let json = r#"{
            "id": 7,
            "filename": "lake.jpeg",
            "filepath": "/server/lake.jpeg",
            "created_at": "2024-01-01T00:00:00Z",
            "ai_tags_list": ["lake"],
            "rating": 3,
            "flag": "yellow"
        }"#;
        let img: Image = serde_json::from_str(json).unwrap();
        assert_eq!(img.rating, Some(3));
        assert_eq!(img.flag.as_deref(), Some("yellow"));
    }

    #[test]
    fn images_list_response_v4_envelope_parses() {
        // From `electron-app-v4/server/routes/images.js:187`.
        let json = r#"{
            "images": [
                {"id": 1, "filename": "a.jpg", "filepath": "/p/a.jpg", "created_at": "2024-01-01T00:00:00Z"}
            ],
            "total": 1
        }"#;
        let env: ImagesListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(env.images.len(), 1);
        assert_eq!(env.total, Some(1));
    }

    #[test]
    fn images_list_response_missing_total_is_tolerated() {
        // A v2 bare-array response wrapped by the HTTP-client
        // adapter into the envelope shape WITHOUT a total field —
        // because v2's bare list doesn't carry one.  We tolerate
        // total=None and the UI shows just `len(images)`.
        let json = r#"{"images": []}"#;
        let env: ImagesListResponse = serde_json::from_str(json).unwrap();
        assert!(env.images.is_empty());
        assert!(env.total.is_none());
    }

    // ── Errors ──────────────────────────────────────────────────────────

    #[test]
    fn error_response_round_trips() {
        round_trip(&ErrorResponse {
            detail: "Invalid credentials".into(),
        });
    }

    #[test]
    fn error_response_parses_fastapi_envelope() {
        // FastAPI's HTTPException renders to `{"detail": "..."}`.
        let json = r#"{"detail": "Not authenticated"}"#;
        let r: ErrorResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.detail, "Not authenticated");
    }

    // ── B5 — Watchfolder cross-reference ────────────────────────────────

    #[test]
    fn watchfolder_v4_raw_row_parses_with_int_booleans() {
        // From electron-app-v4/server/routes/misc.js:335 — `SELECT *`
        // returns SQLite booleans as 0/1 ints.  Permissive
        // serde_json::Value typing makes this parse cleanly; the
        // `recursive_bool()` helper coerces.
        let json = r#"{
            "id": 1,
            "path": "/var/lib/crisplens/photos",
            "recursive": 1,
            "auto_scan": 0,
            "scan_interval": 3600,
            "enabled": 1
        }"#;
        let w: WatchFolder = serde_json::from_str(json).unwrap();
        assert_eq!(w.path, "/var/lib/crisplens/photos");
        assert_eq!(w.recursive_bool(), Some(true));
        assert_eq!(w.auto_scan_bool(), Some(false));
        assert_eq!(w.enabled_bool(), Some(true));
    }

    #[test]
    fn watchfolder_v2_live_payload_parses() {
        // Captured verbatim from POST /api/watchfolders against the
        // live <crisplens-host> server (FastAPI v2): scan_interval
        // is `24.0` (float, because the migration column is REAL),
        // and there are several extra columns (last_scanned_at,
        // files_found, files_added, created_at) the spec didn't
        // anticipate.  All ignored silently except `path`.
        let json = r#"{
            "id": 1,
            "path": "/tmp",
            "recursive": 1,
            "auto_scan": 0,
            "scan_interval_hours": 24.0,
            "last_scanned_at": null,
            "files_found": 0,
            "files_added": 0,
            "created_at": "2026-05-11 16:33:01"
        }"#;
        let w: WatchFolder = serde_json::from_str(json).unwrap();
        assert_eq!(w.path, "/tmp");
        assert_eq!(w.recursive_bool(), Some(true));
        assert_eq!(w.auto_scan_bool(), Some(false));
        // scan_interval_hours alias landed in scan_interval, value
        // preserved as the original 24.0 float.
        assert_eq!(
            w.scan_interval.as_ref().and_then(|v| v.as_f64()),
            Some(24.0)
        );
    }

    #[test]
    fn watchfolder_with_proper_booleans_also_parses() {
        // A future CrispLens Rust port (or a v5 frontend) might emit
        // canonical JSON booleans.  Same Rust type accepts that
        // shape too.
        let json = r#"{
            "id": 1, "path": "/p",
            "recursive": true, "auto_scan": false, "enabled": true
        }"#;
        let w: WatchFolder = serde_json::from_str(json).unwrap();
        assert_eq!(w.recursive_bool(), Some(true));
        assert_eq!(w.auto_scan_bool(), Some(false));
        assert_eq!(w.enabled_bool(), Some(true));
    }

    #[test]
    fn watchfolder_minimal_payload_only_path() {
        let json = r#"{"path": "/p"}"#;
        let w: WatchFolder = serde_json::from_str(json).unwrap();
        assert_eq!(w.path, "/p");
        assert!(w.id.is_none());
        assert_eq!(w.recursive_bool(), None);
        assert_eq!(w.enabled_bool(), None);
    }

    #[test]
    fn coerce_bool_handles_all_realistic_wire_shapes() {
        // Pin the helper against every shape the live + future
        // backends might emit, so a regression here would surface
        // before reaching the UI cross-reference logic.
        use serde_json::{json, Value};
        let cases: &[(Value, Option<bool>)] = &[
            (json!(true),   Some(true)),
            (json!(false),  Some(false)),
            (json!(1),      Some(true)),
            (json!(0),      Some(false)),
            (json!(2),      Some(true)),        // any nonzero int → true
            (json!(-1),     Some(true)),
            (json!(0.0),    Some(false)),
            (json!(0.5),    Some(true)),
            (json!(null),   None),
            (json!("yes"),  None),              // strings ignored on purpose
            (json!("1"),    None),              // (no implicit string-int magic)
        ];
        for (input, expected) in cases {
            assert_eq!(
                coerce_bool(Some(input)),
                *expected,
                "coerce_bool({input:?}) — expected {expected:?}"
            );
        }
        assert_eq!(coerce_bool(None), None);
    }
}
