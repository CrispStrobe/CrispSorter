//! Lexical local paths carried by indexed source URIs.
//! Decode once; callers decide whether filesystem canonicalization is appropriate.
use std::path::{Path, PathBuf};

fn windows_absolute(value: &str) -> bool {
    let value = value.strip_prefix('/').filter(|value| value.as_bytes().get(1) == Some(&b':')).unwrap_or(value);
    (value.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && value.as_bytes().get(1) == Some(&b':')
        && matches!(value.as_bytes().get(2), Some(b'/' | b'\\')))
        || value.starts_with("\\\\") || value.starts_with("//")
}

fn local_path_candidates(uri: &str) -> Vec<String> {
    if uri.contains('\0') { return vec![]; }
    let (raw, encoded) = if let Some(rest) = uri.strip_prefix("crisp+local://") {
        let Some(slash) = rest.find('/') else { return vec![]; };
        (rest[slash..].to_owned(), true)
    } else if let Some(rest) = uri.strip_prefix("file://") {
        if let Some(path) = rest.strip_prefix("localhost/") { (format!("/{path}"), true) }
        else if rest.starts_with('/') || windows_absolute(rest) { (rest.to_owned(), true) }
        else if rest.contains('/') { (format!("//{rest}"), true) }
        else { return vec![]; }
    } else {
        if !uri.starts_with('/') && !windows_absolute(uri) { return vec![]; }
        (uri.to_owned(), false)
    };
    let mut paths = vec![raw.clone()];
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
        if let Ok(decoded) = String::from_utf8(bytes) { paths.push(decoded); }
    }
    paths
}

/// Native filesystem paths used only by the desktop's canonical source guard.
pub fn local_source_paths(uri: &str) -> Vec<PathBuf> {
    local_path_candidates(uri).iter().map(|raw| local_path(raw)).collect()
}

fn local_path(raw: &str) -> PathBuf {
    #[cfg(windows)]
    let raw = if raw.starts_with('/') && raw.as_bytes().get(2) == Some(&b':') { &raw[1..] } else { raw };
    Path::new(raw).to_owned()
}

/// Comparable aliases independent of the server operating system. Windows
/// paths use case-insensitive separator-normalized comparison conservatively;
/// POSIX paths preserve case. No filesystem or second URI decode is involved.
pub fn lexical_path_aliases(path: &str) -> Vec<String> {
    if path.contains('\0') { return vec![]; }
    let mut path = path.to_owned();
    // Extended-length Windows spellings refer to the same ordinary path.
    if let Some(rest) = path.strip_prefix("\\\\?\\UNC\\") { path = format!("//{rest}"); }
    else if let Some(rest) = path.strip_prefix("\\\\?\\") { path = rest.to_owned(); }
    let windows = windows_absolute(&path);
    if windows {
        path = path.replace('\\', "/");
        if path.starts_with('/') && path.as_bytes().get(2) == Some(&b':') { path.remove(0); }
        path = path.to_lowercase();
    } else if !path.starts_with('/') { return vec![]; }
    let (root, rest) = if windows && path.as_bytes().get(1) == Some(&b':') {
        (path[..3].to_owned(), &path[3..])
    } else if windows && path.starts_with("//") { ("//".to_owned(), &path[2..]) }
    else { ("/".to_owned(), path.trim_start_matches('/')) };
    let protected_depth = usize::from(windows && root == "//") * 2;
    let mut parts = Vec::new();
    for part in rest.split('/') {
        match part { "" | "." => {}, ".." => { if parts.len() > protected_depth { parts.pop(); } }, value => parts.push(value) }
    }
    vec![format!("{root}{}", parts.join("/"))]
}

/// Server-side collision checks compare these strings with aliases generated
/// from the desktop's raw and canonical destination paths, never server paths.
pub fn source_path_aliases(uri: &str) -> Vec<String> {
    let mut aliases = std::collections::BTreeSet::new();
    for path in local_path_candidates(uri) { aliases.extend(lexical_path_aliases(&path)); }
    aliases.into_iter().collect()
}

/// Recorded source parent for ingestion and legacy metadata backfill. Preserve
/// the source's case and separators; never use the server's working directory.
pub fn source_parent_directory(uri: &str) -> Option<String> {
    let candidates = local_path_candidates(uri);
    let mut path = candidates.last()?.as_str();
    let windows = windows_absolute(path);
    if windows && path.starts_with('/') && path.as_bytes().get(2) == Some(&b':') { path = &path[1..]; }
    let slash = path.rfind(|ch| ch == '/' || (windows && ch == '\\'))?;
    let end = if slash == 0 || (windows && slash == 2 && path.as_bytes().get(1) == Some(&b':')) { slash + 1 } else { slash };
    Some(path[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn collides(source: &str, destination: &str) -> bool {
        source_path_aliases(source).iter().any(|source| lexical_path_aliases(destination).contains(source))
    }
    #[test]
    fn windows_drive_and_network_paths_match_on_any_host() {
        for source in [r"C:\Docs\Source File.pdf", "file:///C:/Docs/Source%20File.pdf", "crisp+local://owner@machine/C:/Docs/Source%20File.pdf"] {
            assert!(collides(source, r"c:\docs\source file.pdf"), "{source}");
        }
        for source in [r"\\server\Share\Doc.pdf", "file://server/Share/Doc.pdf", "file:////server/Share/Doc.pdf"] {
            assert!(collides(source, r"\\?\UNC\SERVER\share\doc.pdf"), "{source}");
        }
        assert!(collides(r"\\?\C:\Docs\file.pdf", "c:/docs/file.pdf"));
    }
    #[test]
    fn posix_paths_preserve_case_and_decode_only_once() {
        assert!(collides("crisp+local://host/home/me/source%20file.txt", "/home/me/source file.txt"));
        assert!(collides("file://localhost/home/me/source.txt", "/home/me/./source.txt"));
        assert!(!collides("file:///home/me/Source.txt", "/home/me/source.txt"));
        assert!(!collides("file:///home/me/%2520.txt", "/home/me/ .txt"));
        assert!(collides("file:///home/me/%2520.txt", "/home/me/%20.txt"));
        assert!(!collides("file:///home/me/%00.txt", "/home/me/\0.txt"));
        assert!(!collides("https://example.test/home/me/source.txt", "/home/me/source.txt"));
    }
    #[test]
    fn recorded_parent_metadata_is_host_independent_and_decoded_once() {
        for (uri, parent) in [
            ("crisp+local://owner@machine/Research/Folder%20Name/file.pdf", "/Research/Folder Name"),
            ("file:///C:/Research/Folder%20Name/file.pdf", "C:/Research/Folder Name"),
            (r"C:\Research\File.pdf", r"C:\Research"),
            (r"C:\File.pdf", r"C:\"),
            (r"\\server\Share\Folder\File.pdf", r"\\server\Share\Folder"),
            ("file://server/Share/Folder/file.pdf", "//server/Share/Folder"),
            ("file:///Research/%2520/file.pdf", "/Research/%20"),
            ("/file.pdf", "/"),
        ] { assert_eq!(source_parent_directory(uri).as_deref(), Some(parent), "{uri}"); }
        assert_eq!(source_parent_directory("https://example.test/folder/file.pdf"), None);
    }
}
