//! Login flow + credential refresh. Mirrors og/cli auth.service. Persistence is
//! the front-end's concern — nothing here touches the filesystem.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::Value;

use crate::api::DriveApi;
use crate::crypto;
use crate::models::{Credentials, UserInfo};

/// Performs legacy email/password login and returns credentials.
///
/// `tfa` is a ready-to-use TOTP code; `tfa_token` is a TOTP *secret* from which
/// a code is generated and which takes priority over `tfa` (mirrors the node
/// CLI `--twofactortoken` flag).
///
/// `on_need_2fa` is invoked only when the account requires 2FA and no code was
/// supplied — the caller decides how to obtain it (prompt the user, error out in
/// non-interactive mode, …). Core never touches the terminal.
pub async fn login(
    email: &str,
    password: &str,
    tfa: Option<&str>,
    tfa_token: Option<&str>,
    on_need_2fa: impl FnOnce() -> Result<String>,
) -> Result<Credentials> {
    let email = email.to_lowercase();
    let api = DriveApi::new();

    // 1. security details -> encrypted salt + whether 2FA is required
    let (encrypted_salt, tfa_enabled) = api.security_details(&email).await?;

    // 2. decrypt salt, hash password, re-encrypt hash
    let salt = crypto::decrypt_text(&encrypted_salt)?;
    let (_, hash) = crypto::pass_to_hash(password, Some(&salt))?;
    let encrypted_password_hash = crypto::encrypt_text(&hash);

    // 2b. obtain 2FA code if the account requires it. A TOTP secret token takes
    // priority over a literal code; otherwise ask the caller (`on_need_2fa`).
    let tfa_owned: Option<String> = if !tfa_enabled {
        None
    } else if let Some(token) = tfa_token.filter(|t| !t.trim().is_empty()) {
        Some(crypto::totp_now(token.trim())?)
    } else if let Some(code) = tfa.filter(|t| !t.trim().is_empty()) {
        Some(code.to_string())
    } else {
        Some(on_need_2fa()?)
    };

    // 3. login access (without keys)
    let data = api
        .login_access(&email, &encrypted_password_hash, tfa_owned.as_deref())
        .await?;

    let token = data["newToken"]
        .as_str()
        .ok_or_else(|| anyhow!("no newToken in login response: {data}"))?
        .to_string();
    let user = &data["user"];

    let enc_mnemonic = field(user, "mnemonic")?;
    let mnemonic = crypto::decrypt_text_with_key(&enc_mnemonic, password)?;

    if !crypto::validate_mnemonic(&mnemonic) {
        return Err(anyhow!("decrypted mnemonic is invalid"));
    }

    // Decrypt the ecc/kyber private keys when the response carries them. These
    // are only needed to decrypt workspace mnemonics, so failure is non-fatal.
    let (ecc_private_key, kyber_private_key) = decrypt_user_keys(user, password);

    let creds = Credentials {
        token,
        user: UserInfo {
            email: field(user, "email").unwrap_or(email),
            mnemonic,
            bucket: field(user, "bucket")?,
            bridge_user: field(user, "bridgeUser")?,
            user_id: field(user, "userId")?,
            root_folder_id: field(user, "rootFolderId")?,
            ecc_private_key,
            kyber_private_key,
        },
        workspace: None,
    };
    Ok(creds)
}

