import { detectKind, type ViewerKind } from './types';

export type FileAssociations = Record<string, ViewerKind>;

const CONFIGURABLE_KINDS = new Set<ViewerKind>([
    'pdf', 'image', 'docx', 'epub', 'text', 'html', 'csv', 'fallback',
]);

/** Validate extension → existing viewer-kind overrides from persisted JSON. */
export function normalizeFileAssociations(raw: unknown): FileAssociations {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return {};
    const result: FileAssociations = {};
    for (const [extension, kind] of Object.entries(raw)) {
        const ext = extension.toLowerCase().replace(/^\./, '');
        if (/^[a-z0-9][a-z0-9+_-]{0,15}$/.test(ext)
            && typeof kind === 'string'
            && CONFIGURABLE_KINDS.has(kind as ViewerKind)) {
            result[ext] = kind as ViewerKind;
        }
    }
    return result;
}

export function viewerKindForExtension(ext: string, associations: FileAssociations): ViewerKind {
    return associations[ext.toLowerCase()] ?? detectKind(ext);
}
