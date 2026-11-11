import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf';
import type { ExtractionOptions } from './index';
import { createOcrSession, withExtractionAbort } from './ocrDispatcher';
import { logInfo } from '../log';

pdfjsLib.GlobalWorkerOptions.workerSrc = '/pdf.worker.js';

if (typeof ReadableStream !== 'undefined' && !(ReadableStream.prototype as any)[Symbol.asyncIterator]) {
    (ReadableStream.prototype as any)[Symbol.asyncIterator] = async function* () {
        const reader = this.getReader();
        try {
            while (true) {
                const { done, value } = await reader.read();
                if (done) return;
                yield value;
            }
        } finally { reader.releaseLock(); }
    };
}
if (typeof ReadableStream !== 'undefined' && !(ReadableStream.prototype as any).values) {
    (ReadableStream.prototype as any).values = (ReadableStream.prototype as any)[Symbol.asyncIterator];
}

const MIN_DIGITAL_TEXT = 20;
const MAX_OCR_PIXELS = 12_000_000;
const MAX_OCR_SIDE = 8192;
const IMAGE_OPS = new Set([
    pdfjsLib.OPS.paintImageXObject, pdfjsLib.OPS.paintInlineImageXObject,
    pdfjsLib.OPS.paintImageMaskXObject, pdfjsLib.OPS.paintImageMaskXObjectGroup,
    pdfjsLib.OPS.paintImageXObjectRepeat, pdfjsLib.OPS.paintInlineImageXObjectGroup,
    pdfjsLib.OPS.paintImageMaskXObjectRepeat, pdfjsLib.OPS.paintSolidColorImageMask,
].filter(value => typeof value === 'number'));
const VISIBLE_OPS = new Set([
    ...IMAGE_OPS, pdfjsLib.OPS.stroke, pdfjsLib.OPS.closeStroke, pdfjsLib.OPS.fill,
    pdfjsLib.OPS.eoFill, pdfjsLib.OPS.fillStroke, pdfjsLib.OPS.eoFillStroke,
    pdfjsLib.OPS.closeFillStroke, pdfjsLib.OPS.closeEOFillStroke,
    pdfjsLib.OPS.shadingFill, pdfjsLib.OPS.showText, pdfjsLib.OPS.showSpacedText,
    pdfjsLib.OPS.nextLineShowText, pdfjsLib.OPS.nextLineSetSpacingShowText,
].filter(value => typeof value === 'number'));

type PdfPage = Awaited<ReturnType<Awaited<ReturnType<typeof pdfjsLib.getDocument>['promise']>['getPage']>>;

function textFromItems(items: unknown[]): string {
    const text = items.filter((item): item is { str: string; transform: number[] } => {
        if (!item || typeof item !== 'object') return false;
        const value = item as Record<string, unknown>;
        return typeof value.str === 'string' && Array.isArray(value.transform)
            && Number.isFinite(value.transform[4]) && Number.isFinite(value.transform[5]);
    }).sort((a, b) => {
        const yDiff = b.transform[5] - a.transform[5];
        return Math.abs(yDiff) > 5 ? yDiff : a.transform[4] - b.transform[4];
    });
    return text.map((item, index) => index === 0 ? item.str
        : `${Math.abs(text[index - 1].transform[5] - item.transform[5]) > 5 ? '\n' : ' '}${item.str}`).join('');
}

/** Short digital titles need no OCR; scan images and outlined text still do. */
async function needsOcr(page: PdfPage, text: string, signal?: AbortSignal): Promise<boolean> {
    if (text.trim().length >= MIN_DIGITAL_TEXT) return false;
    try {
        const operators = await withExtractionAbort(page.getOperatorList(), signal);
        return operators.fnArray.some(op => (text.trim() ? IMAGE_OPS : VISIBLE_OPS).has(op));
    } catch (error) {
        signal?.throwIfAborted();
        // An unavailable operator list must not silently skip a scanned page.
        console.warn('[PDFExtractor] Could not inspect page drawing operations:', error);
        return true;
    }
}