/// Builds credentials from a completed web-based (universal-link) SSO login.
///
/// The web app has already decrypted everything client-side and delivered the
/// clear `mnemonic`, session `token` and ecc `private_key_pem` via the callback.
/// We only need to fetch the rest of the user identity from `refreshUserCredentials`.
///
/// The kyber private key is intentionally dropped: the universal link never
/// carries it and the refresh endpoint returns it still encrypted (there is no
/// password in the SSO flow to decrypt it). ecc-only workspaces still work;
/// hybrid (Kyber) workspaces require a legacy login. Mirrors og/cli
/// universal-link.service.ts, which likewise never obtains a usable kyber key.
pub async fn build_sso_credentials(
    mnemonic: &str,
    token: &str,
    private_key_pem: &str,
) -> Result<Credentials> {
    if !crypto::validate_mnemonic(mnemonic) {
        return Err(anyhow!("decrypted mnemonic is invalid"));
    }

    let data = DriveApi::new().refresh_user_credentials(token).await?;
    let new_token = data["newToken"]
        .as_str()
        .ok_or_else(|| anyhow!("no newToken in refresh response: {data}"))?
        .to_string();
    let user = &data["user"];

    let creds = Credentials {
        token: new_token,
        user: UserInfo {
            email: field(user, "email")?,
            mnemonic: mnemonic.to_string(),
            bucket: field(user, "bucket")?,
            bridge_user: field(user, "bridgeUser")?,
            user_id: field(user, "userId")?,
            root_folder_id: field(user, "rootFolderId")?,
            // Stored as base64(armored PEM), same shape as decrypt_user_keys.
            ecc_private_key: Some(B64.encode(private_key_pem.as_bytes())),
            kyber_private_key: None,
        },
        workspace: None,
    };
    Ok(creds)
}

/// Decrypts the user's ecc + kyber private keys from the login response and
/// returns them base64-encoded as stored (mirrors node `doLogin`). Returns
/// `None` for a key when it is absent or cannot be decrypted.
fn decrypt_user_keys(user: &Value, password: &str) -> (Option<String>, Option<String>) {
    let decrypt = |encrypted: &str| -> Option<String> {
        let plain = crypto::decrypt_private_key(encrypted, password).ok()?;
        if plain.is_empty() {
            return None;
        }
        Some(B64.encode(plain.as_bytes()))
    };
    // Prefer the structured `keys.ecc/kyber`; fall back to the legacy `privateKey`.
    let ecc_enc = user["keys"]["ecc"]["privateKey"]
        .as_str()
        .or_else(|| user["privateKey"].as_str())
        .filter(|s| !s.is_empty());
    let kyber_enc = user["keys"]["kyber"]["privateKey"]
        .as_str()
        .filter(|s| !s.is_empty());
    (ecc_enc.and_then(decrypt), kyber_enc.and_then(decrypt))
}

/// Parses the `exp`/`iat` claims (unix seconds) from a JWT without verifying
/// the signature, after checking the header carries an `alg`. Mirrors node
/// `@internxt/lib` `validateJwtAndCheckExpiration`.
fn jwt_claims(token: &str) -> Option<(i64, Option<i64>)> {
    use base64::Engine;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[0])
        .ok()?;
    let header: Value = serde_json::from_slice(&header).ok()?;
    header["alg"].as_str()?;

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;
    let v: Value = serde_json::from_slice(&payload).ok()?;
    let exp = v["exp"].as_i64()?;
    if exp < 0 {
        return None;
    }
    Some((exp, v["iat"].as_i64()))
}

const SIX_HOURS_SECS: i64 = 6 * 60 * 60;

/// Whether a token needs refreshing: once the remaining lifetime drops to 50%
/// of the token's total lifetime (exp - iat), falling back to a fixed 6-hour
/// margin when `iat` is absent. Mirrors node `@internxt/lib`
/// `isTokenRefreshRequired`. Returns `false` for an already-expired token.
fn refresh_required(exp: i64, iat: Option<i64>, now: i64) -> bool {
    let remaining = exp - now;
    if remaining <= 0 {
        return false;
    }
    let threshold = match iat {
        Some(iat) => (exp - iat) / 2,
        None => SIX_HOURS_SECS,
    };
    remaining <= threshold
}

