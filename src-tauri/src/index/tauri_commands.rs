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
use tauri::State;

use super::ingest::{IngestStats, RawDocument};
use super::{IndexConfig, IndexState, SearchResult};
use crate::AppState;

// ── Search ────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn index_search(
    state: State<'_, AppState>,
    query: String,
    mode: String, // "text" | "vector" | "hybrid"
    limit: usize,
    owner_id: Option<String>,
) -> Result<Vec<SearchResult>, String> {
    let lock = state.index.lock().await;

    if !lock.config.enabled {
        return Ok(vec![]);
    }

    let filters = super::SearchFilters {
        owner_id,
        ..Default::default()
    };

    if let Some(engine) = lock.engine.clone() {
        // ── Local path: SearchEngine wraps FTS + ANN + RRF ────────────────
        drop(lock);
        let results = match mode.as_str() {
            "text" => engine.search_text(&query, &filters, limit).await,
            "vector" => engine.search_vector(&query, &filters, limit).await,
            _ => engine.search_hybrid(&query, &filters, limit).await,
        }
        .map_err(|e| e.to_string())?;
        return Ok(results);
    }

    // ── Remote path: embed query locally, then call backend ───────────────
    let backend = lock
        .backend
        .as_ref()
        .ok_or("Index not initialised — call index_init first")?
        .clone();
    let embedder = lock.embedder.clone();

    drop(lock);

    let results = match mode.as_str() {
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
    .map_err(|e| e.to_string())?;

    Ok(results)
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
        let mut emb = embedder.lock().await;
        emb.embed_dense(texts).map_err(|e| e.to_string())?
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

    let models_dir = data_dir.join("models");
    let embedder_cfg = EC::new(model, device, models_dir).with_backend(config.embedder_backend);

    let embedder = Embedder::new(embedder_cfg).await?;

    emit!("embedder_done", "Embedder geladen", 40);

    let embedder_arc = Arc::new(Mutex::new(embedder));

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
                config,
            })
        }

        BackendType::Local => {
            let dims = model.dims();
            let fts_dir = data_dir.join("fts");

            emit!("fts_start", "Öffne Volltext-Index (Tantivy) …", 55);
            let fts = Arc::new(FtsIndex::open_or_create(&fts_dir)?);
            emit!("fts_done", "Volltext-Index bereit", 70);

            emit!("lance_start", "Öffne Vektor-Datenbank (LanceDB) …", 75);
            let local = Arc::new(LocalIndex::open_or_create(data_dir, dims).await?);
            emit!("lance_done", "Vektor-Datenbank bereit", 90);

            let engine = Arc::new(SearchEngine::new(
                fts.clone(),
                local.clone(),
                embedder_arc.clone(),
            ));
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
    let embedder = embedder.ok_or("Embedder not available for remote query embedding")?;
    let mut emb = embedder.lock().await;
    let dense = emb
        .embed_dense(vec![text.to_owned()])
        .map_err(|e| e.to_string())?;
    dense
        .vectors
        .into_iter()
        .next()
        .ok_or_else(|| "Embedder returned no vectors".to_owned())
}
