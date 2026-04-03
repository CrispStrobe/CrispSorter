pub mod index;

use futures_util::StreamExt;
use mistralrs::{
    best_device, initialize_logging, GgufModelBuilder, IsqType, Model, PagedAttentionMetaBuilder,
    RequestBuilder, TextMessageRole, TextMessages,
};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tokio::sync::Mutex;

#[derive(Serialize)]
pub struct FileEntry {
    path: String,
    size: u64,
}

#[derive(Serialize, Clone)]
struct DownloadProgress {
    id: String,
    received: u64,
    total: u64,
}

// Global state to hold the high-level Model instance and current model path
// Using tokio::sync::Mutex because guards need to be Send across await points in Tauri commands
use tokio::process::Child as TokioChild;

pub struct AppState {
    model: Mutex<Option<Arc<Model>>>,
    current_model_path: Mutex<Option<String>>,
    sidecar_process: Mutex<Option<TokioChild>>,
    mlx_process: Mutex<Option<TokioChild>>,
    ollama_process: Mutex<Option<TokioChild>>,
    pub index: Mutex<index::IndexState>,
}

#[tauri::command]
async fn start_llamacpp_sidecar(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    model_path: String,
    port: u16,
) -> Result<String, String> {
    let mut sidecar_lock = state.sidecar_process.lock().await;

    // Kill existing process if running
    if let Some(mut child) = sidecar_lock.take() {
        let _ = child.kill().await;
    }

    println!("[Sidecar] Starting llama-server (CPU)...");
    println!("[Sidecar] Model path: {}", model_path);

    // Resolve the bin directory: in dev it's src-tauri/bin, in release it's next to the exe
    let bin_dir = if cfg!(debug_assertions) {
        let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
        // target/debug/tauri-app.exe  →  src-tauri/bin
        exe_path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("bin")
    } else {
        let resource_dir = app_handle
            .path()
            .resource_dir()
            .map_err(|e: tauri::Error| e.to_string())?;
        resource_dir.join("bin")
    };

    let bin_dir_str = bin_dir.to_string_lossy().to_string();
    println!("[Sidecar] Library/Bin path: {}", bin_dir_str);

    // Locate the llama-server executable (Tauri appends the target triple)
    let exe_name = if cfg!(windows) {
        "llama-server-x86_64-pc-windows-msvc.exe"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "llama-server-aarch64-apple-darwin"
        } else {
            "llama-server-x86_64-apple-darwin"
        }
    } else {
        "llama-server-x86_64-unknown-linux-gnu"
    };

    let exe_path = bin_dir.join(exe_name);
    if !exe_path.exists() {
        return Err(format!(
            "llama-server binary not found at: {}",
            exe_path.display()
        ));
    }

    // Build PATH so Windows can find the DLLs next to the exe
    let current_path = std::env::var("PATH").unwrap_or_default();
    let augmented_path = if cfg!(windows) {
        format!("{};{}", bin_dir_str, current_path)
    } else {
        current_path
    };

    // Always request maximum GPU offload; llama.cpp silently falls back to CPU if no GPU backend is loaded
    let ngl_value = "99";

    let mut child = tokio::process::Command::new(&exe_path)
        .args([
            "-m",
            &model_path,
            "--port",
            &port.to_string(),
            "--host",
            "0.0.0.0",
            "-ngl",
            ngl_value,
            "--parallel",
            "1",
            "-c",
            "4096",
        ])
        // Set CWD to bin dir so Windows finds the DLLs relative to the exe
        .current_dir(&bin_dir)
        .env("PATH", &augmented_path)
        // Tells ggml_backend_load_all() where to find backend DLLs
        .env("GGML_BACKEND_PATH", &bin_dir_str)
        .env("DYLD_LIBRARY_PATH", &bin_dir_str) // macOS
        .env("LD_LIBRARY_PATH", &bin_dir_str) // Linux
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            println!("[Sidecar] SPAWN ERROR: {}", e);
            e.to_string()
        })?;

    println!("[Sidecar] Spawned PID: {:?}", child.id());

    // Stream stdout
    if let Some(stdout) = child.stdout.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[llama.cpp] {}", line);
                let _ = app.emit("sidecar-log", &line);
            }
        });
    }

    // Stream stderr + detect early exit
    if let Some(stderr) = child.stderr.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                eprintln!("[llama.cpp] ERR: {}", line);
                let _ = app.emit("sidecar-log", format!("[ERR] {}", line));
                // Detect a fatal startup error and surface it immediately
                if line.contains("no backends are loaded")
                    || line.contains("failed to load model")
                    || line.contains("exiting due to")
                {
                    let _ = app.emit("sidecar-failed", &line);
                }
            }
        });
    }

    // Health-check loop — emits sidecar-ready or sidecar-failed
    {
        let app = app_handle.clone();
        let health_url = format!("http://localhost:{}/health", port);
        println!("[Sidecar] Health check target: {}", health_url);
        tokio::spawn(async move {
            for attempt in 1..=30u32 {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                match reqwest::get(&health_url).await {
                    Ok(r) if r.status().is_success() => {
                        println!("[Sidecar] llama-server ready after {}s", attempt * 2);
                        let _ = app.emit("sidecar-ready", true);
                        return;
                    }
                    _ => {
                        println!("[Sidecar] Waiting for server... attempt {}/30", attempt);
                    }
                }
            }
            println!("[Sidecar] llama-server did not become ready within 60s");
            let _ = app.emit("sidecar-failed", "Server did not respond within 60s");
        });
    }

    *sidecar_lock = Some(child);
    Ok("Sidecar starting".to_string())
}

