import { invoke } from '@tauri-apps/api/core';
import { runOperation } from '../operationLog';
import type { ExtractionOptions, ExtractionResult } from './index';
import { nativeExtractionResult, requireNativePath } from './nativePath';
import { nativeOcrOptions } from './nativeOptions';

/** Native extraction preserves one root receipt and independently indexable members. */
export async function extractArchive(name: string, path: string | undefined, options: ExtractionOptions = {}): Promise<ExtractionResult> {
    return runOperation('document.extract', { kind: 'archive', backend: 'native' }, async operation => {
        options.signal?.throwIfAborted();
        const localPath = requireNativePath(path, 'Archive');
        const ocr = await nativeOcrOptions(options);
        operation.stage('dispatching');
        const extracted = await invoke<{ text: string; headings: string[]; tags: string[] }>('archive_extract_text', {
            path: localPath,
            options: ocr,
        });
        // Native extraction is not interruptible through AbortSignal. A cancelled
        // caller must still discard its late result instead of publishing success.
        if (options.signal?.aborted) { operation.cancel(); options.signal.throwIfAborted(); }
        return nativeExtractionResult(extracted, 'archive');
    }, result => ({ characters: result.text.length, count: result.members?.length ?? 0 }));
}
