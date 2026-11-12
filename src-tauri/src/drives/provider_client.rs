//! Per-drive reuse of the native provider clients (Filen, Internxt) from crisp-cloud-rs.
//!
//! Both clients own a pooled HTTP client and an in-memory folder-listing
//! cache. Building a fresh client for every `CloudDrive` call discarded both
//! and re-walked every path from the root. A drive instance now keeps one
//! client per session for its lifetime and drops it when the provider rejects
//! the credentials, so the next call re-reads the keychain (picking up a
//! refreshed session, or reporting a logout) instead of reusing a dead token.
//!
//! The listing caches trade freshness for round trips. Callers therefore
//! resolve *ancestors* through the cache but read the *leaf* entry from a
//! fresh listing of its parent, and retry once with an emptied cache when a
//! cached ancestor no longer leads anywhere. See [`retry_with_fresh_listings`].
#![cfg_attr(
    not(any(feature = "drive-filen-native", feature = "drive-internxt-native")),
    allow(dead_code)
)]

use crate::operation_log::OperationLog;
use anyhow::{anyhow, Result};
use std::sync::{Arc, Mutex, MutexGuard};

/// One session plus the client built from it.
struct Cached<S, C> {
    session: Arc<S>,
    client: Arc<C>,
}

/// A borrowed view of the cached session/client pair.
pub(crate) struct Lease<S, C> {
    pub session: Arc<S>,
    pub client: Arc<C>,
    /// `true` when the client already existed (pool + listing cache warm).
    pub reused: bool,
}

/// Lazily built, invalidatable session/client pair for one drive instance.
///
/// A failed session load or client build is *not* cached: the next call tries
/// again, so a login completed after a failure is picked up without
/// re-instantiating the drive.
pub(crate) struct ProviderClientCache<S, C> {
    state: Mutex<Option<Cached<S, C>>>,
}

impl<S, C> ProviderClientCache<S, C> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(None),
        }
    }

    fn state(&self) -> MutexGuard<'_, Option<Cached<S, C>>> {
        // The guarded value is replaced whole, so a panic elsewhere cannot
        // leave it half-written; recover instead of propagating the poison.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Return the cached pair, building it on first use (or after
    /// [`Self::invalidate`]). Construction is local (keychain read + HTTP
    /// client builder), so holding the lock across it only serializes the
    /// first concurrent callers instead of building duplicate clients.
    pub(crate) fn lease(
        &self,
        load_session: impl FnOnce() -> Result<S>,
        build_client: impl FnOnce(&S) -> Result<C>,
    ) -> Result<Lease<S, C>> {
        let mut state = self.state();
        if let Some(cached) = state.as_ref() {
            return Ok(Lease {
                session: Arc::clone(&cached.session),
                client: Arc::clone(&cached.client),
                reused: true,
            });
        }
        let session = Arc::new(load_session()?);
        let client = Arc::new(build_client(&session)?);
        *state = Some(Cached {
            session: Arc::clone(&session),
            client: Arc::clone(&client),
        });
        Ok(Lease {
            session,
            client,
            reused: false,
        })
    }

    /// Drop the cached session and client. In-flight operations keep their
    /// own `Arc`s; only the next lease rebuilds.
    pub(crate) fn invalidate(&self) {
        *self.state() = None;
    }

    #[cfg(test)]
    pub(crate) fn is_cached(&self) -> bool {
        self.state().is_some()
    }
}

