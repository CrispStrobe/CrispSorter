// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Release builds use `windows_subsystem = "windows"` which detaches from
    // any console. When the user launches the app from PowerShell / cmd they
    // still expect to see stdout. `AttachConsole(ATTACH_PARENT_PROCESS)`
    // reattaches to the parent's console if one exists; no-op (returns 0)
    // when double-clicked from Explorer.
    #[cfg(all(windows, not(debug_assertions)))]
    unsafe {
        extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF; // (DWORD)-1
        AttachConsole(ATTACH_PARENT_PROCESS);
    }

    tauri_app_lib::run()
}
