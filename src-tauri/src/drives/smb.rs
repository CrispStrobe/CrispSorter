//! One CloudDrive implementation selected by both GUI and CLI's registry factory.
use super::{CloudDrive, DirEntry, DriveCapabilities, DriveType, FileStat};
use anyhow::{anyhow, ensure, Result};
use crisp_smb_source::{relative_path, Backend, BlockingSession, Settings, CHUNK};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

const BUFFER_LIMIT: u64 = 64 * 1024 * 1024;
type Logger = fn(&str, String);
enum Transport {
    Mounted(PathBuf),
    Rust(BlockingSession),
}
pub struct SmbDrive {
    label: String,
    transport: Transport,
    log: Logger,
}
impl SmbDrive {
    pub fn new(
        label: String,
        url: &str,
        settings: Settings,
        username: &str,
        password: &str,
        log: Logger,
    ) -> Result<Self> {
        settings.validate(url)?;
        let transport = match settings.backend {
            Backend::Mounted => {
                let root = std::fs::canonicalize(settings.mount_path.as_ref().unwrap())
                    .map_err(|_| anyhow!("SMB mount is not accessible"))?;
                ensure!(root.is_dir(), "SMB mount must be a directory");
                Transport::Mounted(root)
            }
            Backend::Rust => Transport::Rust(BlockingSession::new(
                settings.config(url, username, password)?,
            )?),
        };
        log(
            "info",
            format!(
                "SMB transport selected: backend={} encryption={}",
                settings.backend.name(),
                if settings.backend == Backend::Mounted {
                    "os-managed"
                } else if settings.require_encryption {
                    "required"
                } else {
                    "optional"
                }
            ),
        );
        Ok(Self {
            label,
            transport,
            log,
        })
    }
    fn backend(&self) -> &'static str {
        match self.transport {
            Transport::Mounted(_) => "mounted",
            Transport::Rust(_) => "rust",
        }
    }
    // No labels, URLs, paths, credentials, or upstream error strings in app logs.
    fn operation<T>(&self, op: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let started = Instant::now();
        let result = f();
        let status = if result.is_ok() { "ok" } else { "error" };
        (self.log)(
            if result.is_err() {
                "error"
            } else if matches!(op, "list" | "stat") {
                "debug"
            } else {
                "info"
            },
            format!(
                "SMB operation: backend={} operation={op} status={status} elapsed_ms={}",
                self.backend(),
                started.elapsed().as_millis()
            ),
        );
        // Mounted OS errors can include paths; expose only the operation to all callers.
        result.map_err(|e| {
            if let Some(core) = e.downcast_ref::<crisp_smb_source::Error>() {
                anyhow!("{core}")
            } else {
                anyhow!("SMB {op} failed")
            }
        })
    }
    fn local(root: &Path, path: &Path) -> Result<PathBuf> {
        let full = root.join(relative_path(path)?);
        let mut ancestor = full.as_path();
        while !ancestor.exists() {
            // A dangling symlink is still an existing link, not a new filename.
            ensure!(
                std::fs::symlink_metadata(ancestor).is_err(),
                "SMB path is unavailable"
            );
            ancestor = ancestor
                .parent()
                .ok_or_else(|| anyhow!("Invalid SMB path"))?;
        }
        ensure!(
            std::fs::canonicalize(ancestor)?.starts_with(root),
            "SMB path leaves mount"
        );
        Ok(full)
    }
    fn request(session: &BlockingSession, op: &str, path: &Path) -> Result<Value> {
        Ok(session.request(op, &relative_path(path)?, "", 0)?)
    }
    fn number(value: &Value, key: &str) -> Result<u64> {
        value[key]
            .as_u64()
            .ok_or_else(|| anyhow!("Invalid SMB metadata"))
    }
    fn stream_read(
        session: &BlockingSession,
        path: &Path,
        writer: &mut dyn Write,
        range: Option<(u64, u64)>,
    ) -> Result<u64> {
        let relative = relative_path(path)?;
        let opened = session.request("openRead", &relative, "", 0)?;
        let handle = Self::number(&opened, "handle")?;
        let result = (|| {
            let size = Self::number(&opened, "size")?;
            let (start, stop) = range.unwrap_or((0, size));
            ensure!(start <= stop && stop <= size, "Invalid SMB range");
            let mut offset = start;
            while offset < stop {
                let bytes =
                    session.read_chunk(handle, offset, (stop - offset).min(CHUNK as u64) as u32)?;
                ensure!(!bytes.is_empty(), "Incomplete SMB read");
                writer.write_all(&bytes)?;
                offset += bytes.len() as u64;
            }
            let after = session.request("stat", &relative, "", 0)?;
            ensure!(
                after["version"] == opened["version"],
                "SMB file changed during read"
            );
            Ok(offset - start)
        })();
        let closed = session.request("closeRead", "", "", handle);
        let count = result?;
        closed?;
        Ok(count)
    }
    fn stream_write(
        session: &BlockingSession,
        path: &Path,
        reader: &mut dyn Read,
        size: u64,
    ) -> Result<()> {
        let relative = relative_path(path)?;
        // CloudDrive's write contract creates parents. Never replay a failed write.
        if let Some((parent, _)) = relative.rsplit_once('/') {
            Self::mkdir(session, parent)?;
        }
        let handle = session
            .request("openWrite", &relative, "", 0)?
            .as_u64()
            .ok_or_else(|| anyhow!("Invalid SMB handle"))?;
        let result = (|| {
            let mut remaining = size;
            let mut buffer = vec![0; CHUNK];
            while remaining > 0 {
                let count = remaining.min(CHUNK as u64) as usize;
                reader.read_exact(&mut buffer[..count])?;
                session.write_chunk(handle, buffer[..count].to_vec())?;
                remaining -= count as u64;
            }
            let mut extra = [0];
            ensure!(
                reader.read(&mut extra)? == 0,
                "SMB input exceeds declared size"
            );
            session.request("finishWrite", "", "", handle)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = session.request("abortWrite", "", "", handle);
        }
        result
    }
    fn mkdir(session: &BlockingSession, path: &str) -> Result<()> {
        let mut current = String::new();
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(segment);
            if let Err(error) = session.request("mkdir", &current, "", 0) {
                // Existing directories are okay; no retry of the mutation.
                if session
                    .request("stat", &current, "", 0)
                    .ok()
                    .and_then(|v| v["directory"].as_bool())
                    != Some(true)
                {
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }
}
impl CloudDrive for SmbDrive {
    fn label(&self) -> &str {
        &self.label
    }
    fn drive_type(&self) -> DriveType {
        DriveType::Smb
    }
    fn capabilities(&self) -> DriveCapabilities {
        DriveCapabilities {
            create_dir: true,
            rename: true,
            move_path: true,
            streaming: true,
            ..DriveCapabilities::basic()
        }
    }
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.operation("list", || {
            let mut entries = match &self.transport {
                Transport::Mounted(root) => std::fs::read_dir(Self::local(root, path)?)?
                    .map(|entry| {
                        let entry = entry?;
                        let meta = entry.metadata()?;
                        Ok(DirEntry {
                            name: entry.file_name().to_string_lossy().into(),
                            is_dir: meta.is_dir(),
                            size: (!meta.is_dir()).then_some(meta.len()),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                Transport::Rust(session) => Self::request(session, "list", path)?
                    .as_array()
                    .ok_or_else(|| anyhow!("Invalid SMB listing"))?
                    .iter()
                    .map(|v| {
                        let name = v["name"]
                            .as_str()
                            .ok_or_else(|| anyhow!("Invalid SMB name"))?;
                        ensure!(
                            !name.is_empty()
                                && !name.contains(['/', '\\'])
                                && name != "."
                                && name != "..",
                            "Invalid SMB name"
                        );
                        Ok(DirEntry {
                            name: name.into(),
                            is_dir: v["directory"]
                                .as_bool()
                                .ok_or_else(|| anyhow!("Invalid SMB type"))?,
                            size: v["size"].as_u64(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            };
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        })
    }
    fn stat(&self, path: &Path) -> Result<FileStat> {
        self.operation("stat", || match &self.transport {
            Transport::Mounted(root) => {
                let meta = std::fs::metadata(Self::local(root, path)?)?;
                Ok(FileStat {
                    size: meta.len(),
                    is_dir: meta.is_dir(),
                    mtime_unix: meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|d| i64::try_from(d.as_secs()).ok()),
                })
            }
            Transport::Rust(session) => {
                let v = Self::request(session, "stat", path)?;
                // SMB FILETIME: 100ns since 1601; 0 means unavailable.
                let mtime = v["modified"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .map(|n| (n / 10_000_000) as i64 - 11_644_473_600);
                Ok(FileStat {
                    size: Self::number(&v, "size")?,
                    is_dir: v["directory"]
                        .as_bool()
                        .ok_or_else(|| anyhow!("Invalid SMB type"))?,
                    mtime_unix: mtime,
                })
            }
        })
    }
    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        self.operation("read", || {
            let size = self.stat(path)?.size;
            ensure!(
                size <= BUFFER_LIMIT,
                "Use SMB streaming for files larger than 64 MiB"
            );
            let mut bytes = Vec::with_capacity(size as usize);
            // A hard writer cap also covers a file growing after stat.
            struct Bounded<'a>(&'a mut Vec<u8>);
            impl Write for Bounded<'_> {
                fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                    if self.0.len() as u64 + b.len() as u64 > BUFFER_LIMIT {
                        return Err(std::io::Error::other("SMB buffer limit"));
                    }
                    self.0.extend_from_slice(b);
                    Ok(b.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            self.read_file_to_writer(path, &mut Bounded(&mut bytes))?;
            Ok(bytes)
        })
    }
    fn read_range(&self, path: &Path, start: u64, end: u64) -> Result<Vec<u8>> {
        self.operation("read_range", || {
            let stop = end.checked_add(1).ok_or_else(|| anyhow!("Invalid range"))?;
            ensure!(
                start < stop && stop - start <= BUFFER_LIMIT,
                "Invalid range"
            );
            let mut bytes = Vec::with_capacity((stop - start) as usize);
            match &self.transport {
                Transport::Mounted(root) => {
                    let mut file = std::fs::File::open(Self::local(root, path)?)?;
                    ensure!(stop <= file.metadata()?.len(), "Invalid range");
                    file.seek(SeekFrom::Start(start))?;
                    file.take(stop - start).read_to_end(&mut bytes)?;
                    ensure!(bytes.len() as u64 == stop - start, "Incomplete range");
                }
                Transport::Rust(session) => {
                    Self::stream_read(session, path, &mut bytes, Some((start, stop)))?;
                }
            }
            Ok(bytes)
        })
    }
    fn read_file_to_writer(&self, path: &Path, writer: &mut dyn Write) -> Result<u64> {
        self.operation("download", || {
            let count = match &self.transport {
                Transport::Mounted(root) => {
                    super::mounted_read::read_to_writer(&Self::local(root, path)?, writer)?
                }
                Transport::Rust(session) => Self::stream_read(session, path, writer, None)?,
            };
            (self.log)(
                "debug",
                format!(
                    "SMB transfer: backend={} direction=download bytes={count}",
                    self.backend()
                ),
            );
            Ok(count)
        })
    }
    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        self.write_file_from_reader(path, &mut &*data, data.len() as u64)
    }
    fn write_file_from_reader(&self, path: &Path, reader: &mut dyn Read, size: u64) -> Result<()> {
        self.operation("upload", || {
            match &self.transport {
                Transport::Rust(session) => Self::stream_write(session, path, reader, size)?,
                Transport::Mounted(root) => {
                    let full = Self::local(root, path)?;
                    if let Some(parent) = full.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let mut file = std::fs::File::create(&full)?;
                    let written = std::io::copy(&mut reader.take(size), &mut file)?;
                    ensure!(written == size, "Incomplete input");
                    let mut extra = [0];
                    ensure!(reader.read(&mut extra)? == 0, "Input exceeds size");
                    file.sync_all()?;
                }
            }
            (self.log)(
                "debug",
                format!(
                    "SMB transfer: backend={} direction=upload bytes={size}",
                    self.backend()
                ),
            );
            Ok(())
        })
    }
    fn create_dir(&self, path: &Path) -> Result<()> {
        self.operation("mkdir", || match &self.transport {
            Transport::Mounted(root) => Ok(std::fs::create_dir_all(Self::local(root, path)?)?),
            Transport::Rust(session) => Self::mkdir(session, &relative_path(path)?),
        })
    }
    fn delete(&self, path: &Path) -> Result<()> {
        self.operation("delete", || {
            ensure!(!relative_path(path)?.is_empty(), "Cannot delete SMB root");
            match &self.transport {
                Transport::Mounted(root) => {
                    let p = Self::local(root, path)?;
                    if p.is_dir() {
                        std::fs::remove_dir(p)?;
                    } else {
                        std::fs::remove_file(p)?;
                    }
                }
                Transport::Rust(session) => {
                    Self::request(
                        session,
                        if self.stat(path)?.is_dir {
                            "rmdir"
                        } else {
                            "delete"
                        },
                        path,
                    )?;
                }
            }
            Ok(())
        })
    }
    fn move_path(&self, source: &Path, destination: &Path) -> Result<()> {
        self.operation("rename", || {
            let from = relative_path(source)?;
            let to = relative_path(destination)?;
            ensure!(!from.is_empty() && !to.is_empty(), "Cannot move SMB root");
            match &self.transport {
                Transport::Mounted(root) => {
                    let from = Self::local(root, source)?;
                    let to = Self::local(root, destination)?;
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::rename(from, to)?;
                }
                Transport::Rust(session) => {
                    if let Some((parent, _)) = to.rsplit_once('/') {
                        Self::mkdir(session, parent)?;
                    }
                    session.request("rename", &from, &to, 0)?;
                }
            }
            Ok(())
        })
    }
}
