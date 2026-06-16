/// Tauri commands for the CrispSorter frontend → index integration.
///
/// ## Local backend path
///
///   `index_init`             → builds LocalIndex + FtsIndex + Embedder + SearchEngine + Pipeline
///   `index_ingest_document`  → `IngestPipeline::ingest_document` (chunk → embed → write both)
///   `index_search`           → `SearchEngine` (FTS + ANN + RRF)
///   `index_build_ivf_pq`        → `LocalIndex::build_vector_index`
///   `index_build_scalar_index`  → `LocalIndex::build_scalar_index` (BTree on parent_dir)
///
/// ## Remote backend path
///
///   `index_init`             → builds Embedder (locally) + RemoteClient
///   `index_ingest_document`  → chunk + embed locally → push per-chunk via HTTP to server
///   `index_search`           → embed query locally → send text + embedding to server → server does FTS+ANN+RRF
///   `index_build_ivf_pq`     → error (not supported for remote backend)
///   `index_build_scalar_index` → error (not supported for remote backend)
use tauri::{Manager, State};

use super::ingest::{IngestStats, L1FileEntry, RawDocument};
use super::{IndexConfig, IndexState, SearchResult};
use crate::AppState;
use crisp_index_protocol::{IngestBatch as RemoteIngestBatch, IngestChunk as RemoteIngestChunk};

// ── Search ────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn index_search(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    query: String,
    mode: String, // "text" | "vector" | "hybrid"
    limit: usize,
    owner_id: Option<String>,
    // PLAN P7.6 follow-up. When false (the default), drop hits whose
    // recorded `volume_id` isn't in the currently-mounted set —
    // archive drives that aren't plugged in disappear from results
    // until the user mounts them again. Pass `Some(true)` to override
    // (e.g. "show me everything I've ever indexed, including offline
    // drives" — useful for browse / inventory cases). Rows with no
    // `volume_id` always pass through (legacy ingests, frontend-driven
    // ingests, catalog hits).
    include_unmounted: Option<bool>,
) -> Result<Vec<SearchResult>, String> {
    // PLAN P7.4.4 — flag the foreground search so the bg_ingest worker
    // pauses while we run. RAII drops at function return.
    let _fg = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());

    let lock = state.index.lock().await;
    let config_enabled = lock.config.enabled;

    let filters = super::SearchFilters {
        owner_id,
        ..Default::default()
    };

    // ── Documents-table channel ──────────────────────────────────────────
    // Run the existing documents-table search first. When the index is
    // disabled or uninitialized we still want to surface catalog hits, so
    // empty results from this branch are fine — they get appended to (not
    // replace) the catalog channel below.
    let mut results: Vec<SearchResult> = if !config_enabled {
        Vec::new()
    } else if let Some(engine) = lock.engine.clone() {
        // ── Local path: SearchEngine wraps FTS + ANN + RRF ────────────
        drop(lock);
        match mode.as_str() {
            "text" => engine.search_text(&query, &filters, limit).await,
            "vector" => engine.search_vector(&query, &filters, limit).await,
            _ => engine.search_hybrid(&query, &filters, limit).await,
        }
        .map_err(|e| e.to_string())?
    } else {
        // ── Remote path: embed query locally, then call backend ───────
        let backend = lock
            .backend
            .as_ref()
            .ok_or("Index not initialised — call index_init first")?
            .clone();
        let embedder = lock.embedder.clone();
        let config = lock.config.clone();
        drop(lock);
        match mode.as_str() {
            "text" => backend.search_text(&query, &filters, limit).await,
            "vector" => {
                if let Some(embedder) = embedder {
                    let embedding = embed_query(Some(embedder), &query).await?;
                    backend.search_vector(&embedding, &filters, limit).await
                } else {
                    let remote = super::remote_client::RemoteClient::new(
                        config
                            .remote_url
                            .clone()
                            .ok_or("remote_url must be set for Remote backend")?,
                        config.remote_api_key.clone().unwrap_or_default(),
                    );
                    remote.search_vector_server(&query, &filters, limit).await
                }
            }
            _ => {
                if let Some(embedder) = embedder {
                    let embedding = embed_query(Some(embedder), &query).await?;
                    backend
                        .search_hybrid(&query, &embedding, &filters, limit)
                        .await
                } else {
                    let remote = super::remote_client::RemoteClient::new(
                        config
                            .remote_url
                            .clone()
                            .ok_or("remote_url must be set for Remote backend")?,
                        config.remote_api_key.clone().unwrap_or_default(),
                    );
                    remote.search_hybrid_server(&query, &filters, limit).await
                }
            }
        }
        .map_err(|e| e.to_string())?
    };

    // ── Catalog channel (PLAN P6 4c / P7.1) ──────────────────────────────
    // Substring-match across active-catalog filenames. Independent of the
    // index_init state — a user with cataloged drives but no embedding
    // index still gets filename hits. We cap the catalog channel at
    // `limit` total slots, but back off if the documents channel already
    // returned that many — net result is at most `limit` rows from each
    // channel, presented in two visually distinct groups (the frontend
    // uses `catalog_source` to badge them).
    if let Ok(data_dir) = app.path().app_data_dir() {
        let remaining = limit.saturating_sub(results.len()).max(limit / 2);
        if let Ok(hits) =
            crate::catalog::lance::search(&data_dir, &query, Some(remaining)).await
        {
            for hit in hits {
                results.push(catalog_hit_to_search_result(hit));
            }
        }
    }

    // ── Volume-availability filter (PLAN P7.6 follow-up) ─────────────────
    // Hide hits whose recorded volume_id isn't currently mounted, unless
    // the caller opts out. Computed once per call (a single shell-out
    // per platform — see `volume::list_mounted_volumes`). Rows without
    // `volume_id` always pass.
    if !include_unmounted.unwrap_or(false) {
        let mounted_ids: std::collections::HashSet<String> =
            tokio::task::spawn_blocking(crate::volume::list_mounted_volumes)
                .await
                .map_err(|e| format!("list_mounted_volumes join: {e}"))?
                .into_iter()
                .map(|v| v.id)
                .collect();
        results.retain(|r| match &r.volume_id {
            Some(id) => mounted_ids.contains(id),
            None => true,
        });
    }

    Ok(results)
}

/// Synthesise a `SearchResult` from a catalog hit so the documents-table
/// + catalog-table results return as a single homogeneous list. Score is
/// a fixed 0.4 — below typical RRF-fused scores from the documents
/// channel but high enough that catalog hits still display when there
/// are no documents-channel matches at all.
fn catalog_hit_to_search_result(hit: crate::catalog::lance::CatalogHit) -> SearchResult {
    let ext = std::path::Path::new(&hit.entry_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());
    let title = hit.filename.clone().or_else(|| {
        std::path::Path::new(&hit.entry_path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
    });
    SearchResult {
        // Synthetic doc_id keyed on `catalog:` prefix so a future
        // documents-channel hit on the same path doesn't collide.
        doc_id: format!("catalog:{}", hit.entry_path),
        location_uri: hit.entry_path.clone(),
        owner_id: String::new(),
        title,
        author: None,
        year: None,
        filename: hit.filename,
        ext,
        language: None,
        snippet: String::new(),
        score: 0.4,
        // -1 marks a non-chunk row in the existing convention
        // (used by the documents-table whole-doc metadata rows too).
        chunk_index: -1,
        metadata_json: None,
        catalog_source: Some(hit.catalog_path),
        // Catalog rows pre-date the volume-id metadata; nothing to
        // surface yet. A future per-catalog volume_id field would
        // populate this so catalog hits also disappear when the
        // archive drive isn't mounted.
        volume_id: None,
        indexed_at: 0,
        // Catalog hits are synthetic (built from .caf entries); the
        // .caf format pre-dates the source_hash promotion, so leave
        // it empty. The images dup view filters empty hashes anyway.
        source_hash: String::new(),
        // Catalog channel hits never carry translations — they're
        // metadata-only references.
        text_translated: None,
        text_translated_lang: None,
        // Catalog hits carry no source-URL provenance or tags.
        url: None,
        tags: vec![],
    }
}

// ── Index status ──────────────────────────────────────────────────────────────

/// Returns true if the index backend is initialised and ready.
#[tauri::command]
pub async fn index_is_ready(state: State<'_, AppState>) -> Result<bool, String> {
    let lock = state.index.lock().await;
    Ok(lock.config.enabled && lock.backend.is_some())
}

/// Stats about the local index: total rows, unique docs, chunks.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStats {
    pub total_rows: usize,
    pub doc_count: usize,
    pub chunk_count: usize,
}

#[tauri::command]
pub async fn index_stats(state: State<'_, AppState>) -> Result<IndexStats, String> {
    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled".into());
    }
    let local = lock
        .local
        .as_ref()
        .ok_or("Stats are only available for the local backend")?
        .clone();
    drop(lock);

    let total_rows = local.count().await.map_err(|e| e.to_string())?;
    let doc_count = local.count_docs().await.map_err(|e| e.to_string())?;
    Ok(IndexStats {
        total_rows,
        doc_count,
        chunk_count: total_rows.saturating_sub(doc_count),
    })
}

/// List all indexed documents (one entry per doc, first chunk only).
#[tauri::command]
pub async fn index_list_documents(
    state: State<'_, AppState>,
    limit: usize,
) -> Result<Vec<super::SearchResult>, String> {
    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Ok(vec![]);
    }
    let local = lock
        .local
        .as_ref()
        .ok_or("Document listing is only available for the local backend")?
        .clone();
    drop(lock);

    local.list_documents(limit).await.map_err(|e| e.to_string())
}

/// PLAN P9 — paginated, filterable, sortable browse of the documents
/// table. Replaces the load-the-whole-table fetch in the Catalog
/// overview pane.
///
/// The request is `(filter, sort, page)`; the response is one page of
/// rows + a `next_cursor` (opaque) + a `total_estimate` for the same
/// filter (regardless of page).
#[tauri::command]
pub async fn index_query_documents(
    state: State<'_, AppState>,
    filter: super::schema::DocumentFilter,
    sort: super::schema::SortSpec,
    page: super::schema::PageSpec,
) -> Result<super::schema::DocumentPage, String> {
    let lock = state.index.lock().await;
    let local = match (lock.config.enabled, lock.local.as_ref()) {
        // Index disabled OR not yet initialised on the local backend:
        // hand back an empty page silently rather than an error. The
        // Übersicht pane polls this on every chip change and during
        // app boot, before init_index has finished -- erroring there
        // would surface as a wave of "Document query is only available
        // for the local backend" log lines instead of a clean empty
        // state. Remote-backend mode also lands here (no local
        // LocalIndex); browsing the remote catalog isn't supported
        // yet (P9 step 8 territory).
        (false, _) | (true, None) => {
            return Ok(super::schema::DocumentPage {
                rows: vec![],
                next_cursor: None,
                total_estimate: 0,
            });
        }
        (true, Some(l)) => l.clone(),
    };
    drop(lock);

    local
        .query_documents(&filter, sort, page)
        .await
        .map_err(|e| e.to_string())
}

/// Tag-cloud facets for the Übersicht sidebar (Tier 2). Returns the
/// distinct tags present under `filter` (ignoring its own `tags`
/// selection) with per-tag document counts, sorted by count descending
/// and capped at `limit`. Empty `Vec` when the index is disabled or the
/// local backend isn't initialised yet — same silent-empty contract as
/// `index_query_documents`, since the sidebar polls this on every chip
/// change and during boot.
#[tauri::command]
pub async fn index_tag_facets(
    state: State<'_, AppState>,
    filter: super::schema::DocumentFilter,
    limit: usize,
) -> Result<Vec<super::schema::TagFacet>, String> {
    let lock = state.index.lock().await;
    let local = match (lock.config.enabled, lock.local.as_ref()) {
        (false, _) | (true, None) => return Ok(vec![]),
        (true, Some(l)) => l.clone(),
    };
    drop(lock);

    local
        .tag_facets(&filter, limit)
        .await
        .map_err(|e| e.to_string())
}

/// Cross-corpus deduplication by canonical URL.  Returns groups of ≥2
/// documents sharing the same `url` — e.g. the same article ingested via
/// a wallabag import and a manual folder.
#[tauri::command]
pub async fn index_url_duplicates(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<super::schema::UrlDuplicateGroup>, String> {
    let lock = state.index.lock().await;
    let local = match (lock.config.enabled, lock.local.as_ref()) {
        (false, _) | (true, None) => return Ok(vec![]),
        (true, Some(l)) => l.clone(),
    };
    drop(lock);

    local
        .url_duplicates(limit.unwrap_or(200))
        .await
        .map_err(|e| e.to_string())
}

/// PLAN P9 step 4 — return the immediate subdirectories of `parent` with
/// their subtree doc counts. Used by the folder-tree pane in Übersicht.
///
/// `parent` is the `parent_dir` value to explore one level deeper.
/// Pass `""` (empty string) to enumerate top-level roots from all indexed rows.
/// `owner_id` is the same per-user filter applied everywhere else.
///
/// Returns an empty array when the index isn't ready or the parent has no
/// children — the UI treats both cases identically (no chevron / leaf node).
#[tauri::command]
pub async fn index_folder_children(
    state: State<'_, AppState>,
    parent: String,
    owner_id: Option<String>,
) -> Result<Vec<super::schema::FolderChild>, String> {
    let lock = state.index.lock().await;
    let local = match (lock.config.enabled, lock.local.as_ref()) {
        (false, _) | (true, None) => return Ok(vec![]),
        (true, Some(l)) => l.clone(),
    };
    drop(lock);

    local
        .folder_children(&parent, owner_id.as_deref())
        .await
        .map_err(|e| e.to_string())
}

// ── Ingest ────────────────────────────────────────────────────────────────────

/// Ingest progress event emitted as `index://ingest-progress`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestProgress {
    pub filename: String,
    pub step: &'static str, // "extracting" | "embedding" | "writing" | "done" | "error"
    pub chunk_index: usize,
    pub chunk_total: usize,
    pub message: String,
}

/// Frontend sends camelCase (Tauri convention); rename so `fullText` ->
/// `full_text` etc.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentIngestInput {
    pub full_text: String,
    pub full_text_md: String,
    pub headings: Vec<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub filename: String,
    pub ext: String,
    pub language: String,
    pub location_uri: String,
    pub owner_id: String,
    pub source_hash: String,
    pub tags: Vec<String>,
}

