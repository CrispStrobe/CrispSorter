// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
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

    // PLAN P8.2 — CLI mode. If argv[1] is a recognised subcommand,
    // skip GUI bootstrap and run the CLI to completion. Anything else
    // (no args = double-click launch; or unknown args = let Tauri / OS
    // decide) falls through to the GUI.
    //
    // We sniff argv[1] before clap so the GUI launch (no args) doesn't
    // pay any clap-parsing cost. Tauri itself passes its own args
    // (e.g. `--debug` in dev mode) which we don't want to claim.
    // Scan all args (not just argv[1]) so `--format text catalog scan ...`
    // is recognised. Stops at the first known subcommand. Skips any
    // `--flag value` pair that precedes the subcommand by leaving the
    // detection to clap downstream — we only need a positive hit.
    let args: Vec<String> = std::env::args().collect();
    let cli_mode = args
        .iter()
        .skip(1)
        .any(|a| tauri_app_lib::cli::SUBCOMMANDS.contains(&a.as_str()));
    if cli_mode {
        return tauri_app_lib::cli::run();
    }

    tauri_app_lib::run();
    ExitCode::SUCCESS
}
