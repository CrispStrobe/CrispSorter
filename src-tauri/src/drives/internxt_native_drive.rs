//! `CloudDrive` adapter for a keychain-backed native Internxt session.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

use super::internxt_native::{InternxtNativeClient, InternxtSession};
use super::{CloudDrive, DirEntry, DriveType, FileStat};

pub struct NativeInternxtDrive {
    label: String,
    session: Result<InternxtSession>,
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
        client.upload_file(&session, &folder_uuid, &plain_name, &file_type, data)
    }

    fn delete(&self, path: &Path) -> Result<()> {
        let (_session, client, item) = self.resolved(path)?;
        client.trash(&item.uuid, if item.is_dir { "folder" } else { "file" })
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_metadata_is_available_without_a_session() {
        super::secret::install_mock_for_tests();
        let drive = NativeInternxtDrive::from_keychain("Native Internxt", "missing-drive");
        assert_eq!(drive.label(), "Native Internxt");
        assert_eq!(drive.drive_type(), DriveType::Internxt);
    }

    #[test]
    fn missing_session_is_reported_before_network_access() {
        super::secret::install_mock_for_tests();
        let drive = NativeInternxtDrive::from_keychain("Native Internxt", "missing-drive");
        let error = drive
            .list_dir(Path::new("/"))
            .expect_err("missing session should fail");
        assert!(format!("{error:#}").contains("native Internxt session unavailable"));
    }
}