/// PLAN P7.4.2 — single-shot path-based ingest. Takes a filesystem
/// path, runs the per-filetype extractor (P7.4.1), computes the
/// source hash, builds a `RawDocument`, and feeds the existing
/// IngestPipeline. The frontend / CLI can call this in a loop to do
/// background ingest of a folder; full scheduler with rate-limiting +
/// progress persistence lands in 7.4.2b.
///
/// `owner_id` defaults to nil-UUID when not supplied — single-user
/// installs can ignore it; multi-user setups should always pass.
#[tauri::command]
pub async fn index_ingest_path(
    state: State<'_, AppState>,
    path: String,
    owner_id: Option<String>,
    title: Option<String>,
    author: Option<String>,
    year: Option<i32>,
    language: Option<String>,
) -> Result<IngestStats, String> {
    use sha2::{Digest, Sha256};
    let p = std::path::PathBuf::from(&path);
    let owner = owner_id.unwrap_or_else(|| uuid::Uuid::nil().to_string());

    // Read bytes once: needed for both source_hash and (lossily) for
    // the text extractor in some paths. We bind it locally so the
    // tokio task below doesn't need to re-stat / re-read.
    let bytes = tokio::task::spawn_blocking({
        let p = p.clone();
        move || std::fs::read(&p)
    })
    .await
    .map_err(|e| format!("read join: {e}"))?
    .map_err(|e| format!("reading {}: {e}", p.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let source_hash = hex::encode(h.finalize());

    // Run the dispatcher off the runtime — pdf_extract is sync and CPU-
    // heavy, the others are quick but still blocking I/O.
    let extracted = tokio::task::spawn_blocking({
        let p = p.clone();
        move || crate::extractors::extract_text_from_path(&p)
    })
    .await
    .map_err(|e| format!("extract join: {e}"))?
    .map_err(|e| format!("extracting {}: {e}", p.display()))?;

    // Build the location URI in the canonical `crisp+local://` shape.
    // `Uuid::nil()` for machine_id is the placeholder for single-machine
    // setups; a multi-machine deployment would feed a real machine UUID.
    let loc = super::location::FileLocation::Local {
        user_id: uuid::Uuid::parse_str(&owner).unwrap_or_else(|_| uuid::Uuid::nil()),
        machine_id: uuid::Uuid::nil(),
        path: p.clone(),
    };
    let location_uri = loc.to_uri();

    let filename = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Stat once for mtime (P7.4.3 skip-check) and file size (P9 UX: Übersicht size column).
    let p_meta = std::fs::metadata(&p).ok();
    let raw = RawDocument {
        full_text: extracted.full_text,
        full_text_md: String::new(),
        headings: extracted.headings,
        title,
        author,
        year,
        filename,
        ext: extracted.ext,
        language: language.unwrap_or_default(),
        source_hash,
        location_uri,
        owner_id: owner,
        tags: Vec::new(),
        mtime_unix: p_meta.as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64),
        file_size: p_meta.map(|m| m.len() as i64),
        volume_id: crate::volume::volume_id_for_path(&p),
        parent_dir: p.parent().and_then(|d| d.to_str()).map(|s| s.to_owned()),
        translated_text: extracted.translated_text,
        translated_to_lang: extracted.translated_to_lang,
        // P13.6 Step 7 — audio L2 from the symphonia probe inside
        // extractors::audio::extract.  None for non-audio extractors.
        audio_duration_seconds: extracted.audio.as_ref().and_then(|a| a.duration_seconds),
        audio_codec: extracted.audio.as_ref().and_then(|a| a.codec.clone()),
        audio_sample_rate_hz: extracted.audio.as_ref().and_then(|a| a.sample_rate_hz.map(|s| s as i32)),
        audio_channels: extracted.audio.as_ref().and_then(|a| a.channels.map(|c| c as i32)),
        audio_bitrate_kbps: extracted.audio.as_ref().and_then(|a| a.bitrate_kbps.map(|b| b as i32)),
        // P13.6 Step 9 — image L2 (EXIF) curated subset.
        image_camera_make:  extracted.image_exif.as_ref().and_then(|e| e.camera_make.clone()),
        image_camera_model: extracted.image_exif.as_ref().and_then(|e| e.camera_model.clone()),
        image_lens_model:   extracted.image_exif.as_ref().and_then(|e| e.lens_model.clone()),
        image_taken_at_unix: extracted.image_exif.as_ref().and_then(|e| e.taken_at_unix),
        image_iso:          extracted.image_exif.as_ref().and_then(|e| e.iso.map(|i| i as i32)),
        multivec_packed: None,
        multivec_n_tokens: None,
        url: None,
    };

    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled in settings".to_owned());
    }
    let pipeline = lock
        .pipeline
        .clone()
        .ok_or("No local ingest pipeline (remote backend?)")?;
    drop(lock);

    pipeline
        .ingest_document(raw)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn index_ingest_document(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: DocumentIngestInput,
) -> Result<IngestStats, String> {
    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Err("Index is disabled in settings".to_owned());
    }

    let raw = RawDocument {
        full_text: input.full_text,
        full_text_md: input.full_text_md,
        headings: input.headings,
        title: input.title,
        author: input.author,
        year: input.year,
        filename: input.filename,
        ext: input.ext,
        language: input.language,
        source_hash: input.source_hash,
        location_uri: input.location_uri,
        owner_id: input.owner_id,
        tags: input.tags,
        // Frontend-driven ingest doesn't carry source mtime — the file
        // could be many months old at this point. The cheap-skip in
        // bg_ingest will treat None as "no recorded mtime → re-ingest"
        // which is the safe default.
        mtime_unix: None,
        file_size: None,
        // PLAN P7.6 — frontend ingest is path-less (input is the
        // already-extracted text), so we don't have a volume to tag.
        volume_id: None,
        parent_dir: None,
        // Frontend-driven ingest doesn't run the extractor's MT pass —
        // the caller's already-extracted text is what we get.  Phase 8
        // on-demand surface (index::translate_commands::translate_text)
        // is the path for translating these post-ingest.
        translated_text: None,
        translated_to_lang: None,
        audio_duration_seconds: None,
        audio_codec: None,
        audio_sample_rate_hz: None,
        audio_channels: None,
        audio_bitrate_kbps: None,
        image_camera_make: None,
        image_camera_model: None,
        image_lens_model: None,
        image_taken_at_unix: None,
        image_iso: None,
        multivec_packed: None,
        multivec_n_tokens: None,
        url: None,
    };

    use tauri::Emitter;
    let fname = raw.filename.clone();

    macro_rules! emit_ingest {
        ($step:expr, $ci:expr, $ct:expr, $msg:expr) => {
            let _ = app.emit(
                "index://ingest-progress",
                IngestProgress {
                    filename: fname.clone(),
                    step: $step,
                    chunk_index: $ci,
                    chunk_total: $ct,
                    message: $msg.to_owned(),
                },
            );
        };
    }

    if let Some(pipeline) = lock.pipeline.clone() {
        // ── Local path: pipeline handles chunk + embed + write ────────────
        drop(lock);
        emit_ingest!("embedding", 0, 0, "Embedding & writing via pipeline …");
        let result = pipeline
            .ingest_document(raw)
            .await
            .map_err(|e| e.to_string())?;
        emit_ingest!("done", result.chunk_count, result.chunk_count, "Done");
        return Ok(result);
    }

    // ── Remote path: chunk, optionally embed locally, push to server ──────
    let backend = lock
        .backend
        .as_ref()
        .ok_or("Index not initialised — call index_init first")?
        .clone();
    let config = lock.config.clone();
    let embedder_opt = lock.embedder.clone(); // None when embedder_location=Server
    drop(lock);

    use super::embedder::chunk_text;
    use super::ingest::{chunk_row_id, doc_id_for};

    let cfg = super::ingest::IngestConfig::default();
    let doc_id = doc_id_for(&raw);
    let max_tokens = cfg.chunk_max_words;
    let stride = cfg.chunk_stride;
    let text_chunks = chunk_text(&raw.full_text, max_tokens, stride, &[]);
    let chunk_count_total = text_chunks.len();
    let chunk_total = chunk_count_total as i32;

    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let embed_ms;
    let embeddings: Vec<Option<Vec<f32>>>;

    if let Some(embedder) = embedder_opt {
        // embedder_location = Client: embed locally before posting.
        emit_ingest!("embedding", 0, chunk_count_total, "Embedding …");
        let start_embed = std::time::Instant::now();
        let texts: Vec<String> = text_chunks.iter().map(|c| c.text.clone()).collect();
        let dense = {
            use super::embedder::EmbedRole;
            let mut emb = embedder.lock().await;
            emb.embed_dense(texts, EmbedRole::Passage)
                .map_err(|e| e.to_string())?
        };
        embed_ms = start_embed.elapsed().as_millis() as u64;
        embeddings = dense.vectors.into_iter().map(Some).collect();
    } else {
        // embedder_location = Server: post raw text, server embeds on arrival.
        emit_ingest!("embedding", 0, chunk_count_total, "Skipping local embedding (server-side)");
        embed_ms = 0;
        embeddings = vec![None; chunk_count_total];
    }

    emit_ingest!("writing", 0, chunk_count_total, "Writing to index …");
    let start_write = std::time::Instant::now();

    for (i, (tc, emb_vec)) in text_chunks.iter().zip(embeddings.into_iter()).enumerate() {
        emit_ingest!(
            "writing",
            i,
            chunk_count_total,
            format!("Chunk {}/{}", i + 1, chunk_count_total)
        );
        let chunk = super::schema::DocumentChunk {
            id: chunk_row_id(&doc_id, i as i32),
            doc_id: doc_id.clone(),
            location_uri: raw.location_uri.clone(),
            owner_id: raw.owner_id.clone(),
            filename: Some(raw.filename.clone()),
            title: raw.title.clone(),
            author: raw.author.clone(),
            year: raw.year,
            ext: Some(raw.ext.clone()),
            language: Some(raw.language.clone()),
            page_count: None,
            headings_text: Some(raw.headings.join(" ")),
            full_text: Some(tc.text.clone()),
            full_text_md: if i == 0 {
                Some(raw.full_text_md.clone())
            } else {
                None
            },
            embedding: emb_vec,
            embedding_sparse: None,
            embedding_model: Some(format!("{:?}", config.embedder_model)),
            chunk_index: tc.chunk_index,
            chunk_total,
            chunk_start_char: Some(tc.start_char as i32),
            chunk_end_char: Some(tc.end_char as i32),
            indexed_at: now_ms,
            source_hash: raw.source_hash.clone(),
            tags: raw.tags.clone(),
            metadata_json: None,
            parent_dir: raw.parent_dir.clone(),
            volume_id: raw.volume_id.clone(),
            // Pass per-doc translation through to every chunk row —
            // same replication pattern the main make_chunk path uses.
            text_translated: raw.translated_text.clone(),
            text_translated_lang: raw.translated_to_lang.clone(),
            // P13.6 Step 7 — audio L2 carries through the same way.
            audio_duration_seconds: raw.audio_duration_seconds,
            audio_codec: raw.audio_codec.clone(),
            audio_sample_rate_hz: raw.audio_sample_rate_hz,
            audio_channels: raw.audio_channels,
            audio_bitrate_kbps: raw.audio_bitrate_kbps,
            // P13.6 Step 9 — image L2 carries through likewise.
            image_camera_make: raw.image_camera_make.clone(),
            image_camera_model: raw.image_camera_model.clone(),
            image_lens_model: raw.image_lens_model.clone(),
            image_taken_at_unix: raw.image_taken_at_unix,
            image_iso: raw.image_iso,
            multivec_packed: None,
            multivec_n_tokens: None,
            url: None,
        };
        backend.ingest(chunk).await.map_err(|e| e.to_string())?;
    }
    emit_ingest!("done", chunk_count_total, chunk_count_total, "Done");

    let write_ms = start_write.elapsed().as_millis() as u64;

    Ok(IngestStats {
        chunk_count: chunk_count_total,
        embed_time_ms: embed_ms,
        write_time_ms: write_ms,
    })
}

// ── Batched L3 ingest (PLAN P11 step 1) ───────────────────────────────────

/// PLAN P11 step 1 -- bulk-ingest N already-extracted documents in one
/// shot. Same per-doc inputs as `index_ingest_document`; the saving
/// is server-side (one Arrow record-batch per ~64*4 chunks instead of
/// one per doc, one Tantivy commit instead of one per doc -- Tantivy
/// commits do segment merges so they're the dominant cost when doc
/// count is high).
///
/// Foreground use case: the producer/consumer pipeline accumulates
/// up to N freshly-embedded documents and flushes them through this
/// command in one call instead of N. Programmatic use case
/// (P12 step 13): the cb-manifest L1 import dispatches one batch
/// per HARD_BATCH chunks of cloud-backup file_manifest rows.
///
/// Local-backend only; remote-mode bulk-ingest is P11 step 4 and goes
/// through `crisp_index_protocol::IngestBatch` over HTTP. Calling this
/// against `BackendType::Remote` returns an error today; callers
/// should fall back to per-doc `index_ingest_document` against the
/// remote backend, or wait for step 4.
#[tauri::command]
pub async fn index_ingest_batch(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    inputs: Vec<DocumentIngestInput>,
) -> Result<IngestStats, String> {
    if inputs.is_empty() {
        return Ok(IngestStats { chunk_count: 0, embed_time_ms: 0, write_time_ms: 0 });
    }

    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Err("Index is disabled in settings".to_owned());
    }

    let pipeline = lock.pipeline.clone();
    drop(lock);

    if let Some(pipeline) = pipeline {
        use tauri::Emitter;
        let total = inputs.len();

        macro_rules! emit_batch {
            ($step:expr, $msg:expr) => {
                let _ = app.emit(
                    "index://ingest-progress",
                    IngestProgress {
                        filename: format!("(batch of {})", total),
                        step: $step,
                        chunk_index: 0,
                        chunk_total: 0,
                        message: $msg.to_owned(),
                    },
                );
            };
        }

        emit_batch!("embedding", "Embedding & writing batch …");

        let raws: Vec<RawDocument> = inputs
            .into_iter()
            .map(|input| RawDocument {
                full_text: input.full_text,
                full_text_md: input.full_text_md,
                headings: input.headings,
                title: input.title,
                author: input.author,
                year: input.year,
                filename: input.filename,
                ext: input.ext,
                language: input.language,
                source_hash: input.source_hash,
                location_uri: input.location_uri,
                owner_id: input.owner_id,
                tags: input.tags,
                // Same rationale as index_ingest_document: frontend ingest
                // is path-less / mtime-less; bg_ingest skip-on-mtime
                // treats None as "re-ingest" which is the safe default.
                mtime_unix: None,
                file_size: None,
                volume_id: None,
                parent_dir: None,
                // Frontend-driven batch ingest doesn't run the extractor's
                // MT pass — Phase 8 on-demand handles per-result translation.
                translated_text: None,
                translated_to_lang: None,
                audio_duration_seconds: None,
                audio_codec: None,
                audio_sample_rate_hz: None,
                audio_channels: None,
                audio_bitrate_kbps: None,
                image_camera_make: None,
                image_camera_model: None,
                image_lens_model: None,
                image_taken_at_unix: None,
                image_iso: None,
                multivec_packed: None,
                multivec_n_tokens: None,
                url: None,
            })
            .collect();

        let result = pipeline
            .ingest_documents_batch(raws)
            .await
            .map_err(|e| e.to_string())?;

        emit_batch!(
            "done",
            format!(
                "Done: {} docs / {} chunks, embed {} ms, write {} ms",
                total, result.chunk_count, result.embed_time_ms, result.write_time_ms
            )
        );

        return Ok(result);
    }

    // ── Remote path: chunk + embed locally, enqueue one HTTP batch, poll ───
    let lock = state.index.lock().await;
    let config = lock.config.clone();
    let embedder = lock.embedder.clone();
    drop(lock);

    use tauri::Emitter;
    let total = inputs.len();
    macro_rules! emit_batch_remote {
        ($step:expr, $msg:expr) => {
            let _ = app.emit(
                "index://ingest-progress",
                IngestProgress {
                    filename: format!("(batch of {})", total),
                    step: $step,
                    chunk_index: 0,
                    chunk_total: 0,
                    message: $msg.to_owned(),
                },
            );
        };
    }

    let server_embeds = embedder.is_none();
    emit_batch_remote!(
        "embedding",
        if server_embeds {
            "Preparing remote batch (server-side embedding) …"
        } else {
            "Embedding remote batch …"
        }
    );
    let embed_start = std::time::Instant::now();
    let mut chunks: Vec<RemoteIngestChunk> = Vec::new();

    for input in inputs {
        let raw = RawDocument {
            full_text: input.full_text,
            full_text_md: input.full_text_md,
            headings: input.headings,
            title: input.title,
            author: input.author,
            year: input.year,
            filename: input.filename,
            ext: input.ext,
            language: input.language,
            source_hash: input.source_hash,
            location_uri: input.location_uri,
            owner_id: input.owner_id,
            tags: input.tags,
            mtime_unix: None,
            file_size: None,
            volume_id: None,
            parent_dir: None,
            // Remote-batch ingest doesn't run the extractor's MT pass.
            translated_text: None,
            translated_to_lang: None,
            audio_duration_seconds: None,
            audio_codec: None,
            audio_sample_rate_hz: None,
            audio_channels: None,
            audio_bitrate_kbps: None,
            image_camera_make: None,
            image_camera_model: None,
            image_lens_model: None,
            image_taken_at_unix: None,
            image_iso: None,
            multivec_packed: None,
            multivec_n_tokens: None,
            url: None,
        };
        let doc_id = super::ingest::doc_id_for(&raw);
        let cfg = super::ingest::IngestConfig::default();
        let text_chunks = super::embedder::chunk_text(
            &raw.full_text,
            cfg.chunk_max_words,
            cfg.chunk_stride,
            &[],
        );
        let dense = if let Some(ref embedder) = embedder {
            let texts: Vec<String> = text_chunks.iter().map(|c| c.text.clone()).collect();
            Some({
                use super::embedder::EmbedRole;
                let mut emb = embedder.lock().await;
                emb.embed_dense(texts, EmbedRole::Passage)
                    .map_err(|e| e.to_string())?
            })
        } else {
            None
        };
        for (i, tc) in text_chunks.iter().enumerate() {
            chunks.push(RemoteIngestChunk {
                doc_id: doc_id.clone(),
                chunk_index: tc.chunk_index,
                full_text: tc.text.clone(),
                full_text_md: if i == 0 {
                    Some(raw.full_text_md.clone())
                } else {
                    None
                },
                headings: if i == 0 {
                    Some(raw.headings.clone())
                } else {
                    None
                },
                embedding: dense
                    .as_ref()
                    .and_then(|d| d.vectors.get(i).cloned())
                    .unwrap_or_default(),
                title: raw.title.clone(),
                author: raw.author.clone(),
                year: raw.year,
                filename: raw.filename.clone(),
                ext: raw.ext.clone(),
                language: raw.language.clone(),
                location_uri: raw.location_uri.clone(),
                owner_id: raw.owner_id.clone(),
                source_hash: raw.source_hash.clone(),
                tags: raw.tags.clone(),
            });
        }
    }
    let embed_time_ms = embed_start.elapsed().as_millis() as u64;

    emit_batch_remote!("writing", "Queueing remote batch …");
    let write_start = std::time::Instant::now();
    let remote = super::remote_client::RemoteClient::new(
        config.remote_url.clone().ok_or("remote_url missing for remote backend")?,
        config.remote_api_key.clone().unwrap_or_default(),
    );
    let batch = RemoteIngestBatch { chunks };
    let accepted = remote.ingest_batch(&batch).await.map_err(|e| e.to_string())?;
    {
        let mut lock = state.index.lock().await;
        lock.remote_queue_depth = accepted.queue_depth;
    }
    emit_batch_remote!(
        "writing",
        format!(
            "Remote batch queued: {} chunks, server queue depth {}",
            accepted.chunk_count, accepted.queue_depth
        )
    );

    // Poll task status with adaptive backoff: 500 ms for the first 4 polls
    // (≤ 2 s), 2 s for the next 4 polls (≤ 10 s), 5 s thereafter.
    let status = {
        let mut poll_count: u32 = 0;
        loop {
            let status = remote
                .task_status(&accepted.task_id)
                .await
                .map_err(|e| e.to_string())?;
            {
                let mut lock = state.index.lock().await;
                lock.remote_queue_depth = status.queue_depth;
            }

            let msg = match status.state.as_str() {
                "queued" => format!(
                    "Remote task queued: {} / {} chunks complete, server queue depth {}",
                    status.completed_chunks, status.total_chunks, status.queue_depth
                ),
                "processing" => format!(
                    "Remote task processing: {} / {} chunks complete, server queue depth {}",
                    status.completed_chunks, status.total_chunks, status.queue_depth
                ),
                "done" => format!(
                    "Remote task complete: {} / {} chunks, remaining queue depth {}",
                    status.completed_chunks, status.total_chunks, status.queue_depth
                ),
                "failed" => status
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("remote task {} failed", accepted.task_id)),
                other => format!("Remote task state: {other}"),
            };
            let _ = app.emit(
                "index://ingest-progress",
                IngestProgress {
                    filename: format!("(batch of {})", total),
                    step: if status.state == "done" {
                        "done"
                    } else if status.state == "failed" {
                        "error"
                    } else {
                        "writing"
                    },
                    chunk_index: status.completed_chunks,
                    chunk_total: status.total_chunks,
                    message: msg,
                },
            );

            match status.state.as_str() {
                "queued" | "processing" => {
                    let delay_ms = if poll_count < 4 { 500 } else if poll_count < 8 { 2_000 } else { 5_000 };
                    poll_count += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                "done" => break status,
                "failed" => {
                    let mut lock = state.index.lock().await;
                    lock.remote_queue_depth = status.queue_depth;
                    return Err(status
                        .error
                        .unwrap_or_else(|| format!("remote task {} failed", accepted.task_id)));
                }
                other => return Err(format!("unknown remote task state: {other}")),
            }
        }
    };
    let write_time_ms = write_start.elapsed().as_millis() as u64;

    emit_batch_remote!(
        "done",
        format!(
            "Done: {} docs / {} chunks, embed {} ms, remote queue depth {}",
            total, status.completed_chunks, embed_time_ms, status.queue_depth
        )
    );

    Ok(IngestStats {
        chunk_count: status.completed_chunks,
        embed_time_ms,
        write_time_ms,
    })
}

