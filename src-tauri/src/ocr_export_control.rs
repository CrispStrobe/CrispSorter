//! Cooperative export cancellation with an explicit publication boundary.
//!
//! The native writer is awaited after a cancellation request. A cancellation
//! cannot turn an output that already entered publication into a reported stop.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, OnceLock, atomic::{AtomicU8, Ordering}};
use std::time::{Duration, Instant};

pub(super) const CANCELLED: &str = "OCR_EXPORT_CANCELLED";
const RUNNING: u8 = 0;
const STOPPED: u8 = 1;
const COMMITTING: u8 = 2;

fn cancelled() -> io::Error { io::Error::new(io::ErrorKind::Interrupted, CANCELLED) }

#[derive(Clone, Default)]
pub(super) struct ExportControl(Arc<AtomicU8>);

impl ExportControl {
    pub(super) fn check(&self) -> io::Result<()> {
        if self.0.load(Ordering::Acquire) == STOPPED { Err(cancelled()) } else { Ok(()) }
    }

    fn cancel(&self) -> bool {
        match self.0.compare_exchange(RUNNING, STOPPED, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) | Err(STOPPED) => true,
            Err(_) => false,
        }
    }

    /// Linearize cancellation against publication before the staged file is
    /// synchronized and renamed. A later stop cannot revoke that completed work.
    pub(super) fn begin_commit(&self) -> io::Result<()> {
        match self.0.compare_exchange(RUNNING, COMMITTING, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(()),
            Err(STOPPED) => Err(cancelled()),
            Err(_) => Err(io::Error::other("This export has already entered publication.")),
        }
    }
}

#[derive(Default)]
struct Registry {
    active: Option<(String, ExportControl)>,
    pending: VecDeque<(String, Instant)>,
}

impl Registry {
    fn begin(&mut self, id: &str) -> Result<ExportControl, String> {
        if self.active.is_some() {
            return Err("Another document is exporting. Wait for it to finish or cancel it first.".into());
        }
        self.pending.retain(|(_, started)| started.elapsed() < Duration::from_secs(300));
        let control = ExportControl::default();
        if let Some(index) = self.pending.iter().position(|(pending, _)| pending == id) {
            self.pending.remove(index);
            control.cancel();
        }
        self.active = Some((id.to_owned(), control.clone()));
        Ok(control)
    }

    fn cancel(&mut self, id: &str) -> bool {
        if let Some((_, control)) = self.active.as_ref().filter(|(active, _)| active == id) {
            return control.cancel();
        }
        // A stop can reach IPC before its export. Keep a bounded, expiring
        // tombstone so that ordering does not lose the user's cancellation.
        self.pending.retain(|(pending, started)| pending != id && started.elapsed() < Duration::from_secs(300));
        while self.pending.len() >= 64 { self.pending.pop_front(); }
        self.pending.push_back((id.to_owned(), Instant::now()));
        true
    }

    fn finish(&mut self, id: &str) {
        if self.active.as_ref().is_some_and(|(active, _)| active == id) { self.active = None; }
    }
}

static EXPORTS: OnceLock<Mutex<Registry>> = OnceLock::new();

pub(super) struct ExportJob { id: String, control: ExportControl }

impl ExportJob {
    pub(super) fn begin(request_id: Option<String>) -> Result<Self, String> {
        let id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        uuid::Uuid::parse_str(&id).map_err(|_| "The export request ID is invalid.")?;
        let control = EXPORTS.get_or_init(Default::default).lock()
            .map_err(|_| "Export state is unavailable.")?.begin(&id)?;
        let job = Self { id, control };
        job.control.check().map_err(|error| error.to_string())?;
        Ok(job)
    }
    pub(super) fn id(&self) -> &str { &self.id }
    pub(super) fn control(&self) -> &ExportControl { &self.control }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        if let Ok(mut registry) = EXPORTS.get_or_init(Default::default).lock() {
            registry.finish(&self.id);
        }
    }
}

pub(super) fn cancel(request_id: &str) -> Result<bool, String> {
    uuid::Uuid::parse_str(request_id).map_err(|_| "The export request ID is invalid.")?;
    Ok(EXPORTS.get_or_init(Default::default).lock()
        .map_err(|_| "Export state is unavailable.")?.cancel(request_id))
}

/// Place this OUTSIDE a buffered writer: its Interrupted result must reach the
/// native callback directly, without a buffering layer retrying it forever.
pub(super) struct ControlledWriter<'a, W, F> {
    inner: &'a mut W,
    control: &'a ExportControl,
    progress: F,
    bytes: u64,
}

