//! Print and native share (P32.2).
//!
//! Desktop implementations now; the seam is shaped so an iOS one drops in
//! without touching callers. Everything goes through [`PlatformOps`], so
//! the editor calls `share_file` / `print_file` and never learns which
//! platform it is on.
//!
//! ## Why a trait rather than `cfg!` at the call site
//!
//! The iOS versions (`UIActivityViewController`, `UIPrintInteraction-
//! Controller`) need a view controller and must run on the main thread,
//! which is a different shape of call from spawning `lp`. Keeping that
//! behind one interface means the mobile port is a new impl block, not a
//! sweep through the UI code.
//!
//! ## Capability reporting
//!
//! Platforms differ in what they can actually do, and a button that
//! silently does nothing is worse than one that is absent. [`capabilities`]
//! tells the frontend what to render.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareKind {
    /// A real system share sheet (AirDrop, Mail, Messages…).
    SystemSheet,
    /// No share sheet available; the file was revealed in the file
    /// manager so the user can drag or send it themselves.
    RevealedInFileManager,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareResult {
    pub kind: ShareKind,
    /// Present when the platform could not offer a real share sheet.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCapabilities {
    /// A real system share sheet exists.
    pub system_share_sheet: bool,
    /// Printing can be dispatched without leaving the app.
    pub direct_print: bool,
    /// Human-readable platform name, for diagnostics.
    pub platform: String,
}

pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        system_share_sheet: cfg!(target_os = "macos"),
        // Printing is `lp` / the Windows Print verb — a spawn on every
        // platform, so a build without `sidecars` has no print path at
        // all and must say so (P36.2). This is the mechanism that keeps
        // the button off the sandboxed build rather than letting it error.
        direct_print: cfg!(all(
            feature = "sidecars",
            any(target_os = "macos", target_os = "linux", target_os = "windows")
        )),
        platform: std::env::consts::OS.to_string(),
    }
}

fn ensure_exists(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("no such file: {}", path.display()));
    }
    Ok(())
}

// ── Printing ───────────────────────────────────────────────────────────

/// Send a file to the default printer.
///
/// Unix goes through CUPS `lp`; Windows through the shell's `Print` verb.
/// Neither shows a print dialog — that needs a UI toolkit call, and on
/// macOS specifically an `NSPrintOperation` against a rendered document.
/// [`open_in_default_app`] is the escape hatch when the user wants the
/// dialog: their PDF viewer will have one.
///
/// Every implementation here runs another program, so the whole function
/// is `sidecars`-gated (P36.2). Sandboxed printing is a genuinely
/// different shape of call — `NSPrintOperation` against a rendered
/// document, on the main thread — and belongs behind this same
/// [`PlatformOps`] seam when it is written, not bolted onto `lp`.
/// [`capabilities`] reports `direct_print: false` meanwhile, so the UI
/// hides the button rather than offering one that errors.
#[cfg(feature = "sidecars")]
pub fn print_file(path: &Path) -> Result<String, String> {
    ensure_exists(path)?;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let out = std::process::Command::new("lp")
            .arg(path)
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "no `lp` command found — CUPS does not appear to be installed".to_string()
                } else {
                    format!("failed to run lp: {e}")
                }
            })?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // The common case by far is simply having no printer set up;
            // say so rather than echoing a bare exit code.
            if err.contains("no default destination") || err.contains("No default") {
                return Err("no default printer is configured".into());
            }
            return Err(if err.is_empty() {
                format!("lp exited with status {}", out.status)
            } else {
                err
            });
        }
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }

    #[cfg(target_os = "windows")]
    {
        // Start-Process -Verb Print hands the file to whatever is
        // registered for it, which for a PDF is the user's reader.
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "Start-Process", "-FilePath"])
            .arg(path)
            .args(["-Verb", "Print"])
            .output()
            .map_err(|e| format!("failed to invoke print verb: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        return Ok("sent to the default printer".into());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("printing is not implemented on this platform".to_string())
    }
}

