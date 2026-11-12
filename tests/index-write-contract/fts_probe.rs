//! Test-only tracing adapter around Tantivy's real filesystem directory.
use std::{fmt, io::{self, Write}, path::{Path, PathBuf}, sync::Arc};
use tantivy::directory::{AntiCallToken, Directory, DirectoryLock, FileHandle, Lock, MmapDirectory, TerminatingWrite, WatchCallback, WatchHandle, WritePtr};
use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};

#[derive(Clone)]
pub struct TraceDir(MmapDirectory, PathBuf);
impl fmt::Debug for TraceDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("TraceDir") }
}
impl TraceDir {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        // Only the generated fixture directory and index-relative path appear
        // in diagnostics; never print host roots or account directory names.
        let fixture = path.parent().and_then(Path::file_name).unwrap_or_default();
        let label = PathBuf::from(fixture).join(path.file_name().unwrap_or_default());
        Ok(Self(MmapDirectory::open(path)?, label))
    }
}
/// Process-local mode: control and trace run in separate test processes.
pub fn enabled() -> bool {
    std::env::var_os("CRISP_FTS_TRACE").is_some_and(|value| value == "1")
}
struct TraceWriter { inner: Box<dyn TerminatingWrite>, path: PathBuf }
impl Write for TraceWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.inner.write(data).inspect_err(|e| eprintln!("fts write {:?}: {e:?}", self.path))
    }
    fn write_vectored(&mut self, data: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.inner.write_vectored(data).inspect_err(|e| eprintln!("fts write_vectored {:?}: {e:?}", self.path))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().inspect_err(|e| eprintln!("fts flush {:?}: {e:?}", self.path))
    }
}
impl TerminatingWrite for TraceWriter {
    fn terminate_ref(&mut self, token: AntiCallToken) -> io::Result<()> {
        self.inner.terminate_ref(token).inspect_err(|e| eprintln!("fts terminate {:?}: {e:?}", self.path))
    }
}
fn trace_writer(writer: WritePtr, path: &Path) -> WritePtr {
    // MmapDirectory returns a newly empty buffer. Extract it without flushing
    // or closing the raw writer, then restore exactly one buffer of the same
    // capacity. Never wrap the original BufWriter in another BufWriter.
    let capacity = writer.capacity();
    let (inner, buffer) = writer.into_parts();
    assert!(matches!(buffer, Ok(buffer) if buffer.is_empty()), "new Tantivy writer must have an empty buffer");
    let traced: Box<dyn TerminatingWrite> = Box::new(TraceWriter { inner, path: path.to_owned() });
    io::BufWriter::with_capacity(capacity, traced)
}
impl Directory for TraceDir {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.0.get_file_handle(path).inspect_err(|e| eprintln!("fts get_file_handle {:?}: {e:?}", self.1.join(path)))
    }
    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.0.delete(path).inspect_err(|e| eprintln!("fts delete {:?}: {e:?}", self.1.join(path)))
    }
    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.0.exists(path).inspect_err(|e| eprintln!("fts exists {:?}: {e:?}", self.1.join(path)))
    }
    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        let writer = self.0.open_write(path).inspect_err(|e| eprintln!("fts open_write {:?}: {e:?}", self.1.join(path)))?;
        Ok(trace_writer(writer, &self.1.join(path)))
    }
    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.0.atomic_read(path).inspect_err(|e| eprintln!("fts atomic_read {:?}: {e:?}", self.1.join(path)))
    }
    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.0.atomic_write(path, data).inspect_err(|e| eprintln!("fts atomic_write {:?}: {e:?}", self.1.join(path)))
    }
    fn sync_directory(&self) -> io::Result<()> {
        self.0.sync_directory().inspect_err(|e| eprintln!("fts sync_directory {:?}: {e:?}", self.1))
    }
    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.0.acquire_lock(lock).inspect_err(|e| eprintln!("fts acquire_lock {:?}: {e:?}", self.1.join(&lock.filepath)))
    }
    fn watch(&self, callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.0.watch(callback).inspect_err(|e| eprintln!("fts watch {:?}: {e:?}", self.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Clone, Copy, PartialEq)]
    enum Fault { None, Write, Flush, Terminate }
    #[derive(Debug, PartialEq)]
    enum Event { Write(Vec<u8>), Flush, Terminate }
    struct RecordingWriter { events: Arc<Mutex<Vec<Event>>>, fault: Fault }
    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.events.lock().unwrap().push(Event::Write(bytes.to_vec()));
            if self.fault == Fault::Write { return Err(io::Error::from_raw_os_error(5)); }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.events.lock().unwrap().push(Event::Flush);
            if self.fault == Fault::Flush { return Err(io::Error::from_raw_os_error(5)); }
            Ok(())
        }
    }
    impl TerminatingWrite for RecordingWriter {
        fn terminate_ref(&mut self, _: AntiCallToken) -> io::Result<()> {
            self.events.lock().unwrap().push(Event::Terminate);
            if self.fault == Fault::Terminate { return Err(io::Error::from_raw_os_error(5)); }
            Ok(())
        }
    }
    fn exercise(traced: bool, fault: Fault) -> (Vec<Event>, Option<i32>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let raw: Box<dyn TerminatingWrite> = Box::new(RecordingWriter { events: events.clone(), fault });
        let writer = io::BufWriter::with_capacity(4, raw);
        let mut writer = if traced { trace_writer(writer, Path::new("fixture.segment")) } else { writer };
        assert_eq!(writer.capacity(), 4);
        assert!(writer.buffer().is_empty());
        assert!(events.lock().unwrap().is_empty(), "wrapping must perform no I/O");
        let result = (|| {
            writer.write_all(b"abc")?;
            writer.write_all(b"defgh")?;
            writer.flush()?;
            writer.terminate()
        })();
        let recorded = std::mem::take(&mut *events.lock().unwrap());
        (recorded, result.err().and_then(|error| error.raw_os_error()))
    }
    #[test]
    fn windows_fts_trace_preserves_single_buffer_and_errors() {
        eprintln!("Trace-equivalence check: expected injected errors for fixture.segment below are not index failures");
        for fault in [Fault::None, Fault::Write, Fault::Flush, Fault::Terminate] {
            let control = exercise(false, fault);
            let traced = exercise(true, fault);
            assert_eq!(traced, control, "tracing changed raw operations or error propagation");
            assert_eq!(traced.1, if fault == Fault::None { None } else { Some(5) });
            if fault == Fault::None {
                assert_eq!(traced.0, vec![Event::Write(b"abc".to_vec()), Event::Write(b"defgh".to_vec()), Event::Flush, Event::Flush, Event::Terminate]);
            }
        }
    }
}
