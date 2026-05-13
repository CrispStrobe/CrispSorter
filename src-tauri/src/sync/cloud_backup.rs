//! P13.7 Step 5 — HTTP wire client for the cloud-backup VPS API.
//!
//! Lives alongside the P11 [`super::SyncManager`] which targets
//! `crisp-index-server`; this module is the sibling target for the
//! new cloud-backup HTTP surface.  Both share the same outbox
//! state-KV (the `sync_state` table) so watermarks for both
//! backends can be inspected via one `sync_status` call.
//!
//! Wire shape matches `../../cloud-backup/api/app.py` exactly —
//! see that file for the pydantic models the routes accept.  Any
//! drift here is a protocol break and shows up as 422 from
//! FastAPI's pydantic validation.
//!
//! ## Wire endpoints
//!
//! | route                              | direction | purpose                       |
//! |------------------------------------|-----------|-------------------------------|
//! | `POST /api/manifest/push`          | up        | upsert source-file batch      |
//! | `GET  /api/manifest/pull?since=…`  | down      | rows newer than `since`       |
//! | `POST /api/index/push-embeddings`  | up        | store text/vector embeddings  |
//! | `GET  /api/index/by-embedding?…`   | query     | brute-force k-NN              |
//! | `GET  /api/health`                 | probe     | unauthenticated health check  |
//!
//! ## Authentication
//!
//! Every authenticated request carries
//! `Authorization: Bearer <key>`.  Keys are minted on the VPS via
//! `python -m api.admin mint` (see `../../cloud-backup/api/admin.py`)
//! and stored client-side in the OS keychain through
//! [`super::secret`].

use anyhow::{Context, Result};
use reqwest::header::AUTHORIZATION;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const PUSH_PATH: &str = "/api/manifest/push";
const PULL_PATH: &str = "/api/manifest/pull";
const EMBED_PUSH_PATH: &str = "/api/index/push-embeddings";
const BY_EMBED_PATH: &str = "/api/index/by-embedding";
const HEALTH_PATH: &str = "/api/health";

/// Single row of the manifest-push payload.  Field names match the
/// FastAPI `ManifestRow` model exactly; reordering or renaming
/// requires a protocol bump.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestRow {
    pub path:       String,
    pub size_bytes: i64,
    pub sha256:     String,
    pub mtime_unix: f64,
    /// Hint only — the server overwrites with the authenticated
    /// key's owner_id (per-owner scoping).  Sending a stable value
    /// in shared-catalog mode keeps cross-client provenance tidy.
    pub owner_id:   String,
    pub filename:   String,
    pub ext:        String,
    pub parent_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year:       Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestPushRequest<'a> {
    pub rows: &'a [ManifestRow],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestPushResponse {
    pub accepted: usize,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Row shape returned by `/api/manifest/pull`.  Maps onto
