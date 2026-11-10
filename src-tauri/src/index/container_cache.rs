//! Managed, bounded copies of verified container members. Logical member names
//! are display provenance and never become output paths.
use anyhow::{ensure, Context, Result};
use crisp_index_protocol::{ContainerMemberLocator, container::MAX_CONTAINER_MEMBER_BYTES};
use sha2::{Digest, Sha256};
use std::{fs, io::{Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}, time::UNIX_EPOCH};

const MAX_CACHE_FILES: usize = 64;
const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const PUBLISHED_PREFIX: &str = "member-v1-";
static CACHE_PUBLICATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub struct PreparedMemberCache {
    staged: tempfile::NamedTempFile,
    destination: PathBuf,
    root: PathBuf,
    size: u64,
}

impl PreparedMemberCache {
    pub fn prepare(root: &Path, locator: &ContainerMemberLocator, mut member: tempfile::NamedTempFile,
        extension: &str, sha256: &str, size: u64) -> Result<Self> {
        locator.validate().map_err(anyhow::Error::msg)?;
        let receipt = locator.entries.last().context("The selected member has no byte receipt")?;
        ensure!(size == receipt.size && size <= MAX_CONTAINER_MEMBER_BYTES && sha256.eq_ignore_ascii_case(&receipt.sha256),
            "The extracted member does not match its indexed byte receipt; reindex the source container");
        let extension = if extension.is_empty() { "bin" } else { extension };
        ensure!(extension.len() <= 16 && extension.bytes().all(|byte| byte.is_ascii_alphanumeric()),
            "The extracted member has an invalid file extension");
        if let Ok(metadata) = fs::symlink_metadata(root) {
            ensure!(metadata.is_dir() && !metadata.file_type().is_symlink(), "The managed member cache must be a real directory");
        } else {
            let mut builder = fs::DirBuilder::new(); builder.recursive(true);
            #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
            builder.create(root).context("Create the managed member cache")?;
        }
        let root = root.canonicalize().context("Resolve the managed member cache")?;
        let mut staged = tempfile::Builder::new().prefix(".member-stage-").tempfile_in(&root)
            .context("Stage the extracted member")?;
        member.as_file_mut().seek(SeekFrom::Start(0))?;
        let mut hash = Sha256::new(); let mut copied = 0_u64; let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = member.as_file_mut().read(&mut buffer)?;
            if count == 0 { break; }
            copied += count as u64;
            ensure!(copied <= size, "The materialized member changed before cache publication");
            hash.update(&buffer[..count]); staged.write_all(&buffer[..count])?;
        }
        ensure!(copied == size && format!("{:x}", hash.finalize()).eq_ignore_ascii_case(sha256),
            "The materialized member changed before cache publication");
        staged.flush()?;
        staged.as_file().sync_all()?;
        let nonce = staged.path().file_name().and_then(|name| name.to_str()).context("Invalid staging path")?
            .strip_prefix(".member-stage-").context("Invalid staging path")?;
        let destination = root.join(format!("{PUBLISHED_PREFIX}{}-{nonce}.{}", locator.member_id().map_err(anyhow::Error::msg)?, extension.to_ascii_lowercase()));
        Ok(Self { staged, destination, root, size })
    }

    /// Call only after the captured index/locator has been checked again.
    pub fn publish(self) -> Result<PathBuf> {
        self.publish_with_limits(MAX_CACHE_FILES, MAX_CACHE_BYTES)
    }

    fn publish_with_limits(self, maximum_files: usize, maximum_bytes: u64) -> Result<PathBuf> {
        let _publication = CACHE_PUBLICATION.lock().map_err(|_| anyhow::anyhow!("The member cache is unavailable"))?;
        ensure!(self.size <= maximum_bytes && maximum_files > 0, "The member exceeds the managed cache limit");
        make_room(&self.root, self.size, maximum_files, maximum_bytes)?;
        let mut permissions = self.staged.as_file().metadata()?.permissions();
        permissions.set_readonly(true); self.staged.as_file().set_permissions(permissions)?;
        // A unique generated name is never an overwrite target. Failed or stale
        // preparations drop the private staging file without publishing it.
        if let Err(error) = self.staged.persist_noclobber(&self.destination) {
            #[cfg(windows)] {
                if let Ok(metadata) = error.file.as_file().metadata() {
                    let mut permissions = metadata.permissions(); permissions.set_readonly(false);
                    let _ = error.file.as_file().set_permissions(permissions);
                }
            }
            return Err(error.error).context("Publish the verified member copy");
        }
        Ok(self.destination)
    }
}

