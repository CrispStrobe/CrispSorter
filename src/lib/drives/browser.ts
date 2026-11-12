/**
 * Mirrors the Rust `DriveCapabilities` (src-tauri/src/drives/mod.rs).
 *
 * The mutation flags the browser gates its buttons on are required. The rest are
 * optional for compatibility with older frontend fixtures that build this type
 * by hand — but they are *declared*, so reading a field the backend genuinely
 * sends is never a type error. The full list is kept in sync with the Rust
 * struct deliberately: `versions` was missing once and `DriveBrowser.svelte`
 * failed to type-check on a value that was there at runtime.
 */
export type DriveCapabilities = {
    create_dir: boolean;
    rename: boolean;
    move_path: boolean;
    copy: boolean;
    delete: boolean;
    list?: boolean;
    read?: boolean;
    write?: boolean;
    stat?: boolean;
    streaming?: boolean;
    resumable_upload?: boolean;
    resumable_download?: boolean;
    share_links?: boolean;
    versions?: boolean;
    reversible_trash?: boolean;
};

export type DriveBrowserAction = 'create_dir' | 'rename' | 'move' | 'copy' | 'delete';

/** Whether a provider can be queried for remote file versions. */
export function supportsCloudVersions(capabilities: DriveCapabilities): boolean {
    return capabilities.versions === true;
}

/** Keep provider paths absolute and free of duplicate separators. */
export function normalizeDrivePath(path: string): string {
    const parts = path.split('/').filter(Boolean);
    return parts.length ? `/${parts.join('/')}` : '/';
}

export function joinDrivePath(base: string, name: string): string {
    return normalizeDrivePath(`${normalizeDrivePath(base)}/${name}`);
}

export function parentDrivePath(path: string): string {
    const normalized = normalizeDrivePath(path);
    if (normalized === '/') return '/';
    const parts = normalized.split('/').filter(Boolean);
    parts.pop();
    return parts.length ? `/${parts.join('/')}` : '/';
}

/** Return a local filesystem path only for local search provenance. */
export function localPathFromSearchUri(uri: string): string | null {
    if (uri.startsWith('crisp+local://')) {
        const rest = uri.slice('crisp+local://'.length);
        const slash = rest.indexOf('/');
        if (slash < 0) return null;
        try {
            const path = decodeURIComponent(rest.slice(slash)).replace(/^\/+/, '/');
            // Invalid provenance stays visible as a URI, but must not enable
            // filesystem actions or prevent the other results from rendering.
            return path.includes('\0') ? null : path;
        } catch {
            return null;
        }
    }
    if (uri.startsWith('crisp+drive://') || uri.startsWith('crisp+cb-archive://')) return null;
    if (/^[a-z][a-z0-9+.-]*:\/\//i.test(uri)) return null;
    return uri && !uri.includes('\0') ? uri : null;
}

/** Parse registered-drive provenance without turning malformed metadata into UI errors. */
export function drivePathFromSearchUri(uri: string): { driveId: string; remotePath: string } | null {
    if (!uri.startsWith('crisp+drive://')) return null;
    const rest = uri.slice('crisp+drive://'.length);
    const slash = rest.indexOf('/');
    if (slash <= 0 || rest.includes('\0')) return null;
    try {
        const remotePath = decodeURIComponent(rest.slice(slash));
        return remotePath.includes('\0') ? null : { driveId: rest.slice(0, slash), remotePath };
    } catch {
        return null;
    }
}

/** Serialize a decoded provider path without changing literal percent sequences. */
export function driveSearchUri(driveId: string, remotePath: string): string {
    const encodedPath = normalizeDrivePath(remotePath).split('/').map(encodeURIComponent).join('/');
    return `crisp+drive://${driveId}${encodedPath}`;
}

/** Update duplicate provenance by decoded identity, including older URI spellings. */
export function relocateDriveSearchUri(
    uri: string,
    driveId: string,
    source: string,
    destination: string,
): string {
    const parsed = drivePathFromSearchUri(uri);
    return parsed?.driveId === driveId && normalizeDrivePath(parsed.remotePath) === normalizeDrivePath(source)
        ? driveSearchUri(driveId, destination)
        : uri;
}

export function pathBaseName(path: string): string {
    return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

export function availableDriveActions(
    capabilities: DriveCapabilities,
    hasSelection: boolean,
): Record<DriveBrowserAction, boolean> {
    return {
        create_dir: capabilities.create_dir,
        rename: capabilities.rename && hasSelection,
        move: capabilities.move_path && hasSelection,
        copy: capabilities.copy && hasSelection,
        delete: capabilities.delete && hasSelection,
    };
}
