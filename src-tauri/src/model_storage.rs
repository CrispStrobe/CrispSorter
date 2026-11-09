//! Reclaims only managed, unreferenced model generations. Callers serialize
//! store changes with `lock_store`; SQLite read transactions keep generations
//! used by this or another process alive until that process releases them.

use super::{ModelStorageEntry, ModelStorageReport, ModelStorageCleanup};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const MAX_MODELS: usize = 1024;
const MAX_GENERATIONS: usize = 256;
const MAX_FILES: usize = 16;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

pub(super) struct StoreLock { _connection: Connection }
#[derive(Default)]
struct UsageLocks { connections: Mutex<HashMap<PathBuf, Connection>> }
static USAGE: OnceLock<UsageLocks> = OnceLock::new();

fn private_directory(path: &Path) -> Result<(), String> {
    let meta = path.symlink_metadata().map_err(|error| format!("Inspect model storage: {error}"))?;
    if !meta.is_dir() || meta.file_type().is_symlink()
        || path.canonicalize().ok().as_deref() != Some(path) {
        return Err("Model storage contains an unsafe directory; no files were removed.".into());
    }
    Ok(())
}

fn open_lock(path: &Path) -> Result<Connection, String> {
    // SQLite may open journal sidecars before returning the connection. Reject
    // pre-existing links there as well as at the database itself.
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string(); name.push(suffix);
        let sidecar = PathBuf::from(name);
        match sidecar.symlink_metadata() {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink()
                || sidecar.canonicalize().ok().as_deref() != Some(sidecar.as_path()) =>
                return Err("The model storage lock has an unsafe journal file.".into()),
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(format!("Inspect model storage journal: {error}")),
        }
    }
    match path.symlink_metadata() {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 1024 * 1024 =>
            return Err("The model storage lock is not a regular private file.".into()),
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // create_new never follows a pre-existing link. The store lock
            // serializes lease creation; simultaneous store creation may race.
            match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(_) => {},
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let meta = path.symlink_metadata().map_err(|error| format!("Inspect storage lock: {error}"))?;
                    if !meta.is_file() || meta.file_type().is_symlink() { return Err("Unsafe model storage lock.".into()); }
                },
                Err(error) => return Err(format!("Create model storage lock: {error}")),
            }
        },
        Err(error) => return Err(format!("Inspect model storage lock: {error}")),
    }
    if path.canonicalize().ok().as_deref() != Some(path) { return Err("Unsafe model storage lock path.".into()); }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|error| format!("Open model storage lock: {error}"))?;
    connection.busy_timeout(Duration::from_secs(2)).map_err(|error| error.to_string())?;
    let journal: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(|error| format!("Inspect model storage lock mode: {error}"))?;
    // BEGIN EXCLUSIVE excludes readers only in rollback-journal mode. Never
    // silently convert a database that another process may currently lease.
    if journal != "delete" { return Err("The model storage lock uses an unsupported journal mode; its files were preserved.".into()); }
    // Initialization is short and idempotent. Reading this table below obtains
    // a real SHARED lock, unlike an empty deferred transaction.
    connection.execute_batch("CREATE TABLE IF NOT EXISTS lifetime (id INTEGER PRIMARY KEY);")
        .map_err(|error| format!("Initialize model storage lock: {error}"))?;
    Ok(connection)
}

pub(super) fn lock_store(root: &Path) -> Result<StoreLock, String> {
    private_directory(root)?;
    let connection = open_lock(&root.join("storage-lock.sqlite"))?;
    connection.execute_batch("BEGIN IMMEDIATE")
        .map_err(|_| "Model storage is busy in another process. Wait for its download or cleanup to finish and retry.")?;
    Ok(StoreLock { _connection: connection })
}