function pageLimit(value: number | undefined, name: string): number {
    if (value === undefined) return Infinity;
    if (!Number.isFinite(value) || value < 0) throw new Error(`${name} must be a nonnegative finite number.`);
    return Math.floor(value);
}

export async function extractPdf(arrayBuffer: ArrayBuffer, options: ExtractionOptions = {}): Promise<string> {
    options.signal?.throwIfAborted();
    const maxPages = pageLimit(options.maxPages, 'maxPages');
    const maxChars = pageLimit(options.maxChars, 'maxChars');
    if (maxPages === 0 || maxChars === 0) return '';
    const ocr = await createOcrSession(options);
    options.signal?.throwIfAborted();
    const ocrEnabled = options.forceOCR === true || ocr.policy.enabled;
    const loadingTask = pdfjsLib.getDocument({
        data: new Uint8Array(arrayBuffer), useSystemFonts: true, disableFontFace: true,
        disableRange: true, disableStream: true, disableAutoFetch: true,
    });
    let reportedEngine = false;
    try {
        const pdf = await withExtractionAbort(loadingTask.promise, options.signal);
        const chunks: string[] = [];
        let length = 0;
        for (let pageNumber = 1; pageNumber <= Math.min(pdf.numPages, maxPages) && length < maxChars; pageNumber++) {
            options.signal?.throwIfAborted();
            options.onProgress?.(pageNumber, pdf.numPages);
            const page = await withExtractionAbort(pdf.getPage(pageNumber), options.signal);
            try {
                let pageText = '';
                let textFailed = false;
                if (!options.forceOCR) {
                    try {
                        const content = await withExtractionAbort(page.getTextContent(), options.signal);
                        pageText = textFromItems(content.items);
                    } catch (error) {
                        options.signal?.throwIfAborted();
                        if (!ocrEnabled) throw error;
                        textFailed = true;
                    }
                }
                if (ocrEnabled && (options.forceOCR || textFailed || await needsOcr(page, pageText, options.signal))) {
                    const base = page.getViewport({ scale: 1 });
                    if (!Number.isFinite(base.width) || !Number.isFinite(base.height) || base.width <= 0 || base.height <= 0) {
                        throw new Error(`PDF page ${pageNumber} has invalid dimensions.`);
                    }
                    const scale = Math.min(2, Math.sqrt(MAX_OCR_PIXELS / (base.width * base.height)), MAX_OCR_SIDE / Math.max(base.width, base.height));
                    const viewport = page.getViewport({ scale });
                    const canvas = document.createElement('canvas');
                    try {
                        canvas.width = Math.max(1, Math.floor(viewport.width));
                        canvas.height = Math.max(1, Math.floor(viewport.height));
                        const context = canvas.getContext('2d');
                        if (!context) throw new Error('A canvas context is unavailable for PDF OCR.');
                        const render = page.render({ canvasContext: context, viewport });
                        await withExtractionAbort(render.promise, options.signal, () => render.cancel());
                        options.signal?.throwIfAborted();
                        const result = await ocr.recognize(canvas, `page-${pageNumber}.png`);
                        if (!reportedEngine) { logInfo(`PDF OCR uses ${result.engine}`); reportedEngine = true; }
                        for (const warning of result.warnings) console.warn(`[PDFExtractor] Page ${pageNumber}: ${warning}`);
                        // A sparse existing text layer remains useful if OCR finds no text.
                        if (result.text.trim() || options.forceOCR) pageText = result.text;
                    } finally { canvas.width = 0; canvas.height = 0; }
                }
                const chunk = `${chunks.length ? '\n' : ''}${pageText}`.slice(0, maxChars - length);
                chunks.push(chunk);
                length += chunk.length;
            } finally {
                try { page.cleanup(); } catch (error) { console.warn('[PDFExtractor] Page cleanup failed:', error); }
            }
        }
        options.signal?.throwIfAborted();
        return chunks.join('').trim();
    } finally {
        // Covers parse, text, render, recognition, cancellation and early-stop paths.
        try { await loadingTask.destroy(); } catch (error) { console.warn('[PDFExtractor] PDF cleanup failed:', error); }
    }
}