// ── Level-1 (filesystem-only) ingest ──────────────────────────────────────────

/// Quick metadata-only ingest: writes one filesystem-info row per file.
/// No text extraction, no embedding. Use this to bootstrap a catalog
/// before deciding which files to deep-index.
#[tauri::command]
pub async fn index_ingest_l1(
    state: State<'_, AppState>,
    files: Vec<L1FileEntry>,
) -> Result<IngestStats, String> {
    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Err("Index is disabled in settings".to_owned());
    }

    let pipeline = lock
        .pipeline
        .clone()
        .ok_or("L1 ingest requires the local backend; remote not yet wired")?;
    drop(lock);

    pipeline
        .ingest_l1(&files)
        .await
        .map_err(|e| e.to_string())
}

// ── Level-2 promotion ─────────────────────────────────────────────────────────

/// Per-doc result of an L2 promotion.
#[derive(Debug, Clone, serde::Serialize)]
pub struct L2PromoteResult {
    pub doc_id: String,
    /// True if any field was updated.
    pub updated: bool,
    /// Title / author / year that ended up in the row (post-merge).
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    /// Filled with a human-readable cause when the file couldn't be read.
    pub error: Option<String>,
}

/// Read embedded metadata (PDF Info dict, DOCX core.xml, EPUB OPF) for each
/// `doc_id`, write the discovered Title / Author / Year fields back to the
/// row, and bump `metadata_json.level` to 2.
#[tauri::command]
pub async fn index_promote_l2(
    state: State<'_, AppState>,
    doc_ids: Vec<String>,
) -> Result<Vec<L2PromoteResult>, String> {
    use super::l2_metadata;

    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled".to_owned());
    }
    let local = lock
        .local
        .as_ref()
        .ok_or("L2 promotion requires the local backend")?
        .clone();
    drop(lock);

    let mut out = Vec::with_capacity(doc_ids.len());
    for doc_id in doc_ids {
        // Look up the row to find location_uri + existing metadata_json.
        let rows = local
            .list_documents(usize::MAX)
            .await
            .map_err(|e| e.to_string())?;
        let row = match rows.iter().find(|r| r.doc_id == doc_id) {
            Some(r) => r.clone(),
            None => {
                out.push(L2PromoteResult {
                    doc_id,
                    updated: false,
                    title: None,
                    author: None,
                    year: None,
                    error: Some("doc_id not found".into()),
                });
                continue;
            }
        };

        // Resolve location_uri to a path. Strip our `crisp+local://…/` prefix.
        let path_str = if row.location_uri.starts_with("crisp+local://") {
            let after = &row.location_uri["crisp+local://".len()..];
            let slash = after.find('/').map(|i| i + 1).unwrap_or(0);
            after[slash.saturating_sub(1)..].to_string()
        } else {
            row.location_uri.clone()
        };
        let path = std::path::PathBuf::from(&path_str);
        if !path.exists() {
            out.push(L2PromoteResult {
                doc_id,
                updated: false,
                title: row.title,
                author: row.author,
                year: row.year,
                error: Some(format!("file missing: {}", path.display())),
            });
            continue;
        }

        let meta = l2_metadata::read(&path);

        // Build merged metadata_json: existing keys + L2 fields + level=2.
        let mut merged: serde_json::Map<String, serde_json::Value> = row
            .metadata_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        merged.insert("level".to_owned(), serde_json::Value::from(2));
        if let Some(pc) = meta.page_count {
            merged.insert("page_count".to_owned(), serde_json::Value::from(pc));
        }
        for (k, v) in meta.extra.iter() {
            merged.insert(k.clone(), v.clone());
        }
        let merged_str = serde_json::to_string(&merged).unwrap_or_else(|_| "{}".to_owned());

        // Only patch fields the row didn't already have — never clobber.
        let title_to_set = match (row.title.as_ref(), meta.title.as_ref()) {
            (None, Some(t)) if !t.trim().is_empty() => Some(t.as_str()),
            _ => None,
        };
        let author_to_set = match (row.author.as_ref(), meta.author.as_ref()) {
            (None, Some(a)) if !a.trim().is_empty() => Some(a.as_str()),
            _ => None,
        };
        let year_to_set = match (row.year, meta.year) {
            (None, Some(y)) => Some(y),
            _ => None,
        };
        let lang_to_set = match (row.language.as_ref(), meta.language.as_ref()) {
            (None, Some(l)) if !l.trim().is_empty() => Some(l.as_str()),
            _ => None,
        };

        let updated = title_to_set.is_some()
            || author_to_set.is_some()
            || year_to_set.is_some()
            || lang_to_set.is_some()
            || meta.page_count.is_some();

        if let Err(e) = local
            .update_l2_fields(
                &doc_id,
                title_to_set,
                author_to_set,
                year_to_set,
                lang_to_set,
                meta.page_count,
                Some(&merged_str),
            )
            .await
        {
            out.push(L2PromoteResult {
                doc_id,
                updated: false,
                title: row.title,
                author: row.author,
                year: row.year,
                error: Some(format!("update failed: {e:#}")),
            });
            continue;
        }

        out.push(L2PromoteResult {
            doc_id,
            updated,
            title: title_to_set.map(|s| s.to_owned()).or(row.title),
            author: author_to_set.map(|s| s.to_owned()).or(row.author),
            year: year_to_set.or(row.year),
            error: None,
        });
    }

    Ok(out)
}

// ── CAF (Catfish/Cathy file index) import / export ───────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct CafImportResult {
    pub ingested: usize,
    pub skipped: usize,
    pub errors: usize,
    /// Volume label / serial / scan date carried over from the .caf header,
    /// surfaced so the UI can show what catalog the user just imported.
    pub volume_label: String,
    pub volume_serial: u32,
    pub volume_date: u32,
}

/// Read a `.caf` file produced by Cathy / Catfish (or a previous
/// CrispSorter session) and ingest each entry as a Level-1 row in the
/// active LanceDB catalog. The on-disk paths in the .caf are preserved
/// in `location_uri` so promotion to L2 / L3 later still works when the
/// drive is mounted.
#[tauri::command]
pub async fn index_import_caf(
    state: State<'_, AppState>,
    path: String,
) -> Result<CafImportResult, String> {
    use crate::catalog::caf;
    use std::path::PathBuf;

    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled in settings".to_owned());
    }
    let pipeline = lock
        .pipeline
        .clone()
        .ok_or("Local index pipeline not available — switch to Local backend")?;
    drop(lock);

    let caf_path = PathBuf::from(&path);
    crate::app_log!("info", "CAF: reading {}", caf_path.display());
    let idx = caf::read_file(&caf_path).map_err(|e| format!("read {}: {e}", caf_path.display()))?;
    let total = idx.all_files.len();
    crate::app_log!(
        "info",
        "CAF: {} entries, label='{}', root='{}'",
        total,
        idx.header.label,
        idx.root_path.display()
    );

    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    // Try to resolve the volume UUID for the scan root. Succeeds when the
    // drive is currently mounted (the root_path exists); returns None when
    // cataloging an offline archive — callers must supply the UUID via the
    // stored `RegisteredCatalog.volumeId` if they want volume-filter to work.
    let caf_volume_id = crate::volume::volume_id_for_path(&idx.root_path);
    if let Some(ref vid) = caf_volume_id {
        crate::app_log!("info", "CAF: volume UUID = {vid}");
    }

    // Convert each FileEntry into an L1 row. doc_id is sha256 of the
    // absolute path so subsequent imports of the same .caf are idempotent
    // (same id => updates the row instead of duplicating).
    let mut entries = Vec::with_capacity(total);
    for f in &idx.all_files {
        let abs = f.path.to_string_lossy().to_string();
        let parent = f
            .path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let filename = f
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| abs.clone());
        let ext = f
            .path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let doc_id = format!("{:x}", sha2_path(&abs));

        entries.push(super::ingest::L1FileEntry {
            doc_id: doc_id.clone(),
            source_hash: doc_id,
            location_uri: format!("crisp+local://caf-import{}/{}", idx.header.serial, abs),
            owner_id: "local".to_string(),
            filename,
            ext,
            parent_dir: parent,
            size: f.size as i64,
            mtime_ms: (f.mtime as i64) * 1000,
            ctime_ms: (f.mtime as i64) * 1000,
            volume_id: caf_volume_id.clone(),
        });
    }

    let mut ingested = 0usize;
    let mut errors = 0usize;
    // Process in batches to keep LanceDB writes manageable on huge catalogs.
    for chunk in entries.chunks(500) {
        match pipeline.ingest_l1(chunk).await {
            Ok(stats) => ingested += stats.chunk_count,
            Err(e) => {
                errors += chunk.len();
                crate::app_log!("error", "CAF: ingest_l1 batch failed: {e:#}");
            }
        }
    }

    let _ = now_ms; // (timestamp lives inside ingest_l1 already)

    Ok(CafImportResult {
        ingested,
        skipped: total - ingested - errors,
        errors,
        volume_label: idx.header.label.clone(),
        volume_serial: idx.header.serial,
        volume_date: idx.header.date,
    })
}

/// Write the catalog's L1+ rows out as a `.caf` file readable by
/// Catfish / Cathy and any other CrispSorter installation.
///
/// Export a volume slice (or full snapshot) as a portable `.cidx` archive.
///
/// `dest_path` — directory path for the output (created if absent). Conventionally
///   ends in `.cidx`, e.g. `/Volumes/MyDrive/MyDrive.cidx`.
/// `volume_id` — if set, only rows for that volume are exported.
/// `include_embeddings` — include the vector column (large; off by default).
///
/// Returns the number of rows exported.
#[tauri::command]
pub async fn index_export_cidx(
    state: State<'_, AppState>,
    dest_path: String,
    volume_id: Option<String>,
    include_embeddings: Option<bool>,
    include_fts: Option<bool>,
) -> Result<usize, String> {
    let local = state.index.lock().await.local.clone()
        .ok_or("Local index not initialised")?;
    let dest = std::path::PathBuf::from(&dest_path);
    local
        .export_cidx(
            &dest,
            volume_id.as_deref(),
            include_embeddings.unwrap_or(false),
            include_fts.unwrap_or(false),
        )
        .await
        .map_err(|e| e.to_string())
}

/// Open a `.cidx` archive for read-only search. Returns basic stats.
#[tauri::command]
pub async fn index_open_cidx(
    _state: State<'_, AppState>,
    cidx_path: String,
) -> Result<serde_json::Value, String> {
    let path = std::path::PathBuf::from(&cidx_path);
    let idx = crate::index::LocalIndex::open_cidx(&path)
        .await
        .map_err(|e| e.to_string())?;
    let chunks = idx.count().await.map_err(|e| e.to_string())?;
    let docs = idx.count_docs().await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "path": cidx_path, "docs": docs, "chunks": chunks }))
}

/// `doc_ids` (optional) limits the export to those ids; otherwise every
/// row in the active catalog is written.
#[tauri::command]
pub async fn index_export_caf(
    state: State<'_, AppState>,
    path: String,
    doc_ids: Option<Vec<String>>,
) -> Result<usize, String> {
    use crate::catalog::caf;
    use crate::catalog::index::{FileEntry, FileIndex, VolumeHeader};
    use std::path::PathBuf;

    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled".to_owned());
    }
    let local = lock
        .local
        .as_ref()
        .ok_or("Local backend required")?
        .clone();
    drop(lock);

    let rows = local
        .list_documents(usize::MAX)
        .await
        .map_err(|e| e.to_string())?;
    let filtered: Vec<_> = match doc_ids {
        Some(ref ids) => rows
            .into_iter()
            .filter(|r| ids.contains(&r.doc_id))
            .collect(),
        None => rows,
    };

    // Pick a sensible root: the longest common ancestor of all paths.
    // Fall back to `/` when paths don't share a prefix.
    let paths: Vec<PathBuf> = filtered
        .iter()
        .map(|r| {
            let uri = &r.location_uri;
            let p = if uri.starts_with("crisp+local://") {
                let after = &uri["crisp+local://".len()..];
                let slash = after.find('/').map(|i| i + 1).unwrap_or(0);
                after[slash.saturating_sub(1)..].to_string()
            } else {
                uri.clone()
            };
            PathBuf::from(p)
        })
        .collect();
    let root = common_path_prefix(&paths).unwrap_or_else(|| PathBuf::from("/"));
    let is_windows_path = paths
        .iter()
        .next()
        .map(|p| p.to_string_lossy().contains('\\') || p.to_string_lossy().chars().nth(1) == Some(':'))
        .unwrap_or(false);

    let mut idx = FileIndex::new(root.clone(), is_windows_path);
    idx.header = VolumeHeader {
        label: "CrispSorter".to_string(),
        alias: "CrispSorter".to_string(),
        serial: 0,
        comment: format!("Exported from CrispSorter ({} rows)", filtered.len()),
        freesize: 0.0,
        archive: 0,
        date: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32,
    };

    // Pull file size + mtime out of metadata_json.fs_size / fs_mtime; fall
    // back to 0 when an L1 row never had them.
    for (path, row) in paths.iter().zip(filtered.iter()) {
        let (size, mtime) = parse_l1_meta(&row.metadata_json);
        idx.add(FileEntry::new(path.clone(), size, mtime));
    }

    let out = PathBuf::from(&path);
    crate::app_log!(
        "info",
        "CAF: writing {} entries to {}",
        idx.all_files.len(),
        out.display()
    );
    caf::write_file(&out, &idx, idx.header.date)
        .map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(idx.all_files.len())
}

/// SHA-256 over a path string, hex-encoded — gives us a deterministic
/// `doc_id` so re-importing the same .caf doesn't duplicate rows.
fn sha2_path(path: &str) -> impl std::fmt::LowerHex {
    use std::hash::{Hash, Hasher};
    // We don't pull `sha2`; a SipHash digest is deterministic enough for
    // doc-id uniqueness and avoids adding a dep just for this.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    HashHex(h.finish())
}

struct HashHex(u64);
impl std::fmt::LowerHex for HashHex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

fn common_path_prefix(paths: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    let first = paths.first()?;
    let comps: Vec<_> = first.components().collect();
    let mut shared = comps.len();
    for p in paths.iter().skip(1) {
        let pc: Vec<_> = p.components().collect();
        let n = comps
            .iter()
            .zip(pc.iter())
            .take_while(|(a, b)| a == b)
            .count();
        shared = shared.min(n);
        if shared == 0 {
            break;
        }
    }
    if shared == 0 {
        None
    } else {
        Some(comps[..shared].iter().collect())
    }
}

fn parse_l1_meta(meta_json: &Option<String>) -> (u64, u32) {
    let Some(s) = meta_json else { return (0, 0); };
    let Ok(v): Result<serde_json::Value, _> = serde_json::from_str(s) else {
        return (0, 0);
    };
    let size = v.get("fs_size").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as u64;
    let mtime_ms = v.get("fs_mtime").and_then(|x| x.as_i64()).unwrap_or(0).max(0);
    (size, (mtime_ms / 1000) as u32)
}

// ── Document delete ───────────────────────────────────────────────────────────

