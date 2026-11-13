//! Native Windows directory-object operations for manual pull destinations.
//!
//! Only the initial
//! explicitly selected root uses an ambient pathname. All later operations use
//! a retained directory/file handle plus a single filename. No reconstructed
//! pathname, current directory, or cap-std rename/cleanup fallback is used.
#![cfg(windows)]
use std::{ffi::OsStr, fs::{File, OpenOptions, Permissions}, io, mem::{self},
    os::windows::{ffi::OsStrExt, fs::{MetadataExt, OpenOptionsExt}, io::{AsRawHandle, FromRawHandle}},
    path::{Component, Path}, ptr};
use windows_sys::{
    Wdk::{Foundation::OBJECT_ATTRIBUTES, Storage::FileSystem::{
        NtCreateFile, NtSetInformationFile, FILE_CREATE, FILE_OPEN, FILE_DIRECTORY_FILE,
        FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        FILE_RENAME_INFORMATION, FILE_DISPOSITION_INFORMATION, FileRenameInformation,
        FileDispositionInformation,
    }},
    Win32::{Foundation::{HANDLE, NTSTATUS, RtlNtStatusToDosError, STATUS_PENDING, UNICODE_STRING, INVALID_HANDLE_VALUE, CloseHandle},
        Storage::FileSystem::{DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_SHARE_DELETE, FILE_READ_ATTRIBUTES, FILE_TRAVERSE,
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, SYNCHRONIZE},
        System::{IO::IO_STATUS_BLOCK, Threading::{WaitForSingleObject, INFINITE}},
    },
};
const SHARING: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const DIR_ACCESS: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

fn invalid(message: &'static str) -> io::Error { io::Error::new(io::ErrorKind::InvalidInput, message) }
fn status_result(status: NTSTATUS) -> io::Result<()> {
    if status >= 0 { Ok(()) } else {
        // SAFETY: conversion of a returned NTSTATUS does not dereference memory.
        Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32))
    }
}
fn completed(status: NTSTATUS, handle: HANDLE, ios: &mut IO_STATUS_BLOCK) -> io::Result<()> {
    if status != STATUS_PENDING { return status_result(status); }
    // Requests use synchronous handles, but keep buffers/IO_STATUS_BLOCK alive
    // until a pending operation finishes rather than treating PENDING as success.
    // SAFETY: handle remains owned by the caller and was opened for SYNCHRONIZE.
    let waited = unsafe { WaitForSingleObject(handle, INFINITE) };
    if waited != 0 { return Err(io::Error::last_os_error()); }
    // SAFETY: the operation has completed and initialized this status union.
    status_result(unsafe { ios.Anonymous.Status })
}
fn filename(name: &OsStr) -> io::Result<Vec<u16>> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(invalid("a single relative filename is required"));
    }
    let wide: Vec<_> = name.encode_wide().collect();
    if wide.is_empty() || wide.len() > 32767 || wide.iter().any(|c| matches!(*c, 0 | 47 | 58 | 92)) {
        return Err(invalid("a single relative filename is required"));
    }
    Ok(wide)
}