fn model_key(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn generation(value: &str) -> bool {
    value.len() == 36 && uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
fn incoming(value: &str) -> bool {
    value.strip_prefix("incoming-").is_some_and(|suffix|
        (6..=32).contains(&suffix.len()) && suffix.bytes().all(|c| c.is_ascii_alphanumeric()))
}

fn lease_path(model: &Path, version: &str) -> Result<PathBuf, String> {
    if !generation(version) { return Err("Invalid model generation.".into()); }
    private_directory(model)?;
    let leases = model.join(".leases");
    match std::fs::create_dir(&leases) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(format!("Create model usage locks: {error}")),
    }
    private_directory(&leases)?;
    Ok(leases.join(format!("{version}.sqlite")))
}

impl UsageLocks {
    fn retain(&self, model: &Path, version: &str) -> Result<(), String> {
        let path = lease_path(model, version)?;
        let mut connections = self.connections.lock().map_err(|_| "Model usage tracking is unavailable.")?;
        if connections.contains_key(&path) { return Ok(()); }
        let connection = open_lock(&path)?;
        connection.execute_batch("BEGIN")
            .map_err(|error| format!("Protect model generation: {error}"))?;
        let _: i64 = connection.query_row("SELECT COUNT(*) FROM lifetime", [], |row| row.get(0))
            .map_err(|error| format!("Protect model generation: {error}"))?;
        connections.insert(path, connection);
        Ok(())
    }
}

/// Explicit local paths inside the managed store need the same lifetime guard
/// as registry names. External user-managed files are never reclaimed here.
pub(super) fn retain_path_locked(root: &Path, path: &Path) -> Result<(), String> {
    let canonical = path.canonicalize().map_err(|error| format!("Resolve local model: {error}"))?;
    if !canonical.starts_with(root) {
        if path.starts_with(root) { return Err("The managed model path leaves private storage.".into()); }
        return Ok(());
    }
    let relative = canonical.strip_prefix(root).map_err(|error| error.to_string())?;
    let components: Vec<_> = relative.components().collect();
    if components.len() != 3 { return Err("The managed model path is not a published generation.".into()); }
    let key = components[0].as_os_str().to_str().ok_or("Invalid model directory.")?;
    let version = components[1].as_os_str().to_str().ok_or("Invalid model generation.")?;
    if !model_key(key) || !generation(version) { return Err("The managed model path is not a published generation.".into()); }
    let model = root.join(key);
    private_directory(&model.join(version))?;
    USAGE.get_or_init(UsageLocks::default).retain(&model, version)
}

fn exclusive(model: &Path, version: &str) -> Result<Option<Connection>, String> {
    let connection = open_lock(&lease_path(model, version)?)?;
    connection.busy_timeout(Duration::ZERO).map_err(|error| error.to_string())?;
    match connection.execute_batch("BEGIN EXCLUSIVE") {
        Ok(()) => Ok(Some(connection)),
        Err(rusqlite::Error::SqliteFailure(error, _))
            if matches!(error.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => Ok(None),
        Err(error) => Err(format!("Check model usage: {error}")),
    }
}

#[derive(serde::Deserialize)]
struct Pointer { schema_version: u32, model_name: String, version: String }
fn pointer(model: &Path) -> Result<Option<Pointer>, String> {
    let path = model.join("current.json");
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Inspect model manifest: {error}")),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_MANIFEST_BYTES {
        return Err("An unsafe model manifest was preserved for inspection.".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(|error| error.to_string())?.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES { return Err("An oversized model manifest was preserved.".into()); }
    let value: Pointer = serde_json::from_slice(&bytes).map_err(|_| "An unreadable model manifest was preserved for inspection.")?;
    let key = format!("{:x}", Sha256::digest(value.model_name.as_bytes()));
    if value.schema_version != 1 || value.model_name.is_empty() || !generation(&value.version)
        || model.file_name().and_then(|value| value.to_str()) != Some(key.as_str()) {
        return Err("An unrecognized model manifest was preserved for inspection.".into());
    }
    Ok(Some(value))
}

fn children(path: &Path, limit: usize) -> Result<Vec<PathBuf>, String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|error| format!("Read model storage: {error}"))? {
        entries.push(entry.map_err(|error| error.to_string())?.path());
        if entries.len() > limit { return Err("Model storage contains too many entries for a safe cleanup. No entries in this directory were removed.".into()); }
    }
    entries.sort();
    Ok(entries)
}

fn payload_bytes(path: &Path) -> Result<u64, String> {
    private_directory(path)?;
    let mut bytes = 0u64;
    for file in children(path, MAX_FILES)? {
        let meta = file.symlink_metadata().map_err(|error| error.to_string())?;
        let name = file.file_name().and_then(|value| value.to_str()).ok_or("Invalid model artifact name.")?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_FILE_BYTES
            || name.is_empty() || name.starts_with('.') || name.len() > 240
            || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':')) {
            return Err("An unrecognized model artifact was preserved for inspection.".into());
        }
        bytes = bytes.checked_add(meta.len()).ok_or("Model storage size overflow.")?;
    }
    Ok(bytes)
}

