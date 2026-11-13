//! Manual pull destinations retain opened directory objects through queue waits.
//! Moves follow the original directory object. This protects ancestry, not
//! hostile same-user edits inside held directories. Windows needs its own
//! handle-relative backend; no pathname fallback is implemented here.
use anyhow::{ensure, Context, Result};
use cap_fs_ext::DirExt;
use cap_std::{
    ambient_authority,
    fs::{Dir, DirBuilder, OpenOptions, Permissions},
};
use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    sync::Arc,
};

struct HeldDir {
    dir: Dir,
}
impl HeldDir {
    fn child(&self, name: &str, create: bool) -> Result<Arc<Self>> {
        let dir = match self.dir.open_dir_nofollow(name) {
            Ok(dir) => dir,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                self.create_dir(name, false)?;
                self.dir.open_dir_nofollow(name)?
            }
            Err(error) => return Err(error).context("open pull directory without following links"),
        };
        Ok(Arc::new(Self { dir }))
    }
    fn create_dir(&self, name: &str, private: bool) -> Result<()> {
        use cap_std::fs::DirBuilderExt;
        let mut builder = DirBuilder::new();
        builder.mode(if private { 0o700 } else { 0o777 });
        match self.dir.create_dir_with(name, &builder) {
            Ok(()) => Ok(()),
            Err(error) if !private && error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error).context("create pull directory"),
        }
    }
}

/// Capture once for the entire pull, before an asynchronous queue wait.
pub(crate) struct PullRoot {
    ancestry: Vec<Arc<HeldDir>>,
}
impl PullRoot {
    pub(crate) fn capture(path: &Path) -> Result<Arc<Self>> {
        // Resolve the explicitly configured root (including an intended alias)
        // once. Every later operation uses this directory object, not its path.
        let dir =
            Dir::open_ambient_dir(path, ambient_authority()).context("open selected pull root")?;
        Ok(Arc::new(Self {
            ancestry: vec![Arc::new(HeldDir { dir })],
        }))
    }
    pub(crate) fn destination(&self, relative: &str) -> Result<Arc<PullDestination>> {
        let parts: Vec<_> = relative.split('/').collect();
        for part in &parts {
            crate::sync::paths::validate_component(part)?;
        }
        let mut ancestry = self.ancestry.clone();
        for part in &parts[..parts.len() - 1] {
            ancestry.push(ancestry.last().unwrap().child(part, true)?);
        }
        let target = Arc::new(PullDestination {
            ancestry,
            leaf: parts.last().unwrap().to_string(),
        });
        target.permissions()?;
        Ok(target)
    }
}

pub(crate) struct PullDestination {
    ancestry: Vec<Arc<HeldDir>>,
    leaf: String,
}
impl PullDestination {
    fn parent(&self) -> &HeldDir {
        self.ancestry.last().unwrap()
    }
    fn permissions(&self) -> Result<Option<Permissions>> {
        match self.parent().dir.symlink_metadata(&self.leaf) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "pull destination is not a regular file"
                );
                ensure!(
                    !metadata.permissions().readonly(),
                    "pull destination is read-only"
                );
                Ok(Some(metadata.permissions()))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("inspect held pull destination"),
        }
    }
    pub(crate) fn stage(
        self: &Arc<Self>,
        size: Option<u64>,
        write: impl FnOnce(&mut dyn Write) -> Result<u64>,
    ) -> Result<PullStage> {
        let name = format!(".crispsorter-pull-{}", uuid::Uuid::new_v4());
        self.stage_named(name, size, write)
    }
    fn stage_named(
        self: &Arc<Self>,
        name: String,
        size: Option<u64>,
        write: impl FnOnce(&mut dyn Write) -> Result<u64>,
    ) -> Result<PullStage> {
        let permissions = self.permissions()?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // Own the entry only after exclusive creation succeeds. A name collision
        // must never arm cleanup for somebody else's existing file.
        let file = self.parent().dir.open_with(&name, &options)?.into_std();
        let mut stage = PullStage {
            target: self.clone(),
            name,
            file: Some(file),
            bytes: 0,
            cleanup: true,
        };
        let file = stage.file.as_mut().unwrap();
        stage.bytes = crate::drives::materialize::write_stage(file, size, write)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions.into_std(file)?)?;
        }
        Ok(stage)
    }
}

pub(crate) struct PullStage {
    target: Arc<PullDestination>,
    name: String,
    file: Option<File>,
    bytes: u64,
    cleanup: bool,
}
impl PullStage {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn publish(mut self) -> Result<u64> {
        self.target.permissions()?;
        #[cfg(test)]
        BEFORE_PUBLISH.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        // Source and destination use the held parent. This is an exclusive
        // 0600 stage, as in ordinary downloads, not protection against a writer
        // with permission to replace entries inside that same directory.
        let result = self.target.parent().dir.rename(
            &self.name,
            &self.target.parent().dir,
            &self.target.leaf,
        );
        if result.is_ok() {
            self.cleanup = false;
        }
        #[cfg(test)]
        let result = PUBLISH_RESULT.with(|hook| match hook.borrow_mut().take() {
            Some(hook) => hook(result),
            None => result,
        });
        result.map_err(|error| {
            crate::drives::NonReplayableWrite::mark(
                anyhow::Error::new(error).context("publish held pull destination"),
            )
        })?;
        Ok(self.bytes)
    }
}
impl Drop for PullStage {
    fn drop(&mut self) {
        drop(self.file.take());
        if self.cleanup {
            let _ = self.target.parent().dir.remove_file(&self.name);
        }
    }
}
#[cfg(test)]
thread_local! {
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static PUBLISH_RESULT: std::cell::RefCell<Option<Box<dyn FnOnce(io::Result<()>) -> io::Result<()>>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[path = "pull_destination_tests.rs"]
mod tests;
