import { invoke } from '@tauri-apps/api/core';
import { runOperation } from '../operationLog';
import type { ExtractionOptions, ExtractionResult } from './index';
import { nativeExtractionResult, requireNativePath } from './nativePath';
import { nativeOcrOptions } from './nativeOptions';

/** Mail attachments share the bounded document pipeline; presentations read stored text. */
export async function extractNativeText(name: string, path: string | undefined, options: ExtractionOptions = {}): Promise<ExtractionResult> {
    const mail = ['eml', 'mbox'].includes(name.split('.').at(-1)?.toLowerCase() ?? '');
    return runOperation('document.extract', { kind: mail ? 'mail' : 'presentation', backend: 'native' }, async operation => {
        options.signal?.throwIfAborted();
        const localPath = requireNativePath(path, 'Mail and presentation');
        operation.stage('dispatching');
        const result = await invoke<unknown>('native_extract_text', { path: localPath,
            ...(mail ? { options: await nativeOcrOptions(options) } : {}) });
        // Cancelling the waiting UI discards the late result; it does not stop an
        // already running native format parser.
        if (options.signal?.aborted) { operation.cancel(); options.signal.throwIfAborted(); }
        return nativeExtractionResult(result, 'native text');
    }, result => ({ characters: result.text.length, count: result.members?.length ?? 0 }));
}