/// Hand the file to the OS default application.
///
/// Doubles as the "print with a dialog" path: the user's PDF viewer has
/// one, and this is the only way to reach it without embedding a print
/// panel ourselves.
///
/// ## macOS goes through Launch Services, not `open(1)` (P36.2)
///
/// `NSWorkspace.openURL:` asks the window server to open the file; App
/// Sandbox permits it precisely because the app is not the one executing
/// anything. `Command::new("open")` — and, note, `tauri_plugin_opener::
/// open_path`, which looks like the sandbox-safe choice but delegates to
/// the `open` crate and so runs `/usr/bin/open` — is a fork/exec of a
/// binary outside the bundle, which the sandbox denies. Same user-visible
/// behaviour, and it is now the path on every macOS build rather than a
/// second one to keep in sync.
pub fn open_in_default_app(path: &Path) -> Result<(), String> {
    ensure_exists(path)?;

    #[cfg(target_os = "macos")]
    {
        return mac_workspace::open(path);
    }

    #[cfg(all(feature = "sidecars", not(target_os = "macos")))]
    {
        let cmd = if cfg!(target_os = "windows") { "explorer" } else { "xdg-open" };
        std::process::Command::new(cmd)
            .arg(path)
            .spawn()
            .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        Ok(())
    }

    #[cfg(all(not(feature = "sidecars"), not(target_os = "macos")))]
    {
        Err(format!(
            "opening {} in another application needs the `sidecars` feature \
             on this platform",
            path.display()
        ))
    }
}

/// Show the file in the platform's file manager, selected.
pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    ensure_exists(path)?;
    #[cfg(target_os = "macos")]
    {
        // Finder reveal, same Launch Services route as `open_in_default_app`.
        return mac_workspace::reveal(path);
    }
    #[cfg(all(feature = "sidecars", target_os = "windows"))]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map_err(|e| format!("failed to reveal: {e}"))?;
        return Ok(());
    }
    #[cfg(all(feature = "sidecars", not(any(target_os = "macos", target_os = "windows"))))]
    {
        // No portable "select this file" on Linux; open the directory.
        let dir = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map_err(|e| format!("failed to open folder: {e}"))?;
        Ok(())
    }
    #[cfg(all(not(feature = "sidecars"), not(target_os = "macos")))]
    {
        Err(format!(
            "revealing {} in the file manager needs the `sidecars` feature \
             on this platform",
            path.display()
        ))
    }
}

/// Launch Services, reached through AppKit rather than through `open(1)`.
///
/// Both calls are `NSWorkspace` messages: they hand a URL to the window
/// server and return. Nothing is spawned by this process, which is what
/// makes them usable inside the sandbox — and they are already on the
/// objc2-app-kit dependency the share sheet below needs, so this costs no
/// new crate.
#[cfg(target_os = "macos")]
mod mac_workspace {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSArray, NSString, NSURL};
    use std::path::Path;

    fn file_url(path: &Path) -> objc2::rc::Retained<NSURL> {
        NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
    }

    pub fn open(path: &Path) -> Result<(), String> {
        // `openURL:` returns false when Launch Services has no handler for
        // the type — a real outcome the user should hear about, not a
        // silent no-op.
        if NSWorkspace::new().openURL(&file_url(path)) {
            Ok(())
        } else {
            Err(format!(
                "macOS has no application registered to open {}",
                path.display()
            ))
        }
    }

    pub fn reveal(path: &Path) -> Result<(), String> {
        let urls = NSArray::from_retained_slice(&[file_url(path)]);
        NSWorkspace::new().activateFileViewerSelectingURLs(&urls);
        Ok(())
    }
}

// ── Sharing ────────────────────────────────────────────────────────────

/// macOS: the real share sheet, anchored to the app window.
#[cfg(target_os = "macos")]
mod mac_share {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::AnyThread; // brings `alloc` into scope
    use objc2_app_kit::{NSSharingServicePicker, NSView, NSWindow};
    // NSRectEdge lives in Foundation's geometry module, not AppKit.
    use objc2_foundation::{NSArray, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL};
    use std::path::Path;

    /// Show `NSSharingServicePicker` for `path`, anchored to `ns_window`.
    ///
    /// # Safety
    /// `ns_window` must be a live `NSWindow` pointer, and this must be
    /// called on the main thread — AppKit requires both.
    pub unsafe fn show(ns_window: *mut std::ffi::c_void, path: &Path) -> Result<(), String> {
        if ns_window.is_null() {
            return Err("no application window to anchor the share sheet to".into());
        }
        let path_str = NSString::from_str(&path.to_string_lossy());
        let url: Retained<NSURL> = NSURL::fileURLWithPath(&path_str);

        // The picker takes an untyped array of shareable items.
        let item: &AnyObject = &url;
        let items: Retained<NSArray> = NSArray::from_slice(&[item]);

        let picker = unsafe {
            NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items)
        };

        let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
        let view: Retained<NSView> = window
            .contentView()
            .ok_or_else(|| "application window has no content view".to_string())?;

