import { invoke } from '@tauri-apps/api/core';
import { getSetting } from '../store';
import { logInfo } from '../log';
import { extractionTags, type ExtractionOptions, type ExtractionResult } from './index';
import { loadSavedOcrPipeline } from './ocrSettings';

/** One ZIP remains one source; native extraction labels and bounds its members. */
export async function extractArchive(name: string, path: string | undefined, options: ExtractionOptions = {}): Promise<ExtractionResult> {
    options.signal?.throwIfAborted();
    if (!path || /^[a-z][a-z0-9+.-]*:\/\//i.test(path) || path.includes('\0')) {
        throw new Error('ZIP extraction requires a local file-system path. Save the archive locally and reopen it with Add Files.');
    }
    const [enabled, tier, language, pipeline] = await Promise.all([
        getSetting('ocrEnabled', false), getSetting('ocrTier', 'auto'), getSetting('ocrRecLang', 'auto'),
        loadSavedOcrPipeline(),
    ]);
    options.signal?.throwIfAborted();
    logInfo(`Extracting ZIP archive ${name} with bounded native member extraction`);
    const extracted = await invoke<{ text: string; headings: string[]; tags: string[] }>('archive_extract_text', {
        path,
        options: {
            tryOcr: options.forceOCR === true || enabled === true,
            ocrTier: ['auto', 'tier1', 'tier2', 'tier3', 'tier4'].includes(tier) ? tier : 'auto',
            ocrRecLang: ['auto', 'latin', 'cjk'].includes(language) ? language : 'auto',
            ...(pipeline ? { ocrPipeline: pipeline } : {}),
        },
    });
    // Native extraction is not interruptible through AbortSignal. A cancelled
    // caller must still discard its late result instead of publishing success.
    options.signal?.throwIfAborted();
    if (!extracted || typeof extracted.text !== 'string') throw new Error('The archive extractor returned no usable response.');
    return {
        text: extracted.text,
        headings: Array.isArray(extracted.headings) ? extracted.headings.filter(heading => typeof heading === 'string') : [],
        metadata: { tags: extractionTags({ text: '', metadata: { tags: extracted.tags } }) },
    };
}
