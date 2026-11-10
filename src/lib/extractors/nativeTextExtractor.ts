import { invoke } from '@tauri-apps/api/core';
import { logInfo } from '../log';
import type { ExtractionOptions, ExtractionResult } from './index';
import { nativeExtractionResult, requireNativePath } from './nativePath';

/** Mail and presentation readers extract stored text; this command does not run OCR. */
export async function extractNativeText(name: string, path: string | undefined, options: ExtractionOptions = {}): Promise<ExtractionResult> {
    options.signal?.throwIfAborted();
    const localPath = requireNativePath(path, 'Mail and presentation');
    logInfo(`Extracting ${name} with the bounded native text reader`);
    const result = await invoke<unknown>('native_extract_text', { path: localPath });
    // Cancelling the waiting UI discards the late result; it does not stop an
    // already running native format parser.
    options.signal?.throwIfAborted();
    return nativeExtractionResult(result, 'native text');
}
