//! Shared, content-free operation receipts for GUI, CLI and native workers.
//! Detailed errors remain in the operation's Result; logs contain fixed codes.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, atomic::{AtomicU8, Ordering}};
use std::time::Instant;

static VERBOSITY: AtomicU8 = AtomicU8::new(2);
const MAX_TRANSPORT_BYTES: u64 = 4 * 1024 * 1024;

pub fn set_verbosity(level: &str) -> Result<(), String> {
    let rank = match level { "silent" => 0, "error" => 1, "info" => 2, "debug" => 3,
        _ => return Err("Unknown log verbosity.".into()) };
    VERBOSITY.store(rank, Ordering::Relaxed);
    Ok(())
}

fn enabled_at(rank: u8, level: &str) -> bool {
    match level { "error" => true, "warn" => rank >= 1, "debug" => rank >= 3, _ => rank >= 2 }
}

pub fn enabled(level: &str) -> bool { enabled_at(VERBOSITY.load(Ordering::Relaxed), level) }

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 80
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

fn valid_id(value: &str) -> bool {
    if uuid::Uuid::parse_str(value).is_ok() { return true; }
    let mut parts = value.split('-');
    matches!((parts.next(), parts.next(), parts.next(), parts.next()), (Some("fe"), Some(a), Some(b), None)
        if !a.is_empty() && a.len() <= 20 && !b.is_empty() && b.len() <= 16
            && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
}

fn language(value: &str) -> bool {
    value.len() <= 128 && value.split('+').all(|code| {
        let code = code.strip_prefix("script/").unwrap_or(code);
        let mut parts = code.split(['-', '_']);
        let first = parts.next().unwrap_or_default();
        (first == "auto" || (2..=3).contains(&first.len()) && first.bytes().all(|c| c.is_ascii_lowercase()))
            && parts.all(|part| (2..=4).contains(&part.len()) && part.bytes().all(|c| c.is_ascii_alphabetic()))
    })
}

fn mode(value: &str) -> bool {
    if ["detailed", "review", "document", "read", "write", "download", "upload", "stream", "grid", "extract", "native_text", "ocr", "metadata", "skip"].contains(&value) { return true; }
    identifier(value) && ["version", "doctor", "catalog", "index", "batch", "chat", "images", "sync", "ocr", "kie", "table", "math-ocr", "zone", "pdf", "docx", "convert", "search", "watch", "drives", "secrets", "intended-purpose", "manpage", "completion", "help"]
        .contains(&value.split('.').next().unwrap_or_default())
}

/// An allowlist, rather than redacting arbitrary text after it reaches a log.
pub fn safe_context(value: Value) -> Map<String, Value> {
    let Value::Object(fields) = value else { return Map::new(); };
    fields.into_iter().filter(|(key, value)| match key.as_str() {
        "page" | "pages" | "page_index" | "page_count" | "bytes" | "input_bytes" | "output_bytes"
        | "characters" | "regions" | "warnings" | "results" | "completed" | "failed" | "pending"
        | "count" | "total" | "items" | "files" | "stages" | "models" | "rows" | "cols"
        | "width" | "height" | "revision" | "diagnostic_lines" | "event_count" | "attempt" | "job_id" => value.as_u64().is_some(),
        "cpu_budget" | "cpuBudget" | "forced" | "partial" | "saved" | "reused" | "pipeline"
        | "cache_hit" | "nativeContinues" | "cleanup" | "denoise" | "dewarp" | "restore"
        | "upscale" | "layout" | "overwrite" | "accepted" => value.is_boolean(),
        "engine" => value.as_str().is_some_and(|s| ["auto", "chain", "system_tesseract", "tesseract_js", "ocrs", "paddle",
            "dbnet_trocr", "surya", "got", "glm", "qwen2vl", "internvl2", "tesseract", "parseq", "deepseek_ocr2",
            "pix2struct", "granite_vision", "lightonocr", "qwen3vl", "unlimited_ocr", "unlimited_ocr2", "minicpm_ocr",
            "unified", "tesseract_fraktur", "ppocrv6", "easyocr", "olmocr", "lfm2_vl"].contains(&s)),
        "language" => value.as_str().is_some_and(language),
        "format" => value.as_str().is_some_and(|s| ["text", "txt", "markdown", "md", "html", "json", "csv", "pdf", "pdfa", "hocr", "alto", "docx", "xlsx"].contains(&s)),
        "surface" => value.as_str().is_some_and(|s| ["gui", "cli", "background", "worker", "native", "browser"].contains(&s)),
        "backend" | "protocol" => value.as_str().is_some_and(|s| ["native", "browser", "local", "remote", "smb", "webdav", "s3", "filen", "internxt", "google_drive", "onedrive", "cloud_backup", "memory"].contains(&s)),
        "kind" => value.as_str().is_some_and(|s| ["image", "pdf", "archive", "mail", "presentation", "document", "table", "fields", "formula", "checkbox", "text", "copy", "move", "script"].contains(&s)),
        "mode" => value.as_str().is_some_and(mode),
        "model_kind" => value.as_str().is_some_and(|s| ["registry", "custom", "default"].contains(&s)),
        "status" => value.as_str().is_some_and(|s| ["pending", "running", "paused", "done", "error", "in_progress", "skipped"].contains(&s)),
        _ => false,
    }).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationEvent {
    pub operation_id: String,
    pub operation: String,
    pub phase: String,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub code: Option<String>,
    pub context: Map<String, Value>,
}

fn event_level(event: &OperationEvent) -> &'static str {
    match event.phase.as_str() {
        "failed" => "error",
        "stage" if event.code.is_some() => "warn",
        "stage" => "debug",
        _ if event.context.get("partial") == Some(&Value::Bool(true)) => "warn",
        _ => "info",
    }
}

struct Transport { file: File, parent_id: String, bytes: u64 }
thread_local! { static TRANSPORT: RefCell<Option<Transport>> = const { RefCell::new(None) }; }
#[cfg(test)]
thread_local! { static CAPTURE: RefCell<Option<Vec<OperationEvent>>> = const { RefCell::new(None) }; }

#[cfg(test)]
pub(crate) fn capture<T>(work: impl FnOnce() -> T) -> (T, Vec<OperationEvent>) {
    struct Restore(Option<Vec<OperationEvent>>);
    impl Drop for Restore { fn drop(&mut self) { CAPTURE.with(|slot| {slot.replace(self.0.take());}); } }
    let _restore=Restore(CAPTURE.with(|slot|slot.replace(Some(Vec::new()))));
    let result=work();
    let events=CAPTURE.with(|slot|slot.borrow_mut().take().unwrap_or_default());
    (result,events)
}

fn emit(event: &OperationEvent) {
    let mut event = event.clone();
    TRANSPORT.with(|transport| {
        if let Some(transport) = transport.borrow_mut().as_mut() {
            event.parent_operation_id = Some(transport.parent_id.clone());
            if let Ok(mut bytes) = serde_json::to_vec(&event) {
                bytes.push(b'\n');
                if transport.bytes.saturating_add(bytes.len() as u64) <= MAX_TRANSPORT_BYTES {
                    if transport.file.write_all(&bytes).is_ok() { transport.bytes += bytes.len() as u64; }
                }
            }
        }
    });
    #[cfg(test)]
    CAPTURE.with(|slot| {if let Some(events)=slot.borrow_mut().as_mut() {events.push(event.clone());}});
    if let Ok(message) = serde_json::to_string(&event) {
        crate::app_log(event_level(&event), format!("operation {message}"));
    }
}

type Sink = Arc<dyn Fn(&OperationEvent) + Send + Sync>;

/// Each scope has exactly one terminal receipt. Early returns and unwinds are
/// visible failures; callers identify accepted cancellation explicitly.
pub struct OperationLog {
    id: String,
    operation: &'static str,
    context: Map<String, Value>,
    started: Instant,
    state: Mutex<OperationState>,
    sink: Sink,
}

#[derive(Default)]
struct OperationState { terminal: bool, stage: Option<&'static str> }

impl OperationLog {
    pub fn start(operation: &'static str, context: Value) -> Self {
        Self::start_with_id(operation, context, None)
    }

    pub fn start_with_id(operation: &'static str, context: Value, id: Option<&str>) -> Self {
        Self::with_sink(operation, context, id, Arc::new(emit))
    }

    fn with_sink(operation: &'static str, context: Value, id: Option<&str>, sink: Sink) -> Self {
        let log = Self {
            id: id.filter(|id| valid_id(id)).map(str::to_owned).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            operation: if identifier(operation) { operation } else { "operation" },
            context: safe_context(context), started: Instant::now(), state: Mutex::new(OperationState::default()), sink,
        };
        log.record("started", None, None, Value::Null);
        log
    }

    pub fn id(&self) -> &str { &self.id }

    fn record(&self, phase: &str, stage: Option<&str>, code: Option<&str>, context: Value) {
        let mut fields = self.context.clone();
        fields.extend(safe_context(context));
        let event = OperationEvent { operation_id: self.id.clone(), operation: self.operation.into(),
            phase: phase.into(), elapsed_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            parent_operation_id: None, stage: stage.filter(|s| identifier(s)).map(str::to_owned),
            code: code.filter(|s| identifier(s)).map(str::to_owned), context: fields };
        // A broken observer must not fail an OCR result or committed write.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.sink)(&event)));
    }

    pub fn step(&self, stage: &'static str, context: Value) {
        if let Ok(mut state) = self.state.lock() {
            if !state.terminal { state.stage=Some(stage); self.record("stage", Some(stage), None, context); }
        }
    }
    pub fn warn(&self, code: &'static str, context: Value) {
        if let Ok(state) = self.state.lock() {
            if !state.terminal { self.record("stage", Some("warning"), Some(code), context); }
        }
    }
    fn finish(&self, phase: &str, code: Option<&str>, context: Value) {
        if let Ok(mut state) = self.state.lock() {
            if !state.terminal {
                state.terminal=true;
                self.record(phase, if phase=="failed" {state.stage} else {None}, code, context);
            }
        }
    }
    pub fn succeed(&self, context: Value) { self.finish("completed", None, context); }
    pub fn fail(&self, code: &'static str) { self.finish("failed", Some(code), Value::Null); }
    pub fn cancel(&self) { self.finish("cancelled", None, Value::Null); }
}

impl Drop for OperationLog {
    fn drop(&mut self) {
        self.fail(if std::thread::panicking() { "panicked" } else { "incomplete" });
    }
}

/// A dedicated bounded channel carries typed child events, even on successful
/// workers. Free-form native stderr is never copied into the application's log.
pub(crate) fn with_worker_transport<T>(path: &Path, parent_id: &str, work: impl FnOnce() -> T) -> io::Result<T> {
    if !valid_id(parent_id) { return Ok(work()); }
    let Ok(file) = std::fs::OpenOptions::new().write(true).truncate(true).open(path) else { return Ok(work()); };
    let previous = TRANSPORT.with(|slot| slot.replace(Some(Transport { file, parent_id: parent_id.into(), bytes: 0 })));
    struct Reset(Option<Transport>);
    impl Drop for Reset { fn drop(&mut self) { TRANSPORT.with(|slot| { slot.replace(self.0.take()); }); } }
    let _reset = Reset(previous);
    Ok(work())
}

fn replay_worker_events_with(path: &Path, parent_id: &str, sink: impl Fn(&OperationEvent)) -> io::Result<usize> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_TRANSPORT_BYTES { return Err(io::Error::new(io::ErrorKind::InvalidData, "Worker log exceeds limit")); }
    let mut count = 0;
    for line in io::BufReader::new(file).lines() {
        let line = line?;
        if line.len() > 16 * 1024 { continue; }
        let Ok(mut event) = serde_json::from_str::<OperationEvent>(&line) else { continue; };
        if event.parent_operation_id.as_deref() != Some(parent_id) || !valid_id(&event.operation_id)
            || !identifier(&event.operation)
            || !["started", "stage", "completed", "failed", "cancelled"].contains(&event.phase.as_str())
            || event.stage.as_deref().is_some_and(|s| !identifier(s)) || event.code.as_deref().is_some_and(|s| !identifier(s)) { continue; }
        event.context = safe_context(Value::Object(event.context));
        sink(&event); count += 1;
    }
    Ok(count)
}

pub(crate) fn replay_worker_events(path: &Path, parent_id: &str) -> io::Result<usize> {
    replay_worker_events_with(path, parent_id, |event| {
        if let Ok(message) = serde_json::to_string(event) { crate::app_log(event_level(event), format!("operation {message}")); }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recorder() -> (Arc<Mutex<Vec<OperationEvent>>>, Sink) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let copy = events.clone();
        (events, Arc::new(move |event| copy.lock().unwrap().push(event.clone())))
    }

    #[test]
    fn scopes_emit_actual_ordered_events_and_only_one_terminal() {
        let (events, sink) = recorder();
        let log = OperationLog::with_sink("ocr.image", json!({"engine":"qwen2vl"}), Some("fe-abc-1"), sink);
        log.step("recognizing", json!({"page":2}));
        log.succeed(json!({"characters":17}));
        log.fail("late_error"); log.cancel(); log.step("late_stage", Value::Null);
        drop(log);
        let events = events.lock().unwrap();
        assert_eq!(events.iter().map(|e|e.phase.as_str()).collect::<Vec<_>>(), ["started","stage","completed"]);
        assert!(events.iter().all(|e|e.operation_id=="fe-abc-1"));
        assert_eq!(events[1].stage.as_deref(), Some("recognizing"));
        assert_eq!(events[2].context.get("characters"), Some(&json!(17)));
        assert!(events.windows(2).all(|e|e[0].elapsed_ms<=e[1].elapsed_ms));
    }

    #[test]
    fn failures_cancellation_and_early_return_never_report_success() {
        for ending in ["error","cancel","early"] {
            let (events, sink) = recorder();
            let log = OperationLog::with_sink("ocr.image", Value::Null, None, sink);
            if ending == "error" { log.fail("decode_failed"); }
            if ending == "cancel" { log.cancel(); }
            drop(log);
            let events = events.lock().unwrap();
            assert_eq!(events.len(),2);
            assert_eq!(events[1].phase, if ending=="cancel" {"cancelled"} else {"failed"});
            if ending=="early" { assert_eq!(events[1].code.as_deref(),Some("incomplete")); }
        }
    }

    #[test]
    fn racing_completion_and_progress_keep_one_terminal_as_the_last_event() {
        let (events,sink)=recorder();
        let log=Arc::new(OperationLog::with_sink("ocr.image",Value::Null,None,sink));
        std::thread::scope(|scope| {
            for i in 0..8 { let log=log.clone(); scope.spawn(move || {
                log.step("recognizing",json!({"page":i+1}));
                if i%2==0 {log.succeed(Value::Null);} else {log.cancel();}
            }); }
        });
        drop(log);
        let events=events.lock().unwrap();
        assert_eq!(events.iter().filter(|e|["completed","failed","cancelled"].contains(&e.phase.as_str())).count(),1);
        assert!(["completed","cancelled"].contains(&events.last().unwrap().phase.as_str()));
    }

    #[test]
    fn context_cannot_log_document_content_paths_queries_or_credentials() {
        let safe = safe_context(json!({"bytes":123,"engine":"tesseract","language":"eng+deu",
            "path":"/private/report.pdf","url":"smb://user:password@host/share","text":"medical report",
            "query":"sensitive query","prompt":"secret prompt","error":"private input text","token":"secret",
            "config":{"rec_model":"/private/model.gguf"},"backend":"smb://secret","model":"private-model",
            "count":-2,"mode":"a document paragraph","format":"pdf\nsecret"}));
        assert_eq!(safe.len(),3);
        let serialized=serde_json::to_string(&safe).unwrap();
        for denied in ["private","secret","medical","sensitive","password"] { assert!(!serialized.contains(denied)); }
    }

    #[test]
    fn verbosity_always_preserves_errors_and_filters_other_levels_consistently() {
        for rank in 0..=3 { assert!(enabled_at(rank,"error")); }
        assert!(!enabled_at(0,"warn")); assert!(enabled_at(1,"warn"));
        assert!(!enabled_at(1,"info")); assert!(enabled_at(2,"info"));
        assert!(!enabled_at(2,"debug")); assert!(enabled_at(3,"debug"));
        assert!(set_verbosity("unsupported").is_err());
    }

    #[test]
    fn child_events_survive_success_and_replay_only_the_expected_operation() {
        let file=tempfile::NamedTempFile::new().unwrap();
        let parent=uuid::Uuid::new_v4().to_string();
        with_worker_transport(file.path(), &parent, || {
            let log=OperationLog::start("ocr.worker_child",json!({"surface":"worker"}));
            log.step("recognizing",json!({"pages":1}));
            log.succeed(json!({"characters":4}));
        }).unwrap();
        let events=Mutex::new(Vec::new());
        assert_eq!(replay_worker_events_with(file.path(), &parent, |e|events.lock().unwrap().push(e.clone())).unwrap(),3);
        assert_eq!(events.lock().unwrap()[2].phase,"completed");
        assert_eq!(replay_worker_events_with(file.path(), &uuid::Uuid::new_v4().to_string(), |_| panic!("wrong parent")).unwrap(),0);
    }

    #[test]
    fn a_failing_log_observer_cannot_change_the_operation_result() {
        let log=OperationLog::with_sink("ocr.image",Value::Null,None,Arc::new(|_|panic!("observer failed")));
        log.succeed(json!({"pages":1}));
        let directory=tempfile::tempdir().unwrap();
        assert_eq!(with_worker_transport(&directory.path().join("absent/channel"),&uuid::Uuid::new_v4().to_string(),||42).unwrap(),42);
    }
}
