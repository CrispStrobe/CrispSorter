/// Tauri commands for the CrispSorter frontend → index integration.
///
/// ## Local backend path
///
///   `index_init`             → builds LocalIndex + FtsIndex + Embedder + SearchEngine + Pipeline
///   `index_ingest_document`  → `IngestPipeline::ingest_document` (chunk → embed → write both)
///   `index_search`           → `SearchEngine` (FTS + ANN + RRF)
///   `index_build_ivf_pq`     → `LocalIndex::build_vector_index`
///
/// ## Remote backend path
///
///   `index_init`             → builds Embedder (locally) + RemoteClient
///   `index_ingest_document`  → chunk + embed locally → push per-chunk via HTTP to server
///   `index_search`           → embed query locally → send text + embedding to server → server does FTS+ANN+RRF
///   `index_build_ivf_pq`     → error (not supported for remote backend)
use tauri::{Manager, State};

use super::ingest::{IngestStats, RawDocument};
use super::{IndexConfig, IndexState, SearchResult};
use crate::AppState;

// ── Search ────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn index_search(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    query: String,
    mode: String, // "text" | "vector" | "hybrid"
    limit: usize,
    owner_id: Option<String>,
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
        drop(lock);
        match mode.as_str() {
            "text" => backend.search_text(&query, &filters, limit).await,
            "vector" => {
                let embedding = embed_query(embedder, &query).await?;
                backend.search_vector(&embedding, &filters, limit).await
            }
            _ => {
                let embedding = embed_query(embedder, &query).await?;
                backend
                    .search_hybrid(&query, &embedding, &filters, limit)
                    .await
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
        catalog_source: Some(hit.catalog_path),
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

#[derive(Debug, Clone, serde::Deserialize)]
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

    let raw = RawDocument {
        full_text: extracted.full_text,
        full_text_md: String::new(), // path-based ingest has no Markdown view
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
        // Cheap path-based ingest stat()s the file for mtime so the
        // P7.4.3 mtime-skip on re-ingest can short-circuit.
        mtime_unix: std::fs::metadata(&p)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32),
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

    // ── Remote path: chunk + embed locally, push each chunk to server ─────
    let backend = lock
        .backend
        .as_ref()
        .ok_or("Index not initialised — call index_init first")?
        .clone();
    let embedder = lock
        .embedder
        .clone()
        .ok_or("Embedder not available — check backend configuration")?;
    let config = lock.config.clone();

    drop(lock);

    use super::embedder::chunk_text;
    use super::ingest::{chunk_row_id, doc_id_for};

    let cfg = super::ingest::IngestConfig::default();
    let doc_id = doc_id_for(&raw);
    let max_tokens = cfg.chunk_max_words;
    let stride = cfg.chunk_stride;
    let text_chunks = chunk_text(&raw.full_text, max_tokens, stride, &[]);
    let chunk_count_total = text_chunks.len();

    emit_ingest!("embedding", 0, chunk_count_total, "Embedding …");

    let start_embed = std::time::Instant::now();
    let texts: Vec<String> = text_chunks.iter().map(|c| c.text.clone()).collect();

    let dense = {
        use super::embedder::EmbedRole;
        let mut emb = embedder.lock().await;
        emb.embed_dense(texts, EmbedRole::Passage)
            .map_err(|e| e.to_string())?
    };
    let embed_ms = start_embed.elapsed().as_millis() as u64;

    emit_ingest!("writing", 0, chunk_count_total, "Writing to index …");

    let start_write = std::time::Instant::now();
    let chunk_count = text_chunks.len();
    let chunk_total = chunk_count as i32;
    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    for (i, (tc, vec)) in text_chunks.iter().zip(dense.vectors.iter()).enumerate() {
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
            embedding: Some(vec.clone()),
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
        };
        backend.ingest(chunk).await.map_err(|e| e.to_string())?;
    }
    emit_ingest!("done", chunk_count_total, chunk_count_total, "Done");

    let write_ms = start_write.elapsed().as_millis() as u64;

    Ok(IngestStats {
        chunk_count,
        embed_time_ms: embed_ms,
        write_time_ms: write_ms,
    })
}

