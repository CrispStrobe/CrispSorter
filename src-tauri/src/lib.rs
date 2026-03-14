use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use futures_util::StreamExt;
use std::io::Write;
use tauri::Emitter;
use tauri::Manager;
use std::sync::Arc;
use tokio::sync::Mutex;
use mistralrs::{
    GgufModelBuilder, TextMessageRole, TextMessages, RequestBuilder,
    best_device, Model, initialize_logging,
    PagedAttentionMetaBuilder
};

#[derive(Deserialize)]
pub struct MoveRequest {
    source: String,
    destination: String,
}

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
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;
use tokio::process::Child as TokioChild;

pub struct AppState {
    model: Mutex<Option<Arc<Model>>>,
    current_model_path: Mutex<Option<String>>,
    sidecar_process: Mutex<Option<CommandChild>>,
    mlx_process: Mutex<Option<TokioChild>>,
}

#[tauri::command]
async fn start_llamacpp_sidecar(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    model_path: String,
) -> Result<String, String> {
    let mut sidecar_lock = state.sidecar_process.lock().await;
    
    // Kill existing process if running
    if let Some(child) = sidecar_lock.take() {
        let _ = child.kill();
    }

    println!("[Sidecar] Starting llama-server with Metal acceleration...");
    println!("[Sidecar] Model path: {}", model_path);
    
    // In dev mode, sidecars are in src-tauri/bin
    // In production, they are in the resources folder
    let bin_dir = if cfg!(debug_assertions) {
        let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
        // target/debug/tauri-app -> src-tauri/bin
        exe_path.parent().unwrap().parent().unwrap().parent().unwrap().join("bin")
    } else {
        let resource_dir = app_handle.path().resource_dir().map_err(|e: tauri::Error| e.to_string())?;
        resource_dir.join("bin")
    };
    
    let bin_dir_str = bin_dir.to_string_lossy().to_string();
    println!("[Sidecar] Library path: {}", bin_dir_str);

    let (mut rx, child) = app_handle
        .shell()
        .sidecar("llama-server")
        .map_err(|e| {
            println!("[Sidecar] DEFINE ERROR: {}", e);
            e.to_string()
        })?
        .args(["-m", &model_path, "--port", "8080", "--host", "0.0.0.0", "-ngl", "99", "--parallel", "1", "-c", "4096"])
        .env("DYLD_LIBRARY_PATH", &bin_dir_str)
        .spawn()
        .map_err(|e| {
            println!("[Sidecar] SPAWN ERROR: {}", e);
            e.to_string()
        })?;

    println!("[Sidecar] Spawned PID: {:?}", child.pid());

    // Monitor output in background
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                tauri_plugin_shell::process::CommandEvent::Stdout(line) => {
                    println!("[llama.cpp] {}", String::from_utf8_lossy(&line));
                }
                tauri_plugin_shell::process::CommandEvent::Stderr(line) => {
                    eprintln!("[llama.cpp] ERR: {}", String::from_utf8_lossy(&line));
                }
                tauri_plugin_shell::process::CommandEvent::Terminated(payload) => {
                    println!("[llama.cpp] Terminated with code: {:?}", payload.code);
                }
                _ => {}
            }
        }
    });

    *sidecar_lock = Some(child);
    Ok("Sidecar started".to_string())
}

#[tauri::command]
async fn stop_llamacpp_sidecar(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let mut sidecar_lock = state.sidecar_process.lock().await;
    if let Some(child) = sidecar_lock.take() {
        let _ = child.kill();
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
        .and_then(|o| if o.status.success() { String::from_utf8(o.stdout).ok() } else { None })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "mlx_lm.server".to_string());

    println!("[MLX] Resolved binary: '{}' — model: {}, port: {}", resolved_bin, model_path, port);

    let mut child = tokio::process::Command::new(&resolved_bin)
        .args(["--model", &model_path, "--port", &port.to_string(), "--trust-remote-code"])
        .env("PATH", &augmented_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start '{}': {}. Install with: pip install mlx-lm", resolved_bin, e))?;

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
            let _ = app.emit("mlx-log", "[MLX] Server did not respond within 120s — check logs");
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
    repo_ids.iter().map(|repo_id| {
        let dir_name = format!("models--{}", repo_id.replace('/', "--"));
        hub_dir.join(&dir_name).exists()
    }).collect()
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
                let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                entries.push(FileEntry { path: folder_path.clone(), size });
            }
        }
    } else if path.is_dir() {
        scan_dir_recursive(path, &extensions, &mut entries)
            .map_err(|e| e.to_string())?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
    } else {
        return Err(format!("Path does not exist: {}", folder_path));
    }
    println!("[Rust] scan_folder: found {} files in/at {}", entries.len(), folder_path);
    Ok(entries)
}

