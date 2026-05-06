/// Shared application state for crisp-index-server.
use std::sync::Arc;

use crate::index::SearchIndex;

pub struct SharedState {
    pub index:  Arc<SearchIndex>,
    pub config: ServerConfig,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub api_key:   String,
    pub port:      u16,
    pub data_dir:  std::path::PathBuf,
    pub embed_dims: usize,
}
