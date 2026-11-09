const COMMANDS: &[&str] = &["pick_files", "pick_folder", "read_file", "export_file", "document_capabilities"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .ios_path("ios")
        .build();
}
