//! P13/B1 Tauri command surface for CrispLens (Tier 2) plumbing.
//!
//! All commands prefixed `images_crisplens_*`.  This module is the
//! ONLY place that touches the OS keychain — UI / CLI / tests all
//! talk through these RPC entry points so the secret-store contract
//! is centralised.
//!
//! ## What ships in B1
//!
//! * `images_crisplens_settings_get` — read non-secret settings
//!   (backend selection + URL + UI tunables).
//! * `images_crisplens_settings_set` — persist non-secret settings.
//! * `images_crisplens_session_status` — boolean "do we have a stored
//!   cookie for this URL?", no leakage of the cookie itself.
//! * `images_crisplens_login` — POST `/api/auth/login` against the
//!   configured URL; on success, store the session cookie in the
//!   OS keychain.  Cookie capture works both for v2 (echoed in body)
//!   and v4 (Set-Cookie header).
//! * `images_crisplens_logout` — POST `/api/auth/logout` (best-effort
//!   to invalidate the cookie server-side) then wipe the keychain
//!   entry.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::{
    secret::{self, SecretError},
    settings::{self, ImagesSettings},
};
use crate::AppState;
use crisplens_protocol::{
    ErrorResponse, Face, HealthResponse, LoginRequest, LoginResponse, MeResponse, Person,
    SearchHit, WatchFolder,
};

/// Resolve the writable data directory.  Mirrors `cli::resolve_data_dir`
/// for the GUI path — we use Tauri's app-handle to be platform-correct
/// in the GUI runtime, then fall back to the bare path when the data_dir
/// hasn't been seeded yet (cold start before `init_index`).
async fn resolve_data_dir(state: &State<'_, AppState>) -> Result<PathBuf, String> {
    let guard = state.data_dir.lock().await;
    guard
        .clone()
        .ok_or_else(|| "app data dir not yet initialised".to_string())
}

#[tauri::command]
pub async fn images_crisplens_settings_get(
    state: State<'_, AppState>,
) -> Result<ImagesSettings, String> {
    let data_dir = resolve_data_dir(&state).await?;
    Ok(settings::load(&data_dir))
}

#[tauri::command]
pub async fn images_crisplens_settings_set(
    state: State<'_, AppState>,
    settings_payload: ImagesSettings,
) -> Result<(), String> {
    let data_dir = resolve_data_dir(&state).await?;
    settings::save(&data_dir, &settings_payload).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    /// `true` if a stored cookie exists for the currently-configured
    /// CrispLens URL.  No cookie value or any other identifying
    /// information leaks.
    pub authenticated: bool,
    /// The URL whose status this reports.  Empty when settings have
    /// no URL configured.
    pub url: String,
}

#[tauri::command]
pub async fn images_crisplens_session_status(
    state: State<'_, AppState>,
) -> Result<SessionStatus, String> {
    let data_dir = resolve_data_dir(&state).await?;
    let s = settings::load(&data_dir);
    let url = s.normalised_url().to_owned();
    if url.is_empty() {
        return Ok(SessionStatus { authenticated: false, url });
    }
    // No leak: we read presence only, then discard the cookie value.
    let authenticated = matches!(secret::get_session_for_url(&url), Ok(Some(_)));
    Ok(SessionStatus { authenticated, url })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginOutcome {
    pub ok: bool,
    pub username: String,
    pub role: String,
}

#[tauri::command]
pub async fn images_crisplens_login(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<LoginOutcome, String> {
    let data_dir = resolve_data_dir(&state).await?;
    let s = settings::load(&data_dir);
    let url = s.normalised_url().to_owned();
    if url.is_empty() {
        return Err("no CrispLens URL configured".into());
    }

    // Offload to a blocking task — reqwest's async client + tokio's
    // current_thread runtime in the Tauri command context can get
    // sticky around DNS + cookie jar setup; the simpler blocking
    // client is fine for a one-shot login.
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        login_blocking(&url, &username, &password)
    })
    .await
    .map_err(|e| format!("login join: {e}"))??;
    Ok(outcome)
}

