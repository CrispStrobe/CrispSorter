//! Read-only Wallabag import into the local search index.
//! Credentials and OAuth tokens are held only for the duration of a sync.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Emitter, State, Manager};
use std::collections::{HashMap, HashSet};
use crate::{AppState, index::RawDocument};

#[derive(Deserialize)]
pub struct Credentials {
    instance: String,
    client_id: String,
    client_secret: String,
    username: String,
    password: String,
}
#[derive(Deserialize)]
struct Token { access_token: String }
#[derive(Deserialize, Serialize)]
struct Tag { #[serde(default)] label: String }
#[derive(Deserialize, Serialize)]
struct Entry {
    id: u64,
    #[serde(default)] title: Option<String>,
    #[serde(default)] content: Option<String>,
    #[serde(default)] url: Option<String>,
    #[serde(default)] tags: Vec<Tag>,
    #[serde(default)] language: Option<String>,
    #[serde(default, deserialize_with = "wallabag_bool")] is_archived: bool,
    #[serde(default, deserialize_with = "wallabag_bool")] is_starred: bool,
}
fn wallabag_bool<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value { Bool(bool), Integer(i64) }
    Ok(match Value::deserialize(deserializer)? { Value::Bool(b) => b, Value::Integer(n) => n != 0 })
}
#[derive(Deserialize)]
struct Embedded { items: Vec<Entry> }
#[derive(Deserialize)]
struct Page { _embedded: Embedded, pages: u64 }
#[derive(Serialize, Clone)]
pub struct Progress { page: u64, pages: u64, indexed: u64, skipped: u64 }

