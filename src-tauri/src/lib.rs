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
    GgufModelBuilder, TextMessageRole, TextMessages, 
    best_device, Model, initialize_logging,
    PagedAttentionMetaBuilder
};

#[derive(Deserialize)]
pub struct MoveRequest {
    source: String,
    destination: String,
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

pub struct AppState {
    model: Mutex<Option<Arc<Model>>>,
    current_model_path: Mutex<Option<String>>,
    sidecar_process: Mutex<Option<CommandChild>>,
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
    
    let messages = TextMessages::new()
        .add_message(TextMessageRole::User, prompt.clone());

    println!("[mistral.rs] Sending chat request (prompt len={})...", prompt.len());
    let response = model.send_chat_request(messages)
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
            sidecar_process: Mutex::new(None)
        })
        .invoke_handler(tauri::generate_handler![
            move_files, 
            download_file, 
            run_mistralrs_query,
            start_llamacpp_sidecar,
            stop_llamacpp_sidecar
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
