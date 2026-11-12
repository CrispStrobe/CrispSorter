//! Static configuration. Mirrors og/cli .env.template defaults.
//! Values can be overridden via environment variables of the same name.
//!
//! Only API endpoints + app constants live here — no filesystem paths. Where the
//! front-end stores credentials is the front-end's concern (it owns persistence).

pub fn get(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

pub fn drive_web_url() -> String {
    get("DRIVE_WEB_URL", "https://drive.internxt.com")
}

/// Drive / auth REST API base (DRIVE_NEW_API_URL in the node CLI).
pub fn drive_api_url() -> String {
    get("DRIVE_NEW_API_URL", "https://gateway.internxt.com/drive")
}

/// Network (bridge) base url.
pub fn network_url() -> String {
    get("NETWORK_URL", "https://gateway.internxt.com/network")
}

/// Payments API base (PAYMENTS_API_URL in the node clients). Used only for the
/// best-effort plan/tier lookup; the drive gateway covers usage + limits.
pub fn payments_api_url() -> String {
    get("PAYMENTS_API_URL", "https://gateway.internxt.com/payments")
}

/// Secret used for CryptoJS-compatible AES of salt / password hash / credentials file.
pub fn app_crypto_secret() -> String {
    get("APP_CRYPTO_SECRET", "6KYQBP847D4ATSFA")
}

pub fn desktop_header() -> String {
    get("DESKTOP_HEADER", "3b68706a367fd567b929396290b1de40768bb768")
}

/// Whether image thumbnail generation/upload is enabled (default on). Set
/// `IXR_THUMBNAILS` to `0`/`false`/`no`/`off` (case-insensitive) to turn it
/// off across every upload path (upload-file/-folder and the serve backends).
pub fn thumbnails_enabled() -> bool {
    match std::env::var("IXR_THUMBNAILS") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

use std::sync::OnceLock;

static CLIENT_NAME: OnceLock<String> = OnceLock::new();
static CLIENT_VERSION: OnceLock<String> = OnceLock::new();

/// Set the client identity (the `internxt-client` / `internxt-version` headers)
/// the embedder wants to present. Call once at startup, before any API/network
/// use. The cli front-end sets `internxt-cli-rust` + its own crate version; a
/// GUI or other embedder picks its own. Only the first call per process takes
/// effect. Core reads no env here — any ad-hoc env override is the front-end's
/// concern (the cli reads `IXR_INTERNXT_CLIENT` / `IXR_INTERNXT_VERSION` and
/// passes them in), keeping core free of environment/config policy.
pub fn set_client_identity(name: impl Into<String>, version: impl Into<String>) {
    let _ = CLIENT_NAME.set(name.into());
    let _ = CLIENT_VERSION.set(version.into());
}

/// Value of the `internxt-client` header. Identifies the client to the backend.
/// We do **not** impersonate the official `internxt-cli` — default is
/// `internxt-core-rust`; embedders override via [`set_client_identity`].
///
/// This header is *not* how non-Ultimate plans are gated — that gate lives on the
/// `/cli/`-namespaced endpoints (`/auth/cli/login/access`, `/users/cli/refresh`),
/// which we avoid entirely in favour of the general `/auth/login/access` +
/// `/users/refresh`. Endpoints, crypto, payloads and the `x-internxt-desktop-header`
/// token are otherwise identical across all official clients.
pub fn client_name() -> String {
    CLIENT_NAME
        .get()
        .cloned()
        .unwrap_or_else(|| "internxt-core-rust".to_string())
}

/// Value of the `internxt-version` header. Free-form. Defaults to this crate's
/// version; embedders override via [`set_client_identity`].
pub fn client_version() -> String {
    CLIENT_VERSION
        .get()
        .cloned()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

