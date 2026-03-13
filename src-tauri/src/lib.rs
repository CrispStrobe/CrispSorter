use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use futures_util::StreamExt;
use std::io::Write;
use tauri::Emitter;
use std::sync::{Arc, Mutex};
use mistralrs::{
    IsqType, Response, SamplingParams, 
    TextMessageRole, Device, RequestBuilder, best_device
};
use mistralrs::core::{
    GGUFLoaderBuilder, GGUFSpecificConfig, DeviceMapMetadata, MistralRs
};
use tokio::sync::mpsc;

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

// Global state to hold the engine instance
struct AppState {
    mistralrs: Mutex<Option<Arc<MistralRs>>>,
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
    let mut mistralrs_lock = state.mistralrs.lock().unwrap();
    
    if mistralrs_lock.is_none() {
        let loader = GGUFLoaderBuilder::new(
            GGUFSpecificConfig::default(),
            None,
            None,
            Some(model_path),
        ).build();
        
        let pipeline = loader.load_model(
            None,
            IsqType::Q4K,
            &best_device(),
            false,
            DeviceMapMetadata::dummy(),
            None,
            None,
        ).map_err(|e| e.to_string())?;
        
        *mistralrs_lock = Some(pipeline);
    }

    let mistralrs = mistralrs_lock.as_ref().unwrap();
    let (tx, mut rx) = mpsc::channel(10000);
    
    let request = RequestBuilder::new()
        .add_message(TextMessageRole::User, prompt)
        .set_sampling_params(SamplingParams::deterministic())
        .build_chat(tx, None);

    mistralrs.get_sender(None)
        .map_err(|e| e.to_string())?
        .send(request)
        .await
        .map_err(|e| e.to_string())?;

    let mut response_text = String::new();
    while let Some(response) = rx.recv().await {
        match response {
            Response::Done(c) => {
                response_text = c.choices[0].message.content.as_ref().cloned().unwrap_or_default();
                break;
            }
            Response::InternalError(e) => return Err(e.to_string()),
            Response::ValidationError(e) => return Err(e.to_string()),
            Response::ModelError(msg, _) => return Err(msg),
            _ => {}
        }
    }

    Ok(response_text)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .manage(AppState { mistralrs: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![move_files, download_file, run_mistralrs_query])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
