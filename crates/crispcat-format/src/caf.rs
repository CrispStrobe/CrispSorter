// Shared catalogue adaptation: preserve explicit directories/timestamps and legacy unknown sizes; see the crate README.
//! Reader/writer for the classic Cathy `.caf` (Catalog File) format.
//!
//! Reads versions 1 through 8 and writes v6 or v8. Round trips preserve
//! supported metadata; entry ordering and reconstructed directory IDs need
//! not be byte-identical. v7/v8 file sizes include genuine zero-byte files.
//!
//! ## Format reference
//!
//! Header:
//! ```text
//! <L>      magic = version * 1_000_000_000 + 500_410_407
//!          (version 1-2 stops here; version >= 3 reads <i16> saveVersion next)
//! <L>      date (unix epoch seconds)
//! NUL str  device path (>= v2)
//! NUL str  volume label
//! NUL str  alias
//! <L>      serial
//! NUL str  comment (>= v4)
//! <f32>    freesize (>= v1)
//! <i16>    archive flag (>= v6)
//! ```
//!
//! Info block:
//! ```text
//! <i32>    dir_count
//! foreach dir:
//!   if first dir or version <= 3:
//!     NUL str dir name (writer always emits an empty string for the root)
//!   if version >= 3:
//!     <i32> file_count
//!     <f64> total_size
//! ```
//!
//! Element block (the actual file/dir list):
//! ```text
//! <i32>    file_count
//! foreach element:
//!   <L>          mtime
//!   <i32|i64>    size  (i32 for v <= 6; i64 for v >= 7)
//!                 negative ⇒ this is a directory entry, its ID is -size
//!                 (for v > 6) or its 1-based positional index (for v <= 6)
//!   <u16|u32>    parent_id (u16 for v <= 7; u32 for v == 8)
//!   NUL str      name
//! ```
//!
//! All multi-byte ints are little-endian. All strings are latin-1
//! encoded and NUL-terminated — that's how Cathy.exe wrote them on
//! Win9x, and we preserve that for round-trip parity.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::index::{FileEntry, FileIndex};

/// Magic number constants from the original Cathy implementation.
///
/// Reading: `version = magic / UL_MODUS`, validate `magic % UL_MODUS == UL_MAGIC_BASE`.
/// Writing: `magic = version * UL_MODUS + UL_MAGIC_BASE`. The writer always
/// uses `version = 3` here so the on-disk magic matches versions 3-8 (the
/// real version is encoded in the next `<i16>` for v ≥ 3).
pub const UL_MAGIC_BASE: u32 = 500_410_407;
pub const UL_MODUS: u32 = 1_000_000_000;

/// Newest version this writer produces. Matches Catfish's `saveVersion = 8`.
pub const SAVE_VERSION: i16 = 8;

/// Cheap header-only summary read for index listings — skips the body.
#[derive(Debug, Clone)]
pub struct CafMetadata {
    pub version: u8,
    pub device: String,
    pub volume: String,
    pub alias: String,
    pub serial: u32,
    pub comment: String,
    pub date: u32,
    pub file_count: i32,
    pub total_size: u64,
    pub archive: i16,
    pub freesize: f32,
}

/// All errors the .caf reader/writer can surface.
#[derive(Debug)]
pub enum CafError {
    Io(io::Error),
    /// The file's magic number doesn't match Cathy's algebraic check.
    BadMagic,
    /// Version field is outside the supported 1..=8 range.
    UnsupportedVersion(u8),
    /// Body read hit EOF before the declared element count.
    Truncated,
    /// Metadata cannot be represented faithfully by the selected CAF version.
    InvalidData(String),
}

impl From<io::Error> for CafError {
    fn from(e: io::Error) -> Self {
        CafError::Io(e)
    }
}

impl std::fmt::Display for CafError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CafError::Io(e) => write!(f, "I/O error: {e}"),
            CafError::BadMagic => write!(f, "not a valid .caf file (bad magic)"),
            CafError::UnsupportedVersion(v) => {
                write!(f, "unsupported .caf version {v} (supported: 1..=8)")
            }
            CafError::Truncated => write!(f, "truncated .caf file"),
            CafError::InvalidData(message) => write!(f, "invalid .caf metadata: {message}"),
        }
    }
}

impl std::error::Error for CafError {}

// ── Low-level primitive readers ──────────────────────────────────────────

fn read_u32_le<R: Read>(r: &mut R) -> Result<u32, CafError> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_i16_le<R: Read>(r: &mut R) -> Result<i16, CafError> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(i16::from_le_bytes(buf))
}

fn read_u16_le<R: Read>(r: &mut R) -> Result<u16, CafError> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_i32_le<R: Read>(r: &mut R) -> Result<i32, CafError> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

fn read_i64_le<R: Read>(r: &mut R) -> Result<i64, CafError> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(i64::from_le_bytes(buf))
}

fn read_f32_le<R: Read>(r: &mut R) -> Result<f32, CafError> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(f32::from_le_bytes(buf))
}

fn read_f64_le<R: Read>(r: &mut R) -> Result<f64, CafError> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(f64::from_le_bytes(buf))
}

/// Read a NUL-terminated string. Cathy writes Latin-1, whose first 256
/// codepoints map 1:1 onto Unicode 0-255, so any byte sequence decodes
/// losslessly. Strings that Latin-1 cannot represent (CJK, emoji, macOS
/// decomposed umlauts) are written by [`write_cstr_latin1`] as UTF-8; a
/// string whose high bytes form valid UTF-8 is therefore decoded as UTF-8.
fn read_cstr_latin1<R: Read>(r: &mut R) -> Result<String, CafError> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte)? {
            0 => return Err(CafError::Truncated),
            _ => {
                if byte[0] == 0 {
                    break;
                }
                bytes.push(byte[0]);
            }
        }
    }
    Ok(decode_cstr(bytes))
}

fn decode_cstr(bytes: Vec<u8>) -> String {
    if bytes.iter().any(|byte| *byte >= 0x80) {
        if let Ok(text) = std::str::from_utf8(&bytes) {
            return text.to_owned();
        }
    }
    bytes.into_iter().map(|b| b as char).collect()
}

fn skip<R: Read>(r: &mut R, n: usize) -> Result<(), CafError> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(())
}

// ── Low-level primitive writers ──────────────────────────────────────────

fn write_u32_le<W: Write>(w: &mut W, v: u32) -> Result<(), CafError> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}

fn write_i16_le<W: Write>(w: &mut W, v: i16) -> Result<(), CafError> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}

fn write_u32_le_as<W: Write>(w: &mut W, v: u32) -> Result<(), CafError> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}

fn write_i32_le<W: Write>(w: &mut W, v: i32) -> Result<(), CafError> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}

fn write_i64_le<W: Write>(w: &mut W, v: i64) -> Result<(), CafError> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}

fn write_f32_le<W: Write>(w: &mut W, v: f32) -> Result<(), CafError> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}

