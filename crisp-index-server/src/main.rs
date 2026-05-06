/// crisp-index-server — Axum VPS backend for CrispSorter RAG/search.
///
/// ## Environment variables (all optional, defaults shown)
///
///   PORT            = 8473
///   CRISP_API_KEY   = ""          (required for authenticated endpoints)
///   LANCE_DATA_DIR  = ./data      (where lance/ and fts/ subdirs are created)
///   EMBED_DIMS      = 1024        (must match the embedder used by the client)
///
/// ## Routes
///
///   GET  /health                  — liveness probe (no auth)
///   GET  /v1/stats                — row/doc count (auth required)
///   POST /v1/ingest               — store a pre-embedded chunk (auth required)
///   POST /v1/search               — text / vector / hybrid search (auth required)
///   DELETE /v1/docs/:id           — delete all chunks for a document (auth required)
///   POST /v1/docs/:id/location    — update stored location URI (auth required)
///   POST /v1/admin/build-ivf-pq   — build IVF-PQ index (auth required)
mod auth;
mod index;
mod routes;
mod state;

use std::sync::Arc;

use anyhow::Result;
use axum::{
    middleware,
    routing::{delete, get, post},
    Router,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use index::{FtsStore, SearchIndex, VectorStore};
use state::{ServerConfig, SharedState};

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env if present (development convenience).
    let _ = dotenvy::dotenv();

    // Tracing — level controlled by RUST_LOG env var (default: info).
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Config from environment.
    let api_key   = std::env::var("CRISP_API_KEY").unwrap_or_default();
    let port: u16 = std::env::var("PORT")
        .ok().and_then(|p| p.parse().ok()).unwrap_or(8473);
    let data_dir  = std::path::PathBuf::from(
        std::env::var("LANCE_DATA_DIR").unwrap_or_else(|_| "./data".to_owned()),
    );
    let dims: usize = std::env::var("EMBED_DIMS")
        .ok().and_then(|d| d.parse().ok()).unwrap_or(1024);

    if api_key.is_empty() {
        tracing::warn!("CRISP_API_KEY is not set — all authenticated endpoints accept any token");
    }

    std::fs::create_dir_all(&data_dir)?;

    // Initialise indexes.
    tracing::info!("Opening VectorStore at {:?} (dims={})", data_dir, dims);
    let vector = Arc::new(VectorStore::open_or_create(&data_dir, dims).await?);

    tracing::info!("Opening FtsStore");
    let fts = Arc::new(FtsStore::open_or_create(&data_dir.join("fts"))?);

    let search_index = Arc::new(SearchIndex::new(vector, fts));

    let shared = Arc::new(SharedState {
        index: search_index,
        config: ServerConfig { api_key, port, data_dir, embed_dims: dims },
    });

    // Authenticated /v1/* router.
    let v1 = Router::new()
        .route("/stats",                    get(routes::stats::stats))
        .route("/ingest",                   post(routes::ingest::ingest))
        .route("/search",                   post(routes::search::search))
        .route("/docs/:doc_id",             delete(routes::docs::delete_doc))
        .route("/docs/:doc_id/location",    post(routes::docs::update_location))
        .route("/admin/build-ivf-pq",       post(routes::admin::build_ivf_pq))
        .layer(middleware::from_fn_with_state(
            shared.clone(),
            auth::require_bearer,
        ));

    let app = Router::new()
        .route("/health", get(routes::health::health))
        .nest("/v1", v1)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(shared);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("crisp-index-server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