/// `L1FileEntry` 1:1 in the apply path (`tauri_commands.rs`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PullRow {
    pub path:       String,
    pub size_bytes: i64,
    pub sha256:     String,
    pub mtime_unix: f64,
    pub owner_id:   String,
    pub filename:   String,
    pub ext:        String,
    pub parent_dir: String,
    #[serde(default)]
    pub language:   Option<String>,
    #[serde(default)]
    pub title:      Option<String>,
    #[serde(default)]
    pub author:     Option<String>,
    #[serde(default)]
    pub year:       Option<i32>,
    pub indexed_at: i64,
    #[serde(default)]
    pub archived_in: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestPullResponse {
    pub rows: Vec<PullRow>,
    pub max_indexed_at: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingRow {
    pub doc_id:      String,
    pub chunk_index: i32,
    pub embedding:   Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sparse_json: Option<String>,
    pub model_id:    String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingPushRequest<'a> {
    pub rows: &'a [EmbeddingRow],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingPushResponse {
    pub accepted: usize,
    #[serde(default)]
    pub rejected: usize,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ByEmbeddingHit {
    pub doc_id:      String,
    pub chunk_index: i32,
    pub model_id:    String,
    pub distance:    f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ByEmbeddingResponse {
    pub rows: Vec<ByEmbeddingHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: String,
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub shared_catalog: bool,
}

/// Thin async wrapper over `reqwest::Client` that carries the
/// server's base URL + bearer token.  Cheap to construct per
/// operation; reqwest pools connections internally.
#[derive(Debug, Clone)]
pub struct CloudBackupClient {
    base_url: String,
    api_key:  String,
    client:   Client,
}

impl CloudBackupClient {
    /// Build a client with sensible defaults: 30 s timeout, default
    /// reqwest connection-pool settings.  `base_url` is trimmed
    /// of any trailing `/` so concatenation with path constants
    /// never produces `//api/...`.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("building reqwest client")?;
        let url = base_url.into().trim_end_matches('/').to_string();
        Ok(Self { base_url: url, api_key: api_key.into(), client })
    }

    /// Construct with an explicit pre-built client (used in unit
    /// tests so we can wire a mockito server's address in).
    pub fn with_client(base_url: impl Into<String>, api_key: impl Into<String>, client: Client) -> Self {
        let url = base_url.into().trim_end_matches('/').to_string();
        Self { base_url: url, api_key: api_key.into(), client }
    }

    pub fn base_url(&self) -> &str { &self.base_url }

    fn endpoint(&self, suffix: &str) -> String {
        format!("{}{}", self.base_url, suffix)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// `GET /api/health`.  Public, no auth required; used by the
    /// status surface.
    pub async fn health(&self) -> Result<HealthResponse> {
        let resp = self.client
            .get(self.endpoint(HEALTH_PATH))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("health: send")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("health: HTTP {status}");
        }
        Ok(resp.json::<HealthResponse>().await.context("health: parse body")?)
    }

    /// `POST /api/manifest/push`.  Uploads a batch of source-file
    /// rows.  Empty `rows` is allowed (server returns `accepted=0`).
    pub async fn manifest_push(&self, rows: &[ManifestRow]) -> Result<ManifestPushResponse> {
        let body = ManifestPushRequest { rows, cursor: None };
        let resp = self.client
            .post(self.endpoint(PUSH_PATH))
            .header(AUTHORIZATION, self.auth_header())
            .json(&body)
            .send()
            .await
            .context("manifest_push: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("manifest_push: HTTP {status}: {body}");
        }
        Ok(resp.json::<ManifestPushResponse>().await.context("manifest_push: parse body")?)
    }

    /// `GET /api/manifest/pull?since=…&limit=…`.  Returns rows with
    /// `indexed_at > since`.  `since=0` for an initial pull.
    pub async fn manifest_pull(&self, since: i64, limit: usize) -> Result<ManifestPullResponse> {
        // Hand-roll the query string — `reqwest::RequestBuilder::query`
        // depends on `serde_urlencoded` which isn't on our reqwest
        // feature set; both params here are i64 / usize and need no
        // url-escaping, so plain `format!` is correct.
        let url = format!(
            "{}{}?since={}&limit={}",
            self.base_url, PULL_PATH, since, limit
        );
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("manifest_pull: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("manifest_pull: HTTP {status}: {body}");
        }
        Ok(resp.json::<ManifestPullResponse>().await.context("manifest_pull: parse body")?)
    }

    /// `POST /api/index/push-embeddings`.  Per-row rejections (e.g.
    /// pack failure) show up in the response's `errors` list but
    /// don't fail the whole batch.
    pub async fn embeddings_push(&self, rows: &[EmbeddingRow]) -> Result<EmbeddingPushResponse> {
        let body = EmbeddingPushRequest { rows, cursor: None };
        let resp = self.client
            .post(self.endpoint(EMBED_PUSH_PATH))
            .header(AUTHORIZATION, self.auth_header())
            .json(&body)
            .send()
            .await
            .context("embeddings_push: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("embeddings_push: HTTP {status}: {body}");
        }
        Ok(resp.json::<EmbeddingPushResponse>().await.context("embeddings_push: parse body")?)
    }

    /// `GET /api/index/by-embedding?vec=…&k=…&model=…`.  `vec`
    /// elements are serialised as comma-separated decimals (the
    /// FastAPI route parses them via `vec.split(",")`).
    pub async fn by_embedding(
        &self,
        vec: &[f32],
        k: usize,
        model: Option<&str>,
    ) -> Result<ByEmbeddingResponse> {
        let vec_str = vec
            .iter()
            .map(|f| format!("{f}"))
            .collect::<Vec<_>>()
            .join(",");
        // `vec_str` may contain `,` and `.`/`-` — none of which need
        // percent-encoding in a query string per RFC 3986; the
        // `model_id` token similarly is ASCII alnum + `-` / `_` by
        // convention.  Skipping the percent-encode keeps the wire
        // pretty + readable in nginx logs.
        let mut url = format!(
            "{}{}?vec={}&k={}",
            self.base_url, BY_EMBED_PATH, vec_str, k
        );
        if let Some(m) = model {
            url.push_str("&model=");
            url.push_str(m);
        }
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("by_embedding: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("by_embedding: HTTP {status}: {body}");
        }
        Ok(resp.json::<ByEmbeddingResponse>().await.context("by_embedding: parse body")?)
    }
}

#[cfg(test)]
mod tests {
    //! Mock-server unit tests for every cloud-backup wire command.
    //!
    //! Uses `mockito` to spin up an in-process HTTP server per
    //! test; never touches a real network or VPS.  Covers the
    //! 200/4xx/5xx paths the spec calls out as required.

    use super::*;
    use mockito::{Matcher, Server};

    fn client_for(server: &Server) -> CloudBackupClient {
        CloudBackupClient::new(server.url(), "cbk_test_key").unwrap()
    }

    // ── manifest_push ────────────────────────────────────────────────

    #[tokio::test]
    async fn manifest_push_200_returns_accepted_count() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .match_header("authorization", "Bearer cbk_test_key")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"accepted": 3}"#)
            .create_async()
            .await;

        let cli = client_for(&server);
        let rows = vec![sample_row("a"), sample_row("b"), sample_row("c")];
        let resp = cli.manifest_push(&rows).await.unwrap();
        assert_eq!(resp.accepted, 3);
        assert_eq!(resp.next_cursor, None);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_200_with_cursor_propagates() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(200)
            .with_body(r#"{"accepted": 200, "next_cursor": "page_2"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.manifest_push(&[sample_row("x")]).await.unwrap();
        assert_eq!(resp.accepted, 200);
        assert_eq!(resp.next_cursor.as_deref(), Some("page_2"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_400_returns_error_with_body() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(400)
            .with_body(r#"{"detail": "bad payload"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_push(&[]).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("HTTP 400"), "got: {msg}");
        assert!(msg.contains("bad payload"), "got: {msg}");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_401_returns_unauthorised() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(401)
            .with_body(r#"{"detail": "invalid Bearer token"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_push(&[]).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 401"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_500_returns_server_error() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(500)
            .with_body("server panic")
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_push(&[sample_row("a")]).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 500"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_503_returns_backpressure() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(503)
            .with_body("busy")
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_push(&[sample_row("a")]).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 503"));
        m.assert_async().await;
    }

    // ── manifest_pull ────────────────────────────────────────────────

    #[tokio::test]
    async fn manifest_pull_200_parses_rows_and_watermark() {
        let mut server = Server::new_async().await;
        let body = r#"
            {
                "rows": [
                    {"path":"/a.pdf","size_bytes":10,"sha256":"aaa",
                     "mtime_unix":1.0,"owner_id":"o","filename":"a.pdf",
                     "ext":"pdf","parent_dir":"/","indexed_at":100}
                ],
                "max_indexed_at": 100,
                "has_more": false
            }
        "#;
        let m = server.mock("GET", PULL_PATH)
            .match_query(Matcher::Regex("since=0&limit=200".into()))
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.manifest_pull(0, 200).await.unwrap();
        assert_eq!(resp.rows.len(), 1);
        assert_eq!(resp.rows[0].path, "/a.pdf");
        assert_eq!(resp.max_indexed_at, 100);
        assert!(!resp.has_more);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_pull_401_propagates_auth_failure() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", PULL_PATH)
            .match_query(Matcher::Any)
            .with_status(401)
            .with_body(r#"{"detail":"invalid"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_pull(0, 50).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 401"));
        m.assert_async().await;
    }

    // ── embeddings_push ──────────────────────────────────────────────

    #[tokio::test]
    async fn embeddings_push_200_with_per_row_errors() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", EMBED_PUSH_PATH)
            .with_status(200)
            .with_body(r#"{"accepted": 2, "rejected": 1, "errors": ["doc:1: dim mismatch"]}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.embeddings_push(&[]).await.unwrap();
        assert_eq!(resp.accepted, 2);
        assert_eq!(resp.rejected, 1);
        assert_eq!(resp.errors.len(), 1);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn embeddings_push_500_returns_error() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", EMBED_PUSH_PATH)
            .with_status(500)
            .with_body("internal")
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.embeddings_push(&[]).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 500"));
        m.assert_async().await;
    }

    // ── by_embedding ─────────────────────────────────────────────────

    #[tokio::test]
    async fn by_embedding_200_parses_hits() {
        let mut server = Server::new_async().await;
        let body = r#"
            {"rows":[
                {"doc_id":"a","chunk_index":0,"model_id":"bge","distance":0.01},
                {"doc_id":"b","chunk_index":0,"model_id":"bge","distance":0.5}
            ]}
        "#;
        let m = server.mock("GET", BY_EMBED_PATH)
            .match_query(Matcher::Regex("vec=1,0&k=20&model=bge".into()))
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.by_embedding(&[1.0, 0.0], 20, Some("bge")).await.unwrap();
        assert_eq!(resp.rows.len(), 2);
        assert_eq!(resp.rows[0].doc_id, "a");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn by_embedding_400_malformed_vec_propagates() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", BY_EMBED_PATH)
            .match_query(Matcher::Any)
            .with_status(400)
            .with_body(r#"{"detail":"vec must be comma-separated floats"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.by_embedding(&[1.0], 1, None).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 400"));
        m.assert_async().await;
    }

    // ── health ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_200_parses_response() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", HEALTH_PATH)
            .with_status(200)
            .with_body(r#"{"ok":true,"version":"0.1.0","backend":"cloud-backup-api","shared_catalog":false}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let h = cli.health().await.unwrap();
        assert!(h.ok);
        assert_eq!(h.version, "0.1.0");
        assert_eq!(h.backend, "cloud-backup-api");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn health_5xx_returns_error() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", HEALTH_PATH)
            .with_status(503)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.health().await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 503"));
        m.assert_async().await;
    }

    // ── builder ─────────────────────────────────────────────────────

    #[test]
    fn new_strips_trailing_slash_on_base_url() {
        let c = CloudBackupClient::new("http://localhost:7869/", "k").unwrap();
        assert_eq!(c.base_url(), "http://localhost:7869");
    }

    fn sample_row(suffix: &str) -> ManifestRow {
        ManifestRow {
            path: format!("/data/{suffix}.pdf"),
            size_bytes: 100,
            sha256: format!("{suffix:0>64}"),
            mtime_unix: 1_700_000_000.0,
            owner_id: "owner".into(),
            filename: format!("{suffix}.pdf"),
            ext: "pdf".into(),
            parent_dir: "/data".into(),
            language: None,
            title: None,
            author: None,
            year: None,
        }
    }
}

// ── Env-gated live tests against a real cloud-backup VPS ────────────────
//
// Mirrors `src-tauri/src/drives/webdav.rs:570-625` — the canonical
// pattern.  Skipped silently when the env vars aren't set, so a
// default `cargo test` stays offline.  Run with:
//
//     CB_SYNC_TEST_URL=http://localhost:7869 \
//     CB_SYNC_TEST_API_KEY=cbk_... \
//         cargo test -p tauri-app --lib --no-default-features --ignored cb_sync_live
//
#[cfg(test)]
mod live_tests {
    use super::*;

    fn read_env() -> Option<(String, String)> {
        let url = std::env::var("CB_SYNC_TEST_URL").ok()?;
        let key = std::env::var("CB_SYNC_TEST_API_KEY").ok()?;
        if url.is_empty() || key.is_empty() { None } else { Some((url, key)) }
    }

    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_health_round_trip() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_health_round_trip: \
                CB_SYNC_TEST_URL / CB_SYNC_TEST_API_KEY not set");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();
        let h = cli.health().await.expect("health probe");
        assert!(h.ok);
    }

    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_manifest_push_pull_round_trip() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_manifest_push_pull_round_trip: \
                CB_SYNC_TEST_URL / CB_SYNC_TEST_API_KEY not set");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();
        let unique = format!("{}-{}", std::process::id(),
                             std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default().as_millis());
        let path = format!("/test/live-{unique}.pdf");
        let sha = format!("{unique:0>64}");
        let row = ManifestRow {
            path: path.clone(), size_bytes: 42, sha256: sha.clone(),
            mtime_unix: 1.0, owner_id: "live-test".into(),
            filename: format!("live-{unique}.pdf"), ext: "pdf".into(),
            parent_dir: "/test".into(),
            language: None, title: None, author: None, year: None,
        };
        let pushed = cli.manifest_push(std::slice::from_ref(&row)).await
            .expect("manifest_push");
        assert!(pushed.accepted >= 1);

        // Pull and assert the row makes it back.
        let pulled = cli.manifest_pull(0, 500).await.expect("manifest_pull");
        assert!(
            pulled.rows.iter().any(|r| r.sha256 == sha),
            "live round-trip: pushed sha {sha} not found in {} pulled rows",
            pulled.rows.len()
        );
    }

    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_embedding_push_rejects_empty() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_embedding_push_rejects_empty");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();
        let row = EmbeddingRow {
            doc_id: "live-empty".into(),
            chunk_index: 0,
            embedding: vec![],
            sparse_json: None,
            model_id: "test".into(),
        };
        let resp = cli.embeddings_push(std::slice::from_ref(&row)).await
            .expect("embeddings_push");
        assert!(resp.rejected >= 1);
        assert!(!resp.errors.is_empty());
    }
}