fn write_f64_le<W: Write>(w: &mut W, v: f64) -> Result<(), CafError> {
    w.write_all(&v.to_le_bytes())?;
    Ok(())
}

/// Encode a string exactly. Latin-1-representable strings use Cathy's
/// Latin-1 bytes. Anything else is written as UTF-8 rather than replaced
/// with `?`: substitution would create a different path on reload and could
/// collapse distinct names (`日本.txt`, `中国.txt`) into duplicates that the
/// reader rejects. An embedded NUL would corrupt later fields.
fn write_cstr_latin1<W: Write>(w: &mut W, s: &str) -> Result<(), CafError> {
    validate_latin1(s)?;
    let mut bytes = Vec::with_capacity(s.len() + 1);
    if s.chars().all(|ch| (ch as u32) <= 0xff) {
        bytes.extend(s.chars().map(|ch| ch as u8));
    } else {
        bytes.extend_from_slice(s.as_bytes());
    }
    bytes.push(0);
    w.write_all(&bytes)?;
    Ok(())
}

/// Every string must survive a write/read cycle unchanged: no embedded NUL,
/// and no Latin-1 string whose bytes would be re-read as different UTF-8
/// text (only possible for already-garbled names such as `Ã¤`).
fn validate_latin1(value: &str) -> Result<(), CafError> {
    if value.contains('\0') {
        return Err(CafError::InvalidData(
            "CAF strings cannot contain embedded NUL characters".into(),
        ));
    }
    if value.chars().all(|ch| (ch as u32) <= 0xff) {
        let bytes: Vec<u8> = value.chars().map(|ch| ch as u8).collect();
        if decode_cstr(bytes) != value {
            return Err(CafError::InvalidData(format!(
                "Latin-1 string would be read back as different UTF-8 text: {value:?}"
            )));
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), CafError> {
    validate_latin1(name)?;
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\']) {
        return Err(CafError::InvalidData(
            "a CAF entry needs one unambiguous path component".into(),
        ));
    }
    Ok(())
}

fn validate_export(
    index: &FileIndex,
    target_version: u8,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), CafError> {
    let root = index
        .root_path
        .to_str()
        .ok_or_else(|| CafError::InvalidData("the root path is not valid Unicode".into()))?;
    validate_latin1(root)?;
    if root.is_empty()
        || index
            .root_path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(CafError::InvalidData(
            "the root path must not contain '..'".into(),
        ));
    }
    for text in [
        &index.header.label,
        &index.header.alias,
        &index.header.comment,
    ] {
        validate_latin1(text)?;
    }
    if index.all_files.len() > i32::MAX as usize {
        return Err(CafError::InvalidData(
            "too many file entries for CAF".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for entry in &index.all_files {
        checkpoint(cancelled)?;
        let text = entry
            .path
            .to_str()
            .ok_or_else(|| CafError::InvalidData("a file path is not valid Unicode".into()))?;
        validate_latin1(text)?;
        let relative = entry.path.strip_prefix(&index.root_path).map_err(|_| {
            CafError::InvalidData(format!(
                "file is outside the catalog root: {}",
                entry.path.display()
            ))
        })?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || entry.path.file_name().is_none()
        {
            return Err(CafError::InvalidData(format!(
                "invalid child path: {}",
                entry.path.display()
            )));
        }
        for part in relative.components() {
            validate_name(part.as_os_str().to_str().expect("path was validated above"))?;
        }
        if !seen.insert(entry.path.as_path()) {
            return Err(CafError::InvalidData(format!(
                "duplicate file path: {}",
                entry.path.display()
            )));
        }
        let largest = if target_version <= 6 {
            i32::MAX as u64
        } else {
            i64::MAX as u64
        };
        if entry.size > largest {
            return Err(CafError::InvalidData(format!(
                "file size exceeds CAF v{target_version}: {}",
                entry.path.display()
            )));
        }
        total = total.checked_add(entry.size).ok_or_else(|| {
            CafError::InvalidData("total file size exceeds the supported integer range".into())
        })?;
    }
    for entry in &index.all_files {
        checkpoint(cancelled)?;
        for ancestor in entry.path.ancestors().skip(1) {
            if ancestor == index.root_path {
                break;
            }
            if seen.contains(ancestor) {
                return Err(CafError::InvalidData(format!(
                    "a path cannot be both a file and a directory: {}",
                    ancestor.display()
                )));
            }
        }
    }
    Ok(())
}

/// Removes an unpublished sibling file after any serialization/flush/rename
/// failure. The destination is never opened or truncated during staging.
struct PendingCatalog {
    path: PathBuf,
    published: bool,
}

impl Drop for PendingCatalog {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn create_pending_catalog(destination: &Path) -> Result<(PendingCatalog, File), CafError> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if destination.file_name().is_none() {
        return Err(CafError::InvalidData(
            "the catalog destination needs a filename".into(),
        ));
    }
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for _ in 0..32 {
        let number = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".crispcat-{}-{epoch}-{number}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => {
                return Ok((
                    PendingCatalog {
                        path,
                        published: false,
                    },
                    file,
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a temporary catalog",
    )
    .into())
}

// ── High-level API ───────────────────────────────────────────────────────

/// Parse just the header — fast path for index-listing UIs that need
/// device/volume/file_count without paying for the body decode.
pub fn read_metadata(path: &Path) -> Result<CafMetadata, CafError> {
    let f = File::open(path)?;
    let mut r = BufReader::new(f);

    let magic = read_u32_le(&mut r)?;
    if magic == 0 || magic % UL_MODUS != UL_MAGIC_BASE {
        return Err(CafError::BadMagic);
    }
    let mut version = (magic / UL_MODUS) as u8;
    if version > 2 {
        let stored_version = read_i16_le(&mut r)?;
        if !(1..=8).contains(&stored_version) {
            return Err(CafError::InvalidData(format!(
                "unsupported CAF version {stored_version}"
            )));
        }
        version = stored_version as u8;
    }
    if !(1..=8).contains(&version) {
        return Err(CafError::UnsupportedVersion(version));
    }

    let date = read_u32_le(&mut r)?;
    let device = if version >= 2 {
        read_cstr_latin1(&mut r)?
    } else {
        String::new()
    };
    let volume = read_cstr_latin1(&mut r)?;
    let alias = read_cstr_latin1(&mut r)?;
    let serial = read_u32_le(&mut r)?;
    let comment = if version >= 4 {
        read_cstr_latin1(&mut r)?
    } else {
        String::new()
    };
    let freesize = if version >= 1 {
        read_f32_le(&mut r)?
    } else {
        0.0
    };
    let archive = if version >= 6 {
        read_i16_le(&mut r)?
    } else {
        0
    };

    let dir_count = read_i32_le(&mut r)?;
    if dir_count < 0 {
        return Err(CafError::InvalidData("negative directory count".into()));
    }
    let mut file_count = 0i32;
    let mut total_size = 0u64;
    if dir_count > 0 {
        // Root dir: name string for v >= 3 (always; for v <= 3 it's
        // also the only string written), then per-dir 12-byte stats
        // for v >= 3. Catfish's writer puts an empty string for v8.
        read_cstr_latin1(&mut r)?;
        if version >= 3 {
            file_count = read_i32_le(&mut r)?;
            total_size = read_f64_le(&mut r)? as u64;
        }
    }

    Ok(CafMetadata {
        version,
        device,
        volume,
        alias,
        serial,
        comment,
        date,
        file_count,
        total_size,
        archive,
        freesize,
    })
}

/// Read a complete .caf into a `FileIndex`. Cost is proportional to
/// the catalog size; for huge catalogs prefer `read_metadata` first to
/// decide whether the full read is worth it.
pub fn read_file(path: &Path) -> Result<FileIndex, CafError> {
    read_file_cancellable(path, &|| false)
}
struct ControlledReader<'a, R> {
    inner: R,
    cancelled: &'a dyn Fn() -> bool,
}
impl<R: Read> Read for ControlledReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if (self.cancelled)() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Catalogue read stopped",
            ));
        }
        self.inner.read(buffer)
    }
}
fn checkpoint(cancelled: &dyn Fn() -> bool) -> Result<(), CafError> {
    if cancelled() {
        return Err(
            io::Error::new(io::ErrorKind::Interrupted, "Catalogue operation stopped").into(),
        );
    }
    Ok(())
}
pub fn read_file_cancellable(
    path: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<FileIndex, CafError> {
    let f = File::open(path)?;
    let file_length = f.metadata()?.len();
    let mut r = BufReader::new(ControlledReader {
        inner: f,
        cancelled,
    });

    let magic = read_u32_le(&mut r)?;
    if magic == 0 || magic % UL_MODUS != UL_MAGIC_BASE {
        return Err(CafError::BadMagic);
    }
    let mut version = (magic / UL_MODUS) as u8;
    if version > 2 {
        let stored_version = read_i16_le(&mut r)?;
        if !(1..=8).contains(&stored_version) {
            return Err(CafError::InvalidData(format!(
                "unsupported CAF version {stored_version}"
            )));
        }
        version = stored_version as u8;
    }
    if !(1..=8).contains(&version) {
        return Err(CafError::UnsupportedVersion(version));
    }

    let date = read_u32_le(&mut r)?;
    let device = if version >= 2 {
        read_cstr_latin1(&mut r)?
    } else {
        String::new()
    };

    // Detect Windows-style paths from the device string. Cathy.exe
    // wrote drive letters ("C:\") and UNC paths; Catfish-on-Linux
    // writes POSIX. The flag drives how we reconstruct child paths
    // below — without it a Windows .caf opened on macOS would lose
    // the backslashes.
    let is_windows_path =
        device.contains('\\') || (device.len() > 1 && device.as_bytes().get(1) == Some(&b':'));
    let root_path = PathBuf::from(&device);
    let mut index = FileIndex::new(root_path.clone(), is_windows_path);

    // Volume metadata — preserved through round-trip since v0.1.36
    // (see LEARNINGS.md "Catalog (.caf)"). Earlier versions discarded
    // these, which silently dropped them on save.
    let label = read_cstr_latin1(&mut r)?;
    let alias = read_cstr_latin1(&mut r)?;
    let serial = read_u32_le(&mut r)?;
    let comment = if version >= 4 {
        read_cstr_latin1(&mut r)?
    } else {
        String::new()
    };
    let freesize = if version >= 1 {
        read_f32_le(&mut r)?
    } else {
        0.0
    };
    let archive = if version >= 6 {
        read_i16_le(&mut r)?
    } else {
        0
    };
    index.header = crate::index::VolumeHeader {
        label,
        alias,
        serial,
        comment,
        freesize,
        archive,
        date,
    };
    index.save_version = version;

    // Info block.
    let dir_count = read_i32_le(&mut r)?;
    if dir_count < 0 || dir_count as u64 > file_length {
        return Err(CafError::InvalidData("invalid directory count".into()));
    }
    for i in 0..dir_count {
        if i == 0 || version <= 3 {
            read_cstr_latin1(&mut r)?; // dir name (or empty for root in v8)
        }
        if version >= 3 {
            skip(&mut r, 12)?; // <i32 file_count, f64 total_size>
        }
    }

    // ELM block — the actual entries.
    let element_count = read_i32_le(&mut r)?;
    let minimum_record_bytes = if version <= 6 {
        11
    } else if version == 7 {
        15
    } else {
        17
    };
    if element_count < 0 || element_count as u64 > file_length / minimum_record_bytes {
        return Err(CafError::InvalidData(
            "invalid or truncated element count".into(),
        ));
    }
    let mut raw_elements: Vec<(u32, i64, u32, String)> = Vec::with_capacity(element_count as usize);
    for _ in 0..element_count {
        checkpoint(cancelled)?;
        let mtime = read_u32_le(&mut r).map_err(|_| CafError::Truncated)?;
        let size: i64 = if version <= 6 {
            read_i32_le(&mut r)? as i64
        } else {
            read_i64_le(&mut r)?
        };
        let parent_id: u32 = if version <= 7 {
            read_u16_le(&mut r)? as u32
        } else {
            read_u32_le(&mut r)?
        };
        let name = read_cstr_latin1(&mut r)?;
        validate_name(&name)?;
        raw_elements.push((mtime, size, parent_id, name));
    }

    // Modern CAF directory IDs are independent of entry order. Resolve the
    // graph from root once, so forward references work and an orphan/cycle
    // cannot silently turn a damaged catalog into an apparently complete one.
    use std::collections::{HashMap, HashSet};
    let referenced_parent_ids: HashSet<u32> =
        raw_elements.iter().map(|(_, _, pid, _)| *pid).collect();
    let mut directories: HashMap<u32, (u32, &str)> = HashMap::new();
    let mut files = Vec::new();
    let mut directory_mtimes = HashMap::new();
    for (i, (mtime, size, pid, name)) in raw_elements.iter().enumerate() {
        checkpoint(cancelled)?;
        let is_dir = if version > 6 {
            *size < 0
        } else {
            referenced_parent_ids.contains(&((i as u32) + 1))
        };
        if is_dir {
            let id = if version > 6 {
                size.checked_neg()
                    .and_then(|id| u32::try_from(id).ok())
                    .ok_or_else(|| CafError::InvalidData("directory ID exceeds CAF range".into()))?
            } else {
                i as u32 + 1
            };
            directory_mtimes.insert(id, *mtime);
            if directories.insert(id, (*pid, name)).is_some() {
                return Err(CafError::InvalidData("duplicate directory ID".into()));
            }
        } else {
            let actual_size = if version <= 6 && *size == 0 {
                1024 // Preserve the legacy unknown-size convention only in v1-v6.
            } else {
                u64::try_from(*size)
                    .map_err(|_| CafError::InvalidData("negative file size".into()))?
            };
            files.push((*mtime, actual_size, *pid, name, version <= 6 && *size == 0));
        }
    }
    let mut children: HashMap<u32, Vec<(u32, &str)>> = HashMap::new();
    for (&id, &(parent, name)) in &directories {
        if parent != 0 && !directories.contains_key(&parent) {
            return Err(CafError::InvalidData("unknown directory parent".into()));
        }
        children.entry(parent).or_default().push((id, name));
    }
    let mut dir_paths = HashMap::from([(0, root_path.clone())]);
    let mut seen_paths = HashSet::from([root_path]);
    let mut pending = vec![0];
    while let Some(parent) = pending.pop() {
        checkpoint(cancelled)?;
        for &(id, name) in children.get(&parent).into_iter().flatten() {
            let path = dir_paths[&parent].join(name);
            if !seen_paths.insert(path.clone()) {
                return Err(CafError::InvalidData("duplicate catalog path".into()));
            }
            dir_paths.insert(id, path);
            pending.push(id);
        }
    }
    if dir_paths.len() != directories.len() + 1 {
        return Err(CafError::InvalidData("cyclic directory hierarchy".into()));
    }
    for (&id, path) in &dir_paths {
        if id != 0 {
            index
                .directories
                .push((path.clone(), directory_mtimes[&id]));
        }
    }
    for (mtime, size, parent, name, unknown_size) in files {
        checkpoint(cancelled)?;
        let directory = dir_paths
            .get(&parent)
            .ok_or_else(|| CafError::InvalidData("unknown file parent".into()))?;
        let path = directory.join(name);
        if !seen_paths.insert(path.clone()) {
            return Err(CafError::InvalidData("duplicate catalog path".into()));
        }
        if unknown_size {
            index.unknown_size_paths.push(path.clone());
        }
        index.add(FileEntry::new(path, size, mtime));
    }

    Ok(index)
}

/// Serialize a `FileIndex` to a `.caf`. Picks the on-disk format from
/// `index.save_version` — v6 for legacy Cathy.exe compatibility, v8
/// for current Catfish. Anything outside `{6, 8}` is clamped to v8.
///
/// `created_date` overrides `index.header.date` (so callers that want
/// "stamp this catalog with NOW" don't have to mutate the header
/// first). Pass `index.header.date` to preserve the original value.
///
/// Supported file paths, byte sizes and second-resolution mtimes survive
/// modern CAF round trips. The shared adaptation additionally retains
/// explicit directory entries/timestamps; v1-v6 keep their historical
/// zero-size compatibility convention and expose unknown-size provenance.
/// Formats other than v6/v8 are upgraded to v8 on write. Empty labels/aliases
/// are populated from the root name, as for a fresh scan.
///
/// Serialization is staged beside the destination, flushed, and renamed only
/// after it succeeds. Invalid metadata or a failed write leaves the previous
/// catalog untouched. Classic CAF requires NUL-free Latin-1 names.
pub fn write_file(path: &Path, index: &FileIndex, created_date: u32) -> Result<(), CafError> {
    write_file_cancellable(path, index, created_date, &|| false)
}
pub fn write_file_cancellable(
    path: &Path,
    index: &FileIndex,
    created_date: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), CafError> {
    let target_version: u8 = match index.save_version {
        6 | 8 => index.save_version,
        _ => 8,
    };
    validate_export(index, target_version, cancelled)?;
    let file_paths: std::collections::HashSet<_> =
        index.all_files.iter().map(|e| &e.path).collect();
    let mut explicit = std::collections::HashSet::new();
    for (directory, _) in &index.directories {
        let relative = directory
            .strip_prefix(&index.root_path)
            .map_err(|_| CafError::InvalidData("directory outside root".into()))?;
        if relative.as_os_str().is_empty() || !explicit.insert(directory) {
            return Err(CafError::InvalidData(
                "duplicate or root directory entry".into(),
            ));
        }
        for part in relative.components() {
            if !matches!(part, Component::Normal(_)) {
                return Err(CafError::InvalidData("invalid directory path".into()));
            }
            validate_name(
                part.as_os_str()
                    .to_str()
                    .ok_or_else(|| CafError::InvalidData("non-Unicode directory name".into()))?,
            )?;
        }
        if directory
            .ancestors()
            .any(|ancestor| file_paths.contains(&ancestor.to_path_buf()))
        {
            return Err(CafError::InvalidData("file/directory collision".into()));
        }
    }
    let (mut pending, mut file) = create_pending_catalog(path)?;
    write_contents(&mut file, index, created_date, target_version, cancelled)?;
    // Retain an existing regular catalog's permissions when replacing it.
    // Inspect the directory entry itself instead of following a destination link.
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => file.set_permissions(metadata.permissions())?,
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    file.sync_all()?;
    // Closing before rename also permits replacement on Windows.
    drop(file);
    checkpoint(cancelled)?;
    std::fs::rename(&pending.path, path)?;
    pending.published = true;
    Ok(())
}

fn write_contents(
    file: &mut File,
    index: &FileIndex,
    created_date: u32,
    target_version: u8,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), CafError> {
    let mut w = BufWriter::new(file);

    // Header. v ≥ 3 uses the magic-as-sentinel scheme: magic encodes
    // "version >= 3" (literally `3 * UL_MODUS + base`) and the actual
    // version follows as <i16>. We never write v ≤ 2 (no callers want
    // them), so the magic is constant.
    let magic = 3u32 * UL_MODUS + UL_MAGIC_BASE;
    write_u32_le(&mut w, magic)?;
    write_i16_le(&mut w, target_version as i16)?;
    write_u32_le(&mut w, created_date)?;
    let root_str = index.root_path.as_os_str().to_string_lossy().into_owned();
    write_cstr_latin1(&mut w, &root_str)?;
    // Volume header — round-tripped from the `.caf` we read, or the
    // user's chosen values for fresh scans. Falls back to root-folder
    // name only when `label`/`alias` are blank (matches Catfish's
    // behaviour for catalogs created without explicit labels).
    let root_name = index
        .root_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = if index.header.label.is_empty() {
        root_name.clone()
    } else {
        index.header.label.clone()
    };
    let alias = if index.header.alias.is_empty() {
        root_name
    } else {
        index.header.alias.clone()
    };
    write_cstr_latin1(&mut w, &label)?;
    write_cstr_latin1(&mut w, &alias)?;
    write_u32_le_as(&mut w, index.header.serial)?;
    if target_version >= 4 {
        write_cstr_latin1(&mut w, &index.header.comment)?;
    }
    if target_version >= 1 {
        write_f32_le(&mut w, index.header.freesize)?;
    }
    if target_version >= 6 {
        write_i16_le(&mut w, index.header.archive)?;
    }

    // Build the directory ID map. Root is always ID 0; every distinct
    // parent dir of an entry gets a fresh ID. Sort by depth so parent
    // IDs are always allocated before their children — keeps the
    // resulting elm list trivially topologically valid.
    use std::collections::HashMap;
    let mut dir_id_map: HashMap<PathBuf, u32> = HashMap::new();
    dir_id_map.insert(index.root_path.clone(), 0);
    let mut next_dir_id: u32 = 1;

    let mut all_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for directory in &index.directories {
        all_dirs.insert(directory.0.clone());
    }
    for entry_path in index
        .all_files
        .iter()
        .map(|e| &e.path)
        .chain(index.directories.iter().map(|e| &e.0))
    {
        if let Some(parent) = entry_path.parent() {
            // Walk up to the root, registering every intermediate dir.
            let mut p = parent.to_path_buf();
            loop {
                if dir_id_map.contains_key(&p) || all_dirs.contains(&p) {
                    break;
                }
                all_dirs.insert(p.clone());
                match p.parent() {
                    Some(pp) if pp != p => p = pp.to_path_buf(),
                    _ => break,
                }
            }
        }
    }
    let mut sorted_dirs: Vec<PathBuf> = all_dirs.into_iter().collect();
    sorted_dirs.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });
    for d in sorted_dirs {
        if !dir_id_map.contains_key(&d) {
            dir_id_map.insert(d, next_dir_id);
            next_dir_id += 1;
        }
    }
    if next_dir_id > i32::MAX as u32 || (target_version <= 7 && next_dir_id > u16::MAX as u32 + 1) {
        return Err(CafError::InvalidData(format!(
            "too many directories for CAF v{target_version}"
        )));
    }

    // Per-dir running totals for the info block.
    let mut dir_file_count: HashMap<u32, i32> = HashMap::new();
    let mut dir_total_size: HashMap<u32, u64> = HashMap::new();
    for entry in &index.all_files {
        checkpoint(cancelled)?;
        if let Some(parent) = entry.path.parent() {
            if let Some(&pid) = dir_id_map.get(parent) {
                *dir_file_count.entry(pid).or_insert(0) += 1;
                *dir_total_size.entry(pid).or_insert(0) += entry.size;
            }
        }
    }

    // Build the elm list: directories first, then files. Order within
    // dirs is by dir_id ascending — load-bearing for v ≤ 6 because the
    // reader resolves dir IDs from the **positional index** of the
    // entry, not from a stored field. Sorting means position 0 → dir_id
    // 1, position 1 → dir_id 2, etc.; children's `parent_id` then
    // references those positions correctly. For v ≥ 7 the dir_id is
    // stored as `-size` so position doesn't matter — but sorting is
    // still cheap and produces deterministic output.
    let mut dir_entries: Vec<(u32, PathBuf)> = dir_id_map
        .iter()
        .filter(|(_, &id)| id != 0)
        .map(|(p, &id)| (id, p.clone()))
        .collect();
    dir_entries.sort_by_key(|(id, _)| *id);

    let directory_times: HashMap<_, _> = index
        .directories
        .iter()
        .map(|(path, time)| (path, *time))
        .collect();
    let mut elm: Vec<(u32, i64, u32, String)> = Vec::new();
    for (dir_id, dir_path) in dir_entries {
        let parent = dir_path.parent().unwrap_or(&dir_path);
        let pid = dir_id_map.get(parent).copied().unwrap_or(0);
        let mtime = directory_times.get(&dir_path).copied().unwrap_or_else(|| {
            std::fs::metadata(&dir_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs() as u32)
                })
                .unwrap_or(0)
        });
        let name = dir_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // v ≥ 7 encodes dir-ness as `size < 0` with id = `-size`. v ≤ 6
        // identifies dirs purely by "is this row's 1-based position
        // referenced as some other row's parent_id?" — so the size
        // field is meaningless for v6 dirs; we write 0 (the value
        // Catfish writes too, easy to spot in a hex dump).
        let dir_size: i64 = if target_version >= 7 {
            -(dir_id as i64)
        } else {
            0
        };
        elm.push((mtime, dir_size, pid, name));
    }
    for entry in &index.all_files {
        checkpoint(cancelled)?;
        let parent = entry.path.parent();
        let pid = parent.and_then(|p| dir_id_map.get(p).copied()).unwrap_or(0);
        let name = entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        elm.push((entry.mtime, entry.size as i64, pid, name));
    }

    // Info block. Catfish's layout: <i32> dir_count, then for each dir
    // the empty name string (only for i == 0) plus <i32 file_count, f64
    // total_size>. Root counts the aggregate.
    let total_files: i32 = index.all_files.len() as i32;
    let total_size: u64 = index.total_size();
    write_i32_le(&mut w, next_dir_id as i32)?;
    for i in 0..next_dir_id {
        if i == 0 {
            write_cstr_latin1(&mut w, "")?;
            write_i32_le(&mut w, total_files)?;
            write_f64_le(&mut w, total_size as f64)?;
        } else {
            let fc = dir_file_count.get(&i).copied().unwrap_or(0);
            let ts = dir_total_size.get(&i).copied().unwrap_or(0);
            write_i32_le(&mut w, fc)?;
            write_f64_le(&mut w, ts as f64)?;
        }
    }

    // ELM block. Per-entry struct widths track on-disk format
    // (mirrors the reader's symmetric branch in `read_file`):
    //   * v ≤ 6: <L> mtime, <l> size32, <H> parent_id16
    //   * v = 7:  <L> mtime, <q> size64, <H> parent_id16
    //   * v = 8:  <L> mtime, <q> size64, <L> parent_id32
    //
    // Values have been checked before publication. Never clamp a large file
    // or parent ID into different metadata; callers can explicitly choose v8.
    if elm.len() > i32::MAX as usize {
        return Err(CafError::InvalidData(
            "too many total elements for CAF".into(),
        ));
    }
    write_i32_le(&mut w, elm.len() as i32)?;
    for (mtime, size, pid, name) in elm {
        checkpoint(cancelled)?;
        write_u32_le(&mut w, mtime)?;
        match target_version {
            v if v <= 6 => {
                let size32: i32 = size as i32;
                write_i32_le(&mut w, size32)?;
                let pid16: u16 = pid as u16;
                let mut buf = [0u8; 2];
                buf.copy_from_slice(&pid16.to_le_bytes());
                w.write_all(&buf)?;
            }
            7 => {
                write_i64_le(&mut w, size)?;
                let pid16: u16 = pid as u16;
                let mut buf = [0u8; 2];
                buf.copy_from_slice(&pid16.to_le_bytes());
                w.write_all(&buf)?;
            }
            _ => {
                write_i64_le(&mut w, size)?;
                write_u32_le_as(&mut w, pid)?;
            }
        }
        write_cstr_latin1(&mut w, &name)?;
    }

    w.flush()?;
    Ok(())
}

