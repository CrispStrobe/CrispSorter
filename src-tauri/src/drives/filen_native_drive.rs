//! `CloudDrive` adapter for a keychain-backed native Filen session.

use super::{CloudDrive, DirEntry, DriveCapabilities, DriveType, FileStat};
use anyhow::{anyhow, Result};
use crisp_filen::{FilenNativeClient, FilenSession};
use std::io::{Read, Write};
use std::path::Path;

pub struct NativeFilenDrive {
    label: String,
    session: Result<FilenSession>,
}

impl NativeFilenDrive {
    pub fn from_keychain(label: impl Into<String>, drive_id: &str) -> Self {
        let session = super::secret::get_session(drive_id)
            .and_then(|value| value.ok_or_else(|| anyhow!("no native Filen session")))
            .and_then(|value| FilenSession::decode(&value));
        Self {
            label: label.into(),
            session,
        }
    }
    fn parts(&self) -> Result<(&FilenSession, FilenNativeClient)> {
        let session = self
            .session
            .as_ref()
            .map_err(|e| anyhow!("native Filen session unavailable: {e:#}"))?;
        Ok((session, FilenNativeClient::from_session(session)?))
    }
    fn resolve(
        &self,
        path: &Path,
    ) -> Result<(FilenSession, FilenNativeClient, crisp_filen::NativeItem)> {
        let (session, client) = self.parts()?;
        let item = client.resolve_path(session, path)?;
        Ok((session.clone(), client, item))
    }
    fn parent(&self, path: &Path) -> Result<(FilenSession, FilenNativeClient, String)> {
        let (session, client) = self.parts()?;
        let mut uuid = session.root_folder_uuid.clone();
        for part in path.components() {
            let part = part.as_os_str().to_string_lossy();
            if part.is_empty() || part == "." || part == "/" {
                continue;
            }
            if let Some(found) = client
                .list_folder(&uuid)?
                .into_iter()
                .find(|item| item.is_dir && item.name == part)
            {
                uuid = found.uuid;
            } else {
                uuid = client.create_folder(&uuid, &part)?;
            }
        }
        Ok((session.clone(), client, uuid))
    }
}

impl CloudDrive for NativeFilenDrive {
    fn label(&self) -> &str {
        &self.label
    }
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let (_s, c, item) = self.resolve(path)?;
        anyhow::ensure!(
            item.is_dir,
            "Filen path is not a directory: {}",
            path.display()
        );
        Ok(c.list_folder(&item.uuid)?
            .into_iter()
            .map(|i| DirEntry {
                name: i.name,
                is_dir: i.is_dir,
                size: (!i.is_dir).then_some(i.size),
            })
            .collect())
    }
    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let (_s, c, item) = self.resolve(path)?;
        anyhow::ensure!(
            !item.is_dir,
            "Filen path is a directory: {}",
            path.display()
        );
        c.download_file(&item)
    }
    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        let name = path
            .file_name()
            .ok_or_else(|| anyhow!("Filen write path has no filename"))?
            .to_string_lossy();
        let parent = path.parent().unwrap_or_else(|| Path::new("/"));
        let (_s, c, uuid) = self.parent(parent)?;
        if let Ok(existing) = c.resolve_path(&_s, path) {
            c.trash(
                &existing.uuid,
                if existing.is_dir { "folder" } else { "file" },
            )?;
        }
        c.upload_file(&uuid, &name, "application/octet-stream", data)
    }

    fn upload_file_resumable(
        &self,
        local_path: &Path,
        remote_path: &Path,
        state_path: &Path,
        _workers: usize,
    ) -> Result<()> {
        let name = remote_path
            .file_name()
            .ok_or_else(|| anyhow!("Filen remote path has no filename"))?
            .to_string_lossy()
            .into_owned();
        let parent = remote_path.parent().unwrap_or_else(|| Path::new("/"));
        let (_session, client, parent_uuid) = self.parent(parent)?;
        let size = std::fs::metadata(local_path)
            .map_err(|e| anyhow!("reading resumable upload source: {e}"))?
            .len();
        let mut state = FilenNativeClient::load_upload_resume_state(state_path)?
            .unwrap_or(client.begin_upload(
                &parent_uuid,
                &name,
                "application/octet-stream",
                size,
            )?);
        anyhow::ensure!(state.parent == parent_uuid, "resume state parent mismatch");
        anyhow::ensure!(state.name == name, "resume state filename mismatch");
        anyhow::ensure!(state.size == size, "resume state size mismatch");
        client.resume_upload_from_reader(&mut state, std::fs::File::open(local_path)?)?;
        FilenNativeClient::clear_upload_resume_state(state_path)
    }
    fn delete(&self, path: &Path) -> Result<()> {
        let (_s, c, item) = self.resolve(path)?;
        c.trash(&item.uuid, if item.is_dir { "folder" } else { "file" })
    }
    fn stat(&self, path: &Path) -> Result<FileStat> {
        let (_s, _c, item) = self.resolve(path)?;
        Ok(FileStat {
            size: item.size,
            is_dir: item.is_dir,
            // Filen stores metadata timestamps in milliseconds; CloudDrive
            // exposes Unix seconds. Zero is the native client's sentinel for
            // folders or unavailable gateway metadata.
            mtime_unix: (item.modified > 0).then_some(item.modified / 1_000),
        })
    }
    fn drive_type(&self) -> DriveType {
        DriveType::Filen
    }

    fn capabilities(&self) -> DriveCapabilities {
        DriveCapabilities {
            streaming: true,
            ..DriveCapabilities::basic()
        }
    }

    fn read_file_to_writer(&self, path: &Path, mut writer: &mut dyn Write) -> Result<u64> {
        let (_session, client, item) = self.resolve(path)?;
        anyhow::ensure!(!item.is_dir, "Filen path is a directory: {}", path.display());
        client.download_file_to_writer(&item, &mut writer)
    }

    fn write_file_from_reader(
        &self,
        path: &Path,
        reader: &mut dyn Read,
        size: u64,
    ) -> Result<()> {
        let name = path
            .file_name()
            .ok_or_else(|| anyhow!("Filen write path has no filename"))?
            .to_string_lossy();
        let parent = path.parent().unwrap_or_else(|| Path::new("/"));
        let (_session, client, uuid) = self.parent(parent)?;
        if let Ok(existing) = client.resolve_path(&_session, path) {
            client.trash(
                &existing.uuid,
                if existing.is_dir { "folder" } else { "file" },
            )?;
        }
        client.upload_file_from_reader(
            &uuid,
            &name,
            "application/octet-stream",
            size,
            reader,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_session_is_reported_without_network() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeFilenDrive::from_keychain("Native Filen", "missing-filendrive");
        assert_eq!(drive.drive_type(), DriveType::Filen);
        assert!(format!("{}", drive.list_dir(Path::new("/")).unwrap_err())
            .contains("native Filen session"));
    }
}
