import { invoke } from '@tauri-apps/api/core';
import { logInfo } from '../log';
import type { ExtractionOptions, ExtractionResult } from './index';
import { nativeExtractionResult, requireNativePath } from './nativePath';
import { nativeOcrOptions } from './nativeOptions';

/** Native extraction preserves one root receipt and independently indexable members. */
export async function extractArchive(name: string, path: string | undefined, options: ExtractionOptions = {}): Promise<ExtractionResult> {
    options.signal?.throwIfAborted();
    const localPath = requireNativePath(path, 'Archive');
    const ocr = await nativeOcrOptions(options);
    logInfo(`Extracting archive ${name} with bounded native member extraction`);
    const extracted = await invoke<{ text: string; headings: string[]; tags: string[] }>('archive_extract_text', {
        path: localPath,
        options: ocr,
    });
    // Native extraction is not interruptible through AbortSignal. A cancelled
    // caller must still discard its late result instead of publishing success.
    options.signal?.throwIfAborted();
    return nativeExtractionResult(extracted, 'archive');
}
