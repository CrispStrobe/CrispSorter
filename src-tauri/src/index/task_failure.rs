/// Why a background L3 extraction failed.
///
/// One value is persisted in `metadata_json` as `extraction_failure.reason`
/// so subsequent runs can skip-or-retry intelligently and Übersicht can
/// render the right badge.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFailureReason {
    /// Extraction produced no output within the time budget.
    Timeout,
    /// File is DRM-protected (EPUB ADEPT/FairPlay, PDF owner-lock).
    Drm,
    /// File requires a user password to decrypt.
    Password,
    /// Extractor returned a hard parse error in < 2 s (malformed file).
    Corrupt,
    /// No extractor registered for this extension.
    Unsupported,
    /// Catch-all for errors that don't fit the above categories.
    Other,
}

impl TaskFailureReason {
    /// Classify an error string into the most specific bucket.
    pub fn classify(err: &str) -> Self {
        let lower = err.to_lowercase();
        // DRM / encryption checks come first because both PDF and EPUB
        // sometimes mention "password" in the same message.
        if lower.contains("encryption.xml")
            || lower.contains("adept")
            || lower.contains("fairplay")
            || lower.contains("drm")
            || (lower.contains("encrypt") && !lower.contains("password"))
        {
            Self::Drm
        } else if lower.contains("password") || lower.contains("user-password") {
            Self::Password
        } else if lower.contains("no extractor") || lower.contains("unsupported") {
            Self::Unsupported
        } else {
            // All other extraction errors are treated as corrupt/unknown.
            // Timeout is set before classify() is called (elapsed future).
            Self::Corrupt
        }
    }

    /// Returns `false` for reasons that won't improve on retry
    /// (DRM, Password, Corrupt, Unsupported); `true` for transient ones.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout | Self::Other)
    }

    pub fn as_tag(&self) -> &'static str {
        match self {
            Self::Timeout      => "timeout",
            Self::Drm          => "drm",
            Self::Password     => "password",
            Self::Corrupt      => "corrupt",
            Self::Unsupported  => "unsupported",
            Self::Other        => "other",
        }
    }
}

/// Detect EPUB DRM by checking whether the zip contains
/// `META-INF/encryption.xml`.  Works even when the XHTML chapters are
/// fully encrypted, because the OPF directory listing is always plain-text.
pub fn epub_is_drm_protected(path: &std::path::Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else { return false; };
    let Ok(archive) = zip::ZipArchive::new(file) else { return false; };
    let found = archive.file_names().any(|n| n == "META-INF/encryption.xml");
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn classify_recognises_drm_keywords() {
        assert_eq!(TaskFailureReason::classify("META-INF/encryption.xml not readable"), TaskFailureReason::Drm);
        assert_eq!(TaskFailureReason::classify("Adobe ADEPT-protected file"),           TaskFailureReason::Drm);
        assert_eq!(TaskFailureReason::classify("Apple FairPlay encryption detected"),    TaskFailureReason::Drm);
        assert_eq!(TaskFailureReason::classify("DRM check failed"),                      TaskFailureReason::Drm);
        // "encrypt" without "password" → DRM.
        assert_eq!(TaskFailureReason::classify("file is encrypted with AES"),            TaskFailureReason::Drm);
    }

    #[test]
    fn classify_distinguishes_password_from_drm() {
        // "password" beats "encrypt" — PDF user-passwords are recoverable.
        assert_eq!(TaskFailureReason::classify("PDF requires user-password"),  TaskFailureReason::Password);
        assert_eq!(TaskFailureReason::classify("encrypted with password"),     TaskFailureReason::Password);
        assert_eq!(TaskFailureReason::classify("password required"),           TaskFailureReason::Password);
    }

    #[test]
    fn classify_unsupported_extensions() {
        assert_eq!(TaskFailureReason::classify("no extractor for .xyz"),       TaskFailureReason::Unsupported);
        assert_eq!(TaskFailureReason::classify("Unsupported file format"),     TaskFailureReason::Unsupported);
    }

    #[test]
    fn classify_falls_back_to_corrupt() {
        assert_eq!(TaskFailureReason::classify("xref table malformed"),        TaskFailureReason::Corrupt);
        assert_eq!(TaskFailureReason::classify("unexpected EOF"),              TaskFailureReason::Corrupt);
        assert_eq!(TaskFailureReason::classify(""),                            TaskFailureReason::Corrupt);
    }

    #[test]
    fn classify_is_case_insensitive() {
        assert_eq!(TaskFailureReason::classify("ADEPT"),                       TaskFailureReason::Drm);
        assert_eq!(TaskFailureReason::classify("PASSWORD"),                    TaskFailureReason::Password);
        assert_eq!(TaskFailureReason::classify("UNSUPPORTED"),                 TaskFailureReason::Unsupported);
    }

    #[test]
    fn is_retryable_only_timeout_and_other() {
        assert!( TaskFailureReason::Timeout.is_retryable());
        assert!( TaskFailureReason::Other.is_retryable());
        assert!(!TaskFailureReason::Drm.is_retryable());
        assert!(!TaskFailureReason::Password.is_retryable());
        assert!(!TaskFailureReason::Corrupt.is_retryable());
        assert!(!TaskFailureReason::Unsupported.is_retryable());
    }

    #[test]
    fn as_tag_matches_serde_kebab_case() {
        // The `serde(rename_all = "snake_case")` derive must produce these
        // exact strings — they're what gets persisted in metadata_json.
        for r in [
            TaskFailureReason::Timeout, TaskFailureReason::Drm,
            TaskFailureReason::Password, TaskFailureReason::Corrupt,
            TaskFailureReason::Unsupported, TaskFailureReason::Other,
        ] {
            let serde_str = serde_json::to_value(&r).unwrap()
                .as_str().unwrap().to_owned();
            assert_eq!(r.as_tag(), serde_str,
                "as_tag and serde must agree for {r:?}");
        }
    }

    #[test]
    fn epub_drm_detector_returns_false_for_missing_file() {
        let p = std::path::Path::new("/nonexistent/path/foo.epub");
        assert!(!epub_is_drm_protected(p));
    }

    #[test]
    fn epub_drm_detector_returns_false_for_non_zip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"not a zip file").unwrap();
        assert!(!epub_is_drm_protected(tmp.path()));
    }

    #[test]
    fn epub_drm_detector_returns_false_for_clean_epub() {
        // Build a minimal zip with no encryption.xml.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let file = std::fs::File::create(tmp.path()).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("META-INF/container.xml", SimpleFileOptions::default()).unwrap();
            zip.write_all(b"<container/>").unwrap();
            zip.finish().unwrap();
        }
        assert!(!epub_is_drm_protected(tmp.path()));
    }

    #[test]
    fn epub_drm_detector_returns_true_when_encryption_xml_present() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        {
            let file = std::fs::File::create(tmp.path()).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("META-INF/encryption.xml", SimpleFileOptions::default()).unwrap();
            zip.write_all(b"<encryption/>").unwrap();
            zip.start_file("OEBPS/content.opf", SimpleFileOptions::default()).unwrap();
            zip.write_all(b"<package/>").unwrap();
            zip.finish().unwrap();
        }
        assert!(epub_is_drm_protected(tmp.path()));
    }
}
