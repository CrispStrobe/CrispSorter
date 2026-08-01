export type DriveCapabilities = {
    create_dir: boolean;
    rename: boolean;
    move_path: boolean;
    copy: boolean;
    delete: boolean;
};

export type DriveBrowserAction = 'create_dir' | 'rename' | 'move' | 'copy' | 'delete';

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
        return slash >= 0 ? decodeURIComponent(rest.slice(slash)) : null;
    }
    if (uri.startsWith('crisp+drive://') || uri.startsWith('crisp+cb-archive://')) return null;
    if (/^[a-z][a-z0-9+.-]*:\/\//i.test(uri)) return null;
    return uri || null;
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
