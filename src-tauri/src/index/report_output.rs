//! Atomic report output shared by complete-corpus and returned-window exports.
use std::{fs::{self, File}, io::{Read, Write}, path::{Path, PathBuf}};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};

pub struct StagedReport {
    file: tempfile::NamedTempFile,
    destination: PathBuf,
    protected_roots: Vec<PathBuf>,
    prior_hash: Option<[u8; 32]>,
}

impl StagedReport {
    pub fn new(path: &Path, protected_roots: &[PathBuf], protected_sources: &[String]) -> Result<Self> {
        ensure!(path.is_absolute(), "Choose an absolute report destination in an existing folder");
        let parent = path.parent().context("The report destination has no parent directory")?.canonicalize()
            .context("The report output folder is unavailable")?;
        ensure!(parent.is_dir(), "The report output folder is unavailable");
        let destination = parent.join(path.file_name().context("Choose a report filename")?);
        let protected_roots: Vec<_> = protected_roots.iter().map(|root| root.canonicalize().unwrap_or_else(|_| root.clone())).collect();
        validate_path(&destination, &protected_roots)?;
        for source in protected_sources { reject_source_destination(&destination, source)?; }
        let prior_hash = fingerprint(&destination)?;
        let file = tempfile::NamedTempFile::new_in(&parent).context("Creating a staged report beside the destination")?;
        Ok(Self { file, destination, protected_roots, prior_hash })
    }

    pub fn destination(&self) -> &Path { &self.destination }
    pub fn file_mut(&mut self) -> &mut File { self.file.as_file_mut() }

    /// Only the final rename touches the selected destination. Errors and
    /// cancellation before this point leave any complete old report untouched.
    pub fn publish(mut self, validate: impl FnOnce() -> Result<()>) -> Result<PathBuf> {
        self.file.flush().context("Flushing the complete report")?;
        self.file.as_file().sync_all().context("Synchronizing the complete report")?;
        validate_path(&self.destination, &self.protected_roots)?;
        ensure!(fingerprint(&self.destination)? == self.prior_hash,
            "The report destination changed during export. Choose its destination again.");
        validate()?;
        if self.prior_hash.is_some() {
            self.file.persist(&self.destination).map_err(|error| error.error).context("Publishing the complete report")?;
        } else {
            self.file.persist_noclobber(&self.destination).map_err(|error| error.error).context("Publishing the complete report without replacing a new file")?;
        }
        #[cfg(unix)]
        File::open(self.destination.parent().unwrap())?.sync_all().context("The report was saved but its directory could not be synchronized")?;
        Ok(self.destination)
    }
}