        // Anchor near the top-left of the content view: the picker is a
        // popover, and it must be given a non-empty rect to point at.
        let bounds = view.bounds();
        let rect = NSRect::new(
            NSPoint::new(bounds.size.width / 2.0, bounds.size.height - 1.0),
            NSSize::new(1.0, 1.0),
        );
        picker.showRelativeToRect_ofView_preferredEdge(rect, &view, NSRectEdge::MinY);
        Ok(())
    }
}

pub mod tauri_commands {
    use super::*;
    use tauri::Manager;

    #[tauri::command]
    pub async fn platform_capabilities() -> Result<PlatformCapabilities, String> {
        Ok(capabilities())
    }

    #[tauri::command]
    pub async fn platform_print(path: String) -> Result<String, String> {
        #[cfg(feature = "sidecars")]
        {
            tokio::task::spawn_blocking(move || print_file(Path::new(&path)))
                .await
                .map_err(|e| format!("join: {e}"))?
        }
        #[cfg(not(feature = "sidecars"))]
        {
            let _ = path;
            Err("this build cannot print directly — `platform_capabilities` \
                 reports direct_print:false. Open the file in its default \
                 application and print from there."
                .to_string())
        }
    }

    #[tauri::command]
    pub async fn platform_open_external(path: String) -> Result<(), String> {
        open_in_default_app(Path::new(&path))
    }

    /// Offer the file to the system share sheet where one exists.
    ///
    /// Falls back to revealing it in the file manager, and says so in the
    /// result rather than pretending to have shared it.
    #[tauri::command]
    pub async fn platform_share(
        app: tauri::AppHandle,
        path: String,
    ) -> Result<ShareResult, String> {
        let p = std::path::PathBuf::from(&path);
        ensure_exists(&p)?;

        #[cfg(target_os = "macos")]
        {
            let window = app
                .get_webview_window("main")
                .or_else(|| app.webview_windows().values().next().cloned())
                .ok_or_else(|| "no application window".to_string())?;
            let ns_window = window.ns_window().map_err(|e| format!("ns_window: {e}"))?;

            // AppKit is main-thread-only; a Tauri command is not on it.
            let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
            let path_for_main = p.clone();
            let ptr = ns_window as usize;
            app.run_on_main_thread(move || {
                let r = unsafe { mac_share::show(ptr as *mut std::ffi::c_void, &path_for_main) };
                let _ = tx.send(r);
            })
            .map_err(|e| format!("dispatch to main thread: {e}"))?;

            // The picker itself is modal to the user, not to us — this
            // only waits for it to be presented.
            rx.recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| "timed out presenting the share sheet".to_string())??;

            return Ok(ShareResult { kind: ShareKind::SystemSheet, note: None });
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = &app;
            reveal_in_file_manager(&p)?;
            Ok(ShareResult {
                kind: ShareKind::RevealedInFileManager,
                note: Some(
                    "This platform has no system share sheet; the file has been \
                     revealed in the file manager instead."
                        .into(),
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_report_this_platform() {
        let c = capabilities();
        assert_eq!(c.platform, std::env::consts::OS);
        // Every desktop target we build for can dispatch a print — as long
        // as it may spawn `lp` / the Print verb.
        #[cfg(all(
            feature = "sidecars",
            any(target_os = "macos", target_os = "linux", target_os = "windows")
        ))]
        assert!(c.direct_print);
        #[cfg(not(feature = "sidecars"))]
        assert!(
            !c.direct_print,
            "a build with no print path must not advertise one — the frontend \
             renders the button from this flag"
        );
        // The share sheet is macOS-only until the iOS impl lands.
        #[cfg(target_os = "macos")]
        assert!(c.system_share_sheet);
        #[cfg(not(target_os = "macos"))]
        assert!(!c.system_share_sheet);
    }

    #[test]
    fn missing_files_are_rejected_before_touching_the_os() {
        let missing = Path::new("/definitely/not/here/nope.pdf");
        #[cfg(feature = "sidecars")]
        assert!(print_file(missing).is_err());
        assert!(open_in_default_app(missing).is_err());
        assert!(reveal_in_file_manager(missing).is_err());
    }

    #[cfg(feature = "sidecars")]
    #[test]
    fn missing_file_error_names_the_path() {
        let err = print_file(Path::new("/nope/missing.pdf")).unwrap_err();
        assert!(err.contains("missing.pdf"), "unhelpful error: {err}");
    }
}
