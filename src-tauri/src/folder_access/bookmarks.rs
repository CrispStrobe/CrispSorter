//! macOS security-scoped bookmarks — the only way a sandboxed build can
//! keep write access to a folder the user picked.
//!
//! The App Store SKU ships with `files.user-selected.read-write` and
//! nothing broader, so it may write exactly where the user has pointed a
//! file dialog and nowhere else. That grant normally dies with the
//! process. A *security-scoped bookmark* is the mechanism for persisting
//! it: the app asks `NSURL` for an opaque blob while it still holds
//! access, stores the blob, and on a later launch resolves the blob back
//! into a URL and calls `startAccessingSecurityScopedResource`.
//!
//! Three things make this easy to get wrong, so they are worth stating:
//!
//! * It needs the **`com.apple.security.files.bookmarks.app-scope`**
//!   entitlement. Without it `bookmarkDataWithOptions:` fails at
//!   creation time, not at resolution time, which makes it look like the
//!   folder is at fault.
//! * Access must be **started and kept started**. The `Retained<NSURL>`
//!   has to outlive every file operation under that folder, so
//!   [`Registry`] holds them for the life of the process.
//! * A bookmark can go **stale** (the folder moved or was renamed).
//!   Resolution still succeeds and sets a stale flag; the honest
//!   response is to ask the user to pick the folder again rather than
//!   pretend the grant still means something.
//!
//! Everything here is a no-op returning `Unsupported` off macOS, so
//! callers need no `cfg`.

use std::path::{Path, PathBuf};

/// Why a bookmark operation could not be completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkError {
    /// Not macOS — there is nothing to bookmark.
    Unsupported,
    /// The platform refused. Carries Foundation's own words.
    Failed(String),
    /// Resolved, but the folder has moved since; the user must re-pick.
    Stale,
}

impl std::fmt::Display for BookmarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BookmarkError::Unsupported => {
                write!(f, "security-scoped bookmarks exist only on macOS")
            }
            BookmarkError::Failed(s) => write!(f, "{s}"),
            BookmarkError::Stale => write!(
                f,
                "the saved permission no longer points at that folder — it was moved or renamed"
            ),
        }
    }
}

impl std::error::Error for BookmarkError {}

/// Create a security-scoped bookmark for `dir`.
///
/// Must be called while the app still has access — i.e. right after the
/// user picked the folder in a dialog. Later is too late.
pub fn create(dir: &Path) -> Result<Vec<u8>, BookmarkError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = dir;
        Err(BookmarkError::Unsupported)
    }
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSString, NSURL, NSURLBookmarkCreationOptions};
        let path = NSString::from_str(&dir.to_string_lossy());
        // SAFETY: `fileURLWithPath:` takes an NSString and returns a URL;
        // `bookmarkDataWithOptions:` reads it. No aliasing, no ownership
        // transfer beyond the Retained values objc2 manages.
        unsafe {
            let url = NSURL::fileURLWithPath(&path);
            match url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::WithSecurityScope,
                None,
                None,
            ) {
                Ok(data) => Ok(data.to_vec()),
                Err(e) => Err(BookmarkError::Failed(format!(
                    "could not save permission for {}: {}",
                    dir.display(),
                    e.localizedDescription()
                ))),
            }
        }
    }
}

/// A resolved, actively-accessed folder grant.
///
/// Dropping it stops access, which is why [`Registry`] keeps them.
pub struct Access {
    pub path: PathBuf,
    pub stale: bool,
    #[cfg(target_os = "macos")]
    url: objc2::rc::Retained<objc2_foundation::NSURL>,
}

impl Access {
    /// The path the bookmark actually resolved to, which can differ from
    /// the path it was created for when the folder has been moved.
    pub fn resolved_path(&self) -> &Path {
        &self.path
    }
}

#[cfg(target_os = "macos")]
impl Drop for Access {
    fn drop(&mut self) {
        // SAFETY: paired with the `startAccessingSecurityScopedResource`
        // in `resolve`; macOS reference-counts these per URL.
        unsafe { self.url.stopAccessingSecurityScopedResource() };
    }
}

