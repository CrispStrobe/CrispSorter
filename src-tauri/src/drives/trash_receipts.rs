//! Durable object-bound duplicate trash. A provider mutation is never retried
//! automatically. Read-only reconciliation recovers interrupted receipts.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashIdentity {
    pub provider: String,
    /// Fingerprint of provider endpoint, stable account ID and root ID. No token.
    pub account: String,
    pub object_id: String,
    pub is_dir: bool,
    pub name: String,
    pub original_path: String,
    pub parent_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    Original,
    Trashed,
    Unknown,
}

/// Mutation methods must send at most once, including inside the SDK/client.
/// Gateway, protocol, redirect and auth-refresh replays violate this contract;
/// the journal cannot prevent retries performed below this boundary.
pub trait ReceiptTrash {
    fn prepare(&self, path: &Path) -> Result<TrashIdentity>;
    fn delete_exact(&self, identity: &TrashIdentity) -> Result<()>;
    /// Account, destination and exact UUID/type/name must all be verified.
    fn observe(&self, identity: &TrashIdentity) -> Result<Observation>;
    fn restore_exact(&self, identity: &TrashIdentity) -> Result<()>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    id: String,
    drive_id: String,
    group_id: String,
    identity: TrashIdentity,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReceiptSummary {
    pub id: String,
    pub drive_id: String,
    pub group_id: String,
    pub path: String,
    pub state: String,
}

pub struct ReceiptStore {
    root: PathBuf,
}

impl ReceiptStore {
    pub fn open(data_dir: &Path) -> Result<Self> {
        anyhow::ensure!(
            fs::metadata(data_dir)?.is_dir(),
            "application data directory is required"
        );
        let root = data_dir.join("trash-receipts");
        fs::create_dir_all(&root).context("create private trash journal")?;
        anyhow::ensure!(
            fs::symlink_metadata(&root)?.is_dir(),
            "invalid trash journal directory"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
            File::open(&root)?.sync_all()?;
            // Persist the directory's entry before any receipt can authorize
            // a remote mutation, including on its first creation.
            File::open(data_dir)?.sync_all()?;
        }
        Ok(Self { root })
    }

    fn path(&self, id: &str, suffix: &str) -> Result<PathBuf> {
        let id = Uuid::parse_str(id).context("invalid trash receipt ID")?;
        Ok(self.root.join(format!("{id}.{suffix}")))
    }

    fn sync_directory(&self) -> Result<()> {
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    fn mark(&self, id: &str, marker: &str) -> Result<()> {
        let path = self.path(id, marker)?;
        let mut options = OpenOptions::new();
        options.write(true);
        if marker == "claimed" {
            options.create_new(true);
        } else {
            // A read-only reconciliation can confirm success before the
            // original request returns. Reconfirming success is idempotent.
            options.create(true).truncate(false);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?.sync_all()?;
        self.sync_directory()
    }

    fn has(&self, id: &str, marker: &str) -> bool {
        self.path(id, marker).is_ok_and(|path| path.exists())
    }

    fn load(&self, id: &str, drive_id: &str) -> Result<Receipt> {
        let file = File::open(self.path(id, "json")?).context("read trash receipt")?;
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 65536, "trash receipt is too large");
        let receipt: Receipt = serde_json::from_slice(&bytes).context("invalid trash receipt")?;
        anyhow::ensure!(
            receipt.id == Uuid::parse_str(id)?.to_string() && receipt.drive_id == drive_id,
            "trash receipt belongs to another drive"
        );
        Ok(receipt)
    }

    fn state(&self, id: &str) -> &'static str {
        if self.has(id, "restored") {
            "restored"
        } else if self.has(id, "claimed") {
            "restore_uncertain"
        } else if self.has(id, "deleted") {
            "deleted"
        } else {
            "delete_uncertain"
        }
    }

    fn summary(&self, receipt: &Receipt) -> ReceiptSummary {
        ReceiptSummary {
            id: receipt.id.clone(),
            drive_id: receipt.drive_id.clone(),
            group_id: receipt.group_id.clone(),
            path: receipt.identity.original_path.clone(),
            state: self.state(&receipt.id).into(),
        }
    }