/// Shared sync core for both the Tauri command and the CLI.
/// Performs the POST, captures the session cookie (from response
/// body for v2, from Set-Cookie header for v4), stores it in the
/// OS keychain, and returns the user-visible bits.
pub(crate) fn login_blocking(
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<LoginOutcome, String> {
    let client = reqwest::blocking::Client::builder()
        .cookie_store(true)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client init: {e}"))?;

    let endpoint = format!("{base_url}/api/auth/login");
    let resp = client
        .post(&endpoint)
        .json(&LoginRequest {
            username: username.to_owned(),
            password: password.to_owned(),
        })
        .send()
        .map_err(|e| format!("POST {endpoint}: {e}"))?;

    let status = resp.status();
    // Pull the Set-Cookie session value BEFORE we move resp into
    // `.json()` / `.text()`.
    let set_cookie_value = resp
        .cookies()
        .find(|c| c.name() == "session")
        .map(|c| c.value().to_owned());

    if !status.is_success() {
        // Try to surface CrispLens's `{ detail }` reason; fall back
        // to a generic message when the body isn't well-formed.
        let body = resp.text().unwrap_or_default();
        let detail: String = serde_json::from_str::<ErrorResponse>(&body)
            .map(|e| e.detail)
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(detail);
    }

    let login: LoginResponse = resp
        .json()
        .map_err(|e| format!("login response not JSON: {e}"))?;
    if !login.ok {
        return Err("login server returned ok=false".into());
    }

    // v2 echoes the cookie value in the body; v4 only sets it via
    // Set-Cookie.  Prefer body (it's verifiable) and fall back to
    // header.
    let cookie_value = login
        .token
        .clone()
        .or(set_cookie_value)
        .ok_or_else(|| "login succeeded but no session cookie received".to_string())?;

    secret::set_session_for_url(base_url, &cookie_value)
        .map_err(|e: SecretError| format!("keychain: {e}"))?;

    Ok(LoginOutcome {
        ok: login.ok,
        username: login.username,
        role:     login.role,
    })
}

/// Combined live-status probe for the degradation monitor (slice B4).
/// One Tauri RPC returns everything the UI banner needs to decide
/// between "Tier 2 online", "Tier 2 reachable but session expired",
/// "Tier 2 offline", and "Tier 2 not configured".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrispLensStatus {
    /// `true` when the configured URL is non-empty AND the dropdown
    /// is set to CrispLens.  Independent of reachability — used by
    /// the UI to decide whether to show the banner at all.
    pub tier2_configured: bool,
    /// `true` when the most recent `/api/health` succeeded.  `None`
    /// when `tier2_configured` is `false`.
    pub health_ok: Option<bool>,
    /// Health probe response payload (version / backend identifier)
    /// when the probe succeeded.  Used for the "connected to
    /// CrispLens 4.0.0 (node-js)" copy in the banner-cleared state.
    pub health_version: Option<String>,
    pub health_backend: Option<String>,
    /// Set to the v2-only `model_ready` field.  `Some(false)` means
    /// the server is up but still loading models — UI shows
    /// "warming up" instead of full success.
    pub health_model_ready: Option<bool>,
    /// `true` when the stored session cookie still authenticates
    /// against `/api/auth/me`.  Goes to `false` when the cookie is
    /// missing, the server returns 401, or the network fails.
    /// UI uses this with `health_ok = true` to drive the
    /// "session expired — log in again" CTA.
    pub authenticated: bool,
    /// Username returned by `/api/auth/me` when authenticated.
    pub username: Option<String>,
    /// User role from `/api/auth/me`.  Used by the UI to gate write
    /// actions when CrispLens grows them.
    pub role: Option<String>,
    /// One-line diagnostic surfaced when something went wrong —
    /// banner copy on the offline state.  Empty on success.
    #[serde(default)]
    pub error: String,
}

#[tauri::command]
pub async fn images_crisplens_status(
    state: State<'_, AppState>,
) -> Result<CrispLensStatus, String> {
    let data_dir = resolve_data_dir(&state).await?;
    tauri::async_runtime::spawn_blocking(move || status_blocking(&data_dir))
        .await
        .map_err(|e| format!("status join: {e}"))
}

