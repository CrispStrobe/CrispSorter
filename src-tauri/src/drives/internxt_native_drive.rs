//! `CloudDrive` adapter for a keychain-backed native Internxt session.

use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::internxt_native::{InternxtNativeClient, InternxtSession};
use super::{CloudDrive, DirEntry, DriveCapabilities, DriveType, FileStat};

pub struct NativeInternxtDrive {
    label: String,
    session: Result<InternxtSession>,
}

fn split_remote_name(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, extension),
        _ => (name, "file"),
    }
}

impl NativeInternxtDrive {
    pub fn from_keychain(label: impl Into<String>, drive_id: &str) -> Self {
        let session = super::secret::get_session(drive_id)
            .and_then(|serialized| serialized.ok_or_else(|| anyhow!("no native Internxt session")))
            .and_then(|serialized| InternxtSession::decode(&serialized));
        Self {
            label: label.into(),
            session,
        }
    }

    fn parts(&self) -> Result<(&InternxtSession, InternxtNativeClient)> {
        let session = self
            .session
            .as_ref()
            .map_err(|error| anyhow!("native Internxt session unavailable: {error:#}"))?;
        let client = InternxtNativeClient::new(&session.drive_api_url, session.active_token())?;
        Ok((session, client))
    }

    fn resolve_parent(
        &self,
        path: &Path,
        create: bool,
    ) -> Result<(InternxtSession, InternxtNativeClient, String)> {
        let (session, client) = self.parts()?;
        let mut folder_uuid = session.root_folder_id.clone();
        for component in path.components() {
            let component = component.as_os_str().to_string_lossy();
            if component.is_empty() || component == "." || component == "/" {
                continue;
            }
            let found = client
                .list_folder(&folder_uuid)?
                .into_iter()
                .find(|item| item.is_dir && item.name == component);
            folder_uuid = match found {
                Some(item) => item.uuid,
                None if create => client.create_folder(&folder_uuid, &component)?,
                None => return Err(anyhow!("Internxt folder not found: {component}")),
            };
        }
        Ok((session.clone(), client, folder_uuid))
    }

    fn resolved(
        &self,
        path: &Path,
    ) -> Result<(
        InternxtSession,
        InternxtNativeClient,
        super::internxt_native::NativeItem,
    )> {
        let (session, client) = self.parts()?;
        let item = client.resolve_path(session, path)?;
        Ok((session.clone(), client, item))
    }
}

