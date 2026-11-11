import { invoke } from '@tauri-apps/api/core';
import { logInfo } from '../log';
import type { ExtractionOptions, ExtractionResult } from './index';
import { nativeExtractionResult, requireNativePath } from './nativePath';
import { nativeOcrOptions } from './nativeOptions';

/** Mail attachments share the bounded document pipeline; presentations read stored text. */
export async function extractNativeText(name: string, path: string | undefined, options: ExtractionOptions = {}): Promise<ExtractionResult> {
    options.signal?.throwIfAborted();
    const localPath = requireNativePath(path, 'Mail and presentation');
    logInfo(`Extracting ${name} with the bounded native text reader`);
    const mail = ['eml', 'mbox'].includes(name.split('.').at(-1)?.toLowerCase() ?? '');
    const result = await invoke<unknown>('native_extract_text', { path: localPath,
        ...(mail ? { options: await nativeOcrOptions(options) } : {}) });
    // Cancelling the waiting UI discards the late result; it does not stop an
    // already running native format parser.
    options.signal?.throwIfAborted();
    return nativeExtractionResult(result, 'native text');
}