/// Shared sync core for the Tauri command + CLI parity.  Always
/// returns `Ok(CrispLensStatus)` — failures map to fields on the
/// payload (`health_ok = Some(false)`, `error = "..."`).  That way
/// the UI banner state machine is driven by one shape regardless of
/// what kind of failure happened.
pub(crate) fn status_blocking(data_dir: &std::path::Path) -> CrispLensStatus {
    let mut out = CrispLensStatus {
        tier2_configured:   false,
        health_ok:          None,
        health_version:     None,
        health_backend:     None,
        health_model_ready: None,
        authenticated:      false,
        username:           None,
        role:               None,
        error:              String::new(),
    };

    let s = settings::load(data_dir);
    if !s.tier2_enabled() {
        return out; // Tier 2 not configured — leave the rest blank.
    }
    out.tier2_configured = true;
    let url = s.normalised_url().to_owned();

    // 1. Health probe — unauthenticated, public.  Tight 5 s timeout
    // because the UI polls this every 30 s; a hung probe stalls the
    // banner.
    let client = match reqwest::blocking::Client::builder()
        .cookie_store(true)
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            out.health_ok = Some(false);
            out.error = format!("http client init: {e}");
            return out;
        }
    };

    match client.get(format!("{url}/api/health")).send() {
        Ok(resp) if resp.status().is_success() => match resp.json::<HealthResponse>() {
            Ok(h) => {
                out.health_ok          = Some(h.ok);
                out.health_version     = h.version;
                out.health_backend     = h.backend;
                out.health_model_ready = h.model_ready;
                if !h.ok {
                    out.error = "server reports ok=false".into();
                }
            }
            Err(e) => {
                out.health_ok = Some(false);
                out.error = format!("health body not JSON: {e}");
                return out;
            }
        },
        Ok(resp) => {
            out.health_ok = Some(false);
            out.error = format!("HTTP {}", resp.status());
            return out;
        }
        Err(e) => {
            out.health_ok = Some(false);
            // The on-the-wire failure shape ("dns failed", "connect
            // refused", "request timed out") makes good banner copy
            // verbatim — don't paraphrase.
            out.error = format!("health probe failed: {e}");
            return out;
        }
    }

    if !out.health_ok.unwrap_or(false) {
        // Server reachable but reporting unhealthy — skip the auth
        // probe (it'd just compound the failure noise) and return
        // what we have.
        return out;
    }

    // 2. Auth probe — only when we have a stored cookie.  Calling
    // /me unauthenticated would always return 401 and the UI would
    // then prompt for login even when the user genuinely wants
    // unauth Tier 2 use.
    let cookie = match secret::get_session_for_url(&url) {
        Ok(Some(v)) => v,
        Ok(None) => {
            // No cookie stored — not an error; the UI shows the
            // login form.  Leave `authenticated = false`.
            return out;
        }
        Err(e) => {
            out.error = format!("keychain: {e}");
            return out;
        }
    };

    match client
        .get(format!("{url}/api/auth/me"))
        .header("Cookie", format!("session={cookie}"))
        .send()
    {
        Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
            // Cookie expired or revoked.  Wipe it locally so the
            // next status poll won't repeat the 401 (otherwise the
            // user would see "session expired" copy for as long as
            // the dead cookie sits in keychain).
            let _ = secret::clear_session_for_url(&url);
            out.authenticated = false;
            out.error = "session expired — please log in again".into();
        }
        Ok(resp) if resp.status().is_success() => match resp.json::<MeResponse>() {
            Ok(me) => {
                out.authenticated = true;
                out.username = Some(me.username);
                out.role     = Some(me.role);
            }
            Err(e) => {
                out.error = format!("/me body not JSON: {e}");
            }
        },
        Ok(resp) => {
            out.error = format!("/me HTTP {}", resp.status());
        }
        Err(e) => {
            // Health succeeded but /me failed mid-call — usually a
            // transient network blip.  Leave `health_ok = true`
            // (the broader UI shouldn't fall back to Tier 1 over
            // this); the auth banner copy points at the specific
            // error.
            out.error = format!("/me request failed: {e}");
        }
    }

    out
}

/// `GET /api/watchfolders` — list the folders CrispLens is watching.
/// Used by slice B5's "this folder is also watched by CrispLens" hint
/// in the Bilder preview pane.  Returns an empty list when Tier 2
/// isn't configured / isn't authenticated; the UI treats that the
/// same as "no overlap" so the hint never shows.
#[tauri::command]
pub async fn images_crisplens_watchfolders(
    state: State<'_, AppState>,
) -> Result<Vec<WatchFolder>, String> {
    let data_dir = resolve_data_dir(&state).await?;
    tauri::async_runtime::spawn_blocking(move || watchfolders_blocking(&data_dir))
        .await
        .map_err(|e| format!("watchfolders join: {e}"))?
}

pub(crate) fn watchfolders_blocking(
    data_dir: &std::path::Path,
) -> Result<Vec<WatchFolder>, String> {
    let s = settings::load(data_dir);
    if !s.tier2_enabled() {
        return Ok(Vec::new());
    }
    let url = s.normalised_url().to_owned();
    let cookie = match secret::get_session_for_url(&url) {
        Ok(Some(v)) => v,
        Ok(None) => return Ok(Vec::new()), // unauth → empty, not an error
        Err(e) => return Err(format!("keychain: {e}")),
    };

    let client = reqwest::blocking::Client::builder()
        .cookie_store(true)
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| format!("http client init: {e}"))?;

    let resp = client
        .get(format!("{url}/api/watchfolders"))
        .header("Cookie", format!("session={cookie}"))
        .send()
        .map_err(|e| format!("GET /api/watchfolders: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        let detail = serde_json::from_str::<ErrorResponse>(&body)
            .map(|e| e.detail)
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(detail);
    }
    resp.json::<Vec<WatchFolder>>()
        .map_err(|e| format!("watchfolders body not JSON: {e}"))
}

