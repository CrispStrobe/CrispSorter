use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use futures_util::StreamExt;
use std::io::Write;
use tauri::Emitter;

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
        .invoke_handler(tauri::generate_handler![move_files, download_file])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
