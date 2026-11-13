//! A manual pull captures its selected directory object once, then retains it
//! across queue waits, retries and later entries. Moves follow that same object;
//! replacing its former pathname never redirects a destination. No journals or
//! persistent object identities are involved.
use anyhow::{Context, Result};
use std::{
    ffi::OsStr,
    fs::Permissions,
    io::{self, Write},
    path::Path,
    sync::Arc,
};
#[cfg(unix)]
#[path = "pull_destination_unix.rs"]
mod backend;
#[cfg(windows)]
#[path = "pull_destination_windows.rs"]
mod backend;
use backend::{Directory, StageFile};

struct HeldDir {
    dir: Directory,
}
impl HeldDir {
    fn child(&self, name: &str) -> Result<Arc<Self>> {
        let name = OsStr::new(name);
        let dir = match self.dir.open_child(name) {
            Ok(dir) => dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match self.dir.create_child(name) {
                    Ok(dir) => dir,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        self.dir.open_child(name)?
                    }
                    Err(error) => return Err(error).context("create pull directory"),
                }
            }
            Err(error) => return Err(error).context("open pull directory without following links"),
        };
        Ok(Arc::new(Self { dir }))
    }
}

/// Capture once before asynchronous provider/queue waits, for the whole pull.
pub(crate) struct PullRoot {
    ancestry: Vec<Arc<HeldDir>>,
}
impl PullRoot {
    pub(crate) fn capture(path: &Path) -> Result<Arc<Self>> {
        let dir = Directory::open_root(path).context("open selected pull root")?;
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
            ancestry.push(ancestry.last().unwrap().child(part)?);
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
        self.parent()
            .dir
            .permissions(OsStr::new(&self.leaf))
            .context("inspect held pull destination")
    }
    pub(crate) fn stage(
        self: &Arc<Self>,
        size: Option<u64>,
        write: impl FnOnce(&mut dyn Write) -> Result<u64>,
    ) -> Result<PullStage> {
        self.stage_named(
            format!(".crispsorter-pull-{}", uuid::Uuid::new_v4()),
            size,
            write,
        )
    }
    fn stage_named(
        self: &Arc<Self>,
        name: String,
        size: Option<u64>,
        write: impl FnOnce(&mut dyn Write) -> Result<u64>,
    ) -> Result<PullStage> {
        let permissions = self.permissions()?;
        let mut file = self.parent().dir.create_file(OsStr::new(&name))?;
        let bytes = crate::drives::materialize::write_stage(file.as_file_mut(), size, write)?;
        if let Some(permissions) = permissions {
            file.as_file().set_permissions(permissions)?;
        }
        Ok(PullStage {
            target: self.clone(),
            file,
            bytes,
        })
    }
}

pub(crate) struct PullStage {
    target: Arc<PullDestination>,
    file: StageFile,
    bytes: u64,
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
        let result = self
            .target
            .parent()
            .dir
            .rename_file(&mut self.file, OsStr::new(&self.target.leaf));
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

/// The same loop used by the pair command. The caller captures root once; this
/// routine derives every later destination from that handle, never the old path.
pub(crate) async fn download_entries(
    queue: &crate::sync::transfer_queue::TransferQueue,
    drive_id: &str,
    drive: Arc<dyn crate::drives::CloudDrive>,
    remote_root: &str,
    entries: &[crate::sync::pairs::SyncRemoteEntry],
    root: Arc<PullRoot>,
) -> Result<usize> {
    let mut downloaded = 0;
    for entry in entries {
        let target = root.destination(&entry.relative_path)?;
        let path = std::path::PathBuf::from(format!(
            "{}/{}",
            remote_root.trim_end_matches('/'),
            entry.relative_path
        ));
        let reader = drive.clone();
        let job = queue.submit_pull_download(
            drive_id.to_owned(),
            path,
            target,
            Some(entry.size),
            move |path, writer| reader.read_file_to_writer(path, writer),
        );
        job.handle.await.context("sync transfer task failed")??;
        downloaded += 1;
    }
    Ok(downloaded)
}

#[cfg(test)]
thread_local! {
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static PUBLISH_RESULT: std::cell::RefCell<Option<Box<dyn FnOnce(io::Result<()>) -> io::Result<()>>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
#[path = "pull_destination_common_tests.rs"]
mod common_tests;
#[cfg(all(test, unix))]
#[path = "pull_destination_tests.rs"]
mod tests;
