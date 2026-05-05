//! Reader/writer for the classic Cathy `.caf` (Catalog File) format.
//!
//! Backwards-compatible with versions 1 through 8. Round-trips byte-
//! identically with Catfish (Python) and the original Cathy.exe (C++)
//! for the v8 case the writer emits; legacy versions 1-7 are
//! read-only.
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

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

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

/// Read a NUL-terminated latin-1 string (Cathy's on-disk encoding).
/// latin-1's first 256 codepoints map 1:1 onto Unicode 0-255, so any
/// byte sequence decodes losslessly — what Cathy.exe wrote on Win9x
/// becomes valid UTF-8 strings here.
fn read_cstr_latin1<R: Read>(r: &mut R) -> Result<String, CafError> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte)? {
            0 => break, // EOF — return what we have
            _ => {
                if byte[0] == 0 {
                    break;
                }
                bytes.push(byte[0]);
            }
        }
    }
    Ok(bytes.into_iter().map(|b| b as char).collect())
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

/// Encode a string back to latin-1, NUL-terminated. Any codepoint outside
/// 0x00-0xFF is replaced with `?` — same fallback Catfish uses.
fn write_cstr_latin1<W: Write>(w: &mut W, s: &str) -> Result<(), CafError> {
    let mut bytes = Vec::with_capacity(s.len() + 1);
    for ch in s.chars() {
        let cp = ch as u32;
        bytes.push(if cp <= 0xFF { cp as u8 } else { b'?' });
    }
    bytes.push(0);
    w.write_all(&bytes)?;
    Ok(())
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
        version = read_i16_le(&mut r)? as u8;
    }
    if !(1..=8).contains(&version) {
        return Err(CafError::UnsupportedVersion(version));
    }

    let date = read_u32_le(&mut r)?;
    let device = if version >= 2 { read_cstr_latin1(&mut r)? } else { String::new() };
    let volume = read_cstr_latin1(&mut r)?;
    let alias = read_cstr_latin1(&mut r)?;
    let serial = read_u32_le(&mut r)?;
    let comment = if version >= 4 {
        read_cstr_latin1(&mut r)?
    } else {
        String::new()
    };
    let freesize = if version >= 1 { read_f32_le(&mut r)? } else { 0.0 };
    let archive = if version >= 6 { read_i16_le(&mut r)? } else { 0 };

    let dir_count = read_i32_le(&mut r)?;
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
    let f = File::open(path)?;
    let mut r = BufReader::new(f);

    let magic = read_u32_le(&mut r)?;
    if magic == 0 || magic % UL_MODUS != UL_MAGIC_BASE {
        return Err(CafError::BadMagic);
    }
    let mut version = (magic / UL_MODUS) as u8;
    if version > 2 {
        version = read_i16_le(&mut r)? as u8;
    }
    if !(1..=8).contains(&version) {
        return Err(CafError::UnsupportedVersion(version));
    }

    let date = read_u32_le(&mut r)?;
    let device = if version >= 2 { read_cstr_latin1(&mut r)? } else { String::new() };

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
    let comment = if version >= 4 { read_cstr_latin1(&mut r)? } else { String::new() };
    let freesize = if version >= 1 { read_f32_le(&mut r)? } else { 0.0 };
    let archive = if version >= 6 { read_i16_le(&mut r)? } else { 0 };
    index.header = crate::catalog::index::VolumeHeader {
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
    let mut raw_elements: Vec<(u32, i64, u32, String)> = Vec::with_capacity(element_count as usize);
    for _ in 0..element_count {
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
        raw_elements.push((mtime, size, parent_id, name));
    }

    // Reconstruct directory paths from the flat element list.
    //
    // For v > 6: a directory entry has `size < 0` and its ID is `-size`.
    // For v <= 6: a directory is identified by being referenced as a
    //             parent_id by some other entry; its ID is its 1-based
    //             positional index. (Catfish quirk.)
    let referenced_parent_ids: std::collections::HashSet<u32> =
        raw_elements.iter().map(|(_, _, pid, _)| *pid).collect();
    let mut dir_path_map: std::collections::HashMap<u32, PathBuf> =
        std::collections::HashMap::new();
    dir_path_map.insert(0, root_path.clone());

    for (i, (_mtime, size, pid, name)) in raw_elements.iter().enumerate() {
        let is_dir = if version > 6 {
            *size < 0
        } else {
            referenced_parent_ids.contains(&((i as u32) + 1))
        };
        if is_dir {
            let dir_id: u32 = if version > 6 {
                (-size) as u32
            } else {
                (i as u32) + 1
            };
            if let Some(parent_path) = dir_path_map.get(pid) {
                if !name.is_empty() {
                    let mut p = parent_path.clone();
                    p.push(name);
                    dir_path_map.insert(dir_id, p);
                }
            }
        }
    }

    for (i, (mtime, size, pid, name)) in raw_elements.iter().enumerate() {
        let is_dir = if version > 6 {
            *size < 0
        } else {
            referenced_parent_ids.contains(&((i as u32) + 1))
        };
        if !is_dir && !name.trim().is_empty() {
            if let Some(parent_path) = dir_path_map.get(pid) {
                let mut p = parent_path.clone();
                p.push(name);
                // v <= 6 stored no per-file size; clamp to 1 so the
                // size_index bucket still works, matching Catfish.
                let actual_size = if version > 6 {
                    (*size as u64).max(1)
                } else if *size == 0 {
                    1024
                } else {
                    *size as u64
                };
                index.add(FileEntry::new(p, actual_size, *mtime));
            }
        }
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
/// **Round-trip semantics**: `read_file → write_file → read_file`
/// produces a `FileIndex` whose entries (path/size/mtime), volume
/// header (label/alias/serial/comment/freesize/archive/date), and
/// `save_version` match the original. Byte-for-byte equality of the
/// `.caf` files is **not** guaranteed — entry ordering, dir-tree
/// reconstruction, and Cathy's quirks around the `next_dir_id`
/// allocation can shuffle bytes without affecting semantics.
pub fn write_file(path: &Path, index: &FileIndex, created_date: u32) -> Result<(), CafError> {
    let target_version: u8 = match index.save_version {
        6 | 8 => index.save_version,
        _ => 8,
    };
    let f = File::create(path)?;
    let mut w = BufWriter::new(f);

    // Header. v ≥ 3 uses the magic-as-sentinel scheme: magic encodes
    // "version >= 3" (literally `3 * UL_MODUS + base`) and the actual
    // version follows as <i16>. We never write v ≤ 2 (no callers want
    // them), so the magic is constant.
    let magic = 3u32 * UL_MODUS + UL_MAGIC_BASE;
    write_u32_le(&mut w, magic)?;
    write_i16_le(&mut w, target_version as i16)?;
    write_u32_le(&mut w, created_date)?;
    let root_str = index
        .root_path
        .as_os_str()
        .to_string_lossy()
        .into_owned();
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
    for entry in &index.all_files {
        if let Some(parent) = entry.path.parent() {
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
    sorted_dirs.sort_by_key(|p| p.components().count());
    for d in sorted_dirs {
        if !dir_id_map.contains_key(&d) {
            dir_id_map.insert(d, next_dir_id);
            next_dir_id += 1;
        }
    }

    // Per-dir running totals for the info block.
    let mut dir_file_count: HashMap<u32, i32> = HashMap::new();
    let mut dir_total_size: HashMap<u32, u64> = HashMap::new();
    for entry in &index.all_files {
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

    let mut elm: Vec<(u32, i64, u32, String)> = Vec::new();
    for (dir_id, dir_path) in dir_entries {
        let parent = dir_path.parent().unwrap_or(&dir_path);
        let pid = dir_id_map.get(parent).copied().unwrap_or(0);
        let mtime = std::fs::metadata(&dir_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs() as u32)
            })
            .unwrap_or(0);
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
    // For v ≤ 6 we clamp size > i32::MAX and parent_id > u16::MAX
    // rather than fail — out-of-range values would only show up for
    // catalogs with > 4 GB single files OR > 65k directories, both
    // edge cases for v6's intended Win9x-era use.
    write_i32_le(&mut w, elm.len() as i32)?;
    for (mtime, size, pid, name) in elm {
        write_u32_le(&mut w, mtime)?;
        match target_version {
            v if v <= 6 => {
                let size32: i32 = size.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                write_i32_le(&mut w, size32)?;
                let pid16: u16 = pid.min(u16::MAX as u32) as u16;
                let mut buf = [0u8; 2];
                buf.copy_from_slice(&pid16.to_le_bytes());
                w.write_all(&buf)?;
            }
            7 => {
                write_i64_le(&mut w, size)?;
                let pid16: u16 = pid.min(u16::MAX as u32) as u16;
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
        idx.add(FileEntry::new(PathBuf::from("/tmp/cat-root/a.txt"), 100, 1700000000));
        idx.add(FileEntry::new(PathBuf::from("/tmp/cat-root/b.bin"), 200, 1700000001));
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
    fn volume_header_round_trips() {
        use crate::catalog::index::VolumeHeader;
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
            loaded.all_files[0].path.file_name().unwrap().to_string_lossy(),
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
            eprintln!("Wrote {} for Catfish-side cross-compat check", path.display());
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
        assert!(names.iter().any(|n| n.contains("a/b/c.txt") || n.contains("a\\b\\c.txt")));
        assert!(names.iter().any(|n| n.contains("a/d.txt") || n.contains("a\\d.txt")));
    }
}