fn make_room(root: &Path, incoming: u64, maximum_files: usize, maximum_bytes: u64) -> Result<()> {
    let mut entries = Vec::new(); let mut total = 0_u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with(PUBLISHED_PREFIX) { continue; }
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "The managed member cache contains an invalid entry");
        total = total.saturating_add(metadata.len());
        entries.push((metadata.modified().unwrap_or(UNIX_EPOCH), entry.path(), metadata.len()));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut remaining = entries.len();
    for (_, path, size) in entries {
        if remaining < maximum_files && total.saturating_add(incoming) <= maximum_bytes { break; }
        #[cfg(windows)] {
            let mut permissions = fs::metadata(&path)?.permissions(); permissions.set_readonly(false); fs::set_permissions(&path, permissions)?;
        }
        fs::remove_file(path).context("Retire an older extracted member copy")?;
        remaining -= 1; total = total.saturating_sub(size);
    }
    ensure!(remaining < maximum_files && total.saturating_add(incoming) <= maximum_bytes, "The managed member cache is full");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisp_index_protocol::{ContainerEntry, ContainerEntryKind};

    fn source(bytes: &[u8]) -> (ContainerMemberLocator, tempfile::NamedTempFile, String) {
        let mut file = tempfile::NamedTempFile::new().unwrap(); file.write_all(bytes).unwrap();
        let hash = format!("{:x}", Sha256::digest(bytes));
        let locator = ContainerMemberLocator { version: 1, root_owner_id: "local".into(), root_doc_id: "root".into(),
            root_location_uri: "/records/source.zip".into(), root_sha256: "a".repeat(64),
            entries: vec![ContainerEntry { kind: ContainerEntryKind::Zip, index: 1, occurrence: 0,
                name: "folder/receipt.txt".into(), sha256: hash.clone(), size: bytes.len() as u64 }] };
        (locator, file, hash)
    }

    #[test]
    fn verified_copy_has_a_generated_name_and_failed_preparations_publish_nothing() {
        let directory = tempfile::tempdir().unwrap(); let root = directory.path().join("cache");
        let (locator,file,hash) = source(b"verified source");
        let prepared = PreparedMemberCache::prepare(&root,&locator,file,"txt",&hash,15).unwrap();
        assert!(fs::read_dir(&root).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().starts_with(PUBLISHED_PREFIX)));
        let path = prepared.publish().unwrap();
        assert_eq!(path.parent(),Some(root.as_path())); assert_ne!(path.file_name().unwrap(),"receipt.txt");
        assert_eq!(fs::read(&path).unwrap(),b"verified source"); assert!(fs::metadata(&path).unwrap().permissions().readonly());
        let (_,mut corrupt,_) = source(b"verified source"); corrupt.as_file_mut().seek(SeekFrom::Start(0)).unwrap(); corrupt.write_all(b"corrupted bytes").unwrap();
        assert!(PreparedMemberCache::prepare(&root,&locator,corrupt,"txt",&hash,15).is_err());
        assert_eq!(fs::read(&path).unwrap(),b"verified source"); assert_eq!(fs::read_dir(root).unwrap().count(),1);
    }

    #[test]
    fn abandoned_work_and_unsafe_extensions_never_create_an_openable_copy() {
        let directory = tempfile::tempdir().unwrap();
        let (locator,file,hash) = source(b"body");
        let prepared = PreparedMemberCache::prepare(directory.path(),&locator,file,"txt",&hash,4).unwrap();
        drop(prepared); assert_eq!(fs::read_dir(directory.path()).unwrap().count(),0);
        let (locator,file,hash) = source(b"body");
        assert!(PreparedMemberCache::prepare(directory.path(),&locator,file,"../../exe",&hash,4).is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(),0);
    }

    #[test]
    fn cache_bounds_retire_only_managed_files() {
        let directory = tempfile::tempdir().unwrap(); fs::write(directory.path().join("unrelated.txt"),b"keep").unwrap();
        let (locator,file,hash) = source(b"first");
        let first = PreparedMemberCache::prepare(directory.path(),&locator,file,"txt",&hash,5).unwrap().publish_with_limits(1,10).unwrap();
        let (locator,file,hash) = source(b"second");
        let second = PreparedMemberCache::prepare(directory.path(),&locator,file,"txt",&hash,6).unwrap().publish_with_limits(1,10).unwrap();
        assert!(!first.exists()); assert_eq!(fs::read(second).unwrap(),b"second");
        assert_eq!(fs::read(directory.path().join("unrelated.txt")).unwrap(),b"keep");
    }
}