/// `GET /api/people` — full list of person clusters from CrispLens.
/// Returns empty list when Tier 2 isn't configured/authenticated
/// (same convention as `images_crisplens_watchfolders`).
#[tauri::command]
pub async fn images_crisplens_people(
    state: State<'_, AppState>,
) -> Result<Vec<Person>, String> {
    let data_dir = resolve_data_dir(&state).await?;
    tauri::async_runtime::spawn_blocking(move || {
        get_json::<Vec<Person>>(&data_dir, "/api/people")
    })
    .await
    .map_err(|e| format!("people join: {e}"))?
}

/// `GET /api/images/{id}/faces` — face crops detected in one image.
/// Returns empty list when Tier 2 isn't configured.
#[tauri::command]
pub async fn images_crisplens_image_faces(
    state: State<'_, AppState>,
    image_id: i64,
) -> Result<Vec<Face>, String> {
    let data_dir = resolve_data_dir(&state).await?;
    tauri::async_runtime::spawn_blocking(move || {
        get_json::<Vec<Face>>(&data_dir, &format!("/api/images/{image_id}/faces"))
    })
    .await
    .map_err(|e| format!("faces join: {e}"))?
}

/// Shared authenticated-GET helper for the B3 endpoints.  Reads the
/// session cookie from keychain, hits `<URL><path>`, deserialises
/// the JSON body into `T`.  Returns `Ok(T::default())` for the
/// not-authenticated case so the UI handles "Tier 2 not configured"
/// the same way across all B-slice surfaces.
pub(crate) fn get_json<T>(data_dir: &std::path::Path, path: &str) -> Result<T, String>
where
    T: Default + serde::de::DeserializeOwned,
{
    let s = settings::load(data_dir);
    if !s.tier2_enabled() {
        return Ok(T::default());
    }
    let url = s.normalised_url().to_owned();
    let cookie = match secret::get_session_for_url(&url) {
        Ok(Some(v)) => v,
        Ok(None) => return Ok(T::default()),
        Err(e) => return Err(format!("keychain: {e}")),
    };

    let client = reqwest::blocking::Client::builder()
        .cookie_store(true)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client init: {e}"))?;

    let resp = client
        .get(format!("{url}{path}"))
        .header("Cookie", format!("session={cookie}"))
        .send()
        .map_err(|e| format!("GET {path}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        let detail = serde_json::from_str::<ErrorResponse>(&body)
            .map(|e| e.detail)
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(detail);
    }
    resp.json::<T>()
        .map_err(|e| format!("{path} body not JSON: {e}"))
}

/// `GET /api/search?q=…&limit=…` — text search on CrispLens.
/// **Not semantic** — see `crisplens_protocol::SearchHit` doc-comment.
/// Returns an empty list when Tier 2 isn't authenticated or `q` is
/// empty.
#[tauri::command]
pub async fn images_crisplens_search(
    state: State<'_, AppState>,
    q: String,
    limit: Option<i64>,
) -> Result<Vec<SearchHit>, String> {
    let data_dir = resolve_data_dir(&state).await?;
    if q.trim().is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.unwrap_or(50).clamp(1, 500);
    // URL-encode the query.  Hand-rolled because we don't have a
    // dep on `urlencoding` and only need a single field.
    let encoded_q = q
        .chars()
        .flat_map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect::<Vec<_>>()
            }
        })
        .collect::<String>();
    let path = format!("/api/search?q={encoded_q}&limit={limit}");

    tauri::async_runtime::spawn_blocking(move || get_json::<Vec<SearchHit>>(&data_dir, &path))
        .await
        .map_err(|e| format!("search join: {e}"))?
}

#[tauri::command]
pub async fn images_crisplens_logout(
    state: State<'_, AppState>,
) -> Result<(), String> {
    let data_dir = resolve_data_dir(&state).await?;
    let s = settings::load(&data_dir);
    let url = s.normalised_url().to_owned();
    if url.is_empty() {
        // Nothing configured → no-op success.  Matches "logout is
        // always safe" intuition.
        return Ok(());
    }
    tauri::async_runtime::spawn_blocking(move || logout_blocking(&url))
        .await
        .map_err(|e| format!("logout join: {e}"))?
}

