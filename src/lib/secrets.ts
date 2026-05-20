/**
 * OS-keychain secret store helpers.
 *
 * API keys for LLM providers no longer live in `settings.json` — that
 * file is at risk of leaking via cloud-sync, bug-report tarballs, or
 * other processes with filesystem access. Instead, the key value lives
 * in the OS-managed credential vault (macOS Keychain / Windows
 * Credential Manager / Linux Secret Service) under the service
 * `CrispSorter.LLM` with one entry per account name.
 *
 * `settings.json` holds a sentinel string of the form
 * `@keyring/<account>` instead of the raw key. When the LLM client
 * (or anywhere else) needs the actual key, call {@link resolveSecret}
 * to swap the sentinel for the real value.
 *
 * Naming convention for LLM provider keys:
 *
 *   account = `llm-provider:<provider-id>`   e.g. `llm-provider:openai`
 *
 * On macOS Keychain Access this surfaces as a row named
 * `llm-provider:openai` under the `CrispSorter.LLM` service, so the
 * user can audit or manually revoke.
 */

import { invoke } from '@tauri-apps/api/core';

/** Read a secret from the OS keychain. */
export async function getSecret(account: string): Promise<string | null> {
    return await invoke<string | null>('secret_get', { account });
}

/** Write a secret, overwriting any existing value. */
export async function setSecret(account: string, value: string): Promise<void> {
    await invoke('secret_set', { account, value });
}

/** Delete a stored secret. Idempotent — no error if it didn't exist. */
export async function deleteSecret(account: string): Promise<void> {
    await invoke('secret_delete', { account });
}

/**
 * Bulk-store multiple secrets. Used by the one-time migration that
 * moves plain-text apiKeys out of `settings.json` into the keychain.
 * Returns the accounts that were successfully stored.
 */
export async function bulkSetSecrets(items: Array<[string, string]>): Promise<string[]> {
    return await invoke<string[]>('secrets_bulk_set', { items });
}

/**
 * Of `candidates`, return the subset that have a non-empty value
 * stored in the keychain. Used by the Settings UI to render
 * "which providers do I have keys for?" without iterating one-by-one.
 *
 * The candidates list is required because the OS keychain APIs don't
 * cleanly support "enumerate everything under this service" (macOS
 * would prompt the user for each row). Pass the known account names
 * — e.g. every `llm-provider:<id>` for the providers the app knows
 * about.
 */
export async function listKnownSecrets(candidates: string[]): Promise<string[]> {
    return await invoke<string[]>('secrets_list_known', { accounts: candidates });
}

/** Make a sentinel for the given account. */
export function makeSentinel(account: string): string {
    return `@keyring/${account}`;
}

/** If `s` is a `@keyring/<account>` sentinel, return the account. */
export function sentinelAccount(s: string | undefined | null): string | null {
    if (typeof s !== 'string') return null;
    const m = /^@keyring\/(.+)$/.exec(s);
    return m ? m[1] : null;
}

/**
 * Resolve a possibly-sentinel value to its real form.
 *
 * - If `value` is `@keyring/<account>`, returns the keychain value for
 *   that account (or `''` if nothing is stored).
 * - Otherwise returns `value` verbatim (treating it as already-plain).
 *
 * Returns `''` (not `null`) on misses so callers passing the result
 * straight into an HTTP `Authorization: Bearer …` header still get a
 * well-typed string — the request will simply 401, which is the
 * desired observable failure mode.
 */
export async function resolveSecret(value: string | undefined | null): Promise<string> {
    if (!value) return '';
    const account = sentinelAccount(value);
    if (!account) return value;
    const stored = await getSecret(account);
    return stored ?? '';
}

/** Account name for an LLM provider. */
export function llmProviderAccount(providerId: string): string {
    return `llm-provider:${providerId}`;
}
