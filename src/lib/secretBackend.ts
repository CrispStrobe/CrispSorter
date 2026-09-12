/**
 * Choosing where secrets are stored.
 *
 * Companion to {@link ./secrets}, which reads and writes secrets. This
 * module picks the *store* they go into.
 *
 * The default is the OS credential store, and on macOS that means the
 * login keychain — which is a poor fit for an app that gets rebuilt or
 * re-signed. A keychain item records the exact binary that created it;
 * a new binary is not on its access list, so macOS asks. Answering
 * "Always Allow" edits that access list, and editing one requires the
 * keychain's own password. A user whose login-keychain password has
 * drifted from their account password — an Apple ID reset does this
 * silently — cannot supply it, so the dialog returns for every secret,
 * on every launch, forever.
 *
 * Hence the picker. See `src-tauri/src/secrets/vault/mod.rs` for the
 * backends and what each one actually guarantees.
 */

import { invoke } from '@tauri-apps/api/core';

/** Which store secrets live in. */
export type SecretStoreKind = 'os-default' | 'mac-keychain' | 'file' | 'session';

/** How a named macOS keychain gets unlocked. */
export type MacUnlock = 'app-managed' | 'prompt';

/** Where an encrypted-file vault's key comes from. */
export type FileKeySource = 'device' | 'passphrase';

/** A store selection, in the shape the Rust side accepts. */
export interface StoreChoice {
    kind: SecretStoreKind;
    /** `mac-keychain` only. */
    path?: string;
    /** `mac-keychain` only. */
    unlock?: MacUnlock;
    /** `file` only. */
    key?: FileKeySource;
}

/** The store in force, and everything that qualifies it. */
export interface BackendStatus {
    choice: StoreChoice;
    /** `false` while the app is still on the compatibility default. */
    chosen: boolean;
    /** Set when `CRISPSORTER_SECRET_BACKEND` overrode the stored choice. */
    fromEnv: boolean;
    /** `false` while a passphrase vault awaits its passphrase. */
    unlocked: boolean;
    /** `true` once a read was refused and further reads are paused. */
    denied: boolean;
    /** Why the chosen store could not be built, if it could not be. */
    error: string | null;
}

export interface KeychainInfo {
    path: string;
    name: string;
    /** The login keychain — the one that prompts. */
    isLogin: boolean;
    /** One CrispSorter created for itself. */
    isOurs: boolean;
}

export interface BackendOptions {
    status: BackendStatus;
    keychains: KeychainInfo[];
    suggestedKeychain: string | null;
    supportsNamedKeychains: boolean;
    services: string[];
}

export interface MigrationReport {
    copied: number;
    /** Entries the source store simply did not have. */
    skipped: number;
    /** `[service, account, reason]` per entry that could not be moved. */
    failures: Array<[string, string, string]>;
}

export interface CreatedKeychain {
    path: string;
    /**
     * Shown to the user once. macOS may ask for it later — an item's
     * access list is per-binary — and a password nobody knows is a
     * keychain nobody can answer for.
     */
    password: string;
    generated: boolean;
}

export async function getBackendOptions(): Promise<BackendOptions> {
    return await invoke<BackendOptions>('secret_backend_options');
}

export async function getBackendStatus(): Promise<BackendStatus> {
    return await invoke<BackendStatus>('secret_backend_status');
}

/**
 * Switch stores. Takes effect immediately — no restart — because every
 * read resolves the active store per call.
 *
 * Secrets already in the old store stay there; {@link migrateSecrets}
 * is a separate step on purpose, so switching away from a store you can
 * no longer open does not fail because the copy-out failed.
 */
export async function selectBackend(choice: StoreChoice): Promise<BackendStatus> {
    return await invoke<BackendStatus>('secret_backend_select', { choice });
}

export async function unlockBackend(passphrase: string): Promise<BackendStatus> {
    return await invoke<BackendStatus>('secret_backend_unlock', { passphrase });
}

/** Lower the "already refused this session" latch and allow a retry. */
export async function retryBackend(): Promise<BackendStatus> {
    return await invoke<BackendStatus>('secret_backend_retry');
}

/**
 * Copy secrets out of `from` and into the active store.
 *
 * `accounts` has to be supplied because no credential store offers a
 * safe "list everything under this service": on macOS an enumeration
 * would raise one dialog per row, which is the problem this whole
 * feature exists to end.
 */
export async function migrateSecrets(
    from: StoreChoice,
    accounts: Array<[string, string]>
): Promise<MigrationReport> {
    return await invoke<MigrationReport>('secret_backend_migrate', { from, accounts });
}

export async function createKeychain(
    path?: string,
    password?: string
): Promise<CreatedKeychain> {
    return await invoke<CreatedKeychain>('secret_backend_create_keychain', {
        path: path ?? null,
        password: password ?? null
    });
}

export async function keychainPassword(path: string): Promise<string | null> {
    return await invoke<string | null>('secret_backend_keychain_password', { path });
}

/** The service name LLM provider keys live under. */
export const LLM_SERVICE = 'CrispSorter.LLM';

/**
 * `[service, account]` pairs worth trying when migrating.
 *
 * Built from the provider ids the UI knows about, because there is no
 * enumeration API to ask instead.
 */
export function migrationCandidates(providerIds: string[]): Array<[string, string]> {
    return providerIds.map((id) => [LLM_SERVICE, `llm-provider:${id}`]);
}

/** A one-line description of a store, for the picker. */
export function describeStore(choice: StoreChoice): string {
    switch (choice.kind) {
        case 'os-default':
            return 'System credential store (macOS login keychain, Windows Credential Manager, Linux Secret Service)';
        case 'mac-keychain':
            return `Keychain: ${choice.path ?? '(none chosen)'}`;
        case 'file':
            return choice.key === 'passphrase'
                ? 'Encrypted file, unlocked by a passphrase you type'
                : 'Encrypted file, unlocked by a key file next to it';
        case 'session':
            return 'This session only — nothing is written to disk';
    }
}
