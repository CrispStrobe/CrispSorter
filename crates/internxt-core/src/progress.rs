//! Byte-progress reporting seam. Transfer primitives report progress through a
//! [`ProgressSink`] rather than depending on any particular UI (indicatif, a GUI
//! progress widget, logging, …). The front-end supplies the implementation.

use std::sync::Arc;

/// Sink for transfer byte progress. `inc` is called with the number of bytes
/// that just completed (encrypted-and-uploaded, or downloaded-and-written).
/// Implementations must be cheap and thread-safe: `inc` is called from the
/// streaming body producer and from concurrent multipart PUT tasks.
pub trait ProgressSink: Send + Sync {
    fn inc(&self, bytes: u64);
}

/// A sink that discards all updates. Used internally when a transfer is called
/// without a progress sink.
struct NoopSink;

impl ProgressSink for NoopSink {
    fn inc(&self, _bytes: u64) {}
}

/// Returns a shared no-op sink, so transfer code can normalise an
/// `Option<Arc<dyn ProgressSink>>` to a plain `Arc<dyn ProgressSink>`.
pub fn noop_sink() -> Arc<dyn ProgressSink> {
    Arc::new(NoopSink)
}

