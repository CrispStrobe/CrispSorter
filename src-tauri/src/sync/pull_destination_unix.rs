//! Unix directory-relative operations for the shared manual-pull adapter.
use cap_fs_ext::DirExt;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions, OpenOptionsExt, PermissionsExt},
};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, Permissions},
    io,
    path::Path,
};

pub(crate) struct Directory {
    inner: Dir,
}
impl Directory {
    pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
        Ok(Self {
            inner: Dir::open_ambient_dir(path, ambient_authority())?,
        })
    }
    pub(crate) fn open_child(&self, name: &OsStr) -> io::Result<Self> {
        Ok(Self {
            inner: self.inner.open_dir_nofollow(name)?,
        })
    }
    pub(crate) fn create_child(&self, name: &OsStr) -> io::Result<Self> {
        self.inner.create_dir(name)?;
        self.open_child(name)
    }
    pub(crate) fn permissions(&self, name: &OsStr) -> io::Result<Option<Permissions>> {
        let metadata = match self.inner.symlink_metadata(name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.permissions().readonly()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pull destination must be a writable regular file without a symlink",
            ));
        }
        use std::os::unix::fs::PermissionsExt as _;
        Ok(Some(Permissions::from_mode(metadata.permissions().mode())))
    }
    pub(crate) fn create_file(&self, name: &OsStr) -> io::Result<StageFile> {
        // Capture cleanup authority before creation, then arm only after the
        // exclusive open succeeds. A collision never owns somebody else's file.
        let parent = self.inner.try_clone()?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true).mode(0o600);
        let file = parent.open_with(name, &options)?.into_std();
        Ok(StageFile {
            file,
            parent,
            name: name.to_owned(),
            cleanup_armed: true,
        })
    }
    pub(crate) fn rename_file(&self, stage: &mut StageFile, leaf: &OsStr) -> io::Result<()> {
        let result = stage.parent.rename(&stage.name, &self.inner, leaf);
        if result.is_ok() {
            stage.cleanup_armed = false;
        }
        result
    }
}

/// Unix cleanup remains name-relative to the held parent. Other writers allowed
/// to replace stage entries in that same directory are outside this boundary.
pub(crate) struct StageFile {
    file: File,
    parent: Dir,
    name: OsString,
    cleanup_armed: bool,
}
impl StageFile {
    pub(crate) fn as_file(&self) -> &File {
        &self.file
    }
    pub(crate) fn as_file_mut(&mut self) -> &mut File {
        &mut self.file
    }
}
impl Drop for StageFile {
    fn drop(&mut self) {
        if self.cleanup_armed {
            let _ = self.parent.remove_file(&self.name);
        }
    }
}
