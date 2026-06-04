//! Mobile filesystem helpers — Android SAF + iOS security-scoped bookmarks.
//!
//! On Android, the Storage Access Framework (SAF) returns `content://` URIs
//! that can't be addressed via normal `std::fs` paths.  This module provides
//! Tauri commands that bridge the gap:
//!
//!   * `mobile_fs_list_folder`  — list children of a tree URI
//!   * `mobile_fs_read_file`    — read bytes from a document URI
//!   * `mobile_fs_move_file`    — move a document within a tree
//!   * `mobile_fs_create_dir`   — create a subfolder in a tree
//!   * `mobile_fs_delete`       — delete a document
//!
//! On iOS, security-scoped bookmarks grant persistent folder access.
//! The commands wrap `startAccessingSecurityScopedResource()` /
//! `stopAccessingSecurityScopedResource()` so the Rust side can do
//! normal file I/O within the bookmark scope.
//!
//! On desktop, these commands are no-ops (the normal `std::fs` works).

pub mod tauri_commands {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Clone, Debug)]
    pub struct MobileFileEntry {
        /// content:// URI (Android) or file:// URL (iOS) or file path (desktop)
        pub uri: String,
        pub display_name: String,
        pub mime_type: String,
        pub size: i64,
        pub is_directory: bool,
    }

    /// List children of a folder.
    ///
    /// On Android: uses ContentResolver to query DocumentsContract children.
    /// On iOS/desktop: falls back to std::fs::read_dir.
    #[tauri::command]
    pub async fn mobile_fs_list_folder(uri: String) -> Result<Vec<MobileFileEntry>, String> {
        #[cfg(target_os = "android")]
        {
            android_list_folder(&uri).await
        }
        #[cfg(not(target_os = "android"))]
        {
            fallback_list_folder(&uri).await
        }
    }

    /// Read a file's bytes.
    ///
    /// On Android: opens content:// URI via ContentResolver.
    /// On iOS/desktop: falls back to std::fs::read.
    #[tauri::command]
    pub async fn mobile_fs_read_file(uri: String) -> Result<Vec<u8>, String> {
        #[cfg(target_os = "android")]
        {
            android_read_file(&uri).await
        }
        #[cfg(not(target_os = "android"))]
        {
            tokio::fs::read(&uri).await.map_err(|e| format!("read failed: {e}"))
        }
    }

    /// Move a document within a tree URI scope (same provider).
    ///
    /// On Android: uses DocumentsContract.moveDocument (API 24+).
    /// On iOS/desktop: falls back to std::fs::rename.
    #[tauri::command]
    pub async fn mobile_fs_move_file(
        source_uri: String,
        source_parent_uri: String,
        target_parent_uri: String,
    ) -> Result<String, String> {
        #[cfg(target_os = "android")]
        {
            android_move_file(&source_uri, &source_parent_uri, &target_parent_uri).await
        }
        #[cfg(not(target_os = "android"))]
        {
            // Desktop fallback: source_uri and target_parent_uri are file paths
            let src = std::path::Path::new(&source_uri);
            let dst_dir = std::path::Path::new(&target_parent_uri);
            let dst = dst_dir.join(src.file_name().unwrap_or_default());
            tokio::fs::rename(&src, &dst).await.map_err(|e| format!("move failed: {e}"))?;
            Ok(dst.to_string_lossy().to_string())
        }
    }

    /// Create a subfolder within a tree URI.
    ///
    /// On Android: uses DocumentsContract.createDocument with MIME_TYPE_DIR.
    /// On iOS/desktop: falls back to std::fs::create_dir_all.
    #[tauri::command]
    pub async fn mobile_fs_create_dir(
        parent_uri: String,
        name: String,
    ) -> Result<String, String> {
        #[cfg(target_os = "android")]
        {
            android_create_dir(&parent_uri, &name).await
        }
        #[cfg(not(target_os = "android"))]
        {
            let dir = std::path::Path::new(&parent_uri).join(&name);
            tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("mkdir failed: {e}"))?;
            Ok(dir.to_string_lossy().to_string())
        }
    }

    /// Delete a document.
    #[tauri::command]
    pub async fn mobile_fs_delete(uri: String) -> Result<(), String> {
        #[cfg(target_os = "android")]
        {
            android_delete(&uri).await
        }
        #[cfg(not(target_os = "android"))]
        {
            let p = std::path::Path::new(&uri);
            if p.is_dir() {
                tokio::fs::remove_dir_all(p).await
            } else {
                tokio::fs::remove_file(p).await
            }
            .map_err(|e| format!("delete failed: {e}"))
        }
    }

    // ── iOS security-scoped bookmark support ──────────────────────────────

    /// Start accessing a security-scoped resource (iOS).
    /// No-op on other platforms.
    #[tauri::command]
    pub async fn mobile_fs_start_access(uri: String) -> Result<bool, String> {
        #[cfg(target_os = "ios")]
        {
            ios_start_access(&uri).await
        }
        #[cfg(not(target_os = "ios"))]
        {
            let _ = uri;
            Ok(true)
        }
    }

    /// Stop accessing a security-scoped resource (iOS).
    /// No-op on other platforms.
    #[tauri::command]
    pub async fn mobile_fs_stop_access(uri: String) -> Result<(), String> {
        #[cfg(target_os = "ios")]
        {
            ios_stop_access(&uri).await
        }
        #[cfg(not(target_os = "ios"))]
        {
            let _ = uri;
            Ok(())
        }
    }

    // ── Android JNI implementations ───────────────────────────────────────

    #[cfg(target_os = "android")]
    async fn android_list_folder(uri: &str) -> Result<Vec<MobileFileEntry>, String> {
        // On Android, Tauri commands run in a Rust thread.  We call into
        // the JVM via the `jni` crate to use ContentResolver.  The JNI
        // env is obtained from the cached JavaVM that Tauri stores.
        //
        // For v0.4 we use Tauri's Android plugin bridge: invoke a Kotlin
        // helper via `tauri::plugin::mobile::PluginInvokeResolver`.
        // The actual Kotlin implementation lives in
        // gen/android/app/src/main/java/.../SAFBridge.kt and is called
        // via the Tauri mobile plugin invoke mechanism.
        //
        // Fallback: if the URI looks like a file:// path, use std::fs.
        if uri.starts_with('/') || uri.starts_with("file://") {
            return fallback_list_folder(uri).await;
        }

        // TODO: wire JNI call to SAFBridge.listFolder(uri)
        // For now, return an error prompting the user to use the Tauri
        // dialog plugin for folder selection (which returns file paths
        // on newer Android versions with MANAGE_EXTERNAL_STORAGE).
        Err(format!("SAF content:// listing not yet wired via JNI — use the folder picker dialog"))
    }

    #[cfg(target_os = "android")]
    async fn android_read_file(uri: &str) -> Result<Vec<u8>, String> {
        if uri.starts_with('/') || uri.starts_with("file://") {
            let path = uri.strip_prefix("file://").unwrap_or(uri);
            return tokio::fs::read(path).await.map_err(|e| format!("read failed: {e}"));
        }
        Err("SAF content:// read not yet wired via JNI".to_string())
    }

    #[cfg(target_os = "android")]
    async fn android_move_file(
        _source_uri: &str,
        _source_parent_uri: &str,
        _target_parent_uri: &str,
    ) -> Result<String, String> {
        Err("SAF content:// move not yet wired via JNI".to_string())
    }

    #[cfg(target_os = "android")]
    async fn android_create_dir(_parent_uri: &str, _name: &str) -> Result<String, String> {
        Err("SAF content:// createDir not yet wired via JNI".to_string())
    }

    #[cfg(target_os = "android")]
    async fn android_delete(_uri: &str) -> Result<(), String> {
        Err("SAF content:// delete not yet wired via JNI".to_string())
    }

    // ── iOS implementations ───────────────────────────────────────────────

    #[cfg(target_os = "ios")]
    async fn ios_start_access(url: &str) -> Result<bool, String> {
        // On iOS, security-scoped URLs from UIDocumentPickerViewController
        // need startAccessingSecurityScopedResource() before any file I/O.
        // This is called via objc2 FFI to Foundation's NSURL.
        //
        // TODO: wire objc2 call to [NSURL startAccessingSecurityScopedResource]
        let _ = url;
        Err("iOS security-scoped access not yet wired via objc2".to_string())
    }

    #[cfg(target_os = "ios")]
    async fn ios_stop_access(url: &str) -> Result<(), String> {
        let _ = url;
        Err("iOS security-scoped access not yet wired via objc2".to_string())
    }

    // ── Desktop / fallback ────────────────────────────────────────────────

    async fn fallback_list_folder(path: &str) -> Result<Vec<MobileFileEntry>, String> {
        let clean = path.strip_prefix("file://").unwrap_or(path);
        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(clean)
            .await
            .map_err(|e| format!("read_dir failed: {e}"))?;
        while let Some(entry) = dir.next_entry().await.map_err(|e| format!("{e}"))? {
            let meta = entry.metadata().await.ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len() as i64).unwrap_or(-1);
            let name = entry.file_name().to_string_lossy().to_string();
            let mime = if is_dir {
                "inode/directory".to_string()
            } else {
                mime_from_ext(&name)
            };
            entries.push(MobileFileEntry {
                uri: entry.path().to_string_lossy().to_string(),
                display_name: name,
                mime_type: mime,
                size,
                is_directory: is_dir,
            });
        }
        Ok(entries)
    }

    fn mime_from_ext(name: &str) -> String {
        match name.rsplit('.').next().map(|s| s.to_lowercase()).as_deref() {
            Some("pdf") => "application/pdf",
            Some("txt") => "text/plain",
            Some("md") => "text/markdown",
            Some("html" | "htm") => "text/html",
            Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Some("epub") => "application/epub+zip",
            Some("mp3") => "audio/mpeg",
            Some("m4a") => "audio/mp4",
            Some("wav") => "audio/wav",
            Some("flac") => "audio/flac",
            Some("ogg") => "audio/ogg",
            Some("mp4") => "video/mp4",
            Some("mkv") => "video/x-matroska",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("png") => "image/png",
            Some("webp") => "image/webp",
            _ => "application/octet-stream",
        }
        .to_string()
    }
}
