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
use crisplens_protocol::{ErrorResponse, LoginRequest, LoginResponse};

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
}
