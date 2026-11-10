//! Public, platform-neutral response types for the model storage commands.

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ModelStorageReport {
    pub bytes: u64,
    pub reclaimable_bytes: u64,
    pub models: Vec<ModelStorageEntry>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ModelStorageEntry {
    pub name: String,
    pub version: Option<String>,
    pub bytes: u64,
    pub reclaimable_bytes: u64,
    pub in_use: bool,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ModelStorageCleanup {
    pub removed_bytes: u64,
    pub removed_versions: usize,
    pub report: ModelStorageReport,
}