/// Resolve a stored bookmark and begin accessing the folder.
///
/// The returned [`Access`] must be held for as long as the app wants to
/// write there.
pub fn resolve(blob: &[u8]) -> Result<Access, BookmarkError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = blob;
        Err(BookmarkError::Unsupported)
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::runtime::Bool;
        use objc2_foundation::{NSData, NSURLBookmarkResolutionOptions, NSURL};
        let data = NSData::with_bytes(blob);
        let mut stale = Bool::NO;
        // SAFETY: `is_stale` is a valid out-pointer for the duration of
        // the call; the other arguments are borrowed for the call only.
        unsafe {
            let url = NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &data,
                NSURLBookmarkResolutionOptions::WithSecurityScope,
                None,
                &mut stale,
            )
            .map_err(|e| {
                BookmarkError::Failed(format!(
                    "could not restore a saved folder permission: {}",
                    e.localizedDescription()
                ))
            })?;

            if !url.startAccessingSecurityScopedResource() {
                return Err(BookmarkError::Failed(
                    "macOS declined to reopen a saved folder permission".to_string(),
                ));
            }
            let path = url
                .path()
                .map(|p| PathBuf::from(p.to_string()))
                .unwrap_or_default();
            Ok(Access {
                path,
                stale: stale.as_bool(),
                url,
            })
        }
    }
}

/// Deepest paths first, so the most specific grant is considered first.
pub fn most_specific_first(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
    v.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    v
}

/// Is `dir` equal to, or beneath, any of `grants`?
///
/// Compares path *components*, not bytes: `starts_with` on `Path` already
/// does that, and it is the difference between `/a/texte` covering
/// `/a/texte/Sorted` (it does) and covering `/a/texte-backup` (it must
/// not).
pub fn covered_by(grants: &[PathBuf], dir: &Path) -> bool {
    grants.iter().any(|g| dir.starts_with(g))
}

/// Every grant this process currently holds open.
///
/// Exists to own the [`Access`] values: a grant that is dropped stops
/// working, and the bug that produces is a folder that was writable a
/// moment ago and silently is not any more.
#[derive(Default)]
pub struct Registry {
    held: Vec<Access>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hold(&mut self, access: Access) {
        self.held.push(access);
    }

    /// Paths currently held open, most specific first.
    pub fn paths(&self) -> Vec<PathBuf> {
        most_specific_first(self.held.iter().map(|a| a.path.clone()).collect())
    }

    /// Is `dir` inside a folder we hold a grant for?
    ///
    /// A grant covers descendants, which is the whole reason picking one
    /// parent folder is worth doing: it buys every destination beneath
    /// it, not just the directory itself.
    pub fn covers(&self, dir: &Path) -> bool {
        covered_by(&self.held.iter().map(|a| a.path.clone()).collect::<Vec<_>>(), dir)
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the free functions rather than a fabricated
    // `Access`: building one outside `resolve` would give it a `Drop`
    // that stops an access which was never started.

    #[test]
    fn a_grant_covers_its_descendants() {
        // The point of asking for one parent folder rather than per target.
        let grants = vec![PathBuf::from("/Users/x/Documents/texte")];
        assert!(covered_by(&grants, Path::new("/Users/x/Documents/texte")));
        assert!(covered_by(
            &grants,
            Path::new("/Users/x/Documents/texte/Sorted/Jüster, Markus (Hrsg.)/2023")
        ));
        assert!(!covered_by(&grants, Path::new("/Users/x/Documents/andere")));
    }

    #[test]
    fn a_sibling_sharing_a_prefix_is_not_covered() {
        // A byte-prefix check would call this covered, and the user would
        // be told a grant worked when it bought nothing.
        let grants = vec![PathBuf::from("/a/texte")];
        assert!(!covered_by(&grants, Path::new("/a/texte-backup")));
    }

    #[test]
    fn a_descendant_does_not_cover_its_parent() {
        let grants = vec![PathBuf::from("/a/texte/Sorted")];
        assert!(!covered_by(&grants, Path::new("/a/texte")));
    }

    #[test]
    fn more_specific_grants_sort_first() {
        assert_eq!(
            most_specific_first(vec![
                PathBuf::from("/a"),
                PathBuf::from("/a/b/c"),
                PathBuf::from("/a/b"),
            ]),
            vec![
                PathBuf::from("/a/b/c"),
                PathBuf::from("/a/b"),
                PathBuf::from("/a"),
            ]
        );
    }

    #[test]
    fn an_empty_registry_covers_nothing() {
        assert!(!covered_by(&[], Path::new("/anywhere")));
        assert!(Registry::new().is_empty());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_everything_is_unsupported_rather_than_a_lie() {
        assert_eq!(create(Path::new("/tmp")), Err(BookmarkError::Unsupported));
        assert!(matches!(resolve(b"x"), Err(BookmarkError::Unsupported)));
    }
}