fn digest(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

fn base_url(input: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(input.trim()).map_err(|_| "Invalid Wallabag instance URL")?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
        || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("Use an HTTPS instance URL (HTTP is allowed only for localhost), without credentials, query or fragment".into());
    }
    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

fn make_document(base: &reqwest::Url, username: &str, entry: &Entry) -> Result<RawDocument, String> {
    let id = digest(format!("wallabag\0{}\0{}\0{}", base, username, entry.id).as_bytes());
    let html = entry.content.as_deref().unwrap_or("");
    let body = html2text::from_read(html.as_bytes(), 120).map_err(|_| "Could not extract article text")?;
    // Retain title-only entries as searchable documents too.
    let text = if body.trim().is_empty() { entry.title.clone().unwrap_or_else(|| format!("Wallabag article {}", entry.id)) } else { body };
    let mut tags: Vec<String> = entry.tags.iter().map(|t| t.label.clone()).filter(|t| !t.is_empty()).collect();
    if entry.is_archived { tags.push("wallabag:archived".into()); }
    if entry.is_starred { tags.push("wallabag:starred".into()); }
    Ok(RawDocument {
        full_text: text.clone(), full_text_md: text, headings: vec![], title: entry.title.clone(), author: None, year: None,
        filename: format!("wallabag-{}.html", entry.id), ext: "html".into(), language: entry.language.clone().unwrap_or_default(),
        source_hash: id, location_uri: format!("{}view/{}", base, entry.id), owner_id: uuid::Uuid::nil().to_string(),
        tags,
        mtime_unix: None, file_size: None, volume_id: None, parent_dir: None,
        translated_text: None, translated_to_lang: None, audio_duration_seconds: None, audio_codec: None,
        audio_sample_rate_hz: None, audio_channels: None, audio_bitrate_kbps: None, image_camera_make: None,
        image_camera_model: None, image_lens_model: None, image_taken_at_unix: None, image_iso: None,
        multivec_packed: None, multivec_n_tokens: None, url: entry.url.clone(), embedding_omni: None, embedding_vit: None,
        container: None,
        // Articles are text-only, and the image backfill only revisits files in
        // watched folders, never these URL-addressed rows.
        images_indexed: false,
    })
}

async fn sign_in(client: &reqwest::Client, base: &reqwest::Url, credentials: &Credentials) -> Result<Token, String> {
    client.post(base.join("oauth/v2/token").map_err(|e| e.to_string())?)
        .form(&[("grant_type", "password"), ("client_id", credentials.client_id.as_str()),
            ("client_secret", credentials.client_secret.as_str()), ("username", credentials.username.as_str()),
            ("password", credentials.password.as_str())])
        .send().await.map_err(|e| format!("Wallabag connection failed: {e}"))?
        .error_for_status().map_err(|e| format!("Wallabag sign-in failed: {e}"))?
        .json().await.map_err(|_| "Invalid Wallabag sign-in response".into())
}

async fn fetch_page(client: &reqwest::Client, base: &reqwest::Url, credentials: &Credentials, token: &mut Token, next: u64) -> Result<Page, String> {
    let request = |token: &Token| client.get(base.join("api/entries.json").expect("validated base URL"))
        .bearer_auth(&token.access_token).query(&[("page", next.to_string()), ("perPage", "100".into()), ("sort", "created".into()), ("order", "asc".into())]);
    let mut response = request(token).send().await.map_err(|e| format!("Wallabag page {next}: {e}"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        *token = sign_in(client, base, credentials).await?;
        response = request(token).send().await.map_err(|e| format!("Wallabag page {next}: {e}"))?;
    }
    response.error_for_status().map_err(|e| format!("Wallabag page {next}: {e}"))?
        .json().await.map_err(|e| format!("Invalid Wallabag page {next}: {e}"))
}

/// Fetch all pages; repeated imports update stable article IDs. Interrupted
/// imports can be retried; unchanged articles already in the index are skipped.
#[tauri::command]
pub async fn wallabag_sync(app: tauri::AppHandle, state: State<'_, AppState>, credentials: Credentials) -> Result<Progress, String> {
    // Counts and stages only: never the instance URL, username, tokens or
    // article titles. The failing stage is carried by the failure receipt.
    let log = crate::operation_log::OperationLog::start("wallabag.sync",
        serde_json::json!({"kind":"document","backend":"remote","surface":"native"}));
    let result = wallabag_sync_inner(&app, &state, &credentials, &log).await;
    match &result {
        Ok(progress) => log.succeed(serde_json::json!({"pages": progress.pages,
            "completed": progress.indexed, "skipped": progress.skipped})),
        Err(_) => log.fail("wallabag_sync_failed"),
    }
    result
}

async fn wallabag_sync_inner(app: &tauri::AppHandle, state: &AppState, credentials: &Credentials,
    log: &crate::operation_log::OperationLog) -> Result<Progress, String> {
    static SYNC: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    log.step("acquiring", serde_json::Value::Null);
    let _guard = SYNC.try_lock().map_err(|_| "A Wallabag import is already running")?;
    log.step("validating", serde_json::Value::Null);
    let base = base_url(&credentials.instance)?;
    let pipeline = {
        let lock = state.index.lock().await;
        if !lock.config.enabled { return Err("Enable and initialise the local Search Index first".into()); }
        lock.pipeline.clone().ok_or("Initialise the local Search Index first")?
    };
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(90)).build().map_err(|e| e.to_string())?;
    log.step("signing_in", serde_json::Value::Null);
    let mut token = sign_in(&client, &base, credentials).await?;
    log.step("fetching", serde_json::Value::Null);
    let cache_path = app.path().app_data_dir().map_err(|e| e.to_string())?.join("wallabag-sync.sqlite");
    if let Some(parent) = cache_path.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
    {
        let db = rusqlite::Connection::open(&cache_path).map_err(|e| e.to_string())?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS articles (id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL)").map_err(|e| e.to_string())?;
    }
    let mut progress = Progress { page: 0, pages: 1, indexed: 0, skipped: 0 };
    while progress.page < progress.pages {
        let next = progress.page + 1;
        let page = fetch_page(&client, &base, credentials, &mut token, next).await?;
        if page.pages > 1_000_000 || (page.pages >= next && page._embedded.items.is_empty() && next < page.pages) {
            return Err("Wallabag returned inconsistent pagination; import stopped".into());
        }
        let mut prepared = Vec::new();
        for entry in &page._embedded.items {
            let raw = make_document(&base, &credentials.username, entry)?;
            let fingerprint = digest(&serde_json::to_vec(entry).map_err(|e| e.to_string())?);
            prepared.push((raw, fingerprint));
        }
        let ids: Vec<String> = prepared.iter().map(|(r, _)| r.source_hash.clone()).collect();
        let mut present = HashSet::new();
        for batch in pipeline.vector.fetch_by_doc_ids(&ids).await.map_err(|e| e.to_string())? {
            if let Some(col) = batch.column_by_name("doc_id").and_then(|c| c.as_any().downcast_ref::<arrow_array::StringArray>()) {
                for id in col.iter().flatten() { present.insert(id.to_owned()); }
            }
        }
        let fingerprints: HashMap<String, String> = {
            let db = rusqlite::Connection::open(&cache_path).map_err(|e| e.to_string())?;
            use rusqlite::OptionalExtension;
            let mut stmt = db.prepare("SELECT fingerprint FROM articles WHERE id = ?1").map_err(|e| e.to_string())?;
            let mut found = HashMap::new();
            for id in &ids {
                let value: Option<String> = stmt.query_row([id], |row| row.get(0)).optional().map_err(|e| e.to_string())?;
                if let Some(value) = value { found.insert(id.clone(), value); }
            }
            found
        };
        let mut new_docs = Vec::new();
        let mut completed = Vec::new();
        for (raw, fingerprint) in prepared {
            let id = raw.source_hash.clone();
            if present.contains(&id) && fingerprints.get(&id) == Some(&fingerprint) {
                progress.skipped += 1;
            } else if present.contains(&id) {
                pipeline.reingest_document(raw).await.map_err(|e| format!("Updating article: {e}"))?;
                completed.push((id, fingerprint));
                progress.indexed += 1;
            } else {
                completed.push((id, fingerprint));
                new_docs.push(raw);
                progress.indexed += 1;
            }
        }
        pipeline.ingest_documents_batch(new_docs).await.map_err(|e| format!("Indexing articles: {e}"))?;
        {
            let mut db = rusqlite::Connection::open(&cache_path).map_err(|e| e.to_string())?;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            for (id, fingerprint) in completed {
                tx.execute("INSERT OR REPLACE INTO articles (id, fingerprint) VALUES (?1, ?2)", rusqlite::params![id, fingerprint]).map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        progress.page = next;
        progress.pages = page.pages.max(next);
        log.step("page_indexed", serde_json::json!({"page": progress.page, "pages": progress.pages,
            "completed": progress.indexed, "skipped": progress.skipped}));
        let _ = app.emit("wallabag://progress", &progress);
    }
    Ok(progress)
}

#[tauri::command]
pub async fn wallabag_read(state: State<'_, AppState>, doc_id: String) -> Result<String, String> {
    let local = state.index.lock().await.local.clone().ok_or("Local index is not initialised")?;
    local.fetch_article_text(&doc_id).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_instance_and_supports_subpaths() {
        let b = base_url("https://example.org/wallabag").unwrap();
        assert_eq!(b.join("api/entries.json").unwrap().as_str(), "https://example.org/wallabag/api/entries.json");
        assert!(base_url("http://example.org/").is_err());
        assert!(base_url("https://user:pass@example.org/").is_err());
        assert!(base_url("http://127.0.0.1:8000/").is_ok());
    }
    #[tokio::test]
    async fn oauth_and_paginated_fetch_retry_expired_tokens() {
        use mockito::Matcher;
        let mut server = mockito::Server::new_async().await;
        let auth = server.mock("POST", "/oauth/v2/token").match_body(Matcher::AllOf(vec![
            Matcher::UrlEncoded("grant_type".into(), "password".into()),
            Matcher::UrlEncoded("username".into(), "reader".into()),
        ])).with_status(200).with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"fresh"}"#).create_async().await;
        let expired = server.mock("GET", "/api/entries.json").match_header("authorization", "Bearer expired")
            .match_query(Matcher::UrlEncoded("page".into(), "2".into()))
            .with_status(401).create_async().await;
        let page = server.mock("GET", "/api/entries.json").match_header("authorization", "Bearer fresh")
            .match_query(Matcher::UrlEncoded("page".into(), "2".into()))
            .with_status(200).with_header("content-type", "application/json")
            .with_body(r#"{"pages":2,"_embedded":{"items":[{"id":42,"title":"Article"}]}}"#).create_async().await;
        let base = base_url(&server.url()).unwrap();
        let creds = Credentials { instance: server.url(), client_id: "client".into(), client_secret: "secret".into(), username: "reader".into(), password: "password".into() };
        let mut token = Token { access_token: "expired".into() };
        let result = fetch_page(&reqwest::Client::new(), &base, &creds, &mut token, 2).await.unwrap();
        assert_eq!(result.pages, 2);
        assert_eq!(result._embedded.items[0].id, 42);
        auth.assert_async().await; expired.assert_async().await; page.assert_async().await;
    }

    #[tokio::test]
    async fn articles_can_be_searched_read_and_updated_without_vectors() {
        use std::sync::Arc;
        use crate::index::{FtsIndex, LocalIndex, IngestPipeline, IngestConfig};
        use crate::index::schema::SearchFilters;
        let dir = tempfile::tempdir().unwrap();
        let fts = Arc::new(FtsIndex::open_or_create(&dir.path().join("fts")).unwrap());
        let local = Arc::new(LocalIndex::open_or_create(dir.path(), 4).await.unwrap());
        let pipeline = IngestPipeline::new(fts.clone(), local.clone(), None, IngestConfig { text_only: true, ..Default::default() });
        let base = base_url("https://example.org/").unwrap();
        let mut entry: Entry = serde_json::from_str(r#"{"id":17,"title":"A title","content":"<p>Rareword first body</p>","url":"https://example.net/a","tags":[{"label":"topic"}]}"#).unwrap();
        let raw = make_document(&base, "alice", &entry).unwrap();
        pipeline.ingest_document(raw.clone()).await.unwrap();
        assert_eq!(fts.search("rareword", &SearchFilters::default(), 10).unwrap().len(), 1);
        assert!(local.fetch_article_text(&crate::index::ingest::doc_id_for(&raw)).await.unwrap().contains("Rareword first body"));
        entry.content = Some("<p>Replacementterm updated body</p>".into());
        let updated = make_document(&base, "alice", &entry).unwrap();
        assert_eq!(raw.source_hash, updated.source_hash);
        pipeline.reingest_document(updated).await.unwrap();
        assert_eq!(fts.search("rareword", &SearchFilters::default(), 10).unwrap().len(), 0);
        assert_eq!(fts.search("replacementterm", &SearchFilters::default(), 10).unwrap().len(), 1);
        assert!(local.fetch_article_text(&crate::index::ingest::doc_id_for(&raw)).await.unwrap().contains("Replacementterm"));
    }

    #[test]
    fn article_identity_is_stable_and_metadata_is_preserved() {
        let b = base_url("https://example.org/").unwrap();
        let entry: Entry = serde_json::from_str(r#"{"id":42,"title":"Example","content":"<p>Hello world</p>","url":"https://example.net/story","tags":[{"label":"reading"}],"is_archived":1,"is_starred":true}"#).unwrap();
        let a = make_document(&b, "alice", &entry).unwrap();
        let again = make_document(&b, "alice", &entry).unwrap();
        assert_eq!(a.source_hash, again.source_hash);
        assert_ne!(a.source_hash, make_document(&b, "bob", &entry).unwrap().source_hash);
        assert!(a.full_text.contains("Hello world"));
        assert_eq!(a.tags, vec!["reading", "wallabag:archived", "wallabag:starred"]);
        assert_eq!(a.url.as_deref(), Some("https://example.net/story"));
    }
}