/// An owned directory object, not a reusable ambient pathname. Share deletion
/// permits relocation when the filesystem accepts it; an open descendant can
/// still block an ancestor move. Rooted operations always use this same object.
pub(crate) struct Directory { file: File }
impl Directory {
    /// Root aliases are deliberately resolved once at capture, matching the
    /// existing explicitly selected-root policy. Descendant aliases are refused.
    pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().access_mode(DIR_ACCESS).share_mode(SHARING)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)?;
        Self::checked(file)
    }
    fn checked(file: File) -> io::Result<Self> {
        let attrs = file.metadata()?.file_attributes();
        if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 || attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid("pull directory must be a directory without a reparse point"));
        }
        Ok(Self { file })
    }
    pub(crate) fn open_child(&self, name: &OsStr) -> io::Result<Self> {
        Self::checked(self.open(name, DIR_ACCESS, FILE_OPEN, FILE_DIRECTORY_FILE)?)
    }
    /// Create one ordinary missing descendant without requesting DELETE.
    /// A concurrent AlreadyExists can be recovered by open_child, never follow.
    pub(crate) fn create_child(&self, name: &OsStr) -> io::Result<Self> {
        Self::checked(self.open(name, DIR_ACCESS, FILE_CREATE, FILE_DIRECTORY_FILE)?)
    }
    pub(crate) fn permissions(&self, name: &OsStr) -> io::Result<Option<Permissions>> {
        let file = match self.open(name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, FILE_OPEN, FILE_NON_DIRECTORY_FILE) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || metadata.permissions().readonly() {
            return Err(invalid("pull destination must be a writable regular file without a reparse point"));
        }
        Ok(Some(metadata.permissions()))
    }
    pub(crate) fn create_file(&self, name: &OsStr) -> io::Result<StageFile> {
        let file = self.open(name, FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE, FILE_CREATE, FILE_NON_DIRECTORY_FILE)?;
        // Acquire cleanup ownership only AFTER exclusive creation succeeds.
        // A collision never constructs a guard for someone else's file.
        let stage = StageFile { file, cleanup_armed: true };
        let metadata = stage.file.metadata()?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid("new stage is not a regular file"));
        }
        Ok(stage)
    }
    fn open(&self, name: &OsStr, access: u32, disposition: u32, kind: u32) -> io::Result<File> {
        let mut name = filename(name)?;
        let length = u16::try_from(name.len() * 2).map_err(|_| invalid("filename is too long"))?;
        let mut string = UNICODE_STRING { Length: length, MaximumLength: length, Buffer: name.as_mut_ptr() };
        let attrs = OBJECT_ATTRIBUTES { Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: self.file.as_raw_handle() as HANDLE, ObjectName: &mut string,
            Attributes: 0x40, SecurityDescriptor: ptr::null_mut(), SecurityQualityOfService: ptr::null_mut() };
        let mut ios: IO_STATUS_BLOCK = unsafe { mem::zeroed() };
        let mut handle: HANDLE = ptr::null_mut();
        // SAFETY: all buffers have correct C layout and remain live until
        // completion. RootDirectory is owned for this call; name is one leaf.
        let status = unsafe { NtCreateFile(&mut handle, access, &attrs, &mut ios,
            ptr::null(), FILE_ATTRIBUTE_NORMAL, SHARING, disposition,
            kind | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            ptr::null(), 0) };
        if status < 0 { return status_result(status).map(|_| unreachable!()); }
        // NtCreateFile is requested synchronously. Check its out-handle and
        // finish any valid pending result BEFORE creating a Rust owned File.
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "native open returned no valid handle"));
        }
        if let Err(error) = completed(status, handle, &mut ios) {
            // SAFETY: a successful/pending open returned this validated owned
            // raw handle, and completion has been awaited before releasing it.
            unsafe { CloseHandle(handle); }
            return Err(error);
        }
        // SAFETY: validated, completed NtCreateFile transferred one owned handle.
        Ok(unsafe { File::from_raw_handle(handle as _) })
    }
    pub(crate) fn rename_file(&self, stage: &mut StageFile, leaf: &OsStr) -> io::Result<()> {
        let name = filename(leaf)?;
        let bytes = name.len() * 2;
        // usize storage provides pointer alignment. Reserve the entire C header
        // as well as the variable UTF-16 tail, including when the name is short.
        let allocation = mem::size_of::<FILE_RENAME_INFORMATION>() + bytes;
        let mut buffer = vec![0usize; allocation.div_ceil(mem::size_of::<usize>())];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        // SAFETY: buffer is aligned, initialized, and large enough for header
        // and full tail. No Rust references alias the raw writes/native call.
        unsafe {
            (*info).Anonymous.ReplaceIfExists = true;
            (*info).RootDirectory = self.file.as_raw_handle() as HANDLE;
            (*info).FileNameLength = bytes as u32;
            ptr::copy_nonoverlapping(name.as_ptr(), ptr::addr_of_mut!((*info).FileName).cast::<u16>(), name.len());
        }
        // After a remote rename loses its acknowledgement, this HANDLE may
        // already designate the published destination. Never delete it on Drop.
        // A confirmed pre-rename cancellation still has armed handle cleanup.
        stage.cleanup_armed = false;
        let result = set_information(&stage.file, info.cast(), (buffer.len() * mem::size_of::<usize>()) as u32, FileRenameInformation);
        #[cfg(test)]
        let result = RENAME_RESULT.with(|hook| match hook.borrow_mut().take() { Some(hook) => hook(result), None => result });
        result
    }
}
fn set_information(file: &File, data: *const core::ffi::c_void, length: u32, class: i32) -> io::Result<()> {
    let mut ios: IO_STATUS_BLOCK = unsafe { mem::zeroed() };
    // SAFETY: caller supplies an initialized buffer for this information class;
    // buffer, IO_STATUS_BLOCK and source handle stay live through completion.
    let status = unsafe { NtSetInformationFile(file.as_raw_handle() as HANDLE, &mut ios, data, length, class) };
    completed(status, file.as_raw_handle() as HANDLE, &mut ios)
}
fn mark_delete(file: &File) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    set_information(file, (&info as *const FILE_DISPOSITION_INFORMATION).cast(),
                    mem::size_of_val(&info) as u32, FileDispositionInformation)
}
/// Only an exclusively created stage owns this cleanup. Publication consumes
/// that authority before its first potentially committed native rename call.
pub(crate) struct StageFile { file: File, cleanup_armed: bool }
impl StageFile {
    pub(crate) fn as_file(&self) -> &File { &self.file }
    pub(crate) fn as_file_mut(&mut self) -> &mut File { &mut self.file }
}
impl Drop for StageFile {
    fn drop(&mut self) {
        if self.cleanup_armed { let _ = mark_delete(&self.file); }
    }
}
#[cfg(test)]
thread_local! {
    static RENAME_RESULT: std::cell::RefCell<Option<Box<dyn FnOnce(io::Result<()>) -> io::Result<()>>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write, path::PathBuf,
        sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
    use windows_sys::Win32::{Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_WRITE_ATTRIBUTES},
        System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_REPARSE_POINT}};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture { root: std::path::PathBuf, relocated: Option<std::path::PathBuf> }
    impl Fixture {
        fn new() -> Self {
            let base = std::env::var_os("CRISPSORTER_TEST_MOUNT_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
            let name = format!("crispsorter-pull-{}-{}-{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed));
            let root = base.join(name); fs::create_dir(&root).unwrap(); Self { root, relocated: None }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
            if let Some(path) = &self.relocated { let _ = fs::remove_dir_all(path); }
        }
    }
    fn write(stage: &mut StageFile, bytes: &[u8]) {
        stage.as_file_mut().write_all(bytes).unwrap(); stage.as_file().sync_all().unwrap();
    }
    fn parent(root: &Path) -> Directory { Directory::open_root(root).unwrap() }

    #[test]
    fn native_create_and_short_leaf_replace_keep_file_handle_open() {
        let f = Fixture::new(); let dir = parent(&f.root);
        fs::write(f.root.join("x"), b"old").unwrap();
        assert!(dir.permissions(OsStr::new("x")).unwrap().is_some());
        let mut stage = dir.create_file(OsStr::new("stage")).unwrap(); write(&mut stage, b"new");
        dir.rename_file(&mut stage, OsStr::new("x")).unwrap(); drop(stage);
        assert_eq!(fs::read(f.root.join("x")).unwrap(), b"new");
        assert!(!f.root.join("stage").exists());
        let child = dir.create_child(OsStr::new("nested")).unwrap();
        drop(child.create_file(OsStr::new("cancelled")).unwrap());
        assert!(!f.root.join("nested/cancelled").exists());
    }

    #[test]
    fn cancelled_stage_deletes_only_its_created_handle_and_collision_keeps_existing_file() {
        let f = Fixture::new(); let dir = parent(&f.root);
        fs::write(f.root.join("collision"), b"unrelated").unwrap();
        assert_eq!(dir.create_file(OsStr::new("collision")).err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(f.root.join("collision")).unwrap(), b"unrelated");
        let mut stage = dir.create_file(OsStr::new("cancelled")).unwrap(); write(&mut stage, b"private"); drop(stage);
        assert!(!f.root.join("cancelled").exists());
    }

    #[test]
    fn cleanup_follows_owned_stage_after_its_name_is_replaced() {
        let f = Fixture::new(); let dir = parent(&f.root);
        let mut stage = dir.create_file(OsStr::new("stage")).unwrap(); write(&mut stage, b"owned");
        fs::rename(f.root.join("stage"), f.root.join("moved-stage")).unwrap();
        fs::write(f.root.join("stage"), b"unrelated replacement").unwrap(); drop(stage);
        assert!(!f.root.join("moved-stage").exists());
        assert_eq!(fs::read(f.root.join("stage")).unwrap(), b"unrelated replacement");
    }

    #[test]
    fn confirmed_and_lost_acknowledgement_publication_never_delete_destination_on_drop() {
        for lost_ack in [false, true] {
            let f = Fixture::new(); let dir = parent(&f.root);
            let mut stage = dir.create_file(OsStr::new("stage")).unwrap(); write(&mut stage, b"published");
            if lost_ack {
                RENAME_RESULT.with(|hook| *hook.borrow_mut() = Some(Box::new(|result| {
                    result.unwrap(); Err(io::Error::other("injected lost acknowledgement"))
                })));
            }
            assert_eq!(dir.rename_file(&mut stage, OsStr::new("destination")).is_err(), lost_ack);
            fs::write(f.root.join("stage"), b"unrelated later file").unwrap(); drop(stage);
            assert_eq!(fs::read(f.root.join("destination")).unwrap(), b"published");
            assert_eq!(fs::read(f.root.join("stage")).unwrap(), b"unrelated later file");
        }
    }

    #[test]
    fn held_parent_and_root_follow_relocation_without_touching_replacements() {
        let mut f = Fixture::new(); let root = parent(&f.root);
        let dir = root.create_child(OsStr::new("parent")).unwrap();
        let moved_parent = f.root.join("moved-parent");
        fs::rename(f.root.join("parent"), &moved_parent).unwrap(); fs::create_dir(f.root.join("parent")).unwrap();
        let mut stage = dir.create_file(OsStr::new("stage")).unwrap(); write(&mut stage, b"original parent");
        dir.rename_file(&mut stage, OsStr::new("result")).unwrap(); drop(stage);
        assert_eq!(fs::read(moved_parent.join("result")).unwrap(), b"original parent");
        assert!(!f.root.join("parent/result").exists());
        let moved_root = f.root.with_extension("moved");
        // FILE_SHARE_DELETE permits moves but does not guarantee an ancestor
        // move while descendant handles remain open. NTFS refused this on the
        // native runner; other filesystems may accept it. Both outcomes must
        // preserve object authority, not silently select the former pathname.
        // https://learn.microsoft.com/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information
        let active_root = match fs::rename(&f.root, &moved_root) {
            Ok(()) => {
                f.relocated = Some(moved_root.clone());
                fs::create_dir(&f.root).unwrap();
                fs::write(f.root.join("result"), b"replacement root").unwrap();
                moved_root.clone()
            }
            Err(error) => {
                assert!(matches!(error.raw_os_error(), Some(5 | 32)), "unexpected move error: {error}");
                assert!(f.root.is_dir());
                assert!(!moved_root.exists());
                f.root.clone()
            }
        };
        assert_eq!(fs::read(active_root.join("moved-parent/result")).unwrap(), b"original parent");
        let mut stage = root.create_file(OsStr::new("after-move-attempt")).unwrap();
        write(&mut stage, b"still original root");
        root.rename_file(&mut stage, OsStr::new("result")).unwrap(); drop(stage);
        assert_eq!(fs::read(active_root.join("result")).unwrap(), b"still original root");
        assert_eq!(fs::read(active_root.join("moved-parent/result")).unwrap(), b"original parent");
        assert!(!active_root.join("parent/result").exists());
        if active_root != f.root {
            assert_eq!(fs::read(f.root.join("result")).unwrap(), b"replacement root");
        }
        drop(dir);
        drop(root);
    }

    #[test]
    fn held_root_without_open_descendants_follows_relocation_without_touching_replacement() {
        let mut f = Fixture::new(); let root = parent(&f.root);
        let moved_root = f.root.with_extension("moved");
        // Require actual relocation in this independent case, not a skipped
        // assertion following an OS refusal with a descendant still held.
        fs::rename(&f.root, &moved_root).unwrap();
        f.relocated = Some(moved_root.clone()); fs::create_dir(&f.root).unwrap();
        fs::write(f.root.join("result"), b"replacement root").unwrap();
        let mut stage = root.create_file(OsStr::new("stage")).unwrap(); write(&mut stage, b"original root");
        root.rename_file(&mut stage, OsStr::new("result")).unwrap(); drop(stage);
        assert_eq!(fs::read(moved_root.join("result")).unwrap(), b"original root");
        assert_eq!(fs::read(f.root.join("result")).unwrap(), b"replacement root");
    }

    fn junction(directory: &Path, target: &Path) -> io::Result<()> {
        // Attributes-only access is deliberately used: denying DELETE sharing
        // is not protection against this in-place namespace mutation.
        let file = OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).share_mode(SHARING)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(directory)?;
        let target = target.canonicalize()?;
        let wide: Vec<u16> = target.as_os_str().encode_wide().collect();
        let prefix: Vec<u16> = "\\\\?\\".encode_utf16().collect();
        if !wide.starts_with(&prefix) { return Err(invalid("test requires a canonical verbatim target")); }
        let substitute: Vec<u16> = "\\??\\".encode_utf16().chain(wide[4..].iter().copied()).collect();
        let path_bytes = (substitute.len() + 1) * 2;
        let mut buffer = vec![0u8; 16 + path_bytes + 2];
        buffer[0..4].copy_from_slice(&0xA0000003u32.to_le_bytes());
        buffer[4..6].copy_from_slice(&((8 + path_bytes + 2) as u16).to_le_bytes());
        buffer[10..12].copy_from_slice(&((substitute.len() * 2) as u16).to_le_bytes());
        buffer[12..14].copy_from_slice(&(path_bytes as u16).to_le_bytes());
        for (index, value) in substitute.iter().enumerate() { buffer[16 + index * 2..18 + index * 2].copy_from_slice(&value.to_le_bytes()); }
        let mut returned = 0;
        // SAFETY: valid exclusive test-owned directory handle and complete
        // REPARSE_DATA_BUFFER mount-point payload, synchronous request.
        if unsafe { DeviceIoControl(file.as_raw_handle() as HANDLE, FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(), buffer.len() as u32, ptr::null_mut(), 0, &mut returned, ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[test]
    fn junctions_are_rejected_for_descendants_and_exclusive_creation_never_follows_them() {
        let f = Fixture::new(); let root = parent(&f.root);
        fs::create_dir(f.root.join("outside")).unwrap(); fs::create_dir(f.root.join("link")).unwrap();
        junction(&f.root.join("link"), &f.root.join("outside")).unwrap();
        assert!(root.open_child(OsStr::new("link")).is_err());
        assert!(root.permissions(OsStr::new("link")).is_err());
        assert!(root.create_file(OsStr::new("link")).is_err());
        assert!(root.create_child(OsStr::new("link")).is_err());
        assert_eq!(fs::read_dir(f.root.join("outside")).unwrap().count(), 0);
    }

    #[test]
    fn reparse_mutation_after_capture_is_original_object_or_refusal_never_redirected() {
        for is_root in [false, true] {
            let f = Fixture::new();
            fs::create_dir(f.root.join("outside")).unwrap();
            fs::create_dir(f.root.join("captured")).unwrap();
            let original = parent(&f.root);
            let captured = if is_root { parent(&f.root.join("captured")) }
                else { original.open_child(OsStr::new("captured")).unwrap() };
            junction(&f.root.join("captured"), &f.root.join("outside")).unwrap();
            let _ = captured.create_child(OsStr::new("nested"));
            if let Ok(mut stage) = captured.create_file(OsStr::new("stage")) {
                write(&mut stage, b"authorized object");
                let _ = captured.rename_file(&mut stage, OsStr::new("result")); drop(stage);
            }
            assert_eq!(fs::read_dir(f.root.join("outside")).unwrap().count(), 0,
                "native rooted operation followed a mutated reparse point");
        }
    }

    #[test]
    fn exclusive_file_creation_does_not_follow_a_dangling_symlink() {
        let f = Fixture::new(); let root = parent(&f.root);
        let target = f.root.join("outside-target");
        std::os::windows::fs::symlink_file(&target, f.root.join("link")).unwrap();
        assert!(root.create_file(OsStr::new("link")).is_err());
        assert!(!target.exists());
        assert!(fs::symlink_metadata(f.root.join("link")).unwrap().file_type().is_symlink());
    }

    #[test]
    fn unsafe_names_and_readonly_leaf_are_rejected() {
        let f = Fixture::new(); let root = parent(&f.root);
        for name in ["", ".", "..", "../outside", "nested/file", "nested\\file", "file:stream", "C:\\outside"] {
            assert!(root.create_file(OsStr::new(name)).is_err(), "accepted unsafe leaf");
        }
        fs::write(f.root.join("readonly"), b"unchanged").unwrap();
        let mut permissions = fs::metadata(f.root.join("readonly")).unwrap().permissions(); permissions.set_readonly(true);
        fs::set_permissions(f.root.join("readonly"), permissions.clone()).unwrap();
        assert!(root.permissions(OsStr::new("readonly")).is_err());
        permissions.set_readonly(false); fs::set_permissions(f.root.join("readonly"), permissions).unwrap();
    }

    #[test]
    #[ignore = "requires an explicitly configured real UNC/share test root; not covered by cross-check"]
    fn unc_or_verbatim_root_uses_same_native_backend_when_mounted_fixture_is_supplied() {
        // Set CRISPSORTER_TEST_MOUNT_ROOT to a real UNC/share/mounted test root
        // in the Windows mounted-storage workflow. This is not a runtime gate.
        let root = std::env::var_os("CRISPSORTER_TEST_MOUNT_ROOT").expect("UNC fixture was not configured; this test has not run");
        let root = PathBuf::from(root);
        assert!(matches!(root.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::UNC(_, _) | std::path::Prefix::VerbatimUNC(_, _))),
            "a real UNC/share root is required for UNC coverage");
        let f = Fixture::new(); let directory = parent(&f.root);
        let mut stage = directory.create_file(OsStr::new("stage")).unwrap(); write(&mut stage, b"mounted");
        directory.rename_file(&mut stage, OsStr::new("x")).unwrap(); drop(stage);
        assert_eq!(fs::read(f.root.join("x")).unwrap(), b"mounted");
    }
}