#[tauri::command]
async fn stop_llamacpp_sidecar(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut sidecar_lock = state.sidecar_process.lock().await;
    if let Some(mut child) = sidecar_lock.take() {
        let _ = child.kill().await;
        println!("[Sidecar] Stopped.");
    }
    Ok(())
}

#[tauri::command]
async fn start_mlx_server(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    model_path: String,
    port: u16,
) -> Result<String, String> {
    let mut mlx_lock = state.mlx_process.lock().await;
    if let Some(mut child) = mlx_lock.take() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    // Tauri inherits a minimal PATH — augment with common Python install locations
    let home = std::env::var("HOME").unwrap_or_default();
    let current_path = std::env::var("PATH").unwrap_or_default();
    let augmented_path = format!(
        "{home}/.local/bin:{home}/miniconda3/bin:{home}/miniconda3/condabin:{home}/anaconda3/bin:{home}/.pyenv/shims:{home}/.pyenv/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:{current_path}"
    );

    // Resolve the real binary path via login shell (handles conda/pyenv/homebrew)
    let resolved_bin = tokio::process::Command::new("/bin/zsh")
        .args(["-l", "-c", "which mlx_lm.server 2>/dev/null"])
        .env("PATH", &augmented_path)
        .output()
        .await
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "mlx_lm.server".to_string());

    println!(
        "[MLX] Resolved binary: '{}' — model: {}, port: {}",
        resolved_bin, model_path, port
    );

    let mut child = tokio::process::Command::new(&resolved_bin)
        .args([
            "--model",
            &model_path,
            "--port",
            &port.to_string(),
            "--trust-remote-code",
        ])
        .env("PATH", &augmented_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to start '{}': {}. Install with: pip install mlx-lm",
                resolved_bin, e
            )
        })?;

    if let Some(stdout) = child.stdout.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[MLX] {}", line);
                let _ = app.emit("mlx-log", &line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[MLX ERR] {}", line);
                let _ = app.emit("mlx-log", &line);
            }
        });
    }

    // Poll until server is accepting connections, then emit mlx-ready
    {
        let app = app_handle.clone();
        let health_url = format!("http://localhost:{}/v1/models", port);
        tokio::spawn(async move {
            for attempt in 1..=60u32 {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                match reqwest::get(&health_url).await {
                    Ok(r) if r.status().is_success() => {
                        println!("[MLX] Server ready after {}s", attempt * 2);
                        let _ = app.emit("mlx-ready", true);
                        return;
                    }
                    _ => {
                        println!("[MLX] Waiting for server... attempt {}/60", attempt);
                    }
                }
            }
            println!("[MLX] Server did not become ready within 120s");
            let _ = app.emit(
                "mlx-log",
                "[MLX] Server did not respond within 120s — check logs",
            );
        });
    }

    println!("[MLX] Server spawned (PID: {:?})", child.id());
    *mlx_lock = Some(child);
    Ok(format!("MLX server starting on port {}", port))
}

#[tauri::command]
fn get_mlx_cache_dir() -> String {
    std::env::var("HF_HUB_CACHE")
        .or_else(|_| std::env::var("HF_HOME").map(|h| format!("{}/hub", h)))
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{}/.cache/huggingface/hub", home)
        })
}