struct Candidate { path: PathBuf, version: String, bytes: u64, current: bool, staging: bool }
struct Model { path: PathBuf, pointer: Option<Pointer>, candidates: Vec<Candidate> }
fn scan(root: &Path, warnings: &mut Vec<String>) -> Result<Vec<Model>, String> {
    private_directory(root)?;
    let mut models = Vec::new();
    for path in children(root, MAX_MODELS + 8)? {
        let Some(key) = path.file_name().and_then(|value| value.to_str()) else { continue; };
        if !model_key(key) { continue; }
        let result = (|| {
            private_directory(&path)?;
            let current = pointer(&path)?;
            let mut candidates = Vec::new();
            for item in children(&path, MAX_GENERATIONS + 8)? {
                let Some(version) = item.file_name().and_then(|value| value.to_str()) else { continue; };
                let staging = incoming(version);
                if !generation(version) && !staging { continue; }
                let bytes = payload_bytes(&item)?;
                candidates.push(Candidate { current: current.as_ref().is_some_and(|p| p.version == version),
                    version: version.to_owned(), path: item, bytes, staging });
            }
            Ok::<_, String>(Model { path: path.clone(), pointer: current, candidates })
        })();
        match result { Ok(model) => models.push(model), Err(error) => warnings.push(format!("{}: {error}", &key[..12])) }
    }
    Ok(models)
}

pub(super) fn report_locked(root: &Path) -> Result<ModelStorageReport, String> {
    let mut report = ModelStorageReport::default();
    for model in scan(root, &mut report.warnings)? {
        let mut entry = ModelStorageEntry {
            name: model.pointer.as_ref().map(|p| p.model_name.clone()).unwrap_or_else(|| "Interrupted installation".into()),
            version: model.pointer.as_ref().map(|p| p.version.clone()),
            bytes: 0, reclaimable_bytes: 0, in_use: false,
        };
        for candidate in &model.candidates {
            entry.bytes += candidate.bytes;
            let in_use = if candidate.staging { false } else { exclusive(&model.path, &candidate.version)?.is_none() };
            entry.in_use |= in_use;
            if !candidate.current && !in_use { entry.reclaimable_bytes += candidate.bytes; }
        }
        report.bytes += entry.bytes;
        report.reclaimable_bytes += entry.reclaimable_bytes;
        if entry.bytes > 0 || entry.version.is_some() { report.models.push(entry); }
    }
    report.models.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(report)
}

fn remove_payload(candidate: &Candidate) -> Result<(), String> {
    // Re-check immediately before mutation. Never traverse a link or an
    // unexpected subtree, even when an earlier scan accepted this generation.
    payload_bytes(&candidate.path)?;
    for path in children(&candidate.path, MAX_FILES)? {
        std::fs::remove_file(path).map_err(|error| format!("Remove unused model artifact: {error}"))?;
    }
    std::fs::remove_dir(&candidate.path).map_err(|error| format!("Remove unused model generation: {error}"))
}

fn remove_unused_lease(model: &Path, candidate: &Candidate, warnings: &mut Vec<String>) {
    if candidate.staging || candidate.path.exists() { return; }
    // All callers still hold the store lock and have dropped their exclusive
    // lease connection. The payload no longer exists, so no compliant reader
    // can acquire a new lease for this UUID. Keep metadata bounded as well.
    let result = (|| {
        let path = lease_path(model, &candidate.version)?;
        let meta = path.symlink_metadata().map_err(|error| error.to_string())?;
        if !meta.is_file() || meta.file_type().is_symlink() { return Err("Unsafe unused model lease was preserved.".into()); }
        std::fs::remove_file(path).map_err(|error| format!("Remove unused model lease: {error}"))
    })();
    if let Err(error) = result { warnings.push(error); }
}

pub(super) fn cleanup_locked(root: &Path) -> Result<ModelStorageCleanup, String> {
    let mut warnings = Vec::new();
    let mut removed_bytes = 0u64;
    let mut removed_versions = 0usize;
    for model in scan(root, &mut warnings)? {
        for candidate in &model.candidates {
            if candidate.current { continue; }
            let lock = if candidate.staging { None } else {
                match exclusive(&model.path, &candidate.version)? { Some(lock) => Some(lock), None => continue }
            };
            match remove_payload(candidate) {
                Ok(()) => { removed_bytes += candidate.bytes; removed_versions += 1; },
                Err(error) => warnings.push(error),
            }
            drop(lock);
            remove_unused_lease(&model.path, candidate, &mut warnings);
        }
    }
    let mut report = report_locked(root)?;
    for warning in warnings { if !report.warnings.contains(&warning) { report.warnings.push(warning); } }
    Ok(ModelStorageCleanup { removed_bytes, removed_versions, report })
}