/// P13.6 Step 8 — promote an L1/L2 audio row to L3 (full
/// transcription).  Looks up the row's host path from the
/// `location_uri`, runs the full audio extractor (decode +
/// transcribe + symphonia probe) with `ingest_audio_level=l3`
/// regardless of the IndexConfig setting, then re-ingests through
/// the standard pipeline (existing `reingest_document` deletes
/// the old chunks first, then writes the fresh transcript +
/// embeddings + audio_* columns + FTS body).
///
/// Used by the "Transcribe" search-result action that surfaces
/// on L1/L2 audio rows in the frontend.  Returns the
/// `IngestStats` for the new ingest so the UI can show progress.
///
/// Errors when:
/// - the `crispasr` cargo feature is off (audio extractor stub);
/// - `location_uri` doesn't map to a local path (e.g. cloud
///   drive without a manifest);
/// - the file isn't actually present on disk;
/// - the audio decoder rejects the file.
#[tauri::command]
pub async fn index_audio_promote_l3(
    state: State<'_, AppState>,
    location_uri: String,
) -> Result<IngestStats, String> {
    // Map URI → local path via the existing helper.  Reusing the
    // images-side helper keeps the URI grammar identical across
    // surfaces (crisp+local:// / file:// / bare absolute path).
    let path = crate::images::tauri_commands::location_uri_to_local_path(&location_uri)
        .ok_or_else(|| {
            format!(
                "location_uri does not map to a local path: {location_uri} \
                 (cloud-drive promotes need to first download the file)"
            )
        })?;
    if !path.exists() {
        return Err(format!(
            "file not present on disk: {} \
             (the original may have been moved or deleted since indexing)",
            path.display()
        ));
    }

    // Run the audio extractor with explicit L3 — overrides the
    // IndexConfig.ingest_audio_level setting because the whole
    // point of "promote" is to transcribe regardless of the
    // default-level setting.  audio_extraction_enabled is also
    // ignored: the user explicitly asked for this row, so the
    // master switch doesn't gate it.
    let extracted = {
        let p = path.clone();
        tokio::task::spawn_blocking(move || crate::extractors::audio::extract(&p))
            .await
            .map_err(|e| format!("audio extract join error: {e}"))?
            .map_err(|e| format!("{e:#}"))?
    };

    // Re-build the RawDocument and feed through the standard
    // ingest pipeline.  reingest_document deletes existing rows
    // for the doc_id (via source_hash) and writes fresh ones.
    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled in settings".to_owned());
    }
    let pipeline = lock
        .pipeline
        .clone()
        .ok_or_else(|| "No local ingest pipeline (remote backend?)".to_string())?;
    drop(lock);

    let p_meta = std::fs::metadata(&path).ok();
    let source_hash = {
        let p = path.clone();
        tokio::task::spawn_blocking(move || -> Result<String, String> {
            use sha2::{Digest, Sha256};
            let bytes = std::fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
            let mut h = Sha256::new();
            h.update(&bytes);
            Ok(hex::encode(h.finalize()))
        })
        .await
        .map_err(|e| format!("source_hash spawn_blocking: {e}"))??
    };

    let raw = RawDocument {
        full_text: extracted.full_text.clone(),
        full_text_md: extracted.full_text.clone(),
        headings: extracted.headings.clone(),
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        author: None,
        year: None,
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        ext: extracted.ext.clone(),
        language: extracted.language.clone().unwrap_or_default(),
        source_hash,
        location_uri: location_uri.clone(),
        owner_id: "local".to_string(),
        tags: vec![],
        mtime_unix: p_meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64),
        file_size: p_meta.as_ref().map(|m| m.len() as i64),
        volume_id: crate::volume::volume_id_for_path(&path),
        parent_dir: path
            .parent()
            .and_then(|d| d.to_str())
            .map(|s| s.to_owned()),
        translated_text: extracted.translated_text.clone(),
        translated_to_lang: extracted.translated_to_lang.clone(),
        audio_duration_seconds: extracted.audio.as_ref().and_then(|a| a.duration_seconds),
        audio_codec: extracted.audio.as_ref().and_then(|a| a.codec.clone()),
        audio_sample_rate_hz: extracted.audio.as_ref().and_then(|a| a.sample_rate_hz.map(|s| s as i32)),
        audio_channels: extracted.audio.as_ref().and_then(|a| a.channels.map(|c| c as i32)),
        audio_bitrate_kbps: extracted.audio.as_ref().and_then(|a| a.bitrate_kbps.map(|b| b as i32)),
        // P13.6 Step 9 — image L2 (EXIF) curated subset.
        image_camera_make:  extracted.image_exif.as_ref().and_then(|e| e.camera_make.clone()),
        image_camera_model: extracted.image_exif.as_ref().and_then(|e| e.camera_model.clone()),
        image_lens_model:   extracted.image_exif.as_ref().and_then(|e| e.lens_model.clone()),
        image_taken_at_unix: extracted.image_exif.as_ref().and_then(|e| e.taken_at_unix),
        image_iso:          extracted.image_exif.as_ref().and_then(|e| e.iso.map(|i| i as i32)),
        multivec_packed: None,
        multivec_n_tokens: None,
        url: None,
    };

    pipeline
        .reingest_document(raw)
        .await
        .map_err(|e| format!("re-ingest after L3 promote failed: {e:#}"))
}

/// P13.7 Step 2 — promote an L1/L2 image row to L3 (OCR + EXIF).
/// Parallel to [`index_audio_promote_l3`].  Resolves the row's
/// host path from `location_uri`, runs the OCR pipeline with
/// `ingest_image_level=l3` regardless of the IndexConfig
/// setting, then re-ingests through `reingest_document`.
///
/// Used by the "Re-OCR" search-result action on rows with an
/// empty snippet AND an image extension.  Mirrors the rationale
/// in `index_audio_promote_l3`: a user-initiated promote
/// overrides the user's global Settings (master switch + ingest
/// level) because the explicit click is unambiguous intent.
#[tauri::command]
pub async fn index_image_promote_l3(
    state: State<'_, AppState>,
    location_uri: String,
) -> Result<IngestStats, String> {
    let path = crate::images::tauri_commands::location_uri_to_local_path(&location_uri)
        .ok_or_else(|| {
            format!(
                "location_uri does not map to a local path: {location_uri} \
                 (cloud-drive promotes need to first download the file)"
            )
        })?;
    if !path.exists() {
        return Err(format!(
            "file not present on disk: {} \
             (the original may have been moved or deleted since indexing)",
            path.display()
        ));
    }

    // Force L3 + OCR + master-switch on for this single row.  Mirrors
    // the audio promote — the user explicitly asked, so per-row
    // overrides win.  ocr_tier stays Auto so the tier ladder picks
    // whatever's installed.
    let extract_opts = crate::extractors::ExtractOptions {
        try_ocr: true,
        ocr_pdf_min_chars: 50,
        ocr_tier: crate::extractors::OcrTier::Auto,
        ocr_rec_lang: crate::extractors::OcrRecLang::Auto,
        text_lid_model: None,
        translate_to: None,
        translate_backend: None,
        translate_model: None,
        audio_extraction_enabled: true,
        ingest_audio_level: "l3".to_string(),
        image_extraction_enabled: true,
        ingest_image_level: "l3".to_string(),
        // Manual re-OCR uses the legacy ladder; the smart pipeline applies in
        // bg_ingest where the persisted config is available.
        ocr_pipeline: crate::extractors::OcrPipelineConfig::default(),
    };
    let extracted = {
        let p = path.clone();
        tokio::task::spawn_blocking(move || {
            crate::extractors::extract_text_from_path_with_opts(&p, extract_opts)
        })
        .await
        .map_err(|e| format!("image extract join error: {e}"))?
        .map_err(|e| format!("{e:#}"))?
    };

    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled in settings".to_owned());
    }
    let pipeline = lock
        .pipeline
        .clone()
        .ok_or_else(|| "No local ingest pipeline (remote backend?)".to_string())?;
    drop(lock);

    let p_meta = std::fs::metadata(&path).ok();
    let source_hash = {
        let p = path.clone();
        tokio::task::spawn_blocking(move || -> Result<String, String> {
            use sha2::{Digest, Sha256};
            let bytes = std::fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
            let mut h = Sha256::new();
            h.update(&bytes);
            Ok(hex::encode(h.finalize()))
        })
        .await
        .map_err(|e| format!("source_hash spawn_blocking: {e}"))??
    };

    let raw = RawDocument {
        full_text: extracted.full_text.clone(),
        full_text_md: extracted.full_text.clone(),
        headings: extracted.headings.clone(),
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        author: None,
        year: None,
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        ext: extracted.ext.clone(),
        language: extracted.language.clone().unwrap_or_default(),
        source_hash,
        location_uri: location_uri.clone(),
        owner_id: "local".to_string(),
        tags: vec![],
        mtime_unix: p_meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64),
        file_size: p_meta.as_ref().map(|m| m.len() as i64),
        volume_id: crate::volume::volume_id_for_path(&path),
        parent_dir: path
            .parent()
            .and_then(|d| d.to_str())
            .map(|s| s.to_owned()),
        translated_text: extracted.translated_text.clone(),
        translated_to_lang: extracted.translated_to_lang.clone(),
        audio_duration_seconds: extracted.audio.as_ref().and_then(|a| a.duration_seconds),
        audio_codec: extracted.audio.as_ref().and_then(|a| a.codec.clone()),
        audio_sample_rate_hz: extracted.audio.as_ref().and_then(|a| a.sample_rate_hz.map(|s| s as i32)),
        audio_channels: extracted.audio.as_ref().and_then(|a| a.channels.map(|c| c as i32)),
        audio_bitrate_kbps: extracted.audio.as_ref().and_then(|a| a.bitrate_kbps.map(|b| b as i32)),
        image_camera_make:  extracted.image_exif.as_ref().and_then(|e| e.camera_make.clone()),
        image_camera_model: extracted.image_exif.as_ref().and_then(|e| e.camera_model.clone()),
        image_lens_model:   extracted.image_exif.as_ref().and_then(|e| e.lens_model.clone()),
        image_taken_at_unix: extracted.image_exif.as_ref().and_then(|e| e.taken_at_unix),
        image_iso:          extracted.image_exif.as_ref().and_then(|e| e.iso.map(|i| i as i32)),
        multivec_packed: None,
        multivec_n_tokens: None,
        url: None,
    };

    pipeline
        .reingest_document(raw)
        .await
        .map_err(|e| format!("re-ingest after image L3 promote failed: {e:#}"))
}

/// Delete a document completely: removes all chunks from LanceDB and the
/// corresponding entry from the Tantivy FTS index.
#[tauri::command]
pub async fn index_delete_document(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<(), String> {
    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Ok(());
    }

    let local = lock
        .local
        .as_ref()
        .ok_or("Local index not available for delete")?
        .clone();
    let fts = lock
        .fts
        .as_ref()
        .ok_or("FTS index not available for delete")?
        .clone();

    drop(lock);

    // Remove all chunks from LanceDB.
    local.delete_doc(&doc_id).await.map_err(|e| e.to_string())?;

    // Remove the document entry from Tantivy.
    let mut writer = fts.writer().map_err(|e| e.to_string())?;
    fts.delete_document(&mut writer, &doc_id)
        .map_err(|e| e.to_string())?;
    writer.commit().map_err(|e| e.to_string())?;

    Ok(())
}

// ── Extraction failure management ─────────────────────────────────────────────

/// Clear the `extraction_failure` blob from a document's `metadata_json` so
/// the background ingest worker will re-attempt extraction on the next run.
/// Returns the failure reason that was stored (to let the caller check
/// retryability), or `null` when no failure was recorded.
#[tauri::command]
pub async fn index_retry_extraction(
    state: State<'_, AppState>,
    doc_id: String,
) -> Result<Option<String>, String> {
    let local = state.index.lock().await.local.clone();
    let local = local.ok_or("Local index not initialised")?;
    // Read the stored reason first.
    let reason = local
        .extraction_failure_reason_for_uri_by_doc_id(&doc_id)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(ref r) = reason {
        use crate::index::task_failure::TaskFailureReason;
        // Only clear if the reason is retryable.
        let tfr = match r.as_str() {
            "timeout" => TaskFailureReason::Timeout,
            "other"   => TaskFailureReason::Other,
            _         => return Ok(reason), // non-retryable — refuse
        };
        if tfr.is_retryable() {
            local
                .clear_extraction_failure(&doc_id)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(reason)
}

// ── Location update ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn index_update_location(
    state: State<'_, AppState>,
    doc_id: String,
    new_location_uri: String,
) -> Result<(), String> {
    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Ok(());
    }

    let backend = lock
        .backend
        .as_ref()
        .ok_or("Index backend not initialised")?
        .clone();

    drop(lock);

    backend
        .update_location(&doc_id, &new_location_uri)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn index_update_location_by_path(
    state: State<'_, AppState>,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Ok(());
    }
    let backend = lock
        .backend
        .as_ref()
        .ok_or("Index backend not initialised")?
        .clone();
    drop(lock);
    let old_uri = format!("crisp+local://local/{}", old_path);
    let new_uri = format!("crisp+local://local/{}", new_path);
    backend
        .update_location_by_uri(&old_uri, &new_uri)
        .await
        .map_err(|e| e.to_string())
}

// ── Index management ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn index_build_scalar_index(state: State<'_, AppState>) -> Result<(), String> {
    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Err("Index is disabled".to_owned());
    }

    let local = lock
        .local
        .as_ref()
        .ok_or("Scalar index build is only supported for the local backend")?
        .clone();

    drop(lock);

    local.build_scalar_index().await.map_err(|e| e.to_string())
}

#[tauri::command]
/// `num_partitions` — Voronoi cell count. `null` = auto (`sqrt(row_count)`).
/// `sample_rate`    — K-Means training sample = `sample_rate × num_partitions`.
///   Default 256; raise to 512-1024 for very large tables.
pub async fn index_build_ivf_pq(
    state: State<'_, AppState>,
    num_partitions: Option<u32>,
    sample_rate: Option<u32>,
) -> Result<(), String> {
    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Err("Index is disabled".to_owned());
    }

    let local = lock
        .local
        .as_ref()
        .ok_or("IVF-PQ index build is only supported for the local backend")?
        .clone();

    drop(lock);

    local.build_vector_index(num_partitions, sample_rate)
        .await
        .map_err(|e| e.to_string())
}

/// Initialise (or re-initialise) the index from a data directory path and the
/// current config stored in `AppState`. Called by the Settings UI and by
/// the L1 / L2 / L3 fast-paths in the frontend.
///
/// `with_embedder = false` (or `IndexConfig.use_vector = false`) skips
/// the embedder construction entirely — useful for L1 / L2 ingest which
/// only need the LocalIndex (LanceDB) + FtsIndex. Saves multi-GB
/// downloads + minutes of init time when the user just wants to scan
/// a drive.
#[tauri::command]
pub async fn index_init(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    data_dir: String,
    with_embedder: Option<bool>,
) -> Result<(), String> {
    // Reserve the init slot atomically. If another init is already
    // running, bail out instead of starting a second multi-GB download.
    let config = {
        let mut lock = state.index.lock().await;
        if lock.initializing {
            crate::app_log!(
                "info",
                "Index init already running — ignoring duplicate request"
            );
            return Err("Index initialisation is already in progress".to_owned());
        }
        lock.initializing = true;
        lock.config.clone()
    };

    // Four-way decision tree for whether to load a local embedder:
    //   - global `use_vector = false`: never.
    //   - explicit `with_embedder = false` (L1 / L2 fast-path): skip.
    //   - `embedder_location = Server` + Remote backend: server embeds; skip local load.
    //   - default: load.
    let server_embeds = config.embedder_location == super::EmbedderLocation::Server
        && config.backend_type == super::BackendType::Remote;
    let load_embedder = config.use_vector && with_embedder.unwrap_or(true) && !server_embeds;

    crate::app_log!(
        "info",
        "Index init requested: data_dir={}, model={:?}, backend={:?}, with_embedder={}",
        data_dir,
        config.embedder_model,
        config.backend_type,
        load_embedder
    );

    let path = std::path::PathBuf::from(&data_dir);
    let init_result = init_index(&path, config, Some(app), load_embedder).await;

    let mut lock = state.index.lock().await;
    lock.initializing = false;
    match init_result {
        Ok(new_state) => {
            *lock = new_state;
            crate::app_log!("info", "Index init complete");
            Ok(())
        }
        Err(e) => {
            crate::app_log!("error", "Index init failed: {e:#}");
            Err(format!("{e:#}"))
        }
    }
}

// ── Embedder benchmark ────────────────────────────────────────────────────────

/// One-shot timing for a single (model, backend) pair. Loads a fresh
/// embedder, embeds the supplied texts, measures wall-clock load + embed
/// time, returns aggregate stats. Does NOT touch the live `IndexState`,
/// so running this while an index is initialised is safe.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EmbedderBenchmark {
    pub backend: &'static str,
    pub model_id: String,
    /// Time to construct the `Embedder` (download + ORT/CrispEmbed init).
    pub load_time_ms: u64,
    /// Time to embed all `texts` in one batch.
    pub embed_time_ms: u64,
    /// `text_count / embed_seconds` — embed throughput.
    pub texts_per_second: f32,
    pub dim: usize,
    pub vectors_count: usize,
    /// Self-similarity sanity check: cosine of vec[0] with itself, expected ≈ 1.0.
    pub self_cosine: f32,
    /// Filled when the run failed (model couldn't be loaded for example).
    pub error: Option<String>,
}

