import { getSetting } from '../store';
import type { ExtractionOptions } from './index';
import { loadSavedOcrPipeline } from './ocrSettings';

/** Apply the same saved OCR policy to document members and mail attachments. */
export async function nativeOcrOptions(options: ExtractionOptions) {
    options.signal?.throwIfAborted();
    const [enabled, tier, language, pipeline] = await Promise.all([
        getSetting('ocrEnabled', false), getSetting('ocrTier', 'auto'), getSetting('ocrRecLang', 'auto'),
        loadSavedOcrPipeline(),
    ]);
    options.signal?.throwIfAborted();
    return {
        tryOcr: options.forceOCR === true || enabled === true,
        ocrTier: ['auto', 'tier1', 'tier2', 'tier3', 'tier4'].includes(tier) ? tier : 'auto',
        ocrRecLang: ['auto', 'latin', 'cjk'].includes(language) ? language : 'auto',
        ...(pipeline ? { ocrPipeline: pipeline } : {}),
    };
}