/// Convenience: unix epoch seconds, clamped to u32.
pub fn unix_now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_index() -> FileIndex {
        let mut idx = FileIndex::new(PathBuf::from("/tmp/cat-root"), false);
        idx.add(FileEntry::new(
            PathBuf::from("/tmp/cat-root/a.txt"),
            100,
            1700000000,
        ));
        idx.add(FileEntry::new(
            PathBuf::from("/tmp/cat-root/b.bin"),
            200,
            1700000001,
        ));
        idx.add(FileEntry::new(
            PathBuf::from("/tmp/cat-root/sub/c.dat"),
            300,
            1700000002,
        ));
        idx
    }

    /// **Semantic** round-trip — asserts entries survive
    /// load → save → load with `(file_name, size, mtime)` preserved.
    /// Does NOT assert byte-for-byte equality of the `.caf` files;
    /// see `LEARNINGS.md` "Catalog (.caf)" for why that's
    /// deliberately not a guarantee. For volume-header preservation,
    /// see `volume_header_round_trips`. For v6 emit, see
    /// `round_trip_v6_struct_widths`.
    #[test]
    fn round_trip_v8_preserves_files() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.caf");
        let idx = fake_index();
        write_file(&path, &idx, 1700000000).unwrap();
        let loaded = read_file(&path).unwrap();

        // The reader resolves entry paths through the dir tree; loaded
        // entries should match by (file_name, size, mtime).
        assert_eq!(loaded.len(), idx.len());
        let mut want: Vec<_> = idx
            .all_files
            .iter()
            .map(|e| (e.path.file_name().unwrap().to_owned(), e.size, e.mtime))
            .collect();
        let mut got: Vec<_> = loaded
            .all_files
            .iter()
            .map(|e| (e.path.file_name().unwrap().to_owned(), e.size, e.mtime))
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want);
        // Save version is v8 by default for fresh indexes.
        assert_eq!(loaded.save_version, 8);
    }

    #[test]
    fn zero_byte_files_round_trip_with_full_paths_and_size_buckets() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("zero.caf");
        let root = tmp.path().join("source");
        let mut index = FileIndex::new(root.clone(), cfg!(windows));
        for (relative, size) in [("empty.txt", 0), ("a/b/empty.txt", 0), ("a/data", 7)] {
            index.add(FileEntry::new(root.join(relative), size, 1_700_000_000));
        }
        write_file(&path, &index, 1_700_000_001).unwrap();
        let loaded = read_file(&path).unwrap();
        let mut expected: Vec<_> = index
            .all_files
            .iter()
            .map(|entry| (&entry.path, entry.size, entry.mtime))
            .collect();
        let mut actual: Vec<_> = loaded
            .all_files
            .iter()
            .map(|entry| (&entry.path, entry.size, entry.mtime))
            .collect();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
        assert_eq!(loaded.by_size(0).count(), 2);
        assert_eq!(loaded.total_size(), 7);
        assert_eq!(read_metadata(&path).unwrap().total_size, 7);
    }

    #[test]
    fn v7_zero_size_is_exact_and_v6_legacy_fallback_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        // A small hand-written fixture exercises the v7 reader independently
        // of our writer, which intentionally only emits v6 and v8.
        for (version, expected_size) in [(6, 1024), (7, 0)] {
            let path = tmp.path().join(format!("v{version}.caf"));
            let mut bytes = Vec::new();
            write_u32_le(&mut bytes, 3 * UL_MODUS + UL_MAGIC_BASE).unwrap();
            write_i16_le(&mut bytes, version).unwrap();
            write_u32_le(&mut bytes, 1_700_000_000).unwrap();
            write_cstr_latin1(&mut bytes, "/catalog-root").unwrap();
            write_cstr_latin1(&mut bytes, "fixture").unwrap();
            write_cstr_latin1(&mut bytes, "").unwrap();
            write_u32_le(&mut bytes, 0).unwrap();
            write_cstr_latin1(&mut bytes, "").unwrap();
            write_f32_le(&mut bytes, 0.0).unwrap();
            write_i16_le(&mut bytes, 0).unwrap();
            write_i32_le(&mut bytes, 1).unwrap();
            write_cstr_latin1(&mut bytes, "").unwrap();
            write_i32_le(&mut bytes, 1).unwrap();
            write_f64_le(&mut bytes, 0.0).unwrap();
            write_i32_le(&mut bytes, 1).unwrap();
            write_u32_le(&mut bytes, 1_700_000_000).unwrap();
            if version <= 6 {
                write_i32_le(&mut bytes, 0).unwrap();
            } else {
                write_i64_le(&mut bytes, 0).unwrap();
            }
            bytes.extend_from_slice(&0u16.to_le_bytes());
            write_cstr_latin1(&mut bytes, "empty.txt").unwrap();
            std::fs::write(&path, bytes).unwrap();
            let loaded = read_file(&path).unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded.all_files[0].size, expected_size);
            assert_eq!(
                loaded.all_files[0].path,
                PathBuf::from("/catalog-root/empty.txt")
            );
        }
    }

    #[test]
    fn volume_header_round_trips() {
        use crate::index::VolumeHeader;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hdr.caf");
        let mut idx = fake_index();
        idx.header = VolumeHeader {
            label: "Archive".to_owned(),
            alias: "ARC".to_owned(),
            serial: 0xDEADBEEF,
            comment: "Captured 2026-05".to_owned(),
            freesize: 1.5e10,
            archive: 1,
            date: 1700000000,
        };
        write_file(&path, &idx, 1700000000).unwrap();
        let loaded = read_file(&path).unwrap();
        assert_eq!(loaded.header.label, "Archive");
        assert_eq!(loaded.header.alias, "ARC");
        assert_eq!(loaded.header.serial, 0xDEADBEEF);
        assert_eq!(loaded.header.comment, "Captured 2026-05");
        assert!((loaded.header.freesize - 1.5e10).abs() < 1.0);
        assert_eq!(loaded.header.archive, 1);
        assert_eq!(loaded.header.date, 1700000000);
    }

    #[test]
    fn round_trip_v6_struct_widths() {
        // v6 uses i32 size + u16 parent_id, identifies dirs by
        // positional index. This exercises the writer's v6 branch and
        // confirms the existing reader handles what the writer emits.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("v6.caf");
        let mut idx = fake_index();
        idx.save_version = 6;
        write_file(&path, &idx, 1700000000).unwrap();
        let loaded = read_file(&path).unwrap();
        assert_eq!(loaded.save_version, 6, "v6 magic should round-trip");
        assert_eq!(loaded.len(), idx.len(), "all files survive v6 round-trip");
        let mut want: Vec<_> = idx
            .all_files
            .iter()
            .map(|e| (e.path.file_name().unwrap().to_owned(), e.size, e.mtime))
            .collect();
        let mut got: Vec<_> = loaded
            .all_files
            .iter()
            .map(|e| (e.path.file_name().unwrap().to_owned(), e.size, e.mtime))
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn metadata_matches_full_read() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.caf");
        let idx = fake_index();
        write_file(&path, &idx, 1700000000).unwrap();
        let meta = read_metadata(&path).unwrap();
        assert_eq!(meta.version, 8);
        assert_eq!(meta.file_count, idx.len() as i32);
        assert_eq!(meta.total_size, idx.total_size());
        assert_eq!(meta.date, 1700000000);
    }

    #[test]
    fn bad_magic_returns_bad_magic() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("not-a-caf.bin");
        std::fs::write(&path, b"\x00\x00\x00\x00garbage").unwrap();
        assert!(matches!(read_file(&path), Err(CafError::BadMagic)));
    }

    #[test]
    fn non_latin1_names_round_trip_exactly_as_utf8() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("unicode.caf");
        let root = PathBuf::from("/tmp/Bücher-雪");
        let mut idx = FileIndex::new(root.clone(), false);
        idx.header.label = "Archiv 雪".into();
        idx.header.comment = "Notiz 📚".into();
        let names = [
            "日本.txt",
            "中国.txt", // would collide with 日本.txt under `?` substitution
            "emoji-📚.pdf",
            "Bu\u{308}cher.pdf", // macOS NFD: "u" + COMBINING DIAERESIS
            "Bücher.pdf",        // NFC twin stays distinct
            "Ümläut.txt",        // Latin-1 representable
        ];
        for (i, name) in names.iter().enumerate() {
            idx.add(FileEntry::new(
                root.join("sub-ü").join(name),
                i as u64,
                1_700_000_000,
            ));
        }
        for version in [6u8, 7, 8] {
            idx.save_version = version;
            write_file(&path, &idx, 1_700_000_000).unwrap();
            let loaded = read_file(&path).unwrap();
            assert_eq!(loaded.root_path, root, "v{version}");
            assert_eq!(loaded.header.label, "Archiv 雪", "v{version}");
            assert_eq!(loaded.header.comment, "Notiz 📚", "v{version}");
            let mut got: Vec<_> = loaded.all_files.iter().map(|f| f.path.clone()).collect();
            let mut want: Vec<_> = names.iter().map(|n| root.join("sub-ü").join(n)).collect();
            got.sort();
            want.sort();
            assert_eq!(got, want, "v{version}");
        }
    }

    #[test]
    fn latin1_strings_keep_cathy_single_byte_encoding() {
        let mut bytes = Vec::new();
        write_cstr_latin1(&mut bytes, "Ümläut").unwrap();
        assert_eq!(bytes, b"\xdcml\xe4ut\0");
        let mut bytes = Vec::new();
        write_cstr_latin1(&mut bytes, "雪").unwrap();
        assert_eq!(bytes, "雪\0".as_bytes());
        // Arbitrary legacy bytes that are not UTF-8 still decode as Latin-1.
        assert_eq!(
            read_cstr_latin1(&mut &b"\xe4\xff\0"[..]).unwrap(),
            "\u{e4}\u{ff}"
        );
    }

    #[test]
    fn latin1_roundtrip_preserves_high_bytes() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("latin1.caf");
        let mut idx = FileIndex::new(PathBuf::from("/tmp/cat-root"), false);
        // U+00E4 (ä) → 0xE4 in latin-1 — survives the round-trip.
        idx.add(FileEntry::new(
            PathBuf::from("/tmp/cat-root/Ümläut.txt"),
            42,
            1700000099,
        ));
        write_file(&path, &idx, 1700000000).unwrap();
        let loaded = read_file(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.all_files[0]
                .path
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "Ümläut.txt"
        );
    }

    #[test]
    fn cross_compat_writes_for_catfish_to_read() {
        // Side-effect-only: emit a .caf to /tmp/rust-written.caf so an
        // adjacent Catfish call (run by hand) can verify it reads back.
        // The unit test passes whenever the write succeeds — the
        // Catfish-side check is operator-driven for now (one Python
        // subprocess away from being automated, but skipping to keep
        // the suite hermetic).
        let path = std::path::PathBuf::from("/tmp/rust-written.caf");
        let mut idx = FileIndex::new(PathBuf::from("/tmp/rust-source"), false);
        idx.add(FileEntry::new(
            PathBuf::from("/tmp/rust-source/from-rust.txt"),
            123,
            1700000000,
        ));
        idx.add(FileEntry::new(
            PathBuf::from("/tmp/rust-source/sub/nested.bin"),
            456,
            1700000001,
        ));
        if super::write_file(&path, &idx, 1700000000).is_ok() {
            eprintln!(
                "Wrote {} for Catfish-side cross-compat check",
                path.display()
            );
        }
    }

    #[test]
    fn cross_compat_reads_catfish_fixture() {
        // Ad-hoc cross-compatibility check against a real Catfish-produced
        // .caf if one exists on disk. Skipped silently when not present.
        // Generate with:
        //   cd ../Catfish && python3 -c "import sys, types
        //   sys.modules['tkinter'] = types.ModuleType('tkinter')
        //   sys.modules['tkinter.font'] = types.ModuleType('tkinter.font')
        //   sys.path.insert(0, '.')
        //   from pathlib import Path
        //   from core.file_index import FileIndex
        //   root = Path('/tmp/catfish-fixture').resolve()
        //   idx = FileIndex(root, use_hash=False, hash_algo='md5')
        //   for p in root.rglob('*'):
        //       if p.is_file(): idx.add_file(p)
        //   idx.save_to_caf(Path('/tmp/catfish-fixture.caf'))"
        let path = std::path::Path::new("/tmp/catfish-fixture.caf");
        if !path.exists() {
            eprintln!("skipping: fixture not at {}", path.display());
            return;
        }
        let idx = super::read_file(path).expect("Catfish-written .caf should parse");
        // Catfish fixture has 2 files (a.txt + sub/b.bin).
        assert_eq!(idx.len(), 2);
        let names: std::collections::HashSet<String> = idx
            .all_files
            .iter()
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains("a.txt"));
        assert!(names.contains("b.bin"));
    }

    #[test]
    fn directories_round_trip_via_negative_size() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("dirs.caf");
        let mut idx = FileIndex::new(PathBuf::from("/cat-root"), false);
        // Files in nested subdirs — the writer must allocate dir IDs.
        idx.add(FileEntry::new(
            PathBuf::from("/cat-root/a/b/c.txt"),
            10,
            1700000000,
        ));
        idx.add(FileEntry::new(
            PathBuf::from("/cat-root/a/d.txt"),
            20,
            1700000001,
        ));
        write_file(&path, &idx, 1700000000).unwrap();
        let loaded = read_file(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        let names: std::collections::HashSet<_> = loaded
            .all_files
            .iter()
            .map(|e| e.path.to_string_lossy().into_owned())
            .collect();
        assert!(names
            .iter()
            .any(|n| n.contains("a/b/c.txt") || n.contains("a\\b\\c.txt")));
        assert!(names
            .iter()
            .any(|n| n.contains("a/d.txt") || n.contains("a\\d.txt")));
    }

    fn independent_v8(entries: &[(i64, u32, &str)]) -> (Vec<u8>, usize, usize) {
        let mut bytes = 3_500_410_407u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&8i16.to_le_bytes());
        bytes.extend_from_slice(&1_700_000_000u32.to_le_bytes());
        bytes.extend_from_slice(b"/offline/root\0Volume\0Alias\0");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"fixture\0");
        bytes.extend_from_slice(&0f32.to_le_bytes());
        bytes.extend_from_slice(&0i16.to_le_bytes());
        let dir_count_offset = bytes.len();
        bytes.extend_from_slice(&1i32.to_le_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&(entries.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&0f64.to_le_bytes());
        let element_count_offset = bytes.len();
        bytes.extend_from_slice(&(entries.len() as i32).to_le_bytes());
        for &(size, parent, name) in entries {
            bytes.extend_from_slice(&1_700_000_001u32.to_le_bytes());
            bytes.extend_from_slice(&size.to_le_bytes());
            bytes.extend_from_slice(&parent.to_le_bytes());
            bytes.extend(name.chars().map(|character| character as u8));
            bytes.push(0);
        }
        (bytes, dir_count_offset, element_count_offset)
    }

    fn assert_no_staged_catalogs(directory: &Path) {
        assert!(!std::fs::read_dir(directory).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".crispcat-")
        }));
    }

    #[test]
    fn invalid_exports_preserve_the_previous_catalog() {
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("previous.caf");
        write_file(&destination, &fake_index(), 1_700_000_000).unwrap();
        let original = std::fs::read(&destination).unwrap();
        for case in 0..10 {
            let mut index = fake_index();
            match case {
                // Latin-1 bytes C3 A4 would be re-read as UTF-8 "ä".
                0 => index.all_files[0].path = index.root_path.join("garbled-\u{c3}\u{a4}"),
                1 => index.header.comment = "bad\0comment".into(),
                2 => index.all_files[0].path = index.root_path.join("../outside"),
                3 => index.all_files[0].path = PathBuf::from("/somewhere-else/file"),
                4 => index.all_files[0].size = i64::MAX as u64 + 1,
                5 => index.add(index.all_files[0].clone()),
                6 => {
                    index.save_version = 6;
                    index.all_files[0].size = i32::MAX as u64 + 1;
                }
                7 => index.header.label = "\u{c3}\u{a4}".into(),
                8 => index.all_files[0].path = index.root_path.join("bad\0name"),
                9 => {
                    index.all_files[0].path = index.root_path.join("a");
                    index.all_files[1].path = index.root_path.join("a/b");
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    write_file(&destination, &index, 0),
                    Err(CafError::InvalidData(_))
                ),
                "case {case}"
            );
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                original,
                "case {case}"
            );
            assert_no_staged_catalogs(tmp.path());
        }
        assert_eq!(read_file(&destination).unwrap().len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_paths_are_rejected_without_lossy_replacement() {
        use std::os::unix::ffi::OsStringExt;
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("previous.caf");
        std::fs::write(&destination, b"previous").unwrap();
        let mut index = fake_index();
        index.all_files[0].path = index
            .root_path
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        assert!(matches!(
            write_file(&destination, &index, 0),
            Err(CafError::InvalidData(_))
        ));
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous");
        assert_no_staged_catalogs(tmp.path());
    }

    #[test]
    fn successful_replacement_publishes_a_complete_catalog() {
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("catalog.caf");
        let mut index = fake_index();
        write_file(&destination, &index, 10).unwrap();
        index.add(FileEntry::new(index.root_path.join("extra/zero"), 0, 123));
        write_file(&destination, &index, 20).unwrap();
        let loaded = read_file(&destination).unwrap();
        assert_eq!(loaded.all_files, index.all_files);
        assert_eq!(loaded.header.date, 20);
        assert_no_staged_catalogs(tmp.path());
    }

    #[cfg(unix)]
    #[test]
    fn replacement_retains_existing_regular_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("catalog.caf");
        write_file(&destination, &fake_index(), 10).unwrap();
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_file(&destination, &fake_index(), 20).unwrap();
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }

    #[test]
    fn failed_rename_preserves_destination_and_removes_staged_file() {
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("existing-directory");
        std::fs::create_dir(&destination).unwrap();
        let sentinel = destination.join("keep");
        std::fs::write(&sentinel, b"keep this").unwrap();
        assert!(write_file(&destination, &fake_index(), 10).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep this");
        assert_no_staged_catalogs(tmp.path());
    }

    #[test]
    fn independent_forward_parents_and_whitespace_names_are_preserved() {
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("forward.caf");
        let (bytes, _, _) = independent_v8(&[(-2, 1, "child"), (0, 2, " "), (-1, 0, "café")]);
        std::fs::write(&destination, bytes).unwrap();
        let loaded = read_file(&destination).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.all_files[0].path,
            PathBuf::from("/offline/root/café/child/ ")
        );
        assert_eq!(loaded.all_files[0].size, 0);
        assert_eq!(loaded.all_files[0].mtime, 1_700_000_001);
    }

    #[test]
    fn invalid_directory_graphs_and_names_are_rejected_as_a_whole() {
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("bad.caf");
        let cases = [
            vec![(1, 99, "orphan")],
            vec![(-1, 99, "orphan-directory")],
            vec![(-1, 2, "cycle-a"), (-2, 1, "cycle-b")],
            vec![(-1, 0, "first"), (-1, 0, "duplicate-id")],
            vec![(i64::MIN, 0, "overflow")],
            vec![(-(u32::MAX as i64 + 1), 0, "large-id")],
            vec![(1, 0, "../escape")],
            vec![(1, 0, "back\\slash")],
            vec![(1, 0, "same"), (2, 0, "same")],
            vec![(-1, 0, "same"), (2, 0, "same")],
        ];
        for (case, entries) in cases.iter().enumerate() {
            let (bytes, _, _) = independent_v8(entries);
            std::fs::write(&destination, bytes).unwrap();
            assert!(
                matches!(read_file(&destination), Err(CafError::InvalidData(_))),
                "case {case}"
            );
        }
    }

    #[test]
    fn corrupt_counts_and_unterminated_names_fail_without_panicking() {
        let tmp = TempDir::new().unwrap();
        let destination = tmp.path().join("truncated.caf");
        let (bytes, dirs_offset, entries_offset) = independent_v8(&[(0, 0, "last")]);
        for offset in [dirs_offset, entries_offset] {
            for count in [-1, i32::MAX] {
                let mut corrupt = bytes.clone();
                corrupt[offset..offset + 4].copy_from_slice(&count.to_le_bytes());
                std::fs::write(&destination, corrupt).unwrap();
                assert!(read_file(&destination).is_err());
                if offset == dirs_offset && count < 0 {
                    assert!(read_metadata(&destination).is_err());
                }
            }
        }
        std::fs::write(&destination, &bytes[..bytes.len() - 1]).unwrap();
        assert!(matches!(read_file(&destination), Err(CafError::Truncated)));
    }
}