#[tauri::command]
pub async fn index_benchmark_embedder(
    state: State<'_, AppState>,
    model: String,
    backend: String,
    texts: Vec<String>,
) -> Result<EmbedderBenchmark, String> {
    use super::embedder::{Embedder, EmbedderBackend, EmbedderConfig};

    if texts.is_empty() {
        return Err("at least one text required".to_owned());
    }

    let config_template = state.index.lock().await.config.clone();
    let cache_dir = config_template
        .clone()
        .remote_url
        .map(|_| ()); // unused — keep clippy quiet about config_template move
    let _ = cache_dir;

    let model_enum: super::embedder::EmbedderModel =
        serde_json::from_str(&format!("\"{model}\""))
            .map_err(|e| format!("unknown model id '{model}': {e}"))?;

    let backend_enum: EmbedderBackend = match backend.as_str() {
        "onnx" => EmbedderBackend::Onnx,
        "gguf" => EmbedderBackend::Gguf,
        other => return Err(format!("unknown backend '{other}' (expected 'onnx' or 'gguf')")),
    };

    let backend_label: &'static str = match backend_enum {
        EmbedderBackend::Onnx => "onnx",
        EmbedderBackend::Gguf => "gguf",
    };

    // Reuse the live cache dir (so we benchmark against already-downloaded
    // weights when possible).
    let cache_dir = {
        let lock = state.index.lock().await;
        lock.config
            .clone()
            .remote_url
            .as_deref()
            .filter(|_| false); // ignore
        // Fall back to a sane default.
        let _ = lock;
        std::env::temp_dir().join("crispsorter-bench-cache")
    };
    std::fs::create_dir_all(&cache_dir).ok();

    let cfg = EmbedderConfig::new(model_enum, config_template.embedder_device, cache_dir)
        .with_backend(backend_enum);

    let load_start = std::time::Instant::now();
    let embedder = match Embedder::new(cfg).await {
        Ok(e) => e,
        Err(e) => {
            return Ok(EmbedderBenchmark {
                backend: backend_label,
                model_id: model,
                load_time_ms: load_start.elapsed().as_millis() as u64,
                embed_time_ms: 0,
                texts_per_second: 0.0,
                dim: model_enum.dims(),
                vectors_count: 0,
                self_cosine: 0.0,
                error: Some(format!("{e:#}")),
            });
        }
    };
    let load_time_ms = load_start.elapsed().as_millis() as u64;

    let mut emb = embedder;
    let embed_start = std::time::Instant::now();
    let dense = match emb.embed_dense(texts.clone(), super::embedder::EmbedRole::Passage) {
        Ok(d) => d,
        Err(e) => {
            return Ok(EmbedderBenchmark {
                backend: backend_label,
                model_id: model,
                load_time_ms,
                embed_time_ms: 0,
                texts_per_second: 0.0,
                dim: model_enum.dims(),
                vectors_count: 0,
                self_cosine: 0.0,
                error: Some(format!("embed failed: {e:#}")),
            });
        }
    };
    let embed_ms = embed_start.elapsed().as_millis() as u64;

    let dim = dense.vectors.first().map(|v| v.len()).unwrap_or(0);
    let throughput = if embed_ms > 0 {
        (dense.vectors.len() as f32) * 1000.0 / (embed_ms as f32)
    } else {
        0.0
    };
    let self_cos = dense
        .vectors
        .first()
        .map(|v| {
            let n: f32 = v.iter().map(|x| x * x).sum();
            n / (n.sqrt() * n.sqrt() + f32::EPSILON)
        })
        .unwrap_or(0.0);

    Ok(EmbedderBenchmark {
        backend: backend_label,
        model_id: model,
        load_time_ms,
        embed_time_ms: embed_ms,
        texts_per_second: throughput,
        dim,
        vectors_count: dense.vectors.len(),
        self_cosine: self_cos,
        error: None,
    })
}

// ── Build-time capabilities ───────────────────────────────────────────────────

/// What this binary supports at runtime — driven by Cargo features chosen at
/// compile time. The frontend uses this to disable / annotate backends that
/// aren't actually available in the running build (e.g. CrispEmbed/GGUF when
/// `--features crispembed*` was not passed).
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexCapabilities {
    /// True iff the binary was built with one of `crispembed` /
    /// `crispembed-vulkan` / `crispembed-metal` / `crispembed-cuda`.
    pub crispembed: bool,
    /// Which GPU backend is linked into CrispEmbed at compile time.
    /// `"vulkan"` / `"cuda"` / `"metal"` for the GPU sub-features; `"cpu"`
    /// for plain `crispembed`; `null` when `crispembed` itself is off.
    /// Lets the UI show the correct device choices for the GGUF engine.
    pub crispembed_gpu: Option<&'static str>,
}

#[tauri::command]
pub fn index_capabilities() -> IndexCapabilities {
    // Determine the linked GPU backend by feature. Sub-features are
    // mutually exclusive in practice — only one cmake flag wins per
    // build — so a priority order suffices.
    #[cfg(feature = "crispembed-cuda")]
    const GPU: Option<&str> = Some("cuda");
    #[cfg(all(feature = "crispembed-vulkan", not(feature = "crispembed-cuda")))]
    const GPU: Option<&str> = Some("vulkan");
    #[cfg(all(feature = "crispembed-metal", not(any(feature = "crispembed-cuda", feature = "crispembed-vulkan"))))]
    const GPU: Option<&str> = Some("metal");
    #[cfg(all(
        feature = "crispembed",
        not(any(feature = "crispembed-cuda", feature = "crispembed-vulkan", feature = "crispembed-metal"))
    ))]
    const GPU: Option<&str> = Some("cpu");
    #[cfg(not(feature = "crispembed"))]
    const GPU: Option<&str> = None;

    IndexCapabilities {
        crispembed: cfg!(feature = "crispembed"),
        crispembed_gpu: GPU,
    }
}

/// Approximate first-time download size (in megabytes) for a given embedder
/// model identifier and engine. Lets the UI display something accurate
/// before init runs — the GGUF and ONNX flavours of the same model often
/// differ a lot (e.g. arctic-embed-l-v2 Q4 GGUF is 437 MB whereas the
/// FP32 ONNX is ~1.7 GB).
#[tauri::command]
pub fn index_model_download_mb(model: String, backend: Option<String>) -> u32 {
    use super::embedder::EmbedderModel;
    let normalised = model.replace('_', "-");
    let m: EmbedderModel = match serde_json::from_str(&format!("\"{normalised}\"")) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    match backend.as_deref() {
        Some("gguf") => {
            let g = m.gguf_download_mb();
            if g > 0 { g } else { m.approx_download_mb() }
        }
        _ => m.approx_download_mb(),
    }
}

// ── CrispEmbed registry browse ────────────────────────────────────────────────

/// Single entry in CrispEmbed's bundled model registry.  Mirrors
/// `crispembed::ModelInfo` (which is itself the safe-wrapper view of
/// the C-side `crispembed_model_*` arrays).  Surfacing this through a
/// Tauri command lets the Settings panel display the upstream registry
/// without each new model needing a CrispSorter release.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmbedderRegistryEntry {
    pub name: String,
    pub desc: String,
    pub filename: String,
    pub size: String,
    /// `true` when the GGUF file is already present in the model cache dir.
    pub cached: bool,
}

/// Return CrispEmbed's bundled model registry.  Empty Vec when the
/// `crispembed` cargo feature is off — keeps the frontend unconditional.
///
/// Each entry includes `cached: bool` so the Settings panel can show a
/// "Select" button for already-downloaded models and a "Download" button
/// for the rest, without a separate round-trip.
#[tauri::command]
pub fn embedder_registry_list(state: State<'_, AppState>) -> Vec<EmbedderRegistryEntry> {
    #[cfg(feature = "crispembed")]
    {
        use std::path::PathBuf;
        // Best-effort: resolve cache dir from current config.  Falls back to
        // `None` (unknown) when AppState isn't reachable (shouldn't happen in
        // practice, but we don't want a blocking lock here).
        let cache_dir: Option<PathBuf> = {
            // Try a non-blocking try_lock; fall back to None on contention.
            if let Ok(lock) = state.index.try_lock() {
                let data_dir = state.data_dir.try_lock().ok().and_then(|g| g.clone());
                data_dir.map(|d| super::resolve_model_cache_dir(&lock.config, &d))
            } else {
                None
            }
        };

        crispembed::CrispEmbed::list_models()
            .into_iter()
            .map(|m| {
                let cached = cache_dir.as_ref().map_or(false, |d| {
                    d.join(&m.filename).exists()
                });
                EmbedderRegistryEntry {
                    name: m.name,
                    desc: m.desc,
                    filename: m.filename,
                    size: m.size,
                    cached,
                }
            })
            .collect()
    }
    #[cfg(not(feature = "crispembed"))]
    {
        let _ = state;
        Vec::new()
    }
}

/// Trigger a background download of a registry model by name.  The
/// crispembed `resolve_model` logic checks the cache first and returns
/// immediately if already present, so calling this on a cached model is
/// a no-op.  Returns `Ok(path)` with the resolved cache path on success.
#[tauri::command]
pub async fn embedder_download_registry_model(
    state: State<'_, AppState>,
    name: String,
    accept_license: Option<bool>,
) -> Result<String, String> {
    // License-consent gate: record acceptance if the GUI confirmed, then refuse
    // the download when a restrictive (CC-BY-NC / Gemma) model lacks consent.
    if accept_license.unwrap_or(false) {
        crate::index::license_consent::accept(&name);
    }
    crate::index::license_consent::ensure_for_registry_name(&name).map_err(|e| e.to_string())?;
    #[cfg(feature = "crispembed")]
    {
        use std::path::PathBuf;
        let cache_dir: Option<PathBuf> = {
            if let Ok(lock) = state.index.try_lock() {
                let data_dir = state.data_dir.try_lock().ok().and_then(|g| g.clone());
                data_dir.map(|d| super::resolve_model_cache_dir(&lock.config, &d))
            } else {
                None
            }
        };

        // crispembed resolves by name/alias, checks the cache, downloads if missing.
        let path = tokio::task::spawn_blocking(move || {
            // Override env var so crispembed writes to our models dir.
            if let Some(ref d) = cache_dir {
                unsafe { std::env::set_var("CRISPEMBED_CACHE_DIR", d.as_os_str()) };
            }
            crispembed::CrispEmbed::resolve_model(&name, Some(true))
                .map_err(|e| format!("crispembed resolve_model failed: {e}"))
        })
        .await
        .map_err(|e| format!("spawn_blocking join error: {e}"))??;

        // crispembed::CrispEmbed::resolve_model returns the cached
        // path as `String` (NOT `PathBuf`), so no to_string_lossy
        // conversion is needed.
        Ok(path)
    }
    #[cfg(not(feature = "crispembed"))]
    {
        let _ = (state, name);
        Err("crispembed feature not enabled".into())
    }
}

// ── Model license consent ───────────────────────────────────────────────────

/// Returns the license label if a model (by registry/consent key) requires
/// explicit consent before download/use, else `None`. Lets the GUI decide
/// whether to show the confirmation dialog for any model — not a hardcoded list.
#[tauri::command]
pub fn embedder_model_license(name: String) -> Option<String> {
    let lic = crate::index::license_consent::license_for_registry_name(&name);
    lic.requires_consent().then(|| lic.label().to_string())
}

/// Record consent for one or more restrictive models (by registry/consent key).
/// Called by the GUI after the user confirms the license dialog, and replayed
/// on startup from the persisted accepted-list so search/ingest don't re-prompt.
#[tauri::command]
pub fn embedder_accept_model_license(keys: Vec<String>) {
    for k in keys {
        crate::index::license_consent::accept(&k);
    }
}

// ── Queue depth ───────────────────────────────────────────────────────────────

/// Number of write jobs currently queued or in-flight.
/// Local mode reports the in-process writer-task depth.
/// Remote mode reports the last observed server queue depth for the active
/// foreground remote ingest task.
#[tauri::command]
pub async fn index_queue_depth(state: State<'_, AppState>) -> Result<usize, String> {
    let lock = state.index.lock().await;
    Ok(lock
        .pipeline
        .as_ref()
        .map(|p| p.queue_depth())
        .unwrap_or(lock.remote_queue_depth))
}

// ── Config ────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn index_get_config(state: State<'_, AppState>) -> Result<IndexConfig, String> {
    let lock = state.index.lock().await;
    Ok(lock.config.clone())
}

#[tauri::command]
pub async fn index_set_config(
    state: State<'_, AppState>,
    config: IndexConfig,
) -> Result<(), String> {
    // Snapshot the value we're persisting BEFORE the in-memory move
    // so the disk write reflects exactly what the UI submitted —
    // not some stale clone we forgot to update.
    let to_persist = config.clone();
    let mut lock = state.index.lock().await;
    lock.config = config;
    lock.remote_queue_depth = 0;
    drop(lock);

    // P13.5 follow-up — persist to disk so the user's settings
    // survive an app restart.  Best-effort: failure here is
    // surfaced as a console warning, NOT a Tauri command failure,
    // because the in-memory state already reflects the update and
    // a transient I/O hiccup shouldn't bounce the user back to
    // the old settings panel.  Next call will retry.
    if let Some(dd) = state.data_dir.lock().await.as_ref() {
        if let Err(e) = crate::index::config_persist::save(dd, &to_persist) {
            eprintln!(
                "[index_set_config] persist failed (non-fatal): {e:#} — settings will \
                 reset to defaults on next restart unless saved again"
            );
        }
    } else {
        // Setup hook hasn't run yet — shouldn't happen since
        // index_set_config is called from the UI long after setup
        // completes.  Log so we notice if the ordering ever breaks.
        eprintln!(
            "[index_set_config] data_dir not yet set — settings stored in RAM only \
             until the next index_set_config call"
        );
    }
    Ok(())
}

// ── Init helper ───────────────────────────────────────────────────────────────

/// Progress event payload emitted as `index://init-progress`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InitProgress {
    pub step: &'static str,
    pub label: String,
    pub pct: u8, // 0-100
}

