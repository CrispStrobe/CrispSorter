import type { ExtractionOptions } from './index';
import { loadSavedOcrPolicy, ocrPolicyLanguage } from './ocrSettings';

/** Apply the same saved OCR policy to document members and mail attachments. */
export async function nativeOcrOptions(options: ExtractionOptions) {
    options.signal?.throwIfAborted();
    const policy = await loadSavedOcrPolicy(options.ocr);
    options.signal?.throwIfAborted();
    return {
        tryOcr: options.forceOCR === true || policy.enabled,
        ocrTier: policy.ocrTier,
        ocrRecLang: policy.ocrRecLang,
        ocrPipeline: { ...policy.config, language: ocrPolicyLanguage(policy) },
    };
}