impl CloudDrive for NativeInternxtDrive {
    fn label(&self) -> &str {
        &self.label
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let (_session, client, item) = self.resolved(path)?;
        if !item.is_dir {
            return Err(anyhow!(
                "Internxt path is not a directory: {}",
                path.display()
            ));
        }
        Ok(client
            .list_folder(&item.uuid)?
            .into_iter()
            .map(|item| DirEntry {
                name: item.name,
                is_dir: item.is_dir,
                size: (!item.is_dir).then_some(item.size),
            })
            .collect())
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let (session, client, item) = self.resolved(path)?;
        if item.is_dir {
            return Err(anyhow!("Internxt path is a directory: {}", path.display()));
        }
        client.download_file(&session, &item.uuid)
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        let filename = path
            .file_name()
            .ok_or_else(|| anyhow!("Internxt write path has no filename: {}", path.display()))?
            .to_string_lossy()
            .into_owned();
        let parent = path.parent().unwrap_or_else(|| Path::new("/"));
        let (session, client, folder_uuid) = self.resolve_parent(parent, true)?;
        let filename_path = PathBuf::from(filename.to_string());
        let plain_name = filename_path
            .file_stem()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| filename.to_string());
        let file_type = filename_path
            .extension()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Ok(existing) = client.resolve_path(&session, path) {
            if existing.is_dir {
                return Err(anyhow!(
                    "Internxt write path is a directory: {}",
                    path.display()
                ));
            }
            client.trash(&existing.uuid, "file")?;
        }
        client.upload_file(&session, &folder_uuid, &plain_name, &file_type, data)
    }

    fn upload_file_resumable(
        &self,
        local_path: &Path,
        remote_path: &Path,
        state_path: &Path,
        workers: usize,
    ) -> Result<()> {
        let filename = remote_path
            .file_name()
            .ok_or_else(|| anyhow!("Internxt remote path has no filename"))?
            .to_string_lossy()
            .into_owned();
        let parent = remote_path.parent().unwrap_or_else(|| Path::new("/"));
        let (session, client, folder_uuid) = self.resolve_parent(parent, true)?;
        let (plain_name, file_type) = split_remote_name(&filename);
        client.upload_path_with_resume_state_with_workers(
            &session,
            &folder_uuid,
            plain_name,
            file_type,
            local_path,
            state_path,
            workers.max(1),
        )
    }

    fn download_file_resumable(
        &self,
        remote_path: &Path,
        local_path: &Path,
        state_path: &Path,
    ) -> Result<()> {
        let (session, client, item) = self.resolved(remote_path)?;
        anyhow::ensure!(
            !item.is_dir,
            "Internxt remote path is a directory: {}",
            remote_path.display()
        );
        client.download_file_to_path_resumable(&session, &item.uuid, local_path, state_path)
    }

    fn delete(&self, path: &Path) -> Result<()> {
        let (_session, client, item) = self.resolved(path)?;
        client.trash(&item.uuid, if item.is_dir { "folder" } else { "file" })
    }

    fn capabilities(&self) -> DriveCapabilities {
        DriveCapabilities {
            create_dir: true,
            rename: true,
            move_path: true,
            copy: true,
            streaming: true,
            resumable_upload: true,
            resumable_download: true,
            ..DriveCapabilities::basic()
        }
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        self.resolve_parent(path, true).map(|_| ())
    }

    fn move_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let (_session, client, item) = self.resolved(source)?;
        let destination_parent = destination.parent().unwrap_or_else(|| Path::new("/"));
        let (_, _, destination_folder_uuid) = self.resolve_parent(destination_parent, false)?;
        let destination_name = destination
            .file_name()
            .ok_or_else(|| {
                anyhow!(
                    "move destination has no filename: {}",
                    destination.display()
                )
            })?
            .to_string_lossy()
            .into_owned();

        if item.name != destination_name {
            if item.is_dir {
                client.rename_folder(&item.uuid, &destination_name)?;
            } else {
                let (plain_name, file_type) = split_remote_name(&destination_name);
                client.rename_file(&item.uuid, plain_name, file_type)?;
            }
        }
        let source_parent = source.parent().unwrap_or_else(|| Path::new("/"));
        if source_parent != destination_parent {
            if item.is_dir {
                client.move_folder(&item.uuid, &destination_folder_uuid)?;
            } else {
                client.move_file(&item.uuid, &destination_folder_uuid)?;
            }
        }
        Ok(())
    }

    fn copy_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let (session, client, item) = self.resolved(source)?;
        let destination_parent = destination.parent().unwrap_or_else(|| Path::new("/"));
        let (_, _, destination_folder_uuid) = self.resolve_parent(destination_parent, false)?;
        let destination_name = destination
            .file_name()
            .ok_or_else(|| {
                anyhow!(
                    "copy destination has no filename: {}",
                    destination.display()
                )
            })?
            .to_string_lossy();

        if item.is_dir {
            client.copy_folder(
                &session,
                &item.uuid,
                &destination_folder_uuid,
                Some(&destination_name),
            )?;
        } else {
            // The native copy API takes a plain name and retains the source
            // extension. Rename the returned item afterward when the full
            // destination leaf differs from the source leaf.
            let copied = client.copy_file(&session, &item.uuid, &destination_folder_uuid, None)?;
            if copied.name != destination_name {
                let (plain_name, file_type) = split_remote_name(&destination_name);
                client.rename_file(&copied.uuid, plain_name, file_type)?;
            }
        }
        Ok(())
    }

    fn stat(&self, path: &Path) -> Result<FileStat> {
        let (_session, client, item) = self.resolved(path)?;
        if item.is_dir {
            return Ok(FileStat {
                size: 0,
                is_dir: true,
                mtime_unix: None,
            });
        }
        let metadata = client.file_metadata(&item.uuid)?;
        let size = metadata
            .get("size")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .unwrap_or(item.size);
        Ok(FileStat {
            size,
            is_dir: false,
            mtime_unix: None,
        })
    }

    fn drive_type(&self) -> DriveType {
        DriveType::Internxt
    }

    fn read_file_to_writer(&self, path: &Path, writer: &mut dyn Write) -> Result<u64> {
        let (session, client, item) = self.resolved(path)?;
        anyhow::ensure!(
            !item.is_dir,
            "Internxt path is a directory: {}",
            path.display()
        );
        client.download_file_to_writer(&session, &item.uuid, writer)
    }

    fn write_file_from_reader(&self, path: &Path, reader: &mut dyn Read, size: u64) -> Result<()> {
        let filename = path
            .file_name()
            .ok_or_else(|| anyhow!("Internxt write path has no filename: {}", path.display()))?
            .to_string_lossy();
        let parent = path.parent().unwrap_or_else(|| Path::new("/"));
        let (session, client, folder_uuid) = self.resolve_parent(parent, true)?;
        let filename_path = PathBuf::from(filename.as_ref());
        let plain_name = filename_path
            .file_stem()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| filename.to_string());
        let file_type = filename_path
            .extension()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Ok(existing) = client.resolve_path(&session, path) {
            if existing.is_dir {
                return Err(anyhow!(
                    "Internxt write path is a directory: {}",
                    path.display()
                ));
            }
            client.trash(&existing.uuid, "file")?;
        }
        client.upload_reader(
            &session,
            &folder_uuid,
            &plain_name,
            &file_type,
            reader,
            size,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_metadata_is_available_without_a_session() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeInternxtDrive::from_keychain("Native Internxt", "missing-drive");
        assert_eq!(drive.label(), "Native Internxt");
        assert_eq!(drive.drive_type(), DriveType::Internxt);
        let capabilities = drive.capabilities();
        assert!(capabilities.create_dir);
        assert!(capabilities.rename);
        assert!(capabilities.move_path);
        assert!(capabilities.copy);
        assert!(capabilities.streaming);
        assert!(capabilities.resumable_upload);
        assert!(capabilities.resumable_download);
    }

    #[test]
    fn missing_session_is_reported_before_network_access() {
        crate::drives::secret::install_mock_for_tests();
        let drive = NativeInternxtDrive::from_keychain("Native Internxt", "missing-drive");
        let error = drive
            .list_dir(Path::new("/"))
            .expect_err("missing session should fail");
        assert!(format!("{error:#}").contains("native Internxt session unavailable"));
    }
}