/// Build a complete `IndexState` from a data directory path and config.
///
/// ### Local
/// Creates: Embedder + FtsIndex + LocalIndex + SearchEngine + IngestPipeline.
/// All search and ingest operations run on-device.
///
/// ### Remote
/// Creates: Embedder (local, for query/chunk embedding) + RemoteClient.
/// The server handles vector storage, FTS, and hybrid search.
/// The client is responsible for chunking and embedding before pushing.
pub async fn init_index(
    data_dir: &std::path::Path,
    config: IndexConfig,
    app: Option<tauri::AppHandle>,
    load_embedder: bool,
) -> anyhow::Result<IndexState> {
    use super::embedder::EmbedderConfig as EC;
    use super::remote_client::RemoteClient;
    use super::{
        BackendType, Embedder, FtsIndex, IndexBackend, IndexState, IngestConfig, IngestPipeline,
        LocalIndex, SearchEngine,
    };
    use std::sync::Arc;
    use tauri::Emitter;
    use tokio::sync::Mutex;

    macro_rules! emit {
        ($step:expr, $label:expr, $pct:expr) => {
            crate::app_log!("info", "[index-init] {} ({}%)", $label, $pct);
            if let Some(h) = &app {
                let _ = h.emit(
                    "index://init-progress",
                    InitProgress {
                        step: $step,
                        label: $label.to_owned(),
                        pct: $pct,
                    },
                );
            }
        };
    }

    let model = config.embedder_model;
    let device = config.embedder_device;
    let model_name = format!("{:?}", model);

    // Resolve model cache: env override > UI setting > {data_dir}/models.
    // Same dir is used by fastembed (ONNX), hf-hub (external-data ONNX +
    // GGUF embedder + GGUF reranker) — so a single configurable path
    // controls every downloaded weight. Computed up-front because the
    // reranker handle below needs it even when no embedder is loaded.
    let models_dir = super::resolve_model_cache_dir(&config, data_dir);
    println!("[index] Model cache: {}", models_dir.display());

    // Pre-compute the effective embedding dim so LocalIndex's Arrow schema
    // can be built whether or not we end up loading the embedder.
    let probe_cfg = EC::new(model, device, models_dir.clone())
        .with_backend(config.embedder_backend)
        .with_matryoshka_dim(config.matryoshka_dim)
        .with_model_name_override(config.embedder_model_name.clone());
    let effective_dim = probe_cfg.effective_dim();

    // Optional embedder construction. Skipped when:
    //   - caller passed `load_embedder = false` (L1 / L2 fast-path), or
    //   - global `IndexConfig.use_vector = false` (no vectors at all).
    // Saves multi-GB downloads + minutes of init time.
    let embedder_arc: Option<Arc<Mutex<Embedder>>> = if load_embedder {
        let mb = match config.embedder_backend {
            super::embedder::EmbedderBackend::Gguf => {
                let g = model.gguf_download_mb();
                if g > 0 { g } else { model.approx_download_mb() }
            }
            super::embedder::EmbedderBackend::Onnx => model.approx_download_mb(),
        };
        let size_hint = if mb > 0 { format!(" (~{mb} MB)") } else { String::new() };
        emit!(
            "embedder_start",
            format!("Loading embedder model ({model_name}){size_hint}, first run downloads from the network …"),
            5
        );

        let embedder_cfg = EC::new(model, device, models_dir.clone())
            .with_backend(config.embedder_backend)
            .with_matryoshka_dim(config.matryoshka_dim)
            .with_model_name_override(config.embedder_model_name.clone());
        let embedder = Embedder::new(embedder_cfg).await?;
        emit!("embedder_done", "Embedder loaded", 40);
        Some(Arc::new(Mutex::new(embedder)))
    } else {
        emit!(
            "embedder_skipped",
            "Embedder skipped (L1/L2 mode or use_vector=false)",
            40
        );
        None
    };

    // Reranker handle: cheap to construct (no I/O until first scoring call).
    // Shared between IndexState (kept alive across queries) and SearchEngine
    // (calls score_batch on the post-RRF candidate set).
    let reranker_handle: Option<super::RerankerHandle> = config
        .reranker_model
        .map(|m| super::RerankerHandle::new(m, models_dir.clone()));
    let reranker_multilingual_handle: Option<super::RerankerHandle> = config
        .reranker_model_multilingual
        .map(|m| super::RerankerHandle::new(m, models_dir.clone()));

    // P19 — GLiNER NER handle: cheap to construct (no I/O until first
    // extraction). `None` when `ner_enabled = false`.
    let ner_handle: Option<super::NerHandle> =
        super::ner::handle_from_config(&config, models_dir.clone());

    match config.backend_type {
        BackendType::Remote => {
            let url = config
                .remote_url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("remote_url must be set for Remote backend"))?;
            let key = config.remote_api_key.clone().unwrap_or_default();
            let remote: Arc<dyn IndexBackend> = Arc::new(RemoteClient::new(url, key));

            emit!("done", "Remote-Index verbunden", 100);

            Ok(IndexState {
                backend: Some(remote),
                local: None,
                fts: None,
                embedder: embedder_arc,
                engine: None,
                pipeline: None,
                reranker: reranker_handle,
                config,
                remote_queue_depth: 0,
                initializing: false,
                mounted_cidx: None,
                mounted_cidx_path: None,
                mounted_cidx_fts: None,
            })
        }

        // Hybrid: local-first reads, mirror writes once SyncManager ships.
        // For now behaves identically to Local — same init path.
        BackendType::Hybrid | BackendType::Local => {
            let dims = effective_dim;

            // ── LanceDB ────────────────────────────────────────────────
            // Opens first so the migration runner (which needs a Lance
            // handle) can run before FtsIndex is opened.  The Stage Y
            // v103 migration may delete and rebuild the fts/ dir; FTS
            // must therefore open AFTER migrations complete.
            emit!("lance_start", "Öffne Vektor-Datenbank (LanceDB) …", 55);
            let local = Arc::new(LocalIndex::open_or_create(data_dir, dims).await?);
            emit!("lance_done", "Vektor-Datenbank bereit", 70);

            // ── Schema migrations ──────────────────────────────────────
            //
            // Runs AFTER LocalIndex::open_or_create (which already
            // applied the two legacy ad-hoc migrations for parent_dir
            // + volume_id) and BEFORE FtsIndex::open_or_create so that
            // v103 (FTS rebuild for body_translated) can delete and
            // recreate the fts/ dir, with the init path then opening
            // the freshly-upgraded schema.  Ledger lives in
            // `<data_dir>/.crispsorter_migrations.db`.
            {
                use std::sync::Mutex as StdMutex;
                let ledger_path = data_dir.join(".crispsorter_migrations.db");
                let ledger_conn = rusqlite::Connection::open(&ledger_path)
                    .map_err(|e| anyhow::anyhow!("opening migration ledger {}: {e}", ledger_path.display()))?;
                let ledger = Arc::new(StdMutex::new(ledger_conn));
                let mut runner = crate::migrations::MigrationRunner::new();
                runner.register_all(crate::index::migrations::all())?;
                let ctx = crate::migrations::MigrationContext {
                    lance: Some(local.clone()),
                    sqlite: None,
                    data_dir: data_dir.to_path_buf(),
                };
                let summary = runner.run(&ctx, &ledger).await?;
                if !summary.applied.is_empty() {
                    eprintln!(
                        "[index] applied {} schema migration(s): {:?}",
                        summary.applied.len(),
                        summary.applied
                    );
                }
            }

            // ── Tantivy FTS ────────────────────────────────────────────
            // Opens AFTER migrations so v103 rebuilds are visible here.
            let fts_dir = data_dir.join("fts");
            emit!("fts_start", "Öffne Volltext-Index (Tantivy) …", 75);
            let fts = Arc::new(FtsIndex::open_or_create(&fts_dir)?);
            emit!("fts_done", "Volltext-Index bereit", 90);

            let mut engine_inner = SearchEngine::new(
                fts.clone(),
                local.clone(),
                embedder_arc.clone(),
            );
            if let Some(ref h) = reranker_handle {
                engine_inner = engine_inner.with_reranker(h.clone(), config.rerank_top_n);
            } else if config.use_embedder_as_reranker && embedder_arc.is_some() {
                // P13.5 follow-up — bi-encoder fallback.  Only flips
                // on when the user has NOT installed a dedicated
                // cross-encoder reranker (the `else if` branch
                // guarantees it).  Reuses the loaded dense embedder
                // → no extra model download.  `rerank_top_n` comes
                // from the same IndexConfig field the cross-encoder
                // path uses, so users tune the candidate window
                // once and both paths honour it.
                engine_inner = engine_inner.with_embedder_as_reranker(true, config.rerank_top_n);
            }
            // Stage Z — script-aware multilingual reranker.  Independent of
            // the primary reranker; both can be active simultaneously.
            if let Some(ref h) = reranker_multilingual_handle {
                engine_inner = engine_inner.with_multilingual_reranker(h.clone(), config.rerank_top_n);
            }
            let engine = Arc::new(engine_inner);
            let pipeline = Arc::new(
                IngestPipeline::new(
                    fts.clone(),
                    local.clone(),
                    embedder_arc.clone(),
                    IngestConfig::default(),
                )
                .with_ner(ner_handle.clone()),
            );
            let backend: Arc<dyn IndexBackend> = local.clone();

            emit!("done", "Index bereit", 100);

            Ok(IndexState {
                backend: Some(backend),
                local: Some(local),
                fts: Some(fts),
                embedder: embedder_arc,
                engine: Some(engine),
                pipeline: Some(pipeline),
                reranker: reranker_handle,
                config,
                remote_queue_depth: 0,
                initializing: false,
                mounted_cidx: None,
                mounted_cidx_path: None,
                mounted_cidx_fts: None,
            })
        }
    }
}

// ── P12 — cloud-backup reverse lookup ────────────────────────────────────────

/// Look up a file's cross-tier availability in cloud-backup's manifest SQLite.
/// Reads `source_files` + `archives` to determine where the file lives:
/// local (original_path exists on disk), VPS (archived), cloud (replicated).
///
/// `file_hash`       — from the `crisp+cb-archive://` URI
/// `manifest_db_path` — path to cloud-backup's SQLite database
///
/// Returns `{ original_path, local: bool, vps_archive_id: int|null, status: str }`
#[tauri::command]
pub async fn index_lookup_cb_file(
    manifest_db_path: String,
    file_hash: String,
) -> Result<serde_json::Value, String> {
    use rusqlite::{Connection, OpenFlags};

    let db_path = std::path::PathBuf::from(&manifest_db_path);
    if !db_path.exists() {
        return Err(format!("manifest DB not found: {}", db_path.display()));
    }

    tokio::task::spawn_blocking(move || {
        use rusqlite::OptionalExtension;
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open: {e}"))?;

        // Look up source_files by hash.
        let row: Option<(String, i64, f64, String, Option<i64>)> = conn.query_row(
            "SELECT file_path, file_size_bytes, modified_time, status, archived_in
             FROM source_files WHERE file_hash = ?1 LIMIT 1",
            [&file_hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).optional().map_err(|e| format!("query: {e}"))?;

        let Some((original_path, size, mtime, status, archived_in)) = row else {
            return Ok(serde_json::json!({
                "found": false,
                "file_hash": file_hash,
            }));
        };

        // Check local availability.
        let local_available = std::path::Path::new(&original_path).exists();

        // If archived, fetch full archive row: filename, cloud-upload status, remote path.
        // The `archives` table records:
        //   archive_filename — the .7z name on the VPS
        //   upload_verified  — 1 once the cloud round-trip checksum verified
        //   remote_path      — the Internxt blob path (when uploaded)
        //   local_deleted    — 1 once the original VPS-side file was pruned
        let archive_meta: Option<(Option<String>, i64, Option<String>, i64)> =
            if let Some(aid) = archived_in {
                conn.query_row(
                    "SELECT archive_filename, upload_verified, remote_path, local_deleted
                     FROM archives WHERE archive_id = ?1 LIMIT 1",
                    [aid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                ).optional().map_err(|e| format!("archives query: {e}"))?
            } else {
                None
            };

        let (archive_filename, cloud_uploaded, remote_path, vps_local_deleted) =
            match archive_meta {
                Some((f, v, r, d)) => (f, v != 0, r, d != 0),
                None               => (None, false, None, false),
            };

        Ok(serde_json::json!({
            "found": true,
            "file_hash": file_hash,
            "original_path": original_path,
            "file_size_bytes": size,
            "modified_time": mtime,
            "status": status,
            "local_available": local_available,
            "archived_in": archived_in,
            "archive_filename": archive_filename,
            // P12 — Internxt cloud tier:
            "cloud_uploaded": cloud_uploaded,        // file's archive is verified in Internxt
            "remote_path": remote_path,              // Internxt path of the archive
            "vps_local_deleted": vps_local_deleted,  // VPS-side .7z was pruned (cloud-only)
        }))
    })
    .await
    .map_err(|e| format!("spawn: {e}"))?
}

// ── .cidx mount / browse ─────────────────────────────────────────────────────

/// Mount a `.cidx` archive for offline browse. Stores the read-only
/// `LocalIndex` in `IndexState.mounted_cidx`. Replaces any prior mount.
#[tauri::command]
pub async fn index_mount_cidx(
    state: State<'_, AppState>,
    path: String,
) -> Result<serde_json::Value, String> {
    let cidx_path = std::path::PathBuf::from(&path);
    let idx = crate::index::LocalIndex::open_cidx(&cidx_path)
        .await
        .map_err(|e| e.to_string())?;
    let docs   = idx.count_docs().await.map_err(|e| e.to_string())?;
    let chunks = idx.count().await.map_err(|e| e.to_string())?;

    // Load FTS companion if present.
    let fts_dir = cidx_path.join("fts");
    let fts = if fts_dir.exists() {
        crate::index::FtsIndex::open_or_create(&fts_dir).ok().map(std::sync::Arc::new)
    } else {
        None
    };
    let has_fts = fts.is_some();

    {
        let mut lock = state.index.lock().await;
        lock.mounted_cidx      = Some(std::sync::Arc::new(idx));
        lock.mounted_cidx_path = Some(path.clone());
        lock.mounted_cidx_fts  = fts;
    }
    Ok(serde_json::json!({ "path": path, "docs": docs, "chunks": chunks, "has_fts": has_fts }))
}

/// Unmount the currently-mounted `.cidx`.
#[tauri::command]
pub async fn index_unmount_cidx(state: State<'_, AppState>) -> Result<(), String> {
    let mut lock = state.index.lock().await;
    lock.mounted_cidx      = None;
    lock.mounted_cidx_path = None;
    lock.mounted_cidx_fts  = None;
    Ok(())
}

/// Query documents from the mounted `.cidx`. Identical call shape to
/// `index_query_documents` so the frontend can reuse the same code path.
#[tauri::command]
pub async fn index_query_cidx_documents(
    state: State<'_, AppState>,
    filter: super::schema::DocumentFilter,
    sort: super::schema::SortSpec,
    page: super::schema::PageSpec,
) -> Result<super::schema::DocumentPage, String> {
    let cidx = state.index.lock().await.mounted_cidx.clone()
        .ok_or("No .cidx mounted — call index_mount_cidx first")?;
    cidx.query_documents(&filter, sort, page)
        .await
        .map_err(|e| e.to_string())
}

// ── P10 — failed-extraction CLI helpers ──────────────────────────────────────

/// List all documents that have an `extraction_failure` in their metadata.
/// `retryable_only` restricts to Timeout / Other reasons.
#[tauri::command]
pub async fn index_list_failed_extractions(
    state: State<'_, AppState>,
    retryable_only: Option<bool>,
) -> Result<Vec<serde_json::Value>, String> {
    let local = state.index.lock().await.local.clone()
        .ok_or("Local index not initialised")?;
    let rows = local
        .list_failed_extractions(retryable_only.unwrap_or(false))
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| serde_json::json!({
        "doc_id":       r.doc_id,
        "location_uri": r.location_uri,
        "filename":     r.filename,
        "reason":       r.reason,
        "retryable":    r.retryable,
    })).collect())
}

/// Clear `extraction_failure` for all retryable rows (Timeout / Other).
#[tauri::command]
pub async fn index_retry_all_failed(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let local = state.index.lock().await.local.clone()
        .ok_or("Local index not initialised")?;
    local.retry_all_failed_extractions().await.map_err(|e| e.to_string())
}

// ── P12 — cloud-backup L3 promotion via retrieve.py ──────────────────────────

/// Promote a `crisp+cb-archive://...` row to L3 by spawning `retrieve.py`
/// to download the file from cloud-backup's cheapest available tier, then
/// running the standard extraction + embedding pipeline on it.
///
/// `doc_id`           — the LanceDB doc_id to update
/// `original_path`    — the original file path stored in the cb-archive URI
///                      (used as the --search query for retrieve.py)
/// `retrieve_py_path` — absolute path to the cloud-backup `retrieve.py` script
/// `output_dir`       — where retrieve.py drops the restored file.
///                      Default: `{app_data_dir}/cb_retrieve/`
/// `owner_id`         — owner UUID (nil = single-user install)
///
/// Returns the IngestStats from the full L3 pipeline on success.
#[tauri::command]
pub async fn index_promote_cb_archive(
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
    doc_id: String,
    original_path: String,
    retrieve_py_path: String,
    output_dir: Option<String>,
    owner_id: Option<String>,
) -> Result<serde_json::Value, String> {
    use sha2::{Digest, Sha256};
    use tauri::Manager;

    let owner = owner_id.unwrap_or_else(|| uuid::Uuid::nil().to_string());

    // Resolve output directory.
    let out_dir = if let Some(d) = output_dir {
        std::path::PathBuf::from(d)
    } else {
        handle.path().app_data_dir()
            .map_err(|e| format!("app data dir: {e}"))?
            .join("cb_retrieve")
    };
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir: {e}"))?;

    // Spawn retrieve.py — search by original_path, auto-restore first match.
    // Default interpreter is `python` (Miniconda's name on the user's box).
    // Override via CRISPSORTER_PYTHON env var on hosts where the interpreter
    // is `python3` or lives at an absolute path.
    let py = retrieve_py_path.clone();
    let search_q = original_path.clone();
    let out_arg = out_dir.to_string_lossy().into_owned();
    let py_interp = std::env::var("CRISPSORTER_PYTHON")
        .unwrap_or_else(|_| "python".to_owned());
    let retrieve_out = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&py_interp)
            .args([&py, "-s", &search_q, "-y", "-o", &out_arg])
            .output()
    })
    .await
    .map_err(|e| format!("spawn: {e}"))?
    .map_err(|e| format!("retrieve.py: {e}"))?;

    if !retrieve_out.status.success() {
        let stderr = String::from_utf8_lossy(&retrieve_out.stderr);
        return Err(format!("retrieve.py failed: {}", stderr.trim()));
    }

    // Find the restored file in out_dir (newest file added).
    let basename = std::path::Path::new(&original_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let restored_path = out_dir.join(&basename);
    if !restored_path.exists() {
        // Fallback: find newest file in out_dir.
        let newest = std::fs::read_dir(&out_dir)
            .ok()
            .and_then(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                    .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
            })
            .map(|e| e.path());
        if let Some(p) = newest {
            return promote_path(state, doc_id, p, owner, original_path).await;
        }
        return Err(format!("restored file not found at {}", restored_path.display()));
    }

    promote_path(state, doc_id, restored_path, owner, original_path).await
}

