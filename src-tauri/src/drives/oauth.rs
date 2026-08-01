//! Browser OAuth for public desktop clients.
//!
//! The app is a public client: PKCE protects the authorization-code exchange
//! and no client secret is accepted or bundled. The loopback listener handles
//! the callback in a short-lived background thread and writes tokens straight
//! to the OS keychain.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use super::secret::{self, DriveCredentials};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Google,
    Microsoft,
}

impl Provider {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "google" | "google_drive" => Ok(Self::Google),
            "microsoft" | "onedrive" => Ok(Self::Microsoft),
            _ => Err(anyhow!("unsupported OAuth provider: {value}")),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StartResult {
    pub authorization_url: String,
    pub redirect_uri: String,
}

fn escape(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn start(drive_id: String, provider: Provider, client_id: String) -> Result<StartResult> {
    if client_id.trim().is_empty() {
        return Err(anyhow!("OAuth client ID is required"));
    }
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).context("binding OAuth loopback callback")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
    let state = uuid::Uuid::new_v4().to_string();
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let (authorize, scope) = match provider {
        Provider::Google => (
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://www.googleapis.com/auth/drive",
        ),
        Provider::Microsoft => (
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            "offline_access Files.ReadWrite.All User.Read",
        ),
    };
    let url = format!(
        "{authorize}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        escape(&client_id), escape(&redirect_uri), escape(scope), escape(&state), escape(&challenge)
    );
    let callback_redirect_uri = redirect_uri.clone();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = [0u8; 8192];
        let Ok(size) = stream.read(&mut request) else {
            return;
        };
        let line = String::from_utf8_lossy(&request[..size]);
        let Some(target) = line
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|line| line.split_whitespace().next())
        else {
            return;
        };
        let query = target.split('?').nth(1).unwrap_or("");
        let mut code = None;
        let mut returned_state = None;
        let mut error = None;
        for pair in query.split('&') {
            let mut it = pair.splitn(2, '=');
            match (it.next(), it.next()) {
                (Some("code"), Some(value)) => code = Some(value.to_owned()),
                (Some("state"), Some(value)) => returned_state = Some(value.to_owned()),
                (Some("error"), Some(value)) => error = Some(value.to_owned()),
                _ => {}
            }
        }
        let message = if error.is_some() || returned_state.as_deref() != Some(state.as_str()) {
            "Authentication was cancelled or rejected. You can close this window."
        } else if let Some(code) = code {
            match exchange(
                provider,
                &client_id,
                &callback_redirect_uri,
                &verifier,
                &code,
                &drive_id,
            ) {
                Ok(()) => "Authentication completed. You can close this window.",
                Err(_) => "Authentication failed. You can close this window.",
            }
        } else {
            "Authentication did not return a code. You can close this window."
        };
        let body = format!("<html><body><p>{message}</p></body></html>");
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        let _ = stream.write_all(response.as_bytes());
    });
    Ok(StartResult {
        authorization_url: url,
        redirect_uri,
    })
}

fn exchange(
    provider: Provider,
    client_id: &str,
    redirect_uri: &str,
    verifier: &str,
    code: &str,
    drive_id: &str,
) -> Result<()> {
    let endpoint = match provider {
        Provider::Google => "https://oauth2.googleapis.com/token",
        Provider::Microsoft => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
    };
    let body = format!(
        "client_id={}&code={}&redirect_uri={}&grant_type=authorization_code&code_verifier={}",
        escape(client_id),
        escape(code),
        escape(redirect_uri),
        escape(verifier)
    );
    let response: serde_json::Value = reqwest::blocking::Client::new()
        .post(endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .context("OAuth token request")?
        .error_for_status()
        .context("OAuth token response")?
        .json()
        .context("decoding OAuth token response")?;
    let access = response
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("OAuth response did not contain an access token"))?;
    let refresh = response
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let mut credentials = secret::get_credentials(drive_id)?.unwrap_or_default();
    credentials.access_token = Some(access.to_owned());
    if refresh.is_some() {
        credentials.refresh_token = refresh;
    }
    credentials.client_id = Some(client_id.to_owned());
    secret::set_credentials(drive_id, &DriveCredentials { ..credentials })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_provider_and_escape_are_deterministic() {
        assert_eq!(Provider::parse("onedrive").unwrap(), Provider::Microsoft);
        assert_eq!(escape("a b+/"), "a%20b%2B%2F");
    }
}