#[tauri::command]
fn check_mlx_models_cached(repo_ids: Vec<String>) -> Vec<bool> {
    let hub_dir = std::path::PathBuf::from(get_mlx_cache_dir());
    repo_ids
        .iter()
        .map(|repo_id| {
            let dir_name = format!("models--{}", repo_id.replace('/', "--"));
            hub_dir.join(&dir_name).exists()
        })
        .collect()
}

#[tauri::command]
async fn delete_mlx_model(repo_id: String) -> Result<String, String> {
    let dir_name = format!("models--{}", repo_id.replace('/', "--"));
    let cache_dir = std::path::PathBuf::from(get_mlx_cache_dir()).join(&dir_name);
    if cache_dir.exists() {
        fs::remove_dir_all(&cache_dir).map_err(|e| e.to_string())?;
        Ok(format!("Deleted: {}", cache_dir.display()))
    } else {
        Err(format!("Not found in cache: {}", cache_dir.display()))
    }
}

#[tauri::command]
async fn start_ollama(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    // Check if Ollama is already running externally
    if let Ok(r) = reqwest::get("http://localhost:11434/api/tags").await {
        if r.status().is_success() {
            let _ = app_handle.emit("ollama-ready", true);
            return Ok("Ollama already running".to_string());
        }
    }

    let mut ollama_lock = state.ollama_process.lock().await;
    // Kill any previously managed process
    if let Some(mut child) = ollama_lock.take() {
        let _ = child.kill().await;
    }

    // Find ollama binary
    #[cfg(windows)]
    let ollama_bin = {
        let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let candidate = std::path::PathBuf::from(&local_app_data)
            .join("Programs")
            .join("Ollama")
            .join("ollama.exe");
        if candidate.exists() {
            candidate.to_string_lossy().to_string()
        } else {
            "ollama".to_string()
        }
    };
    #[cfg(not(windows))]
    let ollama_bin = "ollama".to_string();

    println!("[Ollama] Starting: {} serve", ollama_bin);

    let mut child = tokio::process::Command::new(&ollama_bin)
        .arg("serve")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start ollama: {}. Is Ollama installed?", e))?;

    if let Some(stdout) = child.stdout.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[Ollama] {}", line);
                let _ = app.emit("ollama-log", &line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let app = app_handle.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                println!("[Ollama ERR] {}", line);
                let _ = app.emit("ollama-log", format!("[ERR] {}", line));
            }
        });
    }

    {
        let app = app_handle.clone();
        tokio::spawn(async move {
            for attempt in 1..=30u32 {
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                match reqwest::get("http://localhost:11434/api/tags").await {
                    Ok(r) if r.status().is_success() => {
                        println!("[Ollama] Ready after {}s", attempt * 2);
                        let _ = app.emit("ollama-ready", true);
                        return;
                    }
                    _ => println!("[Ollama] Waiting... attempt {}/30", attempt),
                }
            }
            println!("[Ollama] Did not become ready within 60s");
            let _ = app.emit("ollama-failed", "Ollama did not respond within 60s");
        });
    }

    *ollama_lock = Some(child);
    Ok("Ollama starting".to_string())
}

#[tauri::command]
async fn stop_ollama(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut ollama_lock = state.ollama_process.lock().await;
    if let Some(mut child) = ollama_lock.take() {
        let _ = child.kill().await;
        println!("[Ollama] Stopped.");
    }
    Ok(())
}

#[tauri::command]
async fn stop_mlx_server(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut mlx_lock = state.mlx_process.lock().await;
    if let Some(mut child) = mlx_lock.take() {
        let _ = child.kill().await;
        println!("[MLX] Server stopped");
    }
    Ok(())
}

#[tauri::command]
async fn delete_files(paths: Vec<String>) -> Result<Vec<String>, String> {
    let mut results = Vec::new();
    for path in paths {
        match fs::remove_file(&path) {
            Ok(_) => results.push(format!("Deleted: {}", path)),
            Err(e) => results.push(format!("Error deleting {}: {}", path, e)),
        }
    }
    Ok(results)
}

#[tauri::command]
async fn get_app_data_dir(app_handle: tauri::AppHandle) -> Result<String, String> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().to_string())
}

