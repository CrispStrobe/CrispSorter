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

use super::{
    active_store_log_kind, delete_secret, get_secret, secret_error_code, set_secret, SecretError,
};
use crate::operation_log::OperationLog;
use serde_json::json;

/// Store a secret under the given account name. Overwrites any
/// existing value. Returns `Ok(())` on success.
#[tauri::command]
pub async fn secret_set(account: String, value: String) -> Result<(), String> {
    // Logs that a write happened and on which kind of store — never the
    // account name or value.
    let log = OperationLog::start("secrets.store", json!({"store": active_store_log_kind()}));
    match set_secret(&account, &value) {
        Ok(()) => {
            log.succeed(json!({}));
            Ok(())
        }
        Err(e) => {
            log.fail(secret_error_code(&e));
            Err(e.to_string())
        }
    }
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
    let log = OperationLog::start("secrets.delete", json!({"store": active_store_log_kind()}));
    match delete_secret(&account) {
        Ok(()) => {
            log.succeed(json!({}));
            Ok(())
        }
        Err(e) => {
            log.fail(secret_error_code(&e));
            Err(e.to_string())
        }
    }
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
    let log = OperationLog::start(
        "secrets.bulk_store",
        json!({"store": active_store_log_kind(), "total": items.len()}),
    );
    let mut stored = Vec::with_capacity(items.len());
    let mut failed = 0usize;
    let mut stopped = false;
    for (account, value) in items {
        if value.is_empty() {
            continue;
        }
        match set_secret(&account, &value) {
            Ok(()) => stored.push(account),
            // Same reasoning as `secrets_list_known`: a store that has
            // refused once will refuse the rest, one dialog at a time.
            Err(e @ SecretError::Denied(_)) => {
                eprintln!("secrets_bulk_set: store refused, stopping: {e}");
                log.warn("store_denied", json!({}));
                failed += 1;
                stopped = true;
                break;
            }
            Err(e) => {
                // The account name is a credential label; keep it out of logs.
                eprintln!("secrets_bulk_set: one entry failed: {e}");
                log.warn(secret_error_code(&e), json!({}));
                failed += 1;
            }
        }
    }
    log.succeed(json!({
        "completed": stored.len(),
        "failed": failed,
        "partial": failed > 0 || stopped,
    }));
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
///
/// Stops at the first [`SecretError::Denied`]. This loop was the single
/// worst source of the macOS keychain nag: one "CrispSorter wants to
/// use your confidential information" dialog **per provider**, raised
/// every time the Settings pane opened. Once the store has refused, the
/// remaining reads would each raise the same dialog the user just
/// dismissed, so there is nothing to gain by asking again.
#[tauri::command]
pub async fn secrets_list_known(accounts: Vec<String>) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for account in accounts {
        match super::get_secret(&account) {
            Ok(Some(v)) if !v.is_empty() => found.push(account),
            Ok(_) => {}
            Err(e @ SecretError::Denied(_)) => {
                eprintln!("secrets_list_known: store refused, stopping: {e}");
                break;
            }
            Err(e) => {
                eprintln!("secrets_list_known: one entry failed: {e}");
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_writes_log_the_store_kind_but_never_the_account_or_value() {
        let (result, events) = crate::operation_log::capture(|| {
            futures::executor::block_on(async {
                secret_set("llm-provider:private-account".into(), "sk-private-value".into()).await?;
                secret_delete("llm-provider:private-account".into()).await
            })
        });
        result.unwrap();
        let completed: Vec<&str> = events
            .iter()
            .filter(|e| e.phase == "completed")
            .map(|e| e.operation.as_str())
            .collect();
        assert_eq!(completed, ["secrets.store", "secrets.delete"]);
        assert!(events.iter().all(|e| e.context.get("store").is_some()));
        let text = serde_json::to_string(&events).unwrap();
        assert!(!text.contains("private"), "{text}");
    }
}
