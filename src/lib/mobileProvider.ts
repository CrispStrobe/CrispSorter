import { invoke } from '@tauri-apps/api/core';
import { beginOperation } from './operationLog';

/** The first provider workflow supports read-only documents with JS extractors. */
export const PROVIDER_MAX_BYTES = 16 * 1024 * 1024;
const PROVIDER_EXTENSIONS = new Set(['pdf', 'docx', 'epub', 'txt', 'md', 'html', 'htm']);

export interface ProviderFile {
    uri: string;
    name: string;
    /** Providers may not know the size until the file has been opened. */
    size: number;
}

interface PickerResult {
    cancelled: boolean;
    files: ProviderFile[];
}

export function isProviderUri(path: string): boolean {
    return /^(content|file):\/\//i.test(path);
}

/** Provider document IDs are opaque and case-sensitive. */
export function batchSourceKey(path: string): string {
    return isProviderUri(path) ? `uri:${path}` : `path:${path.replace(/\\/g, '/').toLowerCase()}`;
}

export function partitionProviderFiles(files: ProviderFile[]) {
    const accepted: ProviderFile[] = [];
    let unsupported = 0;
    let oversized = 0;
    for (const file of files) {
        const ext = file.name.includes('.') ? file.name.split('.').pop()!.toLowerCase() : '';
        if (!isProviderUri(file.uri) || !PROVIDER_EXTENSIONS.has(ext)) {
            unsupported++;
        } else if (file.size > PROVIDER_MAX_BYTES) {
            oversized++;
        } else {
            accepted.push(file);
        }
    }
    return { accepted, unsupported, oversized };
}

export async function pickProviderDocuments(folder: boolean) {
    // Counts only: provider URIs and document names stay out of the log.
    const operation = beginOperation('app.provider_pick');
    let result: PickerResult;
    try {
        result = await invoke<PickerResult>(folder
            ? 'plugin:mobile-provider|pick_folder'
            : 'plugin:mobile-provider|pick_files');
    } catch (error) {
        operation.fail(error);
        throw error;
    }
    if (result.cancelled) {
        operation.cancel();
        return null;
    }
    const picked = partitionProviderFiles(result.files);
    operation.complete({
        total: result.files.length,
        count: picked.accepted.length,
        failed: picked.unsupported + picked.oversized,
    });
    return picked;
}

/** The native reader enforces the same limit while streaming, including when size is unknown. */
export async function readProviderFile(uri: string): Promise<Uint8Array> {
    if (!isProviderUri(uri)) throw new Error('Expected a document provider URI');
    const result = await invoke<{ data: string }>('plugin:mobile-provider|read_file', { uri });
    const binary = atob(result.data);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
    return bytes;
}
