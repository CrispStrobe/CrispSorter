//! P29.3 — Connectivity gate for durable offline-operation replay.
//!
//! Replay is deliberately gated by the configured cloud-backup health
//! endpoint.  This keeps a disconnected app from repeatedly waking every
//! provider client and lets the maintenance loop apply one shared backoff.
//! Installations without cloud-backup configured still replay provider
//! operations directly; cloud-backup is optional in CrispSorter.

use anyhow::Result;
use std::time::Duration;

use super::cloud_backup::CloudBackupClient;
use super::proxy::ProxyConfig;

/// Probe the configured cloud-backup API before draining the offline queue.
///
/// `Ok(true)` means replay may proceed.  An empty URL is treated as
/// unconfigured and therefore does not gate provider replay.  Health errors
/// are returned to the caller so the caller can apply the normal exponential
/// retry delay without marking queued operations as failed.
pub async fn probe_reconnect(base_url: &str, proxy: &ProxyConfig) -> Result<bool> {
    if base_url.trim().is_empty() {
        return Ok(true);
    }

    let client = CloudBackupClient::new_with_proxy(base_url, "", proxy)?;
    client.health().await.map(|health| health.ok)
}

/// The maintenance loop should not probe more frequently than this even if
/// its outer ticker changes in the future.
pub const MIN_PROBE_INTERVAL: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unconfigured_endpoint_allows_replay() {
        assert!(probe_reconnect("", &ProxyConfig::default()).await.unwrap());
    }
}
