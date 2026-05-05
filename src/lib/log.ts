/**
 * Frontend logging helper that pipes through the same ring buffer +
 * `app-log` Tauri event channel that the Rust side uses, so per-file
 * extraction errors, `tauri-plugin-fs` permission rejections, and
 * embedder-fetch failures all show up in the in-app Logs panel
 * alongside Rust-side messages.
 *
 * Usage:
 *   import { logInfo, logWarn, logError } from '$lib/log';
 *   try { ... } catch (e) { logError(`extract ${path} failed: ${e}`); }
 *
 * Falls back to console.* when the Tauri host isn't reachable (browser
 * preview, unit tests).
 */
import { invoke } from '@tauri-apps/api/core';

type Level = 'info' | 'warn' | 'error';

async function send(level: Level, msg: string) {
    try {
        await invoke('frontend_log', { level, msg });
    } catch {
        // Tauri unavailable — fall back to console.
        const fn = level === 'error' ? console.error : level === 'warn' ? console.warn : console.log;
        fn(`[fe-log:${level}]`, msg);
    }
}

export function logInfo(msg: string)  { void send('info',  msg); }
export function logWarn(msg: string)  { void send('warn',  msg); }
export function logError(msg: string) { void send('error', msg); }