fn scan_dir_recursive(dir: &Path, extensions: &[String], entries: &mut Vec<FileEntry>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with('.')).unwrap_or(false) {
            continue;
        }
        if path.is_dir() {
            scan_dir_recursive(&path, extensions, entries)?;
        } else if let Some(ext) = path.extension() {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            if extensions.iter().any(|e| e == &ext_lower) {
                let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                entries.push(FileEntry { path: path.to_string_lossy().into_owned(), size });
            }
        }
    }
    Ok(())
}

#[tauri::command]
async fn move_files(moves: Vec<MoveRequest>) -> Result<Vec<String>, String> {
    let mut results = Vec::new();
    for req in moves {
        let src_path = Path::new(&req.source);
        let dest_path = Path::new(&req.destination);
        if !src_path.exists() {
            results.push(format!("Error: Source file not found: {}", req.source));
            continue;
        }
        if let Some(parent) = dest_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                results.push(format!("Error: Failed to create directories for {}: {}", req.destination, e));
                continue;
            }
        }
        if dest_path.exists() {
            results.push(format!("Error: Destination already exists: {}", req.destination));
            continue;
        }
        match fs::rename(src_path, dest_path) {
            Ok(_) => results.push(format!("Success: Moved {} to {}", req.source, req.destination)),
            Err(e) => results.push(format!("Error: Failed to move {}: {}", req.source, e)),
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

        let _ = window.emit("download-progress", DownloadProgress {
            id: id.clone(),
            received: downloaded,
            total: total_size,
        });
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
        
        let model = if model_path.ends_with(".gguf") && Path::new(&model_path).exists() {
            // Local GGUF file
            let path = Path::new(&model_path);
            let parent = path.parent().ok_or("Invalid model path")?.to_str().ok_or("Non-UTF8 path")?.to_string();
            let filename = path.file_name().ok_or("Invalid model filename")?.to_str().ok_or("Non-UTF8 filename")?.to_string();
            
            println!("[mistral.rs] Loading local GGUF: ID='{}', File='{}'", parent, filename);
            GgufModelBuilder::new(parent, vec![filename])
                .with_device(best_device(false).map_err(|e| e.to_string())?)
                .with_logging()
                .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())
                .map_err(|e| e.to_string())?
                .build()
                .await
        } else {
            // Assume it's an HF Repo ID or URL that mistralrs can handle
            // For GGUF on HF, we usually need the repo_id and the specific file.
            // If the user provided "repo_id/file.gguf", we split it.
            let parts: Vec<&str> = model_path.split('/').collect();
            if parts.len() >= 3 && model_path.contains(".gguf") {
                // e.g. "bartowski/Llama-3.2-1B-Instruct-GGUF/Llama-3.2-1B-Instruct-Q4_K_M.gguf"
                let filename = parts.last().unwrap().to_string();
                let repo_id = parts[..parts.len()-1].join("/");
                println!("[mistral.rs] Loading remote HF GGUF: Repo='{}', File='{}'", repo_id, filename);
                GgufModelBuilder::new(repo_id, vec![filename])
                    .with_device(best_device(false).map_err(|e| e.to_string())?)
                    .with_logging()
                    .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())
                    .map_err(|e| e.to_string())?
                    .build()
                    .await
            } else {
                // Fallback to TextModelBuilder if no .gguf extension
                println!("[mistral.rs] Loading as TextModel (Repo ID): {}", model_path);
                mistralrs::TextModelBuilder::new(model_path.clone())
                    .with_device(best_device(false).map_err(|e| e.to_string())?)
                    .with_logging()
                    .build()
                    .await
            }
        }.map_err(|e| {
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
    let thinking = !no_thinking.unwrap_or(false);
    let request = RequestBuilder::from(
        TextMessages::new().add_message(TextMessageRole::User, prompt.clone())
    )
    .set_sampler_max_len(max_len)
    .enable_thinking(thinking);

    println!("[mistral.rs] Sending chat request (prompt len={}, max_tokens={}, thinking={})...", prompt.len(), max_len, thinking);
    let response = model.send_chat_request(request)
        .await
        .map_err(|e| {
            println!("[mistral.rs] QUERY ERROR: {}", e);
            e.to_string()
        })?;

    let content = response.choices[0].message.content.as_ref().cloned().unwrap_or_default();
    println!("[mistral.rs] Query complete. Response length: {}. Usage: P={}, C={}", 
        content.len(),
        response.usage.prompt_tokens,
        response.usage.completion_tokens
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
        })
        .invoke_handler(tauri::generate_handler![
            move_files,
            scan_folder,
            download_file,
            run_mistralrs_query,
            start_llamacpp_sidecar,
            stop_llamacpp_sidecar,
            start_mlx_server,
            stop_mlx_server,
            get_mlx_cache_dir,
            check_mlx_models_cached,
            delete_mlx_model,
            delete_files,
            extract_pdf_native
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
