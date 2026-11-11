//! Experimental direct SMB source for Sorter. Existing OS mounts stay default.
//! Uses the same independent core as Flutter, without Dart or a bridge runtime.
#![forbid(unsafe_code)]
pub use crisp_smb::{Config, Error, Result, Session, CHUNK};
use serde_json::Value;
use std::io::{Read, Write};

pub struct SmbSource {
    session: Session,
}
impl SmbSource {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            session: Session::new(config)?,
        })
    }
    pub async fn list(&self, path: &str) -> Result<Value> {
        self.session.request("list", path, "", 0).await
    }
    pub async fn stat(&self, path: &str) -> Result<Value> {
        checked_stat(self.session.request("stat", path, "", 0).await?)
    }
    /// Caller supplies a staging writer and publishes only after success.
    /// Bounded native buffers keep multi-GB sorting/indexing off whole-file APIs.
    pub async fn read_to_writer(&self, path: &str, writer: &mut impl Write) -> Result<u64> {
        let opened = self.session.request("openRead", path, "", 0).await?;
        let handle = opened["handle"]
            .as_u64()
            .ok_or_else(|| Error::new("InvalidData"))?;
        let size = opened["size"]
            .as_u64()
            .ok_or_else(|| Error::new("InvalidData"))?;
        let result = async {
            let mut offset = 0;
            while offset < size {
                let bytes = self
                    .session
                    .read_chunk(handle, offset, (size - offset).min(CHUNK as u64) as u32)
                    .await?;
                if bytes.is_empty() {
                    return Err(Error::new("Incomplete"));
                }
                writer.write_all(&bytes)?;
                offset += bytes.len() as u64;
            }
            let after = self.stat(path).await?;
            if after["version"] != opened["version"] {
                return Err(Error::new("FileChanged"));
            }
            Ok(offset)
        }
        .await;
        let close = self.session.request("closeRead", "", "", handle).await;
        match result {
            Err(e) => Err(e),
            Ok(n) => {
                close?;
                Ok(n)
            }
        }
    }
    /// Sized, bounded upload. No automatic retry/replay or atomic publication.
    pub async fn write_from_reader(
        &self,
        path: &str,
        reader: &mut impl Read,
        size: u64,
    ) -> Result<()> {
        let opened = self.session.request("openWrite", path, "", 0).await?;
        let handle = opened.as_u64().ok_or_else(|| Error::new("InvalidData"))?;
        let result = async {
            let mut remaining = size;
            let mut buffer = vec![0; CHUNK];
            while remaining > 0 {
                let length = remaining.min(CHUNK as u64) as usize;
                reader.read_exact(&mut buffer[..length])?;
                self.session
                    .write_chunk(handle, buffer[..length].to_vec())
                    .await?;
                remaining -= length as u64;
            }
            let mut extra = [0];
            if reader.read(&mut extra)? != 0 {
                return Err(Error::new("SizeMismatch"));
            }
            self.session.request("finishWrite", "", "", handle).await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = self.session.request("abortWrite", "", "", handle).await;
        }
        result
    }
    pub async fn download(
        &self,
        path: &str,
        staging: &str,
        source: &str,
        limit: u64,
    ) -> Result<u64> {
        self.session.download(path, staging, source, limit).await
    }
    pub fn cancel(&self) {
        self.session.cancel();
    }
    pub async fn close(&self) {
        self.session.close().await;
    }
}

/// The shared core represents a missing path as JSON null. The application
/// filesystem boundary requires an error, including for a zero-byte file probe.
fn checked_stat(value: Value) -> Result<Value> {
    if value.is_null() {
        return Err(Error::new("NotFound"));
    }
    if value["size"].as_u64().is_none() || value["directory"].as_bool().is_none()
        || !value["version"].is_object() {
        return Err(Error::new("InvalidData"));
    }
    Ok(value)
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn missing_stat_is_an_error_but_empty_files_are_valid() {
        assert_eq!(checked_stat(Value::Null).unwrap_err().code, "NotFound");
        let empty = serde_json::json!({"size":0,"directory":false,"version":{"size":0}});
        assert_eq!(checked_stat(empty.clone()).unwrap(), empty);
        for malformed in [serde_json::json!([]), serde_json::json!({}),
            serde_json::json!({"size":0,"directory":false})] {
            assert_eq!(checked_stat(malformed).unwrap_err().code, "InvalidData");
        }
    }
}

mod blocking;
pub use blocking::BlockingSession;
mod settings;
pub use settings::{Backend, Settings, relative_path};
