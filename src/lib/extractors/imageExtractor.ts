/**
 * Image OCR extractor using the app's shared configured recognition policy:
 * webp, png, jpg/jpeg, bmp, tiff, gif (single-frame).
 *
 * Returns plain text; no markdown structure (OCR output is flat).
 */

import { createOcrSession, type OcrSessionOptions } from './ocrDispatcher';

export interface ImageExtractionResult {
    text: string;
    markdownText: string;
    headings: string[];
    metadata: { ocr: { engine: string; warnings: string[] } };
}

export async function extractImage(
    arrayBuffer: ArrayBuffer,
    filename: string,
    options: OcrSessionOptions | string = {},
): Promise<ImageExtractionResult> {
    const ext = (filename.split('.').pop() || 'png').toLowerCase();
    const mime =
        ext === 'webp' ? 'image/webp' :
        ext === 'jpg' || ext === 'jpeg' ? 'image/jpeg' :
        ext === 'bmp' ? 'image/bmp' :
        ext === 'tif' || ext === 'tiff' ? 'image/tiff' :
        ext === 'gif' ? 'image/gif' :
        'image/png';

    // Preserve the former explicit-language call signature for existing users.
    const session = await createOcrSession(typeof options === 'string' ? { ocr: { language: options } } : options);
    const result = await session.recognize(new Blob([arrayBuffer], { type: mime }), filename);
    return { text: result.text, markdownText: result.text, headings: [],
        metadata: { ocr: { engine: result.engine, warnings: result.warnings } } };
}
