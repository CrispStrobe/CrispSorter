const COMMANDS: &[&str] = &["pick_files", "pick_folder", "read_file"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
