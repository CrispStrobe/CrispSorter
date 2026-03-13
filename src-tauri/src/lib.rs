use std::fs;
use std::path::Path;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct MoveRequest {
    source: String,
    destination: String,
}

#[tauri::command]
async fn move_files(moves: Vec<MoveRequest>) -> Result<Vec<String>, String> {
    let mut results = Vec::new();
    
    for req in moves {
        let src_path = Path::new(&req.source);
        let dest_path = Path::new(&req.destination);
        
        // Ensure source exists
        if !src_path.exists() {
            results.push(format!("Error: Source file not found: {}", req.source));
            continue;
        }
        
        // Create parent directories if they don't exist
        if let Some(parent) = dest_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                results.push(format!("Error: Failed to create directories for {}: {}", req.destination, e));
                continue;
            }
        }
        
        // Check if destination exists (don't overwrite unless we decide otherwise)
        if dest_path.exists() {
            results.push(format!("Error: Destination already exists: {}", req.destination));
            continue;
        }
        
        // Perform move/rename
        match fs::rename(src_path, dest_path) {
            Ok(_) => results.push(format!("Success: Moved {} to {}", req.source, req.destination)),
            Err(e) => results.push(format!("Error: Failed to move {}: {}", req.source, e)),
        }
    }
    
    Ok(results)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .invoke_handler(tauri::generate_handler![move_files])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