pub(super) fn remove_model_locked(root: &Path, name: &str, expected_version: &str) -> Result<ModelStorageCleanup, String> {
    if name.is_empty() || !generation(expected_version) { return Err("Select an installed model before removing it.".into()); }
    let model = root.join(format!("{:x}", Sha256::digest(name.as_bytes())));
    private_directory(&model)?;
    let current = pointer(&model)?.ok_or("This model is no longer installed. Refresh storage and try again.")?;
    if current.model_name != name || current.version != expected_version {
        return Err("This model changed after the storage view was opened. Refresh storage before removing it.".into());
    }
    let mut warnings = Vec::new();
    let entries = scan(root, &mut warnings)?;
    let entry = entries.into_iter().find(|entry| entry.path == model)
        .ok_or("This model contains unrecognized files and was preserved for inspection.")?;
    let mut locks = Vec::new();
    for candidate in &entry.candidates {
        if !candidate.staging {
            locks.push(exclusive(&model, &candidate.version)?.ok_or(
                "This model is in use by OCR in this or another application window. Restart the application, then remove it before starting recognition.")?);
        }
    }
    // Removing the pointer first makes a crash recoverable: the next cleanup
    // sees unpublished generations. No reader can resolve one while locked.
    std::fs::remove_file(model.join("current.json")).map_err(|error| format!("Remove model installation: {error}"))?;
    #[cfg(unix)]
    std::fs::File::open(&model).and_then(|file| file.sync_all()).map_err(|error| format!("Sync model removal: {error}"))?;
    let mut removed_bytes = 0;
    let mut removed_versions = 0;
    for candidate in &entry.candidates {
        match remove_payload(candidate) {
            Ok(()) => { removed_bytes += candidate.bytes; removed_versions += 1; },
            Err(error) => warnings.push(error),
        }
    }
    drop(locks);
    for candidate in &entry.candidates { remove_unused_lease(&model, candidate, &mut warnings); }
    let mut report = report_locked(root)?;
    report.warnings.extend(warnings);
    Ok(ModelStorageCleanup { removed_bytes, removed_versions, report })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path, name: &str, current: bool) -> (PathBuf, String) {
        let model = root.join(format!("{:x}", Sha256::digest(name.as_bytes())));
        std::fs::create_dir_all(&model).unwrap();
        let version = uuid::Uuid::new_v4().to_string();
        let path = model.join(&version);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("model.gguf"), b"GGUF fixture").unwrap();
        if current { std::fs::write(model.join("current.json"), serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "model_name": name, "version": version,
        })).unwrap()).unwrap(); }
        (model, version)
    }

    #[test]
    fn cleanup_preserves_current_and_leased_versions_and_reclaims_interrupted_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let _store = lock_store(&root).unwrap();
        let (model, old) = fixture(&root, "recognizer", false);
        let (_, current) = fixture(&root, "recognizer", true);
        let (_, obsolete) = fixture(&root, "recognizer", false);
        let usage = UsageLocks::default(); usage.retain(&model, &old).unwrap();
        let staging = model.join("incoming-ABC123"); std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("partial.gguf"), b"GGUF").unwrap();
        let result = cleanup_locked(&root).unwrap();
        assert_eq!(result.removed_versions, 2);
        assert_eq!(result.removed_bytes, 16);
        assert!(model.join(&current).exists()); assert!(model.join(&old).exists());
        assert!(!model.join(&obsolete).exists()); assert!(!staging.exists());
        assert!(!model.join(".leases").join(format!("{obsolete}.sqlite")).exists());
        assert!(result.report.models[0].in_use); assert_eq!(result.report.reclaimable_bytes, 0);
        drop(usage);
        assert_eq!(cleanup_locked(&root).unwrap().removed_versions, 1);
        assert!(model.join(current).exists());
    }

    #[test]
    fn removal_requires_current_version_and_refuses_active_models() {
        let dir = tempfile::tempdir().unwrap(); let root = dir.path().canonicalize().unwrap();
        let _store = lock_store(&root).unwrap();
        let (model, old) = fixture(&root, "test", true);
        let (_, current) = fixture(&root, "test", true);
        assert!(remove_model_locked(&root, "test", &old).unwrap_err().contains("changed"));
        let usage = UsageLocks::default(); usage.retain(&model, &old).unwrap();
        assert!(remove_model_locked(&root, "test", &current).unwrap_err().contains("in use"));
        assert!(model.join("current.json").exists());
        drop(usage);
        let result = remove_model_locked(&root, "test", &current).unwrap();
        assert_eq!(result.removed_versions, 2); assert!(!model.join("current.json").exists());
        assert_eq!(result.report.bytes, 0); assert!(result.report.models.is_empty());
    }

    #[test]
    fn malformed_manifests_and_unknown_subtrees_are_preserved() {
        let dir = tempfile::tempdir().unwrap(); let root = dir.path().canonicalize().unwrap();
        let _store = lock_store(&root).unwrap();
        let (first, version) = fixture(&root, "broken", false);
        std::fs::write(first.join("current.json"), b"{broken").unwrap();
        let (second, nested) = fixture(&root, "nested", false);
        std::fs::create_dir(second.join(&nested).join("unexpected")).unwrap();
        let result = cleanup_locked(&root).unwrap();
        assert_eq!(result.removed_versions, 0); assert_eq!(result.report.warnings.len(), 2);
        assert!(first.join(version).exists()); assert!(second.join(nested).exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_artifacts_and_locks_never_touch_targets() {
        let dir = tempfile::tempdir().unwrap(); let root = dir.path().canonicalize().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap(); std::fs::write(outside.path(), b"preserve").unwrap();
        let (model, version) = fixture(&root, "linked", false);
        std::fs::remove_file(model.join(&version).join("model.gguf")).unwrap();
        std::os::unix::fs::symlink(outside.path(), model.join(&version).join("model.gguf")).unwrap();
        assert_eq!(cleanup_locked(&root).unwrap().removed_versions, 0);
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"preserve");
        std::os::unix::fs::symlink(outside.path(), root.join("storage-lock.sqlite")).unwrap();
        assert!(lock_store(&root).is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"preserve");
        std::fs::remove_file(root.join("storage-lock.sqlite")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("storage-lock.sqlite-journal")).unwrap();
        assert!(lock_store(&root).is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"preserve");
    }

    #[test]
    fn store_lock_serializes_independent_connections() {
        let dir = tempfile::tempdir().unwrap(); let root = dir.path().canonicalize().unwrap();
        let first = lock_store(&root).unwrap();
        assert!(lock_store(&root).is_err());
        drop(first); assert!(lock_store(&root).is_ok());
    }

    #[test]
    fn wal_leases_are_rejected_instead_of_mistaking_a_writer_lock_for_exclusive_use() {
        let dir = tempfile::tempdir().unwrap(); let root = dir.path().canonicalize().unwrap();
        let (model, version) = fixture(&root, "wal", false);
        let path = lease_path(&model, &version).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE lifetime (id INTEGER PRIMARY KEY); BEGIN; SELECT * FROM lifetime;").unwrap();
        assert!(exclusive(&model, &version).unwrap_err().contains("journal mode"));
        assert!(cleanup_locked(&root).is_err());
        assert!(model.join(version).join("model.gguf").exists());
    }

    #[test]
    fn lease_child_process() {
        let Some(root) = std::env::var_os("CRISPSORTER_TEST_MODEL_LEASE_ROOT") else { return; };
        let root = PathBuf::from(root);
        let model = root.join(std::env::var("CRISPSORTER_TEST_MODEL_LEASE_KEY").unwrap());
        let version = std::env::var("CRISPSORTER_TEST_MODEL_LEASE_VERSION").unwrap();
        let usage = UsageLocks::default();
        { let _store = lock_store(&root).unwrap(); usage.retain(&model, &version).unwrap(); }
        std::fs::write(root.join("child-ready"), b"ready").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !root.join("child-release").exists() {
            assert!(std::time::Instant::now() < deadline, "Parent did not release model lease test");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // Spawns a child test process, which only spawn-capable (`sidecars`) builds
    // may do; the compliance guard checks this per file.
    #[cfg(feature = "sidecars")]
    #[test]
    fn another_process_keeps_old_generation_alive_until_it_exits() {
        let dir = tempfile::tempdir().unwrap(); let root = dir.path().canonicalize().unwrap();
        let (model, old) = fixture(&root, "shared", false); fixture(&root, "shared", true);
        let module = module_path!().split_once("::").map(|(_, name)| name).unwrap();
        let child_test = format!("{module}::lease_child_process");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", child_test.as_str(), "--nocapture"])
            .env("CRISPSORTER_TEST_MODEL_LEASE_ROOT", &root)
            .env("CRISPSORTER_TEST_MODEL_LEASE_KEY", model.file_name().unwrap())
            .env("CRISPSORTER_TEST_MODEL_LEASE_VERSION", &old).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !root.join("child-ready").exists() {
            if std::time::Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
                let _ = child.kill(); let _ = child.wait(); panic!("Child model lease was not acquired");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        { let _store = lock_store(&root).unwrap(); assert_eq!(cleanup_locked(&root).unwrap().removed_versions, 0); }
        std::fs::write(root.join("child-release"), b"release").unwrap();
        assert!(child.wait().unwrap().success());
        let _store = lock_store(&root).unwrap();
        assert_eq!(cleanup_locked(&root).unwrap().removed_versions, 1);
        assert!(!model.join(old).exists());
    }
}
