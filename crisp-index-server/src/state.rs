/// Shared application state for crisp-index-server.
use std::sync::Arc;

use crate::embedder::ServerEmbedder;
use crate::index::SearchIndex;
use crate::queue::TaskQueue;

pub struct SharedState {
    pub index:  Arc<SearchIndex>,
    pub queue:  TaskQueue,
    pub embedder: Option<ServerEmbedder>,
    pub config: ServerConfig,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub api_key: String,
    pub port: u16,
    pub data_dir: std::path::PathBuf,
    pub embed_dims: usize,
    pub server_embed_enabled: bool,
    pub queue_lease_secs: u64,
    pub queue_retry_base_ms: u64,
    pub queue_max_attempts: u32,
}
