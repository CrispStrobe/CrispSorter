//! Tauri commands for the OS-keychain secret store.
//!
//! Frontend usage from Svelte:
//!
//! ```ts
//! import { invoke } from '@tauri-apps/api/core';
//!
//! await invoke('secret_set', { account: 'llm-provider:openai', value: 'sk-…' });
//! const value: string | null = await invoke('secret_get', { account: 'llm-provider:openai' });
//! await invoke('secret_delete', { account: 'llm-provider:openai' });
//! ```

use super::{delete_secret, get_secret, set_secret};

/// Store a secret under the given account name. Overwrites any
/// existing value. Returns `Ok(())` on success.
#[tauri::command]
pub async fn secret_set(account: String, value: String) -> Result<(), String> {
    set_secret(&account, &value).map_err(|e| e.to_string())
}

/// Read a secret. Returns `null` (JS) / `None` (Rust) when nothing is
/// stored under that account.
#[tauri::command]
pub async fn secret_get(account: String) -> Result<Option<String>, String> {
    get_secret(&account).map_err(|e| e.to_string())
}

/// Delete a stored secret. Idempotent.
#[tauri::command]
pub async fn secret_delete(account: String) -> Result<(), String> {
    delete_secret(&account).map_err(|e| e.to_string())
}

/// Migration helper: takes a list of `(account, plaintext)` pairs and
/// writes each into the keychain. Returns the list of account names
/// that were successfully stored — the caller is then expected to
/// rewrite those entries in `settings.json` with the corresponding
/// `@keyring/<account>` sentinel.
///
/// On any individual failure, that account is omitted from the
/// returned list but the rest still proceed. Total atomicity isn't
/// worth the complexity here — partial migration just leaves the
/// failing key in plain text, which is the previous status quo.
#[tauri::command]
pub async fn secrets_bulk_set(items: Vec<(String, String)>) -> Result<Vec<String>, String> {
    let mut stored = Vec::with_capacity(items.len());
    for (account, value) in items {
        if value.is_empty() {
            continue;
        }
        match set_secret(&account, &value) {
            Ok(()) => stored.push(account),
            Err(e) => {
                eprintln!("secrets_bulk_set: failed for account={account}: {e}");
            }
        }
    }
    Ok(stored)
}

/// Filter a candidate list of account names down to those that
/// actually have a non-empty value stored in the keychain.
///
/// The `keyring` crate has no native "enumerate all entries under a
/// service" API — and even if it did, the macOS Keychain in
/// particular is allergic to bulk listing because each row may
/// prompt the user. This command instead takes a *known* list of
/// candidates (e.g. the union of provider IDs the UI knows about)
/// and returns the subset present.
///
/// Frontend usage:
///
/// ```ts
/// const known = ['llm-provider:openai', 'llm-provider:groq', …];
/// const stored = await invoke<string[]>('secrets_list_known', { accounts: known });
/// ```
///
/// Returns accounts in the same order they were given (callers can
/// rely on this to match against their candidate list).
#[tauri::command]
pub async fn secrets_list_known(accounts: Vec<String>) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for account in accounts {
        match super::get_secret(&account) {
            Ok(Some(v)) if !v.is_empty() => found.push(account),
            Ok(_) => {}
            Err(e) => {
                eprintln!("secrets_list_known: failed for account={account}: {e}");
            }
        }
    }
    Ok(found)
}