/// Like `refresh_required`, but also returns `true` for an already-expired
/// token. Used for the workspace-token path, which — unlike the main-token
/// path in `refresh_credentials` — has no separate early "already expired"
/// guard before this check, so `refresh_required`'s "already expired -> false"
/// branch must not be mistaken for "no refresh needed" here: an expired
/// workspace token still needs a refresh attempt (and a hard error if that
/// attempt fails), not silent reuse.
fn ws_refresh_required(exp: i64, iat: Option<i64>, now: i64) -> bool {
    exp - now <= 0 || refresh_required(exp, iat, now)
}

/// Validates and, mirroring node's `getAuthDetails`, refreshes credentials when
/// the token is within two days of expiry. Pure: takes the current credentials
/// and returns the (possibly-updated) ones plus a `changed` flag — persistence is
/// the front-end's job (core does no filesystem I/O). The front-end reads the
/// stored creds, calls this, and re-saves when `changed`.
///
/// `on_warn` receives best-effort diagnostic messages (a token/workspace refresh
/// that failed but left a still-valid session); core never prints them itself.
pub async fn refresh_credentials(
    mut creds: Credentials,
    on_warn: impl Fn(&str),
) -> Result<(Credentials, bool)> {
    let (exp, iat) = jwt_claims(&creds.token)
        .ok_or_else(|| anyhow!("Stored credentials are invalid. Run `internxt login` again."))?;
    if !crypto::validate_mnemonic(&creds.user.mnemonic) {
        return Err(anyhow!(
            "Stored credentials are invalid. Run `internxt login` again."
        ));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    if exp - now <= 0 {
        return Err(anyhow!(
            "Your session has expired. Run `internxt login` again."
        ));
    }

    let mut changed = false;
    if refresh_required(exp, iat, now) {
        match DriveApi::new().refresh_user_token(&creds.token).await {
            Ok(new_token) => {
                creds.token = new_token;
                changed = true;
            }
            // Refresh is best-effort: the current token is still valid (not yet
            // expired), so keep using it rather than failing the command.
            Err(e) => {
                on_warn(&format!("token refresh failed: {e}"));
            }
        }
    }

    // Refresh the workspace credentials when their token is near/at expiry, by
    // re-fetching them (the network creds + tokenHeader rotate; the mnemonic and
    // root folder are stable). Best-effort, like the user-token refresh — *unless*
    // the workspace token is already expired: `refresh_required` returns `false`
    // for an already-expired token (see its doc comment), so without this check
    // an expired workspace token would be silently treated as "no refresh
    // needed" and kept as-is, leading to opaque 401s on every workspace call
    // downstream. When already expired we still attempt a refresh (unlike the
    // main-token path, which errors out immediately), but a failed attempt is a
    // hard error rather than a warning, since there is no fallback valid session.
    if let Some(ws) = &creds.workspace {
        let ws_claims = jwt_claims(&ws.token);
        let ws_expired = ws_claims.map(|(exp, _)| exp - now <= 0).unwrap_or(true);
        let ws_refresh_required = ws_claims
            .map(|(exp, iat)| ws_refresh_required(exp, iat, now))
            .unwrap_or(true);
        if ws_refresh_required {
            match refresh_workspace_credentials(&creds.token, ws.id.clone()).await {
                Ok(Some((token, bucket, user, pass))) => {
                    if let Some(w) = creds.workspace.as_mut() {
                        w.token = token;
                        w.bucket = bucket;
                        w.network_user = user;
                        w.network_pass = pass;
                    }
                    changed = true;
                }
                Ok(None) => {
                    if ws_expired {
                        return Err(anyhow!(
                            "Your workspace session has expired. Run `internxt login` again."
                        ));
                    }
                }
                Err(e) => {
                    if ws_expired {
                        return Err(anyhow!(
                            "Your workspace session has expired and could not be refreshed: {e}"
                        ));
                    }
                    on_warn(&format!("workspace refresh failed: {e}"));
                }
            }
        }
    }

    Ok((creds, changed))
}

/// Re-fetches a workspace's network credentials + token header. Returns
/// `(tokenHeader, bucket, networkUser, networkPass)`.
async fn refresh_workspace_credentials(
    token: &str,
    workspace_id: String,
) -> Result<Option<(String, String, String, String)>> {
    let v = DriveApi::new()
        .get_workspace_credentials(token, &workspace_id)
        .await?;
    let token_header = match v["tokenHeader"].as_str() {
        Some(t) => t.to_string(),
        None => return Ok(None),
    };
    let bucket = v["bucket"].as_str().unwrap_or_default().to_string();
    let user = v["credentials"]["networkUser"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let pass = v["credentials"]["networkPass"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    Ok(Some((token_header, bucket, user, pass)))
}

fn field(user: &Value, key: &str) -> Result<String> {
    user[key]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("missing user.{key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_expiration_parses_exp() {
        // header.payload.signature where payload = {"exp":1700000000}
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJleHAiOjE3MDAwMDAwMDB9.sig";
        assert_eq!(jwt_claims(token).map(|(exp, _)| exp), Some(1_700_000_000));
    }

    #[test]
    fn jwt_expiration_rejects_malformed() {
        assert_eq!(jwt_claims("not-a-jwt"), None);
        assert_eq!(jwt_claims("a.b"), None);
        // valid structure but payload has no exp
        assert_eq!(jwt_claims("h.e30.s"), None);
    }

    fn make_jwt(header: &str, payload: &str) -> String {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.sig",
            b64.encode(header.as_bytes()),
            b64.encode(payload.as_bytes())
        )
    }

    #[test]
    fn jwt_claims_rejects_missing_alg() {
        let token = make_jwt(r#"{"typ":"JWT"}"#, r#"{"exp":1700000000}"#);
        assert_eq!(jwt_claims(&token), None);
    }

    #[test]
    fn jwt_claims_rejects_negative_exp() {
        let token = make_jwt(r#"{"alg":"HS256"}"#, r#"{"exp":-1}"#);
        assert_eq!(jwt_claims(&token), None);
    }

    #[test]
    fn jwt_claims_parses_exp_and_iat() {
        let token = make_jwt(r#"{"alg":"HS256"}"#, r#"{"exp":1700000000,"iat":1699990000}"#);
        assert_eq!(jwt_claims(&token), Some((1_700_000_000, Some(1_699_990_000))));
    }

    #[test]
    fn refresh_required_uses_half_lifetime_with_iat() {
        let iat = 1_000_000;
        let exp = 1_000_000 + 1000; // 1000s lifetime -> 500s threshold
        assert!(!refresh_required(exp, Some(iat), exp - 501));
        assert!(refresh_required(exp, Some(iat), exp - 500));
        assert!(refresh_required(exp, Some(iat), exp - 1));
        assert!(!refresh_required(exp, Some(iat), exp)); // already expired -> false
    }

    #[test]
    fn ws_refresh_required_treats_expired_as_needing_refresh() {
        let iat = 1_000_000;
        let exp = 1_000_000 + 1000; // 1000s lifetime -> 500s threshold
        // Same as `refresh_required` while the token is still valid.
        assert!(!ws_refresh_required(exp, Some(iat), exp - 501));
        assert!(ws_refresh_required(exp, Some(iat), exp - 500));
        // Unlike `refresh_required`, an already-expired token is also
        // "refresh required" — this is the bug fix under test: without it,
        // an expired workspace token would be silently treated as fine.
        assert!(ws_refresh_required(exp, Some(iat), exp));
        assert!(ws_refresh_required(exp, Some(iat), exp + 1));
    }

    #[test]
    fn refresh_required_falls_back_to_six_hours_without_iat() {
        let exp = 2_000_000;
        assert!(!refresh_required(exp, None, exp - SIX_HOURS_SECS - 1));
        assert!(refresh_required(exp, None, exp - SIX_HOURS_SECS));
    }
}