async fn promote_path(
    state: State<'_, AppState>,
    doc_id: String,
    restored_path: std::path::PathBuf,
    owner: String,
    original_path: String,
) -> Result<serde_json::Value, String> {
    use sha2::{Digest, Sha256};

    let pipeline = state.index.lock().await.pipeline.clone()
        .ok_or("No local ingest pipeline")?;

    let bytes = tokio::task::spawn_blocking({
        let p = restored_path.clone();
        move || std::fs::read(&p)
    }).await
    .map_err(|e| format!("read join: {e}"))?
    .map_err(|e| format!("read: {e}"))?;

    let mut h = Sha256::new();
    h.update(&bytes);
    let source_hash = hex::encode(h.finalize());

    let extracted = tokio::task::spawn_blocking({
        let p = restored_path.clone();
        move || crate::extractors::extract_text_from_path(&p)
    }).await
    .map_err(|e| format!("extract join: {e}"))?
    .map_err(|e| format!("extract: {e}"))?;

    let loc = crate::index::location::FileLocation::Local {
        user_id: uuid::Uuid::parse_str(&owner).unwrap_or_else(|_| uuid::Uuid::nil()),
        machine_id: uuid::Uuid::nil(),
        path: std::path::PathBuf::from(&original_path),
    };

    let bg_meta = restored_path.metadata().ok();
    let raw = crate::index::ingest::RawDocument {
        full_text: extracted.full_text,
        full_text_md: String::new(),
        headings: extracted.headings,
        title: None,
        author: None,
        year: None,
        filename: restored_path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        ext: extracted.ext,
        language: String::new(),
        source_hash,
        location_uri: loc.to_uri(),
        owner_id: owner,
        tags: vec![],
        mtime_unix: bg_meta.as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64),
        file_size: bg_meta.map(|m| m.len() as i64),
        volume_id: None,
        parent_dir: std::path::Path::new(&original_path)
            .parent()
            .and_then(|p| p.to_str())
            .map(|s| s.to_owned()),
        translated_text: extracted.translated_text,
        translated_to_lang: extracted.translated_to_lang,
        // P13.6 Step 7 — audio L2 from the symphonia probe inside
        // extractors::audio::extract.  None for non-audio extractors.
        audio_duration_seconds: extracted.audio.as_ref().and_then(|a| a.duration_seconds),
        audio_codec: extracted.audio.as_ref().and_then(|a| a.codec.clone()),
        audio_sample_rate_hz: extracted.audio.as_ref().and_then(|a| a.sample_rate_hz.map(|s| s as i32)),
        audio_channels: extracted.audio.as_ref().and_then(|a| a.channels.map(|c| c as i32)),
        audio_bitrate_kbps: extracted.audio.as_ref().and_then(|a| a.bitrate_kbps.map(|b| b as i32)),
        // P13.6 Step 9 — image L2 (EXIF) curated subset.
        image_camera_make:  extracted.image_exif.as_ref().and_then(|e| e.camera_make.clone()),
        image_camera_model: extracted.image_exif.as_ref().and_then(|e| e.camera_model.clone()),
        image_lens_model:   extracted.image_exif.as_ref().and_then(|e| e.lens_model.clone()),
        image_taken_at_unix: extracted.image_exif.as_ref().and_then(|e| e.taken_at_unix),
        image_iso:          extracted.image_exif.as_ref().and_then(|e| e.iso.map(|i| i as i32)),
        multivec_packed: None,
        multivec_n_tokens: None,
        url: None,
    };

    // Delete the existing L1 row before re-ingesting.
    if let Some(local) = state.index.lock().await.local.clone() {
        let _ = local.delete_doc(&doc_id).await;
    }

    let stats = pipeline.ingest_document(raw).await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "doc_id": doc_id,
        "chunks": stats.chunk_count,
        "embed_ms": stats.embed_time_ms,
    }))
}

// ── P12 — cloud-backup manifest import ───────────────────────────────────────

/// Import file metadata from a cloud-backup SQLite manifest database as L1
/// index rows. No file content is read — this is a filesystem-metadata-only
/// pass that makes the entire backup tree browsable in Übersicht instantly.
///
/// Queries `source_files` (original paths + sizes + mtimes + hashes) and
/// builds one L1 `DocumentChunk` per row with a `crisp+cb-archive://...`
/// or `crisp+local://...` URI depending on whether the file is archived.
///
/// Returns `{ ingested, skipped, errors }`.
#[tauri::command]
pub async fn index_ingest_cb_manifest(
    state: State<'_, AppState>,
    manifest_db_path: String,
    owner_id: Option<String>,
) -> Result<serde_json::Value, String> {
    use rusqlite::{Connection, OpenFlags};

    let pipeline = state.index.lock().await.pipeline.clone()
        .ok_or("No local ingest pipeline initialised")?;
    let owner = owner_id.unwrap_or_else(|| uuid::Uuid::nil().to_string());

    // Open the cloud-backup SQLite read-only.
    let db_path = std::path::PathBuf::from(&manifest_db_path);
    if !db_path.exists() {
        return Err(format!("manifest DB not found: {}", db_path.display()));
    }
    let conn = tokio::task::spawn_blocking(move || {
        Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    })
    .await
    .map_err(|e| format!("spawn: {e}"))?
    .map_err(|e| format!("open sqlite: {e}"))?;

    // Pull all non-deleted source files.
    let rows: Vec<(String, i64, f64, Option<String>, Option<i64>)> =
        tokio::task::spawn_blocking(move || {
            let mut stmt = conn.prepare(
                "SELECT file_path, file_size_bytes, modified_time, file_hash, archived_in
                 FROM source_files
                 WHERE status NOT IN ('deleted','error')
                 ORDER BY file_path",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| format!("spawn: {e}"))?
        .map_err(|e| format!("query: {e}"))?;

    // Build L1FileEntry batch (64 at a time).
    let mut ingested = 0usize;
    let mut errors   = 0usize;
    const BATCH: usize = 64;

    let chunks = rows.chunks(BATCH);
    for chunk in chunks {
        let entries: Vec<crate::index::ingest::L1FileEntry> = chunk
            .iter()
            .map(|(path, size, mtime, hash, archived_in)| {
                let hash_str = hash.clone().unwrap_or_default();
                // doc_id: use hash; fall back to UUID if empty.
                let doc_id = if hash_str.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    hash_str.clone()
                };
                // URI: crisp+cb-archive when archived, crisp+local otherwise.
                let location_uri = if let Some(archive_id) = archived_in {
                    crate::index::location::FileLocation::CbArchive {
                        archive_id: *archive_id,
                        file_hash: hash_str.clone(),
                        original_path: path.clone(),
                    }
                    .to_uri()
                } else {
                    crate::index::location::FileLocation::Local {
                        user_id: uuid::Uuid::parse_str(&owner)
                            .unwrap_or_else(|_| uuid::Uuid::nil()),
                        machine_id: uuid::Uuid::nil(),
                        path: std::path::PathBuf::from(path),
                    }
                    .to_uri()
                };

                let p = std::path::Path::new(path);
                let filename = p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let ext = p.extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default();
                let parent_dir = p.parent()
                    .and_then(|d| d.to_str())
                    .unwrap_or("")
                    .to_owned();

                crate::index::ingest::L1FileEntry {
                    doc_id,
                    location_uri,
                    owner_id: owner.clone(),
                    filename,
                    ext,
                    source_hash: hash_str,
                    mtime_ms: (*mtime * 1000.0) as i64,
                    ctime_ms: 0,
                    size: *size,
                    parent_dir,
                    volume_id: None,
                }
            })
            .collect();

        match pipeline.ingest_l1(&entries).await {
            Ok(stats) => ingested += stats.chunk_count,
            Err(e) => {
                errors += 1;
                eprintln!("[cb-manifest] batch error: {e}");
            }
        }
    }

    Ok(serde_json::json!({
        "ingested": ingested,
        "total_rows": rows.len(),
        "errors": errors,
    }))
}

// ── P11/Pillar 5 — generic CloudDrive ingest + promote ───────────────────────

/// Walk a registered cloud drive and write **L1 metadata-only** rows to the
/// local index.  No file content is fetched; the user can hit "Promote to L3"
/// later via `index_promote_drive_archive` to download + extract a specific
/// file on demand.
///
/// `root_path` is the subroot inside the drive to walk (e.g. `/Documents`).
/// `allowed_exts` is a case-insensitive ext filter (without the dot); when
/// empty / None, every file is indexed.  `max_depth` caps recursion (`None`
/// = unbounded).
///
/// Returns `{ ingested, walked, errors }`.  `walked` is the count of file
/// entries the recursive walker emitted; `ingested` is how many made it
/// into LanceDB+Tantivy after L1FileEntry batches went through.
#[tauri::command]
pub async fn index_ingest_drive_manifest(
    state: State<'_, AppState>,
    drive_id: String,
    root_path: String,
    allowed_exts: Option<Vec<String>>,
    max_depth: Option<usize>,
    owner_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let pipeline = state.index.lock().await.pipeline.clone()
        .ok_or("No local ingest pipeline initialised")?;
    let owner = owner_id.unwrap_or_else(|| uuid::Uuid::nil().to_string());

    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;

    // Walk the drive on a blocking thread (CloudDrive ops are sync).
    let did = drive_id.clone();
    let root = root_path.clone();
    let allowed: std::collections::HashSet<String> = allowed_exts
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .collect();
    let walk_res = tokio::task::spawn_blocking(move || {
        let reg = crate::drives::DriveRegistry::open(&data_dir)
            .map_err(|e| format!("open registry: {e}"))?;
        let cfg = reg.drives.iter().find(|d| d.id == did)
            .ok_or_else(|| format!("drive '{did}' not found"))?
            .clone();
        let drive = crate::drives::DriveRegistry::instantiate(&cfg);
        let mut walk_errors: Vec<String> = Vec::new();
        let entries = crate::drives::walk(
            drive.as_ref(),
            std::path::Path::new(&root),
            max_depth,
            &mut |p, e| walk_errors.push(format!("{}: {e:#}", p.display())),
        );
        Ok::<_, String>((entries, walk_errors))
    })
    .await
    .map_err(|e| format!("walk join: {e}"))??;

    let (entries, walk_errors) = walk_res;
    let walked = entries.len();

    // Filter by extension and convert to L1FileEntry.
    let mut to_ingest: Vec<crate::index::ingest::L1FileEntry> = Vec::with_capacity(entries.len());
    for ent in &entries {
        let p = ent.path.clone();
        let ext = p.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if !allowed.is_empty() && !allowed.contains(&ext) { continue; }

        let location_uri = crate::index::location::FileLocation::Drive {
            drive_id:    drive_id.clone(),
            remote_path: p.to_string_lossy().into_owned(),
        }
        .to_uri();

        let filename = p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent_dir = p.parent()
            .and_then(|d| d.to_str())
            .unwrap_or("")
            .to_owned();

        // doc_id = sha256(location_uri).  We never read the bytes during
        // manifest scan, so a content hash isn't available; the URI is
        // stable enough to dedupe within the drive.
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(location_uri.as_bytes());
        let doc_id = hex::encode(h.finalize());

        to_ingest.push(crate::index::ingest::L1FileEntry {
            doc_id,
            source_hash: String::new(),
            location_uri,
            owner_id: owner.clone(),
            filename,
            ext,
            parent_dir,
            size: ent.size.unwrap_or(0) as i64,
            mtime_ms: ent.mtime_unix.unwrap_or(0).saturating_mul(1000),
            ctime_ms: 0,
            volume_id: Some(format!("drive:{drive_id}")),
        });
    }

    let mut ingested = 0usize;
    let mut errors = walk_errors.len();
    const BATCH: usize = 64;
    for chunk in to_ingest.chunks(BATCH) {
        match pipeline.ingest_l1(chunk).await {
            Ok(stats) => ingested += stats.chunk_count,
            Err(e) => {
                errors += 1;
                eprintln!("[drive-ingest] batch error: {e}");
            }
        }
    }

    Ok(serde_json::json!({
        "ingested":   ingested,
        "walked":     walked,
        "errors":     errors,
        "walk_errors": walk_errors,
    }))
}

/// Fetch a file from a registered cloud drive and re-ingest it as L3
/// (full content + embeddings).  Mirrors `index_promote_cb_archive` but
/// uses the `CloudDrive` trait instead of cloud-backup's `retrieve.py`.
///
/// Triggered by the "Promote to L3" button in Übersicht's preview pane
/// when a `crisp+drive://...` row is open.
#[tauri::command]
pub async fn index_promote_drive_archive(
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
    doc_id: String,
    drive_id: String,
    remote_path: String,
    owner_id: Option<String>,
) -> Result<serde_json::Value, String> {
    use tauri::Manager;

    let owner = owner_id.unwrap_or_else(|| uuid::Uuid::nil().to_string());

    // Resolve a staging dir under app_data/drive_retrieve/.
    let out_dir = handle.path().app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?
        .join("drive_retrieve");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir: {e}"))?;

    // Fetch bytes via the registered drive (off the runtime thread).
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let drive_id_clone = drive_id.clone();
    let remote = remote_path.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        let reg = crate::drives::DriveRegistry::open(&data_dir)
            .map_err(|e| format!("open registry: {e}"))?;
        let cfg = reg.drives.iter().find(|d| d.id == drive_id_clone)
            .ok_or_else(|| format!("drive '{drive_id_clone}' not found"))?
            .clone();
        let drive = crate::drives::DriveRegistry::instantiate(&cfg);
        drive.read_file(std::path::Path::new(&remote))
            .map_err(|e| format!("read_file: {e:#}"))
    })
    .await
    .map_err(|e| format!("drive read join: {e}"))??;

    // Stage the file under app_data/drive_retrieve/ so promote_path can
    // re-read + extract from disk like it does for cb-archive.  We use
    // the basename verbatim (collisions get overwritten — same file
    // re-promote is idempotent).
    let basename = std::path::Path::new(&remote_path).file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("remote path has no basename: {remote_path}"))?;
    let staged = out_dir.join(&basename);
    tokio::task::spawn_blocking({
        let p = staged.clone();
        let b = bytes;
        move || std::fs::write(&p, &b)
    })
    .await
    .map_err(|e| format!("stage join: {e}"))?
    .map_err(|e| format!("stage write: {e}"))?;

    // Reuse the cb-archive promote pipeline: it does dedup, extract,
    // embed, ingest — and crucially deletes the existing L1 row before
    // re-inserting at L3 so we don't end up with duplicate doc_ids.
    promote_path(state, doc_id, staged, owner, remote_path).await
}

// ── Volume helpers ────────────────────────────────────────────────────────────

/// List all currently-mounted volumes with their stable UUID, mount point,
/// and human label. Used by the frontend to:
///   1. Show "drive plugged in / unplugged" status next to catalog entries.
///   2. Build the `volume_ids` allowlist for `index_query_documents` so
///      search results from unmounted drives can be hidden.
///
/// Returns an empty array on platforms where the helper isn't available or
/// any OS call fails — frontend falls back to "no per-volume filter".
#[tauri::command]
pub fn index_list_mounted_volumes() -> Vec<crate::volume::MountedVolume> {
    crate::volume::list_mounted_volumes()
}

/// Resolve a filesystem path to its volume's stable UUID (diskutil UUID on
/// macOS, blkid UUID on Linux, VolumeSerialNumber on Windows).
///
/// Returns `None` when:
///   * `path` does not currently exist (drive not mounted)
///   * the volume has no UUID (tmpfs, some network shares)
///   * the platform helper fails
///
/// Intended for one-time capture at catalog-creation or scan time so the
/// UUID can be stored in `RegisteredCatalog.volumeId` for later offline use.
#[tauri::command]
pub fn index_volume_id_for_path(path: String) -> Option<String> {
    crate::volume::volume_id_for_path(std::path::Path::new(&path))
}

// ── Private helpers ───────────────────────────────────────────────────────────

async fn embed_query(
    embedder: Option<std::sync::Arc<tokio::sync::Mutex<super::Embedder>>>,
    text: &str,
) -> Result<Vec<f32>, String> {
    use super::embedder::EmbedRole;
    let embedder = embedder.ok_or("Embedder not available for remote query embedding")?;
    let mut emb = embedder.lock().await;
    let dense = emb
        .embed_dense(vec![text.to_owned()], EmbedRole::Query)
        .map_err(|e| e.to_string())?;
    dense
        .vectors
        .into_iter()
        .next()
        .ok_or_else(|| "Embedder returned no vectors".to_owned())
}

// ── Per-file document tools (GUI surface for the `ocr`/`kie`/`table` CLI) ─────
//
// These wrap the same underlying CrispEmbed-backed functions the CLI uses, so
// the GUI "Doc tools" buttons in IndexSearch produce byte-identical artifacts.
// All resolve a `location_uri` to a local path (cloud-drive rows must be
// downloaded first) and run the heavy work on a blocking thread.

/// Result of [`tool_ocr_export`]: a structured/searchable artifact written
/// beside the source file.
#[derive(serde::Serialize)]
pub struct OcrExportResult {
    pub saved_path: String,
    pub bytes: usize,
    pub pages: usize,
    pub format: String,
}