/// Shared logout core.  Best-effort server-side invalidation;
/// regardless of the server response we always wipe the local
/// keychain entry so the credential is gone on disk.
pub(crate) fn logout_blocking(base_url: &str) -> Result<(), String> {
    let cookie = secret::get_session_for_url(base_url)
        .map_err(|e| format!("keychain: {e}"))?;

    // Try to invalidate server-side.  Failure here is non-fatal —
    // the cookie may already be expired or unreachable, but we
    // still wipe the local copy.
    if let Some(cookie_value) = cookie {
        let _ = reqwest::blocking::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .and_then(|c| {
                c.post(format!("{base_url}/api/auth/logout"))
                    .header(
                        "Cookie",
                        format!("session={cookie_value}"),
                    )
                    .send()
            });
    }

    secret::clear_session_for_url(base_url).map_err(|e| format!("keychain: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_payload_uses_camelcase_authenticated_field() {
        // Pin the wire shape — the Svelte side reads `authenticated`,
        // not `authentic` / `is_authenticated` / etc.
        let s = SessionStatus { authenticated: true, url: "u".into() };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"authenticated\":true"), "got {json}");
        assert!(json.contains("\"url\":\"u\""), "got {json}");
    }

    #[test]
    fn login_outcome_omits_token_field() {
        // Sanity that no surface of LoginOutcome exposes the
        // session cookie — the only place the cookie value lives is
        // the OS keychain.  A future regression that adds a `token`
        // field here would surface as a failed assertion.
        let o = LoginOutcome {
            ok: true,
            username: "alice".into(),
            role: "admin".into(),
        };
        let json = serde_json::to_string(&o).unwrap();
        assert!(!json.contains("token"), "LoginOutcome must not include token: {json}");
        assert!(!json.contains("session"), "LoginOutcome must not include session: {json}");
    }

    // ── B4 — status payload wire shape ──────────────────────────────────

    #[test]
    fn status_payload_uses_camelcase_field_names() {
        // Frontend bindings read these names; a future
        // rename_all-typo would break the banner state machine
        // silently.  Pin every field name we surface.
        let s = CrispLensStatus {
            tier2_configured:   true,
            health_ok:          Some(true),
            health_version:     Some("4.0.0".into()),
            health_backend:     Some("node-js".into()),
            health_model_ready: Some(false),
            authenticated:      true,
            username:           Some("alice".into()),
            role:               Some("admin".into()),
            error:              String::new(),
        };
        let json = serde_json::to_string(&s).unwrap();
        for needle in [
            "\"tier2Configured\":true",
            "\"healthOk\":true",
            "\"healthVersion\":\"4.0.0\"",
            "\"healthBackend\":\"node-js\"",
            "\"healthModelReady\":false",
            "\"authenticated\":true",
            "\"username\":\"alice\"",
            "\"role\":\"admin\"",
        ] {
            assert!(json.contains(needle), "missing field {needle} in {json}");
        }
        // Empty `error` should still appear — banner copy switches
        // on the literal contents in the session-expired case.
        assert!(json.contains("\"error\":\"\""), "got {json}");
    }

    #[test]
    fn status_payload_carries_no_cookie_value() {
        // Same containment invariant as LoginOutcome — the status
        // surface MUST NOT leak the session cookie.  A regression
        // that adds a `token` / `session` field would fail here.
        let s = CrispLensStatus {
            tier2_configured: true,
            health_ok: Some(true),
            health_version: None,
            health_backend: None,
            health_model_ready: None,
            authenticated: true,
            username: Some("u".into()),
            role: Some("r".into()),
            error: String::new(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("\"token\""),   "status must not carry token: {json}");
        assert!(!json.contains("\"session\""), "status must not carry session: {json}");
        assert!(!json.contains("\"cookie\""),  "status must not carry cookie: {json}");
    }

    #[test]
    fn status_returns_unconfigured_payload_when_no_url() {
        // Pin the early-return: when settings.url is empty, no
        // network call is attempted and we report tier2_configured
        // = false with every other field cleared.  This is what
        // the UI relies on to keep the banner hidden in Tier 1
        // setups.
        let tmp = tempfile::TempDir::new().unwrap();
        // No settings file written → load() returns default →
        // url is empty → tier2_enabled() is false.
        let s = status_blocking(tmp.path());
        assert!(!s.tier2_configured);
        assert!(s.health_ok.is_none());
        assert!(!s.authenticated);
        assert!(s.error.is_empty(), "no-URL state should have no error noise: {s:?}");
    }
}