// ── Document delete ───────────────────────────────────────────────────────────

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
pub async fn index_build_ivf_pq(state: State<'_, AppState>) -> Result<(), String> {
    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Err("Index is disabled".to_owned());
    }

    // LocalIndex is stored separately so we can call build_vector_index.
    let local = lock
        .local
        .as_ref()
        .ok_or("IVF-PQ index build is only supported for the local backend")?
        .clone();

    drop(lock);

    local.build_vector_index().await.map_err(|e| e.to_string())
}

/// Initialise (or re-initialise) the index from a data directory path and the
/// current config stored in `AppState`.  Called by the Settings UI.
#[tauri::command]
pub async fn index_init(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    data_dir: String,
) -> Result<(), String> {
    let config = {
        let lock = state.index.lock().await;
        lock.config.clone()
    };

    let path = std::path::PathBuf::from(&data_dir);
    let new_state = init_index(&path, config, Some(app))
        .await
        .map_err(|e| e.to_string())?;

    let mut lock = state.index.lock().await;
    *lock = new_state;
    Ok(())
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
    let mut lock = state.index.lock().await;
    lock.config = config;
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
            eprintln!("[index-init] {} ({}%)", $label, $pct);
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

    emit!(
        "embedder_start",
        format!("Lade Embedder-Modell ({model_name}) … erster Start lädt ~500 MB herunter"),
        5
    );

    // Resolve model cache: env override > UI setting > {data_dir}/models.
    // Same dir is used by fastembed (ONNX), hf-hub (external-data ONNX +
    // GGUF embedder + GGUF reranker) — so a single configurable path
    // controls every downloaded weight.
    let models_dir = super::resolve_model_cache_dir(&config, data_dir);
    println!("[index] Model cache: {}", models_dir.display());

    let embedder_cfg = EC::new(model, device, models_dir.clone())
        .with_backend(config.embedder_backend)
        .with_matryoshka_dim(config.matryoshka_dim);
    let effective_dim = embedder_cfg.effective_dim();

    let embedder = Embedder::new(embedder_cfg).await?;

    emit!("embedder_done", "Embedder geladen", 40);

    let embedder_arc = Arc::new(Mutex::new(embedder));

    // Reranker handle: cheap to construct (no I/O until first scoring call).
    // Shared between IndexState (kept alive across queries) and SearchEngine
    // (calls score_batch on the post-RRF candidate set).
    let reranker_handle: Option<super::RerankerHandle> = config
        .reranker_model
        .map(|m| super::RerankerHandle::new(m, models_dir.clone()));

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
                embedder: Some(embedder_arc),
                engine: None,
                pipeline: None,
                reranker: reranker_handle,
                config,
            })
        }

        BackendType::Local => {
            // Use the matryoshka-aware effective dim so the LanceDB column
            // width matches what the embedder will actually emit.
            let dims = effective_dim;
            let fts_dir = data_dir.join("fts");

            emit!("fts_start", "Öffne Volltext-Index (Tantivy) …", 55);
            let fts = Arc::new(FtsIndex::open_or_create(&fts_dir)?);
            emit!("fts_done", "Volltext-Index bereit", 70);

            emit!("lance_start", "Öffne Vektor-Datenbank (LanceDB) …", 75);
            let local = Arc::new(LocalIndex::open_or_create(data_dir, dims).await?);
            emit!("lance_done", "Vektor-Datenbank bereit", 90);

            let mut engine_inner = SearchEngine::new(
                fts.clone(),
                local.clone(),
                embedder_arc.clone(),
            );
            if let Some(ref h) = reranker_handle {
                engine_inner = engine_inner.with_reranker(h.clone(), config.rerank_top_n);
            }
            let engine = Arc::new(engine_inner);
            let pipeline = Arc::new(IngestPipeline::new(
                fts.clone(),
                local.clone(),
                embedder_arc.clone(),
                IngestConfig::default(),
            ));
            let backend: Arc<dyn IndexBackend> = local.clone();

            emit!("done", "Index bereit", 100);

            Ok(IndexState {
                backend: Some(backend),
                local: Some(local),
                fts: Some(fts),
                embedder: Some(embedder_arc),
                engine: Some(engine),
                pipeline: Some(pipeline),
                reranker: reranker_handle,
                config,
            })
        }
    }
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
