/**
 * Asking for, and remembering, permission to write to a folder.
 *
 * The shipped App Store build is sandboxed and may write only where the
 * user has pointed a file dialog. Sorting into a destination root they
 * never picked fails with `EPERM`, which the backend now reports as
 * `NOT_PERMITTED` naming the folder.
 *
 * Only the system's own folder picker can grant that access — Rust cannot
 * conjure it — so the ask has to happen here, and the bookmark that makes
 * it survive a restart has to be created while the pick is still live.
 * Hence the two-step: {@link requestFolderAccess} opens the picker, then
 * hands the result straight to the backend.
 */

import { invoke } from '@tauri-apps/api/core';
import { open as openDialog } from '@tauri-apps/plugin-dialog';

export type Writability =
    | { state: 'writable' }
    | { state: 'needs-permission'; reason: string }
    | { state: 'unwritable'; reason: string };

export interface FolderStatus {
    path: string;
    writability: Writability;
    /** Picking the folder would plausibly fix it. */
    canRequest: boolean;
    /** `false` when a grant already covers it, or the user declined. */
    shouldAsk: boolean;
}

export interface GrantsView {
    granted: string[];
    /** Held open in this process — shorter than `granted` if one went stale. */
    active: string[];
    declined: string[];
    supported: boolean;
}

export async function probeFolder(path: string): Promise<FolderStatus> {
    return await invoke<FolderStatus>('folder_access_probe', { path });
}

export async function shouldAskFor(path: string): Promise<boolean> {
    return await invoke<boolean>('folder_access_should_ask', { path });
}

/** Stop asking about this folder. */
export async function declineFolder(path: string): Promise<void> {
    await invoke('folder_access_decline', { path });
}

export async function forgetFolder(path: string): Promise<void> {
    await invoke('folder_access_forget', { path });
}

export async function listFolderGrants(): Promise<GrantsView> {
    return await invoke<GrantsView>('folder_access_list');
}

export type RequestOutcome =
    | { ok: true; granted: string }
    | { ok: false; reason: 'cancelled' | 'wrong-folder' | 'failed'; message?: string };

/**
 * Ask the user to grant access to `path`, then persist it.
 *
 * Opens the picker *at* `path` so the obvious action is the right one.
 * The user may pick an ancestor instead, which is fine and in fact better
 * — a grant covers descendants, so picking `~/Documents/texte` buys every
 * destination beneath it rather than one directory at a time.
 *
 * Picking something unrelated is reported as `wrong-folder` rather than
 * silently stored: a bookmark for a folder that does not contain the
 * destination resolves fine and still fails to help, which is a worse
 * outcome than being told.
 */
export async function requestFolderAccess(path: string): Promise<RequestOutcome> {
    let picked: string | null;
    try {
        picked = (await openDialog({
            directory: true,
            multiple: false,
            defaultPath: path,
            title: 'Grant CrispSorter access to this folder'
        })) as string | null;
    } catch (e) {
        return { ok: false, reason: 'failed', message: String(e) };
    }
    if (!picked) return { ok: false, reason: 'cancelled' };

    // A grant only helps if it covers the destination. Compare as path
    // components, not as a string prefix, so `/a/texte-backup` is not
    // mistaken for a grant over `/a/texte`.
    if (!pathContains(picked, path)) {
        return {
            ok: false,
            reason: 'wrong-folder',
            message: `${picked} does not contain ${path}, so it would not grant access to it.`
        };
    }

    try {
        const status = await invoke<FolderStatus>('folder_access_grant', { path: picked });
        if (!status.writability || status.writability.state !== 'writable') {
            const reason =
                status.writability && 'reason' in status.writability
                    ? status.writability.reason
                    : 'still not writable';
            return { ok: false, reason: 'failed', message: reason };
        }
        return { ok: true, granted: picked };
    } catch (e) {
        return { ok: false, reason: 'failed', message: String(e) };
    }
}

/** Does `ancestor` contain `descendant` (or equal it), comparing components? */
export function pathContains(ancestor: string, descendant: string): boolean {
    const split = (p: string) => p.replace(/[/\\]+$/, '').split(/[/\\]/).filter(Boolean);
    const a = split(ancestor);
    const d = split(descendant);
    if (a.length > d.length) return false;
    return a.every((seg, i) => seg === d[i]);
}

/** Pull the folder out of a `NOT_PERMITTED: <path> — …` backend error. */
export function folderFromError(error: string | undefined | null): string | null {
    if (!error) return null;
    const m = /^NOT_PERMITTED:\s*(.+?)\s+—/.exec(error);
    return m ? m[1] : null;
}
