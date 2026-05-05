/**
 * Image OCR extractor for raster formats Tesseract.js can read directly:
 * webp, png, jpg/jpeg, bmp, tiff, gif (single-frame).
 *
 * Returns plain text; no markdown structure (OCR output is flat).
 */

import Tesseract from 'tesseract.js';

export interface ImageExtractionResult {
    text: string;
    markdownText: string;
    headings: string[];
}

export async function extractImage(
    arrayBuffer: ArrayBuffer,
    filename: string,
    languages: string = 'eng+deu',
): Promise<ImageExtractionResult> {
    const ext = (filename.split('.').pop() || 'png').toLowerCase();
    const mime =
        ext === 'webp' ? 'image/webp' :
        ext === 'jpg' || ext === 'jpeg' ? 'image/jpeg' :
        ext === 'bmp' ? 'image/bmp' :
        ext === 'tif' || ext === 'tiff' ? 'image/tiff' :
        ext === 'gif' ? 'image/gif' :
        'image/png';

    const blob = new Blob([arrayBuffer], { type: mime });
    const url = URL.createObjectURL(blob);
    try {
        const { data } = await Tesseract.recognize(url, languages, {
            logger: m => {
                if (m.status === 'recognizing text' && Math.round(m.progress * 100) % 25 === 0) {
                    console.log(`[ImageExtractor] OCR ${m.status}: ${Math.round(m.progress * 100)}%`);
                }
            },
        });
        const text = (data.text ?? '').trim();
        return { text, markdownText: text, headings: [] };
    } finally {
        URL.revokeObjectURL(url);
    }
}