    fn prepare(&self, drive_id: &str, group_id: &str, identity: TrashIdentity) -> Result<Receipt> {
        anyhow::ensure!(
            !drive_id.is_empty() && !group_id.is_empty(),
            "trash context is required"
        );
        anyhow::ensure!(
            !identity.provider.is_empty()
                && !identity.account.is_empty()
                && !identity.object_id.is_empty()
                && !identity.parent_id.is_empty()
                && !identity.name.is_empty()
                && !identity.original_path.is_empty(),
            "incomplete trash identity"
        );
        let receipt = Receipt {
            id: Uuid::new_v4().to_string(),
            drive_id: drive_id.into(),
            group_id: group_id.into(),
            identity,
        };
        let bytes = serde_json::to_vec(&receipt)?;
        anyhow::ensure!(bytes.len() <= 65536, "trash receipt is too large");
        let mut staged = tempfile::NamedTempFile::new_in(&self.root)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        staged
            .persist_noclobber(self.path(&receipt.id, "json")?)
            .map_err(|error| error.error)
            .context("persist trash identity before deletion")?;
        self.sync_directory()?;
        Ok(receipt)
    }

    pub fn list(&self, group_id: &str) -> Result<Vec<ReceiptSummary>> {
        let mut result = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Load through the validated ID path, not an arbitrary directory entry.
            let mut bytes = Vec::new();
            File::open(self.path(id, "json")?)?
                .take(65537)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() <= 65536, "trash receipt is too large");
            let receipt: Receipt = serde_json::from_slice(&bytes)?;
            if receipt.group_id == group_id {
                let receipt = self.load(id, &receipt.drive_id)?;
                result.push(self.summary(&receipt));
            }
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(result)
    }

    pub fn trash(
        &self,
        provider: &dyn ReceiptTrash,
        drive_id: &str,
        group_id: &str,
        path: &Path,
    ) -> Result<ReceiptSummary> {
        let identity = provider.prepare(path)?;
        let receipt = self.prepare(drive_id, group_id, identity)?;
        // Failure/crash leaves an uncertain receipt, never an automatic retry.
        provider.delete_exact(&receipt.identity).context(
            "trash outcome uncertain; recheck the durable receipt before further action",
        )?;
        self.mark(&receipt.id, "deleted")?;
        Ok(self.summary(&receipt))
    }

    pub fn restore(
        &self,
        provider: &dyn ReceiptTrash,
        drive_id: &str,
        id: &str,
    ) -> Result<ReceiptSummary> {
        let receipt = self.load(id, drive_id)?;
        anyhow::ensure!(
            self.state(id) == "deleted",
            "trash receipt is not available for restore; recheck uncertain outcomes"
        );
        anyhow::ensure!(
            provider.observe(&receipt.identity)? == Observation::Trashed,
            "exact deleted object is not in trash"
        );
        // create_new is a cross-process single-use claim. It is never removed,
        // even on failure: remote completion may outlive a failed response.
        self.mark(id, "claimed")
            .context("trash restore was already claimed")?;
        provider
            .restore_exact(&receipt.identity)
            .context("restore outcome uncertain; recheck the durable receipt without replay")?;
        self.mark(id, "restored")?;
        Ok(self.summary(&receipt))
    }

    pub fn reconcile(
        &self,
        provider: &dyn ReceiptTrash,
        drive_id: &str,
        id: &str,
    ) -> Result<ReceiptSummary> {
        let receipt = self.load(id, drive_id)?;
        match (self.state(id), provider.observe(&receipt.identity)?) {
            ("delete_uncertain", Observation::Trashed) => self.mark(id, "deleted")?,
            // Original is only a snapshot: a pending delete may still commit.
            // Never terminally classify it as not deleted, even after a client
            // timeout. Legacy not-deleted markers are deliberately ignored.
            ("restore_uncertain" | "deleted", Observation::Original) => {
                self.mark(id, "restored")?
            }
            _ => {}
        }
        Ok(self.summary(&receipt))
    }
}

/// A receipt selects UUID and kind first. Equal names never substitute identity.
pub fn exact_candidate<'a, T>(
    identity: &TrashIdentity,
    items: &'a [T],
    describe: impl Fn(&T) -> (&str, bool, &str),
) -> Result<&'a T> {
    let mut matches = items
        .iter()
        .filter(|item| describe(item).0 == identity.object_id);
    let item = matches
        .next()
        .ok_or_else(|| anyhow!("receipted object is missing"))?;
    anyhow::ensure!(
        matches.next().is_none(),
        "provider returned ambiguous object identity"
    );
    let (_, is_dir, name) = describe(item);
    anyhow::ensure!(
        is_dir == identity.is_dir && name == identity.name,
        "receipted object metadata changed"
    );
    Ok(item)
}