#[tauri::command]
async fn extract_pdf_native(path: String) -> Result<String, String> {
    println!("[Rust] Extracting PDF via pdf-extract: {}", path);
    pdf_extract::extract_text(&path).map_err(|e| {
        println!("[Rust] Extraction error: {}", e);
        e.to_string()
    })
}

#[tauri::command]
fn scan_folder(folder_path: String, extensions: Vec<String>) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    let path = Path::new(&folder_path);
    if path.is_file() {
        if let Some(ext) = path.extension() {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            if extensions.iter().any(|e| e == &ext_lower) {
                let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                entries.push(FileEntry {
                    path: folder_path.clone(),
                    size,
                });
            }
        }
    } else if path.is_dir() {
        scan_dir_recursive(path, &extensions, &mut entries).map_err(|e| e.to_string())?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
    } else {
        return Err(format!("Path does not exist: {}", folder_path));
    }
    println!(
        "[Rust] scan_folder: found {} files in/at {}",
        entries.len(),
        folder_path
    );
    Ok(entries)
}

fn scan_dir_recursive(
    dir: &Path,
    extensions: &[String],
    entries: &mut Vec<FileEntry>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        if path.is_dir() {
            scan_dir_recursive(&path, extensions, entries)?;
        } else if let Some(ext) = path.extension() {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            if extensions.iter().any(|e| e == &ext_lower) {
                let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                entries.push(FileEntry {
                    path: path.to_string_lossy().into_owned(),
                    size,
                });
            }
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchExecutionPayload {
    items: Vec<BatchExecutionItem>,
    save_txt: bool,
    mode: String, // "move", "copy", "script_move", "script_copy"
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchExecutionItem {
    id: String,
    original_path: String,
    target_path: String,
    extracted_text: Option<String>,
    /// LanceDB doc_id — set when the document was indexed.  If present and the
    /// move succeeds, the index location is updated automatically.
    doc_id: Option<String>,
    /// Pre-built `crisp+local://...` URI for the destination path.  The
    /// frontend computes this using the same user/machine UUIDs stored in
    /// app settings.
    new_location_uri: Option<String>,
}

#[derive(serde::Serialize)]
struct BatchExecutionResult {
    success: bool,
    error: Option<String>,
}

#[tauri::command]
async fn execute_batch(
    state: tauri::State<'_, AppState>,
    payload: BatchExecutionPayload,
) -> Result<std::collections::HashMap<String, BatchExecutionResult>, String> {
    let mut results = std::collections::HashMap::new();
    let is_script_mode = payload.mode.starts_with("script_");
    let mut script_content = String::new();

    // Windows detection for script extension
    let is_windows = std::env::consts::OS == "windows";
    let script_name = if is_windows {
        "sort_files.bat"
    } else {
        "sort_files.sh"
    };

    if is_script_mode {
        if is_windows {
            script_content.push_str("@echo off\n");
        } else {
            script_content.push_str("#!/bin/bash\n");
        }
    }

    for item in &payload.items {
        let src = Path::new(&item.original_path);
        let dest = Path::new(&item.target_path);

        if is_script_mode {
            // Script generation mode
            let cmd = if payload.mode == "script_move" {
                if is_windows {
                    format!("move \"{}\" \"{}\"\n", item.original_path, item.target_path)
                } else {
                    format!("mv \"{}\" \"{}\"\n", item.original_path, item.target_path)
                }
            } else if is_windows {
                format!("copy \"{}\" \"{}\"\n", item.original_path, item.target_path)
            } else {
                format!("cp \"{}\" \"{}\"\n", item.original_path, item.target_path)
            };
            script_content.push_str(&cmd);
            results.insert(
                item.id.clone(),
                BatchExecutionResult {
                    success: true,
                    error: None,
                },
            );
            continue;
        }

        // Direct execution mode
        if !src.exists() {
            results.insert(
                item.id.clone(),
                BatchExecutionResult {
                    success: false,
                    error: Some("SOURCE_NOT_FOUND".to_string()),
                },
            );
            continue;
        }

        if let Some(parent) = dest.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                results.insert(
                    item.id.clone(),
                    BatchExecutionResult {
                        success: false,
                        error: Some(format!("NOT_WRITABLE: {}", e)),
                    },
                );
                continue;
            }
        }

        // 1. Save TXT if requested
        if payload.save_txt {
            if let Some(text) = &item.extracted_text {
                let txt_path = dest.with_extension("txt");
                if let Err(e) = fs::write(txt_path, text) {
                    println!("Warning: Failed to save .txt for {}: {}", item.id, e);
                }
            }
        }

        // 2. Perform file operation — with locked-file copy fallback for move
        let exec_result = match payload.mode.as_str() {
            "copy" => match fs::copy(src, dest) {
                Ok(_) => BatchExecutionResult {
                    success: true,
                    error: None,
                },
                Err(e) => BatchExecutionResult {
                    success: false,
                    error: Some(e.to_string()),
                },
            },
            _ => match fs::rename(src, dest) {
                Ok(()) => BatchExecutionResult {
                    success: true,
                    error: None,
                },
                Err(ref e) if e.raw_os_error() == Some(32) => {
                    // File locked by another process (os error 32) — try copy as fallback
                    match fs::copy(src, dest) {
                        Ok(_) => BatchExecutionResult {
                            success: true,
                            error: Some("COPY_FALLBACK".to_string()),
                        },
                        Err(_) => BatchExecutionResult {
                            success: false,
                            error: Some("LOCKED".to_string()),
                        },
                    }
                }
                Err(e) => BatchExecutionResult {
                    success: false,
                    error: Some(e.to_string()),
                },
            },
        };
        // P11: if the move succeeded and the document was indexed, update the location URI.
        if exec_result.success {
            if let (Some(doc_id), Some(new_uri)) = (&item.doc_id, &item.new_location_uri) {
                let lock = state.index.lock().await;
                if lock.config.enabled {
                    if let Some(backend) = lock.backend.clone() {
                        drop(lock);
                        if let Err(e) = backend.update_location(doc_id, new_uri).await {
                            println!("[index] update_location failed for {}: {}", doc_id, e);
                        }
                    }
                }
            }
        }
        results.insert(item.id.clone(), exec_result);
    }

    if is_script_mode && !payload.items.is_empty() {
        // Save the generated script to the first item's parent directory or a common root if possible.
        // For simplicity, we'll try to put it in the first item's destination parent.
        if let Some(first_item) = payload.items.first() {
            let dest = Path::new(&first_item.target_path);
            if let Some(parent) = dest.parent() {
                let script_path = parent.join(script_name);
                if let Err(e) = fs::write(&script_path, script_content) {
                    return Err(format!("Failed to save script to {:?}: {}", script_path, e));
                }
            }
        }
    }

    Ok(results)
}

#[tauri::command]
async fn download_file(
    window: tauri::Window,
    id: String,
    url: String,
    path: String,
) -> Result<(), String> {
    let response = reqwest::get(url).await.map_err(|e| e.to_string())?;
    let total_size = response
        .content_length()
        .ok_or("Failed to get content length")?;

    let mut file = fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;

        let _ = window.emit(
            "download-progress",
            DownloadProgress {
                id: id.clone(),
                received: downloaded,
                total: total_size,
            },
        );
    }
    Ok(())
}