fn validate_path(path: &Path, protected_roots: &[PathBuf]) -> Result<()> {
    for root in protected_roots {
        ensure!(!path.starts_with(root), "Choose a report destination outside application and index storage");
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(metadata.is_file() && !metadata.file_type().is_symlink(),
            "The report destination must be a regular file, not a symbolic link or directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error).context("Inspecting the report destination"),
    }
    Ok(())
}

fn fingerprint(path: &Path) -> Result<Option<[u8; 32]>> {
    let mut input = match File::open(path) {
        Ok(input) => input,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Reading the existing report before replacement"),
    };
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop { let length = input.read(&mut bytes)?; if length == 0 { break; } hash.update(&bytes[..length]); }
    Ok(Some(hash.finalize().into()))
}

/// Protect both current canonical source paths and the literal path retained
/// for an unavailable source. Encoded local URIs are decoded exactly once.
pub fn reject_source_destination(destination: &Path, source_uri: &str) -> Result<()> {
    let destination = destination.canonicalize().unwrap_or_else(|_| destination.to_owned());
    for path in local_source_paths(source_uri) {
        let canonical = path.canonicalize().or_else(|_| {
            Ok::<_, std::io::Error>(path.parent().unwrap_or(Path::new("/")).canonicalize()?
                .join(path.file_name().unwrap_or_default()))
        }).unwrap_or(path);
        ensure!(canonical != destination, "A search report cannot replace an indexed source document. Choose another destination.");
    }
    Ok(())
}

fn local_source_paths(uri: &str) -> Vec<PathBuf> {
    if uri.contains('\0') { return vec![]; }
    let (raw, encoded) = if let Some(rest) = uri.strip_prefix("crisp+local://") {
        let Some(slash) = rest.find('/') else { return vec![]; };
        (&rest[slash..], true)
    } else if let Some(rest) = uri.strip_prefix("file://") {
        let path = rest.strip_prefix("localhost/").map(|_| &rest["localhost".len()..]).unwrap_or(rest);
        if !path.starts_with('/') { return vec![]; }
        (path, true)
    } else {
        if !Path::new(uri).is_absolute() { return vec![]; }
        (uri, false)
    };
    let mut paths = vec![local_path(raw)];
    if encoded {
        let mut bytes = Vec::with_capacity(raw.len());
        let source = raw.as_bytes(); let mut cursor = 0;
        while cursor < source.len() {
            if source[cursor] == b'%' {
                let Some(pair) = source.get(cursor + 1..cursor + 3) else { return paths; };
                let Some(hi) = (pair[0] as char).to_digit(16) else { return paths; };
                let Some(lo) = (pair[1] as char).to_digit(16) else { return paths; };
                let value = (hi * 16 + lo) as u8;
                if value == 0 { return paths; }
                bytes.push(value); cursor += 3;
            } else { bytes.push(source[cursor]); cursor += 1; }
        }
        if let Ok(decoded) = String::from_utf8(bytes) { paths.push(local_path(&decoded)); }
    }
    paths
}

fn local_path(raw: &str) -> PathBuf {
    #[cfg(windows)]
    let raw = if raw.starts_with('/') && raw.as_bytes().get(2) == Some(&b':') { &raw[1..] } else { raw };
    PathBuf::from(raw)
}

pub fn write_captured_report(path: &Path, content: &str, protected_roots: &[PathBuf], protected_sources: &[String]) -> Result<PathBuf> {
    ensure!(content.len() <= 32 * 1024 * 1024, "This returned-window report exceeds 32 MiB. Export the complete match set as a streamed report instead.");
    let mut staging = StagedReport::new(path, protected_roots, protected_sources)?;
    staging.file_mut().write_all(content.as_bytes()).context("Writing the captured search report")?;
    staging.publish(|| { for source in protected_sources { reject_source_destination(path, source)?; } Ok(()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_cancelled_and_changed_reports_preserve_existing_bytes() {
        let directory = tempfile::tempdir().unwrap(); let path = directory.path().join("report.json");
        fs::write(&path, b"complete previous report").unwrap();
        let mut staged = StagedReport::new(&path, &[], &[]).unwrap();
        staged.file_mut().write_all(b"incomplete new report").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete previous report");
        assert!(staged.publish(|| bail!("cancelled")).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"complete previous report");
        let staged = StagedReport::new(&path, &[], &[]).unwrap();
        fs::write(&path, b"another completed report").unwrap();
        assert!(staged.publish(|| Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"another completed report");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn reports_cannot_replace_sources_or_index_storage_and_complete_reports_replace_atomically() {
        let directory = tempfile::tempdir().unwrap(); let path = directory.path().join("source file.txt");
        fs::write(&path, b"source").unwrap();
        for source in [path.to_string_lossy().into_owned(), format!("file://{}", path.display()).replace(' ', "%20"),
            format!("crisp+local://host{}", path.display())] {
            assert!(write_captured_report(&path, "report", &[], &[source]).is_err());
        }
        assert!(write_captured_report(&path, "report", &[directory.path().to_owned()], &[]).is_err());
        let report = directory.path().join("report.json");
        write_captured_report(&report, "first complete report", &[], &[]).unwrap();
        write_captured_report(&report, "second complete report", &[], &[]).unwrap();
        assert_eq!(fs::read(&report).unwrap(), b"second complete report");
        assert_eq!(fs::read(&path).unwrap(), b"source");
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_source_aliases_and_destination_links_are_protected() {
        let directory = tempfile::tempdir().unwrap(); let source = directory.path().join("source.txt");
        let alias = directory.path().join("alias.txt"); fs::write(&source, b"source").unwrap();
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        assert!(write_captured_report(&alias, "report", &[], &[]).is_err());
        assert!(write_captured_report(&source, "report", &[], &[alias.to_string_lossy().into_owned()]).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"source");
    }
}
