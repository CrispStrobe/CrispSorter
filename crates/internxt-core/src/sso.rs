//! Web-based (universal-link) SSO login — the Internxt-specific flow logic only.
//!
//! Builds the web login URL, decodes the callback the Internxt web app delivers,
//! and turns it into credentials. Mirrors og/cli universal-link.service.ts.
//!
//! The actual local callback *transport* (a temporary HTTP server on native, or
//! whatever a GUI host wants) is abstracted behind [`SsoCallbackServer`], so this
//! crate stays free of axum / tokio-net. The front-end supplies the transport
//! implementation.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use std::future::Future;

use crate::auth;
use crate::config;
use crate::models::Credentials;

/// Raw (still base64) values the web app delivers to the callback. Each is
/// `Some(base64(cleartext))` on success; missing/empty on failure.
pub struct SsoCallback {
    pub mnemonic: Option<String>,
    pub new_token: Option<String>,
    pub private_key: Option<String>,
}

/// The local transport that receives the SSO callback. The front-end implements
/// it (native: a local HTTP server on a bound port; other targets: their own
/// mechanism). Core reads [`redirect_uri`](Self::redirect_uri) to build the login
/// URL, then awaits [`wait`](Self::wait) for the delivered params.
pub trait SsoCallbackServer {
    /// The plaintext redirect URI the web app should call back to. The transport
    /// must already be listening on it (e.g. `http://127.0.0.1:53112/callback`).
    fn redirect_uri(&self) -> String;

    /// Wait for the callback and return the raw delivered params. Consumes the
    /// server (it is shut down afterwards).
    fn wait(self) -> impl Future<Output = Result<SsoCallback>> + Send;
}

/// Decoded (cleartext) callback values.
struct SsoSession {
    mnemonic: String,
    token: String,
    private_key_pem: String,
}

/// Runs the SSO login flow over a caller-provided callback transport.
///
/// `on_login_url` is called once with the web login URL the moment the transport
/// is ready — the front-end decides how to surface it (print, open a browser,
/// show a QR code). Core does no terminal/browser/network-listener work itself.
pub async fn login(
    server: impl SsoCallbackServer,
    on_login_url: impl FnOnce(&str),
) -> Result<Credentials> {
    // The web app expects the redirect URI base64-encoded in the query.
    let redirect_uri = B64.encode(server.redirect_uri());
    let login_url = build_login_url(&redirect_uri);

    on_login_url(&login_url);

    let session = decode_session(server.wait().await?)?;
    auth::build_sso_credentials(&session.mnemonic, &session.token, &session.private_key_pem).await
}

/// Builds the web login URL for a base64-encoded redirect URI. Exposed so a
/// transport that wants to render the URL itself (before `login`) can reuse it.
pub fn build_login_url(redirect_uri_b64: &str) -> String {
    let base = config::drive_web_url();
    let enc = urlencode(redirect_uri_b64);
    format!("{base}/login?universalLink=true&redirectUri={enc}")
}

/// Minimal percent-encoding for the base64 redirect URI (base64 may contain
/// `+`, `/`, `=`). Avoids pulling in a URL-encoding dependency.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Whether a callback carries all three non-empty params — i.e. a successful
/// login. A transport can use this to pick the browser's success/error redirect.
pub fn callback_ok(cb: &SsoCallback) -> bool {
    matches!(
        (&cb.mnemonic, &cb.new_token, &cb.private_key),
        (Some(m), Some(t), Some(p)) if !m.is_empty() && !t.is_empty() && !p.is_empty()
    )
}

fn decode_session(cb: SsoCallback) -> Result<SsoSession> {
    let (mnemonic, token, private_key) = match (cb.mnemonic, cb.new_token, cb.private_key) {
        (Some(m), Some(t), Some(p)) if !m.is_empty() && !t.is_empty() && !p.is_empty() => (m, t, p),
        _ => return Err(anyhow!("Login has failed, please try again")),
    };

    let decode = |v: &str, what: &str| -> Result<String> {
        let bytes = B64
            .decode(v)
            .map_err(|_| anyhow!("invalid base64 {what} in login callback"))?;
        String::from_utf8(bytes).map_err(|_| anyhow!("invalid utf-8 {what} in login callback"))
    };

    Ok(SsoSession {
        mnemonic: decode(&mnemonic, "mnemonic")?,
        token: decode(&token, "token")?,
        private_key_pem: decode(&private_key, "privateKey")?,
    })
}

