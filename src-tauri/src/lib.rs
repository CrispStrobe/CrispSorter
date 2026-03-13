use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use futures_util::StreamExt;
use std::io::Write;
use tauri::Emitter;
use std::sync::Arc;
use tokio::sync::Mutex;
use mistralrs::{
    GgufModelBuilder, TextMessageRole, TextMessages, 
    best_device, Model, initialize_logging
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
pub struct AppState {
    model: Mutex<Option<Arc<Model>>>,
    current_model_path: Mutex<Option<String>>,
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
        println!("[mistral.rs] Loading model from: {}", model_path);
        let path = Path::new(&model_path);
        let parent = path.parent().ok_or("Invalid model path")?.to_str().ok_or("Non-UTF8 path")?.to_string();
        let filename = path.file_name().ok_or("Invalid model filename")?.to_str().ok_or("Non-UTF8 filename")?.to_string();

        println!("[mistral.rs] Building GGUF model: ID='{}', File='{}'", parent, filename);
        // GgufModelBuilder::new takes (model_id, files)
        // For local files, we use the directory as model_id
        let model = GgufModelBuilder::new(parent.clone(), vec![filename])
            .with_device(best_device(false).map_err(|e| e.to_string())?)
            .with_logging()
            .build()
            .await
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
            current_model_path: Mutex::new(None)
        })
        .invoke_handler(tauri::generate_handler![move_files, download_file, run_mistralrs_query])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