/// Whether an error means the stored credentials are no longer accepted, so
/// the cached session must be re-read from the keychain before the next call.
/// Matches the wording of both crisp-cloud-rs clients (`HTTP 401 Unauthorized`,
/// expired JWTs, Filen's "login again" API-key guard).
pub(crate) fn is_auth_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    [
        "unauthorized",
        "token expired",
        "jwt expired",
        "invalid api key",
        "login again",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// Run a lookup against cached listings first; if it fails, empty the cache
/// and run it once more against fresh listings. A genuinely missing path costs
/// one extra listing compared to an always-fresh walk; a stale cached ancestor
/// can no longer produce a wrong "not found" or a wrong folder id.
pub(crate) fn retry_with_fresh_listings<T>(
    mut lookup: impl FnMut() -> Result<T>,
    invalidate_listings: impl FnOnce(),
) -> Result<T> {
    match lookup() {
        Ok(value) => Ok(value),
        Err(_) => {
            invalidate_listings();
            lookup()
        }
    }
}

/// Length of an inclusive `start..=end` byte range, rejecting inverted or
/// overflowing ranges before any request is made.
pub(crate) fn inclusive_range_len(start: u64, end: u64) -> Result<u64> {
    end.checked_sub(start)
        .and_then(|span| span.checked_add(1))
        .ok_or_else(|| anyhow!("invalid byte range {start}..={end}"))
}

/// Clamp an inclusive range request to a file of `size` bytes. Returns the
/// `(offset, length)` to fetch, or `None` when the range starts at or beyond
/// EOF (an empty read, matching what FUSE expects for a final page).
pub(crate) fn clamp_range_to_size(start: u64, end: u64, size: u64) -> Result<Option<(u64, u64)>> {
    let length = inclusive_range_len(start, end)?;
    if start >= size {
        return Ok(None);
    }
    Ok(Some((start, length.min(size - start))))
}

/// Content-free receipt for a provider operation, mirroring the SMB drive's
/// `operation` helper: fixed codes and counts only, never paths or names.
pub(crate) fn logged<T>(
    operation: &'static str,
    backend: &'static str,
    mode: &'static str,
    work: impl FnOnce(&OperationLog) -> Result<T>,
) -> Result<T> {
    let log = OperationLog::start(
        operation,
        serde_json::json!({ "backend": backend, "mode": mode }),
    );
    log.step("execute", serde_json::Value::Null);
    let result = work(&log);
    match &result {
        Ok(_) => log.succeed(serde_json::Value::Null),
        Err(error) if is_auth_error(error) => log.fail("provider_auth_rejected"),
        Err(_) => log.fail("provider_operation_failed"),
    }
    result
}

/// Record that a cached client was dropped because credentials were rejected.
pub(crate) fn log_session_reset(backend: &'static str) {
    let log = OperationLog::start(
        "storage.provider.session_reset",
        serde_json::json!({ "backend": backend }),
    );
    log.warn("provider_session_reset", serde_json::Value::Null);
    log.succeed(serde_json::Value::Null);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn client_is_built_once_and_reused() {
        let cache: ProviderClientCache<String, usize> = ProviderClientCache::new();
        let loads = Cell::new(0);
        let builds = Cell::new(0);
        for expected_reuse in [false, true, true] {
            let lease = cache
                .lease(
                    || {
                        loads.set(loads.get() + 1);
                        Ok("session".to_owned())
                    },
                    |_| {
                        builds.set(builds.get() + 1);
                        Ok(builds.get())
                    },
                )
                .unwrap();
            assert_eq!(lease.reused, expected_reuse);
            assert_eq!(*lease.client, 1);
            assert_eq!(lease.session.as_str(), "session");
        }
        assert_eq!((loads.get(), builds.get()), (1, 1));
    }

    #[test]
    fn invalidation_reloads_session_and_rebuilds_client() {
        let cache: ProviderClientCache<String, String> = ProviderClientCache::new();
        let token = Cell::new("old");
        let lease = |cache: &ProviderClientCache<String, String>| {
            cache
                .lease(|| Ok(token.get().to_owned()), |s| Ok(format!("client:{s}")))
                .unwrap()
        };
        let first = lease(&cache);
        assert_eq!(first.client.as_str(), "client:old");
        // A refreshed session lands in the keychain; the cache must not see
        // it until invalidated (e.g. by a rejected request).
        token.set("new");
        assert_eq!(lease(&cache).client.as_str(), "client:old");
        cache.invalidate();
        assert!(!cache.is_cached());
        let rebuilt = lease(&cache);
        assert!(!rebuilt.reused);
        assert_eq!(rebuilt.client.as_str(), "client:new");
        // An in-flight holder keeps its own client alive across invalidation.
        assert_eq!(first.client.as_str(), "client:old");
    }

    #[test]
    fn failed_session_load_or_build_is_not_cached() {
        let cache: ProviderClientCache<String, String> = ProviderClientCache::new();
        let missing = cache.lease(|| Err(anyhow!("no native session")), |s| Ok(s.clone()));
        assert!(missing.is_err());
        assert!(!cache.is_cached());
        let bad_proxy = cache.lease(|| Ok("s".to_owned()), |_| Err(anyhow!("invalid proxy URL")));
        assert!(bad_proxy.is_err());
        assert!(!cache.is_cached());
        // Logging in afterwards works without re-instantiating the drive.
        assert!(cache
            .lease(|| Ok("s".to_owned()), |s| Ok(s.clone()))
            .is_ok());
        assert!(cache.is_cached());
    }

    #[test]
    fn auth_errors_are_recognised_without_matching_ordinary_failures() {
        for message in [
            "Filen HTTP 401 Unauthorized: {}",
            "Internxt list returned 401 Unauthorized: expired",
            "Filen API error: Invalid API key.",
            "Filen session has no API key; login again",
            "jwt expired",
        ] {
            assert!(is_auth_error(&anyhow!("{message}")), "{message}");
        }
        assert!(is_auth_error(
            &anyhow!("HTTP 401 Unauthorized").context("listing folder")
        ));
        for message in [
            "Filen path not found: \"/a\"",
            "Filen HTTP 500 Internal Server Error: busy",
            "Internxt folder not found: 401",
            "connection reset",
        ] {
            assert!(!is_auth_error(&anyhow!("{message}")), "{message}");
        }
    }

    #[test]
    fn stale_cache_lookup_retries_once_after_invalidation() {
        let attempts = Cell::new(0);
        let invalidated = Cell::new(false);
        let value = retry_with_fresh_listings(
            || {
                attempts.set(attempts.get() + 1);
                if invalidated.get() {
                    Ok(7)
                } else {
                    Err(anyhow!("not found"))
                }
            },
            || invalidated.set(true),
        )
        .unwrap();
        assert_eq!((value, attempts.get()), (7, 2));

        let attempts = Cell::new(0);
        let hit = retry_with_fresh_listings(
            || {
                attempts.set(attempts.get() + 1);
                Ok(1)
            },
            || panic!("a cache hit must not empty the cache"),
        )
        .unwrap();
        assert_eq!((hit, attempts.get()), (1, 1));

        let missing: Result<()> =
            retry_with_fresh_listings(|| Err(anyhow!("still missing")), || {});
        assert!(missing.is_err());
    }

    #[test]
    fn ranges_are_validated_and_clamped_to_file_size() {
        assert_eq!(inclusive_range_len(3, 5).unwrap(), 3);
        assert_eq!(inclusive_range_len(4, 4).unwrap(), 1);
        assert!(inclusive_range_len(5, 2).is_err());
        assert!(inclusive_range_len(0, u64::MAX).is_err());

        assert_eq!(clamp_range_to_size(2, 4, 10).unwrap(), Some((2, 3)));
        assert_eq!(clamp_range_to_size(8, 11, 10).unwrap(), Some((8, 2)));
        assert_eq!(clamp_range_to_size(10, 11, 10).unwrap(), None);
        assert_eq!(clamp_range_to_size(100, 4195, 10).unwrap(), None);
        assert_eq!(clamp_range_to_size(0, 0, 0).unwrap(), None);
        assert!(clamp_range_to_size(5, 2, 10).is_err());
    }

    #[test]
    fn receipts_carry_fixed_codes_only() {
        let (result, events) = crate::operation_log::capture(|| {
            logged(
                "storage.filen.read_range",
                "filen",
                "drives.filen.native",
                |log| {
                    log.step("transferred", serde_json::json!({ "bytes": 3 }));
                    Err::<(), _>(anyhow!("HTTP 401 Unauthorized for /private/name.txt"))
                },
            )
        });
        assert!(result.is_err());
        let last = events.last().unwrap();
        assert_eq!(last.phase, "failed");
        assert_eq!(last.code.as_deref(), Some("provider_auth_rejected"));
        assert!(!serde_json::to_string(&events).unwrap().contains("private"));
    }
}