/// Export a file's OCR as txt / hOCR / ALTO / searchable PDF / PDF-A, written
/// beside the source as `<stem>.ocr.<ext>`. Reuses the user's persisted Smart
/// OCR Pipeline config (Settings → Smart OCR Pipeline), so dewarp / restore /
/// super-resolution / layout choices apply. GUI surface for `crispsorter ocr
/// --render`.
#[tauri::command]
pub async fn tool_ocr_export(
    state: State<'_, AppState>,
    location_uri: String,
    format: String,
    pdfa: bool,
) -> Result<OcrExportResult, String> {
    use crate::extractors::ocr_render::OcrOutputFormat;

    let path = crate::images::tauri_commands::location_uri_to_local_path(&location_uri)
        .ok_or_else(|| {
            format!("location_uri does not map to a local path: {location_uri} (download cloud files first)")
        })?;
    if !path.exists() {
        return Err(format!("file not present on disk: {}", path.display()));
    }
    let fmt = OcrOutputFormat::from_name(&format)
        .ok_or_else(|| format!("unknown render format: {format}"))?;

    // Reuse the persisted Smart-OCR-pipeline config; force it on for this run.
    let mut cfg = state.bg_ingest.lock().await.ocr_pipeline.clone();
    cfg.enabled = true;

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output")
        .to_string();
    let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let out_path = parent.join(format!("{stem}.ocr.{}", fmt.ext()));

    let out2 = out_path.clone();
    let (bytes_written, n_pages) = tokio::task::spawn_blocking(
        move || -> Result<(usize, usize), String> {
            let (bytes, pages) = if fmt == OcrOutputFormat::Text {
                let opts = crate::extractors::ExtractOptions {
                    try_ocr: true,
                    ocr_pdf_min_chars: usize::MAX,
                    ocr_pipeline: cfg,
                    ..Default::default()
                };
                let doc = crate::extractors::extract_text_from_path_with_opts(&path, opts)
                    .map_err(|e| format!("OCR failed: {e:#}"))?;
                (doc.full_text.into_bytes(), 1usize)
            } else {
                let pages = crate::extractors::ocr_render::render_pages_from_file(&path, &ext, &cfg)?;
                let n = pages.len();
                let bytes = crate::extractors::ocr_render::render(&pages, fmt, pdfa)
                    .map_err(|e| format!("render failed: {e:#}"))?;
                (bytes, n)
            };
            std::fs::write(&out2, &bytes)
                .map_err(|e| format!("writing {}: {e}", out2.display()))?;
            Ok((bytes.len(), pages))
        },
    )
    .await
    .map_err(|e| format!("ocr export join error: {e}"))??;

    Ok(OcrExportResult {
        saved_path: out_path.display().to_string(),
        bytes: bytes_written,
        pages: n_pages,
        format: if pdfa && fmt == OcrOutputFormat::Pdf {
            "pdfa".to_string()
        } else {
            format
        },
    })
}

/// One extracted key-information field.
#[derive(serde::Serialize)]
pub struct KieFieldDto {
    pub label: String,
    pub value: String,
    pub score: f32,
}

/// Key-information extraction over a single file. With `lilt`, runs layout-aware
/// LiLT token classification directly on the image; otherwise OCRs the document
/// and runs zero-shot GLiNER NER over the text. Empty `labels` falls back to the
/// labels configured under Settings → NER. GUI surface for `crispsorter kie`.
#[tauri::command]
pub async fn tool_kie_extract(
    state: State<'_, AppState>,
    location_uri: String,
    labels: Vec<String>,
    threshold: Option<f32>,
    lilt: bool,
    lilt_model: Option<String>,
) -> Result<Vec<KieFieldDto>, String> {
    let path = crate::images::tauri_commands::location_uri_to_local_path(&location_uri)
        .ok_or_else(|| {
            format!("location_uri does not map to a local path: {location_uri} (download cloud files first)")
        })?;
    if !path.exists() {
        return Err(format!("file not present on disk: {}", path.display()));
    }

    // Default labels + threshold + model come from the persisted NER settings.
    let (cfg_labels, ner_model, ner_threshold) = {
        let lock = state.index.lock().await;
        (
            lock.config.ner_labels.clone(),
            lock.config.ner_model.clone(),
            lock.config.ner_threshold,
        )
    };
    let labels = if labels.is_empty() { cfg_labels } else { labels };
    if labels.is_empty() {
        return Err("no KIE labels given and Settings → NER has none configured".to_string());
    }
    let thr = threshold.unwrap_or(ner_threshold);

    let fields: Vec<(String, String, f32)> = if lilt || lilt_model.is_some() {
        let path2 = path.clone();
        let labels2 = labels.clone();
        tokio::task::spawn_blocking(move || {
            crate::extractors::ocr_crispembed::kie_extract_lilt(
                &path2,
                &labels2,
                thr,
                lilt_model.as_deref(),
            )
        })
        .await
        .map_err(|e| format!("kie join error: {e}"))?
        .map_err(|e| format!("LiLT KIE failed: {e:#}"))?
    } else {
        // GLiNER path: OCR → NER over the extracted text.
        let data_dir = state
            .data_dir
            .lock()
            .await
            .clone()
            .ok_or_else(|| "index not initialised (no data_dir)".to_string())?;
        let cache_dir = {
            let lock = state.index.lock().await;
            super::resolve_model_cache_dir(&lock.config, &data_dir)
        };
        let model = ner_model.unwrap_or_default();
        let path2 = path.clone();
        let doc = tokio::task::spawn_blocking(move || {
            let opts = crate::extractors::ExtractOptions {
                try_ocr: true,
                ocr_pdf_min_chars: usize::MAX,
                ..Default::default()
            };
            crate::extractors::extract_text_from_path_with_opts(&path2, opts)
        })
        .await
        .map_err(|e| format!("extract join error: {e}"))?
        .map_err(|e| format!("extract failed: {e:#}"))?;
        if doc.full_text.trim().is_empty() {
            return Err("no text extracted from document".to_string());
        }
        let handle =
            crate::index::ner::NerHandle::new(model, labels, thr, 0, 200_000, cache_dir);
        handle.extract_fields(&doc.full_text).await
    };

    Ok(fields
        .into_iter()
        .map(|(label, value, score)| KieFieldDto { label, value, score })
        .collect())
}

/// Result of [`tool_table_extract`]: detected grid + HTML, optionally saved.
#[derive(serde::Serialize)]
pub struct TableExtractResult {
    pub rows: i32,
    pub cols: i32,
    pub html: String,
    pub saved_path: Option<String>,
}

/// Parse a table image into HTML (and report its grid dimensions). When `save`,
/// writes `<stem>.table.html` beside the source. GUI surface for `crispsorter
/// table`.
#[tauri::command]
pub async fn tool_table_extract(
    state: State<'_, AppState>,
    location_uri: String,
    ocr_model: Option<String>,
    save: bool,
) -> Result<TableExtractResult, String> {
    let _ = &state; // resolved purely from the URI; state kept for signature parity
    let path = crate::images::tauri_commands::location_uri_to_local_path(&location_uri)
        .ok_or_else(|| {
            format!("location_uri does not map to a local path: {location_uri} (download cloud files first)")
        })?;
    if !path.exists() {
        return Err(format!("file not present on disk: {}", path.display()));
    }

    let path2 = path.clone();
    let (rows, cols, html) = tokio::task::spawn_blocking(
        move || -> Result<(i32, i32, String), String> {
            let (rows, cols) = crate::extractors::ocr_crispembed::table_grid(&path2)
                .map_err(|e| format!("grid detection failed: {e:#}"))?;
            let html = crate::extractors::ocr_crispembed::table_to_html(&path2, ocr_model.as_deref())
                .map_err(|e| format!("table parse failed: {e:#}"))?;
            Ok((rows, cols, html))
        },
    )
    .await
    .map_err(|e| format!("table join error: {e}"))??;

    let saved_path = if save {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("table")
            .to_string();
        let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let out = parent.join(format!("{stem}.table.html"));
        std::fs::write(&out, html.as_bytes())
            .map_err(|e| format!("writing {}: {e}", out.display()))?;
        Some(out.display().to_string())
    } else {
        None
    };

    Ok(TableExtractResult { rows, cols, html, saved_path })
}

// ── OCR Workbench (interactive proofreading screen) ──────────────────────────
//
// Commands powering the user-steered OCR review loop: open a doc into per-page
// images, OCR a page into regions+confidence, produce the cleaned image to
// compare against, and save corrected results (export / re-ingest / sidecar).

/// Resolve a location_uri to a present local path (shared by the commands below).
fn workbench_resolve(location_uri: &str) -> Result<std::path::PathBuf, String> {
    let path = crate::images::tauri_commands::location_uri_to_local_path(location_uri)
        .ok_or_else(|| format!("location_uri does not map to a local path: {location_uri}"))?;
    if !path.exists() {
        return Err(format!("file not present on disk: {}", path.display()));
    }
    Ok(path)
}

/// Per-document page list returned by [`ocr_doc_open`].
#[derive(serde::Serialize)]
pub struct OcrDocPages {
    pub count: usize,
    /// Stable local image paths, one per page (load via `convertFileSrc`).
    pub pages: Vec<String>,
}

/// Open a document for the workbench: rasterize a PDF/TIFF into per-page PNGs
/// (or pass a single image straight through), copied to a stable temp dir so
/// the frontend can load + re-OCR pages across calls.
#[tauri::command]
pub async fn ocr_doc_open(location_uri: String) -> Result<OcrDocPages, String> {
    let path = workbench_resolve(&location_uri)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let pages = tokio::task::spawn_blocking(move || -> Result<Vec<String>, String> {
        use crate::extractors::page_source;
        // Single image: already a stable source path — no rasterization needed.
        const IMG_EXTS: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff", "bmp", "webp"];
        if ext != "pdf" && IMG_EXTS.contains(&ext.as_str()) && ext != "tif" && ext != "tiff" {
            return Ok(vec![path.display().to_string()]);
        }
        let images = if ext == "pdf" {
            page_source::rasterize_pdf(&path).map_err(|e| format!("rasterize PDF: {e:#}"))?
        } else {
            page_source::rasterize_pages(&path, &ext).map_err(|e| format!("rasterize: {e:#}"))?
        };
        // Copy each page to a stable per-doc dir (rasterized temp dir is dropped
        // when `images` goes out of scope).
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.display().to_string().hash(&mut hasher);
        let id = format!("{:016x}", hasher.finish());
        let dir = std::env::temp_dir().join("crispsorter_ocr_workbench").join(&id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create temp dir: {e}"))?;
        let mut out = Vec::with_capacity(images.len());
        for (i, p) in images.paths().iter().enumerate() {
            let dst = dir.join(format!("page_{:04}.png", i + 1));
            std::fs::copy(p, &dst).map_err(|e| format!("copy page {}: {e}", i + 1))?;
            out.push(dst.display().to_string());
        }
        Ok(out)
    })
    .await
    .map_err(|e| format!("ocr_doc_open join error: {e}"))??;

    Ok(OcrDocPages { count: pages.len(), pages })
}

/// One OCR region returned to the workbench.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct OcrRegionDto {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub confidence: f32,
    /// Per-character confidence (PARSeq / Tesseract-LSTM); empty otherwise.
    #[serde(default)]
    pub char_conf: Vec<f32>,
}

/// Result of OCR'ing one page.
#[derive(serde::Serialize)]
pub struct OcrPageResult {
    pub width: i32,
    pub height: i32,
    pub regions: Vec<OcrRegionDto>,
}

/// OCR a single page image into regions (box + text + confidence) using the
/// persisted Smart-OCR-pipeline config. Coordinates are in image pixels.
#[tauri::command]
pub async fn ocr_page_regions(
    state: State<'_, AppState>,
    page_path: String,
) -> Result<OcrPageResult, String> {
    let path = std::path::PathBuf::from(&page_path);
    if !path.exists() {
        return Err(format!("page image not found: {page_path}"));
    }
    let mut cfg = state.bg_ingest.lock().await.ocr_pipeline.clone();
    cfg.enabled = true;

    tokio::task::spawn_blocking(move || -> Result<OcrPageResult, String> {
        let regions = crate::extractors::ocr_crispembed::ocr_regions_detailed(&path, &cfg)
            .map_err(|e| format!("OCR failed: {e:#}"))?;
        let (w, h) = image::image_dimensions(&path).unwrap_or((0, 0));
        Ok(OcrPageResult {
            width: w as i32,
            height: h as i32,
            regions: regions
                .into_iter()
                .map(|r| OcrRegionDto {
                    text: r.text,
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: r.h,
                    confidence: r.confidence,
                    char_conf: r.char_conf,
                })
                .collect(),
        })
    })
    .await
    .map_err(|e| format!("ocr_page_regions join error: {e}"))?
}

/// Produce the cleaned (classical-cleanup) image for a page → stable temp PNG
/// path, for the workbench's original-vs-cleaned compare. Empty string when
/// cleanup is unavailable (e.g. CrispEmbed not linked).
#[tauri::command]
pub async fn ocr_page_cleaned(page_path: String) -> Result<String, String> {
    let path = std::path::PathBuf::from(&page_path);
    if !path.exists() {
        return Err(format!("page image not found: {page_path}"));
    }
    tokio::task::spawn_blocking(move || {
        crate::extractors::ocr_crispembed::cleaned_page_image(&path)
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    })
    .await
    .map_err(|e| format!("ocr_page_cleaned join error: {e}"))
}

/// One page of corrected regions sent back by the workbench for export.
#[derive(serde::Deserialize)]
pub struct WorkbenchPageInput {
    pub image_path: String,
    pub width: i32,
    pub height: i32,
    pub regions: Vec<OcrRegionDto>,
}

/// Export the workbench's (possibly user-corrected) regions as a structured /
/// searchable artifact (text / hOCR / ALTO / searchable PDF / PDF-A), written
/// beside the source as `<stem>.ocr.<ext>`. Reuses the `ocr_render` path so the
/// corrected text flows into the output layer.
#[tauri::command]
pub async fn ocr_workbench_export(
    location_uri: String,
    format: String,
    pdfa: bool,
    pages: Vec<WorkbenchPageInput>,
) -> Result<OcrExportResult, String> {
    use crate::extractors::ocr_render::{OcrOutputFormat, OcrRegion, RenderPage};

    let path = workbench_resolve(&location_uri)?;
    let fmt = OcrOutputFormat::from_name(&format)
        .ok_or_else(|| format!("unknown render format: {format}"))?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output").to_string();
    let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let out_path = parent.join(format!("{stem}.ocr.{}", fmt.ext()));

    let n_pages = pages.len();
    let out2 = out_path.clone();
    let bytes = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let render_pages: Vec<RenderPage> = pages
            .into_iter()
            .map(|p| {
                let regions = p
                    .regions
                    .into_iter()
                    .map(|r| OcrRegion {
                        text: r.text,
                        x: r.x,
                        y: r.y,
                        w: r.w,
                        h: r.h,
                        confidence: r.confidence,
                    })
                    .collect();
                RenderPage::from_regions(regions, p.width, p.height, p.image_path)
            })
            .collect();
        let bytes = crate::extractors::ocr_render::render(&render_pages, fmt, pdfa)
            .map_err(|e| format!("render failed: {e:#}"))?;
        std::fs::write(&out2, &bytes).map_err(|e| format!("writing {}: {e}", out2.display()))?;
        Ok(bytes.len())
    })
    .await
    .map_err(|e| format!("ocr_workbench_export join error: {e}"))??;

    Ok(OcrExportResult {
        saved_path: out_path.display().to_string(),
        bytes,
        pages: n_pages,
        format: if pdfa && fmt == OcrOutputFormat::Pdf { "pdfa".into() } else { format },
    })
}

/// Write the workbench's corrected text to a sidecar `<stem>.corrected.txt`.
#[tauri::command]
pub async fn ocr_workbench_sidecar(
    location_uri: String,
    text: String,
) -> Result<String, String> {
    let path = workbench_resolve(&location_uri)?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output").to_string();
    let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let out = parent.join(format!("{stem}.corrected.txt"));
    std::fs::write(&out, text.as_bytes()).map_err(|e| format!("writing {}: {e}", out.display()))?;
    Ok(out.display().to_string())
}

/// Re-ingest a document into the local index with the workbench's corrected
/// text (so search reflects the fixes). Mirrors the image L3 promote, but uses
/// the provided text instead of re-extracting.
#[tauri::command]
pub async fn ocr_workbench_reingest(
    state: State<'_, AppState>,
    location_uri: String,
    text: String,
) -> Result<IngestStats, String> {
    let path = workbench_resolve(&location_uri)?;

    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Index is disabled in settings".to_owned());
    }
    let pipeline = lock
        .pipeline
        .clone()
        .ok_or_else(|| "No local ingest pipeline (remote backend?)".to_string())?;
    drop(lock);

    let p_meta = std::fs::metadata(&path).ok();
    let source_hash = {
        let p = path.clone();
        tokio::task::spawn_blocking(move || -> Result<String, String> {
            use sha2::{Digest, Sha256};
            let bytes = std::fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
            let mut h = Sha256::new();
            h.update(&bytes);
            Ok(hex::encode(h.finalize()))
        })
        .await
        .map_err(|e| format!("source_hash spawn_blocking: {e}"))??
    };

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let raw = RawDocument {
        full_text: text.clone(),
        full_text_md: text,
        headings: Vec::new(),
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        author: None,
        year: None,
        filename: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        ext,
        language: String::new(),
        source_hash,
        location_uri: location_uri.clone(),
        owner_id: "local".to_string(),
        tags: vec![],
        mtime_unix: p_meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64),
        file_size: p_meta.as_ref().map(|m| m.len() as i64),
        volume_id: crate::volume::volume_id_for_path(&path),
        parent_dir: path.parent().and_then(|d| d.to_str()).map(|s| s.to_owned()),
        translated_text: None,
        translated_to_lang: None,
        audio_duration_seconds: None,
        audio_codec: None,
        audio_sample_rate_hz: None,
        audio_channels: None,
        audio_bitrate_kbps: None,
        image_camera_make: None,
        image_camera_model: None,
        image_lens_model: None,
        image_taken_at_unix: None,
        image_iso: None,
        multivec_packed: None,
        multivec_n_tokens: None,
        url: None,
    };
    pipeline
        .reingest_document(raw)
        .await
        .map_err(|e| format!("workbench re-ingest failed: {e:#}"))
}
