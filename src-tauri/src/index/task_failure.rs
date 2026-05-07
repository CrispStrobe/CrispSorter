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
    let Ok(mut archive) = zip::ZipArchive::new(file) else { return false; };
    let found = archive.file_names().any(|n| n == "META-INF/encryption.xml");
    found
}
