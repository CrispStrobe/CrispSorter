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

use super::ingest::{IngestStats, L1FileEntry, RawDocument};
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
    // Reserve the init slot atomically. If another init is already running,
    // bail out instead of starting a second multi-GB download.
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

    crate::app_log!(
        "info",
        "Index init requested: data_dir={}, model={:?}, backend={:?}",
        data_dir,
        config.embedder_model,
        config.backend_type
    );

    let path = std::path::PathBuf::from(&data_dir);
    let init_result = init_index(&path, config, Some(app)).await;

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
    let dense = match emb.embed_dense(texts.clone()) {
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
            // Mirror progress to the in-app log panel so users without a console
            // can see what step the init is on.
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

    // Use the actual per-model download size (engine-aware). Older code
    // hard-coded "~500 MB" which was wildly wrong for big models like
    // BGE-M3 (~2.3 GB) and tiny ones like all-MiniLM-L6-v2 (~90 MB).
    let mb = match config.embedder_backend {
        super::embedder::EmbedderBackend::Gguf => {
            let g = model.gguf_download_mb();
            if g > 0 { g } else { model.approx_download_mb() }
        }
        super::embedder::EmbedderBackend::Onnx => model.approx_download_mb(),
    };
    let size_hint = if mb > 0 {
        format!(" (~{mb} MB)")
    } else {
        String::new()
    };
    emit!(
        "embedder_start",
        format!("Lade Embedder-Modell ({model_name}){size_hint}, erster Start lädt aus dem Internet …"),
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
                initializing: false,
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
                initializing: false,
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