impl<'a, W: Write, F: FnMut(u64)> ControlledWriter<'a, W, F> {
    pub(super) fn new(inner: &'a mut W, control: &'a ExportControl, progress: F) -> Self {
        Self { inner, control, progress, bytes: 0 }
    }

    /// std::io::Write::write_all retries Interrupted. Cancellation must instead
    /// stop on the first returned error, including between partial writes.
    pub(super) fn write_all_checked(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.write(bytes)?;
            if count == 0 { return Err(io::Error::new(io::ErrorKind::WriteZero, "Export writer made no progress.")); }
            bytes = &bytes[count..];
        }
        Ok(())
    }
}

impl<W: Write, F: FnMut(u64)> Write for ControlledWriter<'_, W, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.control.check()?;
        let count = self.inner.write(&bytes[..bytes.len().min(64 * 1024)])?;
        if count > bytes.len().min(64 * 1024) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Export writer returned an invalid byte count."));
        }
        self.bytes = self.bytes.checked_add(count as u64)
            .ok_or_else(|| io::Error::other("Export byte counter overflow."))?;
        (self.progress)(self.bytes);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.control.check()?;
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_before_start_is_retained_without_stopping_another_job() {
        let mut registry = Registry::default();
        assert!(registry.cancel("second"));
        let first = registry.begin("first").unwrap();
        assert!(first.check().is_ok());
        assert!(registry.begin("second").is_err());
        registry.finish("wrong-id");
        assert!(registry.active.is_some());
        registry.finish("first");
        let second = registry.begin("second").unwrap();
        assert_eq!(second.check().unwrap_err().to_string(), CANCELLED);
    }

    #[test]
    fn pending_stops_are_bounded_expiring_and_deduplicated() {
        let mut registry = Registry::default();
        for index in 0..100 { registry.cancel(&index.to_string()); }
        assert_eq!(registry.pending.len(), 64);
        registry.cancel("99");
        assert_eq!(registry.pending.len(), 64);
        assert_eq!(registry.pending.iter().filter(|(id, _)| id == "99").count(), 1);
        registry.pending.push_back(("expired".into(), Instant::now() - Duration::from_secs(301)));
        assert!(registry.begin("expired").unwrap().check().is_ok());
    }

    #[test]
    fn stop_and_publication_have_one_winner() {
        let stopped = ExportControl::default();
        assert!(stopped.cancel());
        assert!(stopped.cancel());
        assert_eq!(stopped.begin_commit().unwrap_err().kind(), io::ErrorKind::Interrupted);
        let saving = ExportControl::default();
        saving.begin_commit().unwrap();
        assert!(!saving.cancel());
        assert!(saving.check().is_ok());
        assert!(saving.begin_commit().is_err());
    }

    #[test]
    fn cancellation_between_bounded_writes_stops_without_retrying_interrupted() {
        let control = ExportControl::default();
        let mut output = Vec::new();
        let mut updates = Vec::new();
        let bytes = vec![7; 2 * 64 * 1024];
        let mut writer = ControlledWriter::new(&mut output, &control, |count| {
            updates.push(count);
            control.cancel();
        });
        let error = writer.write_all_checked(&bytes).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(writer.flush().unwrap_err().kind(), io::ErrorKind::Interrupted);
        drop(writer);
        assert_eq!(output, bytes[..64 * 1024]);
        assert_eq!(updates, [64 * 1024]);
    }

    #[test]
    fn writer_preserves_original_io_errors_and_rejects_zero_progress() {
        struct Broken(bool);
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                if self.0 { Err(io::Error::from_raw_os_error(28)) } else { Ok(0) }
            }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        let control = ExportControl::default();
        let mut full = Broken(true);
        let error = ControlledWriter::new(&mut full, &control, |_| {}).write_all_checked(b"data").unwrap_err();
        assert_eq!(error.raw_os_error(), Some(28));
        let mut stalled = Broken(false);
        let error = ControlledWriter::new(&mut stalled, &control, |_| {}).write_all_checked(b"data").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn accepted_stop_prevents_atomic_publication_and_cleans_staging() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reviewed.pdf");
        std::fs::write(&path, b"previous complete document").unwrap();
        let control = ExportControl::default();
        let result = super::super::atomic_output::atomic_write_with(&path, true, |file| {
            file.write_all(b"complete but not published").unwrap();
            control.cancel();
            control.begin_commit().map_err(|error| error.to_string())
        });
        assert_eq!(result.unwrap_err(), CANCELLED);
        assert_eq!(std::fs::read(&path).unwrap(), b"previous complete document");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