#[tauri::command]
async fn run_mistralrs_query(
    state: tauri::State<'_, AppState>,
    model_path: String,
    prompt: String,
    max_tokens: Option<usize>,
    no_thinking: Option<bool>,
) -> Result<String, String> {
    let mut model_lock = state.model.lock().await;
    let mut current_path_lock = state.current_model_path.lock().await;

    // Check if we need to load or swap the model
    let needs_load = match &*current_path_lock {
        Some(path) if path == &model_path => model_lock.is_none(),
        _ => true,
    };

    if needs_load {
        println!("[mistral.rs] Loading model: {}", model_path);

        let device = best_device(false).map_err(|e| e.to_string())?;
        println!("[mistral.rs] Target Hardware Device: {:?}", device);

        if model_path.starts_with("\\\\") {
            println!(
                "[mistral.rs] WARNING: Model path appears to be a UNC network share ('{}').",
                model_path
            );
            println!("[mistral.rs] This will SEVERELY degrade performance even if synced. Please use a local drive (e.g. C:\\).");
        }

        let model = if model_path.ends_with(".gguf") && Path::new(&model_path).exists() {
            // Local GGUF file
            let path = Path::new(&model_path);
            let parent = path
                .parent()
                .ok_or("Invalid model path")?
                .to_str()
                .ok_or("Non-UTF8 path")?
                .to_string();
            let filename = path
                .file_name()
                .ok_or("Invalid model filename")?
                .to_str()
                .ok_or("Non-UTF8 filename")?
                .to_string();

            println!(
                "[mistral.rs] Loading local GGUF: ID='{}', File='{}'",
                parent, filename
            );
            GgufModelBuilder::new(parent, vec![filename])
                .with_device(device)
                .with_logging()
                .with_throughput_logging()
                .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())
                .map_err(|e| e.to_string())?
                .build()
                .await
        } else {
            // Assume it's an HF Repo ID or URL that mistralrs can handle
            let parts: Vec<&str> = model_path.split('/').collect();
            if parts.len() >= 3 && model_path.contains(".gguf") {
                let filename = parts.last().unwrap().to_string();
                let repo_id = parts[..parts.len() - 1].join("/");
                println!(
                    "[mistral.rs] Loading remote HF GGUF: Repo='{}', File='{}'",
                    repo_id, filename
                );
                GgufModelBuilder::new(repo_id, vec![filename])
                    .with_device(device)
                    .with_logging()
                    .with_throughput_logging()
                    .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())
                    .map_err(|e| e.to_string())?
                    .build()
                    .await
            } else {
                println!(
                    "[mistral.rs] Loading as TextModel (Repo ID): {}",
                    model_path
                );
                mistralrs::TextModelBuilder::new(model_path.clone())
                    .with_device(device)
                    .with_logging()
                    .with_throughput_logging()
                    .with_isq(IsqType::Q4K) // Ensure remote models are quantized
                    .build()
                    .await
            }
        }
        .map_err(|e| {
            println!("[mistral.rs] LOAD ERROR: {}", e);
            e.to_string()
        })?;

        *model_lock = Some(Arc::new(model));
        *current_path_lock = Some(model_path.clone());
        println!("[mistral.rs] Model loaded successfully.");
    }

    // We can unwrap here because we ensured it's Some above
    let model = model_lock.as_ref().unwrap();

    let max_len = max_tokens.unwrap_or(512);
    // User requested thinking=false by default for performance
    let thinking = if let Some(nt) = no_thinking {
        !nt
    } else {
        false
    };

    let request = RequestBuilder::from(
        TextMessages::new().add_message(TextMessageRole::User, prompt.clone()),
    )
    .set_sampler_max_len(max_len)
    .enable_thinking(thinking);

    println!(
        "[mistral.rs] Sending chat request (prompt len={}, max_tokens={}, thinking={})...",
        prompt.len(),
        max_len,
        thinking
    );
    let start_time = std::time::Instant::now();
    let response = model.send_chat_request(request).await.map_err(|e| {
        println!("[mistral.rs] QUERY ERROR: {}", e);
        e.to_string()
    })?;
    let duration = start_time.elapsed();

    let content = response.choices[0]
        .message
        .content
        .as_ref()
        .cloned()
        .unwrap_or_default();
    let total_tokens = response.usage.completion_tokens;
    let tps = if duration.as_secs_f32() > 0.0 {
        total_tokens as f32 / duration.as_secs_f32()
    } else {
        0.0
    };

    println!("[mistral.rs] Query complete in {:?}. Response length: {}. Usage: P={}, C={}. Speed: {:.2} t/s", 
        duration,
        content.len(),
        response.usage.prompt_tokens,
        total_tokens,
        tps
    );

    Ok(content)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    initialize_logging();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            model: Mutex::new(None),
            current_model_path: Mutex::new(None),
            sidecar_process: Mutex::new(None),
            mlx_process: Mutex::new(None),
            ollama_process: Mutex::new(None),
            index: Mutex::new(index::IndexState::disabled()),
        })
        .invoke_handler(tauri::generate_handler![
            execute_batch,
            scan_folder,
            download_file,
            run_mistralrs_query,
            start_llamacpp_sidecar,
            stop_llamacpp_sidecar,
            start_mlx_server,
            stop_mlx_server,
            start_ollama,
            stop_ollama,
            get_mlx_cache_dir,
            check_mlx_models_cached,
            delete_mlx_model,
            delete_files,
            extract_pdf_native,
            get_app_data_dir,
            index::tauri_commands::index_search,
            index::tauri_commands::index_ingest_document,
            index::tauri_commands::index_update_location,
            index::tauri_commands::index_build_ivf_pq,
            index::tauri_commands::index_get_config,
            index::tauri_commands::index_set_config,
            index::tauri_commands::index_init,
            index::tauri_commands::index_is_ready,
            index::tauri_commands::index_stats,
            index::tauri_commands::index_list_documents,
            index::tauri_commands::index_delete_document,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
