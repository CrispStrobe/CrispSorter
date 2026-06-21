/**
 * Shared types and utilities for the DocumentViewer component family.
 *
 * Centralises format detection, URI-to-path conversion, and extension
 * constants that were previously duplicated across IndexIngest and
 * IndexSearch.
 */

export type ViewerKind =
    | 'pdf'
    | 'image'
    | 'docx'
    | 'epub'
    | 'text'
    | 'html'
    | 'csv'
    | 'fallback';

/** Extensions rendered as monospace plain text. */
export const TEXT_EXTS = new Set([
    'txt', 'md', 'markdown', 'rst', 'log',
    'json', 'jsonl', 'yaml', 'yml', 'toml', 'xml',
    'rs', 'py', 'js', 'ts', 'tsx', 'jsx', 'svelte', 'vue',
    'go', 'java', 'kt', 'swift', 'scala',
    'c', 'cpp', 'cc', 'cxx', 'h', 'hpp',
    'rb', 'php', 'lua', 'r',
    'sh', 'bash', 'zsh', 'fish',
    'sql', 'graphql',
]);

/** Extensions rendered as images. */
export const IMAGE_EXTS = new Set([
    'png', 'jpg', 'jpeg', 'gif', 'webp', 'avif', 'bmp', 'svg', 'ico',
    'tiff', 'tif', 'heic', 'heif',
]);

/** Extensions rendered as CSV/TSV tables. */
export const CSV_EXTS = new Set(['csv', 'tsv']);

/**
 * Convert a CrispSorter location URI or absolute path to a local
 * filesystem path.  Returns `null` for non-local URIs (remote drives,
 * cloud-backup archives).
 */
export function uriToPath(uri: string): string | null {
    if (!uri) return null;

    // crisp+local://user@machine/absolute/path  →  /absolute/path
    if (uri.startsWith('crisp+local://')) {
        const afterScheme = uri.slice('crisp+local://'.length);
        const slashIdx = afterScheme.indexOf('/');
        if (slashIdx < 0) return null;
        return afterScheme.slice(slashIdx);
    }

    // Absolute path (Unix or Windows drive letter)
    if (uri.startsWith('/') || /^[A-Za-z]:[/\\]/.test(uri)) {
        return uri;
    }

    // Non-local schemes (crisp+drive://, crisp+cb-archive://, etc.)
    return null;
}

/** Extract the lowercase file extension from a path or filename. */
export function extOf(name: string): string {
    const dot = name.lastIndexOf('.');
    if (dot < 0) return '';
    return name.slice(dot + 1).toLowerCase();
}

/** Determine which sub-viewer to use based on file extension. */
export function detectKind(ext: string): ViewerKind {
    if (ext === 'pdf') return 'pdf';
    if (IMAGE_EXTS.has(ext)) return 'image';
    if (ext === 'docx') return 'docx';
    if (ext === 'epub') return 'epub';
    if (ext === 'htm' || ext === 'html') return 'html';
    if (CSV_EXTS.has(ext)) return 'csv';
    if (TEXT_EXTS.has(ext)) return 'text';
    return 'fallback';
}
