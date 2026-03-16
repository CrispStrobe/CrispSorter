// src/lib/extractors/pdfExtractor.ts:

import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf';
import Tesseract from 'tesseract.js';
import { getSetting } from '../store';

// Set worker to the local legacy file we just copied to static/
pdfjsLib.GlobalWorkerOptions.workerSrc = '/pdf.worker.js';

// Robust polyfill for async iteration on ReadableStream
if (typeof ReadableStream !== 'undefined' && !ReadableStream.prototype[Symbol.asyncIterator]) {
    console.log("[PDFExtractor] Polyfilling ReadableStream.prototype[Symbol.asyncIterator]");
    // @ts-ignore
    ReadableStream.prototype[Symbol.asyncIterator] = async function* () {
        const reader = this.getReader();
        try {
            while (true) {
                const { done, value } = await reader.read();
                if (done) return;
                yield value;
            }
        } finally {
            reader.releaseLock();
        }
    };
}

// Polyfill for values() specifically if that's what's failing
if (typeof ReadableStream !== 'undefined' && !ReadableStream.prototype.values) {
    console.log("[PDFExtractor] Polyfilling ReadableStream.prototype.values");
    // @ts-ignore
    ReadableStream.prototype.values = ReadableStream.prototype[Symbol.asyncIterator];
}

export async function extractPdf(arrayBuffer: ArrayBuffer, options: { forceOCR?: boolean; signal?: AbortSignal; onProgress?: (page: number, total: number) => void; maxChars?: number; maxPages?: number } = {}): Promise<string> {
    console.log(`[PDFExtractor] Starting extraction, buffer size: ${arrayBuffer.byteLength}, forceOCR: ${options.forceOCR ?? false}`);
    const ocrEnabled = (options.forceOCR ?? false) || await getSetting('ocrEnabled', false);
    
    try {
        const data = new Uint8Array(arrayBuffer);
        
        const loadingTask = pdfjsLib.getDocument({ 
            data: data,
            useSystemFonts: true,
            disableFontFace: true,
            disableRange: true,
            disableStream: true,
            disableAutoFetch: true
        });
        
        const pdfDocument = await loadingTask.promise;
        let fullText = '';
        const numPages = pdfDocument.numPages;
        console.log(`[PDFExtractor] Document loaded successfully, pages: ${numPages}`);

        for (let pageNum = 1; pageNum <= numPages; pageNum++) {
            // Abort signal check
            if (options.signal?.aborted) {
                console.log(`[PDFExtractor] Aborted at page ${pageNum}/${numPages}`);
                break;
            }
            // Progress callback
            options.onProgress?.(pageNum, numPages);
            // Early stop: enough text already
            if (options.maxChars && fullText.length >= options.maxChars) {
                console.log(`[PDFExtractor] Reached maxChars (${options.maxChars}) at page ${pageNum}/${numPages}, stopping early.`);
                break;
            }
            // Page limit
            if (options.maxPages && pageNum > options.maxPages) {
                console.log(`[PDFExtractor] Reached maxPages (${options.maxPages}), stopping early.`);
                break;
            }
            console.log(`[PDFExtractor] Processing page ${pageNum}/${numPages}...`);
            const page = await pdfDocument.getPage(pageNum);

            let pageText = '';

            // If NOT forcing OCR, try to get digital text first
            if (!(options.forceOCR ?? false)) {
                try {
                    const textContent = await page.getTextContent();
                    const items = textContent.items as any[];
                    
                    items.sort((a, b) => {
                        const yDiff = b.transform[5] - a.transform[5];
                        if (Math.abs(yDiff) > 5) return yDiff;
                        return a.transform[4] - b.transform[4];
                    });

                    let lastY = -1;
                    for (const item of items) {
                        const currentY = item.transform[5];
                        if (lastY !== -1 && Math.abs(currentY - lastY) > 5) {
                            pageText += '\n';
                        } else if (lastY !== -1) {
                            pageText += ' ';
                        }
                        pageText += item.str || '';
                        lastY = currentY;
                    }
                } catch (textErr) {
                    console.warn(`[PDFExtractor] Text extraction failed for page ${pageNum}:`, textErr);
                }
            } else {
                console.log(`[PDFExtractor] Skipping digital text extraction for page ${pageNum} (forceOCR enabled)`);
            }

            // If text is still empty (or we are forcing OCR) and OCR is enabled, run OCR
            if ((pageText.trim().length < 20 || (options.forceOCR ?? false)) && ocrEnabled) {
                console.log(`[PDFExtractor] Page ${pageNum} triggering OCR (forceOCR=${options.forceOCR ?? false}, length=${pageText.trim().length})...`);
                try {
                    const canvas = document.createElement('canvas');
                    const viewport = page.getViewport({ scale: 2.0 }); 
                    canvas.height = viewport.height;
                    canvas.width = viewport.width;
                    
                    const renderContext = {
                        canvasContext: canvas.getContext('2d')!,
                        viewport: viewport
                    };
                    
                    await page.render(renderContext).promise;
                    const imageData = canvas.toDataURL('image/png');
                    
                    const { data: { text } } = await Tesseract.recognize(imageData, 'deu+eng', {
                        logger: m => {
                            if (m.status === 'recognizing text' && Math.round(m.progress * 100) % 25 === 0) {
                                console.log(`[OCR] Page ${pageNum} - ${m.status}: ${Math.round(m.progress * 100)}%`);
                            }
                        }
                    });
                    pageText = text;
                    console.log(`[PDFExtractor] OCR Success for page ${pageNum}, length: ${pageText.length}`);
                } catch (ocrErr) {
                    console.error(`[PDFExtractor] OCR Failed for page ${pageNum}:`, ocrErr);
                }
            }
            
            fullText += pageText + '\n';
        }

        console.log(`[PDFExtractor] Extraction complete, total text length: ${fullText.length}`);
        // Release pdf.js internal resources and any OS file handles
        await pdfDocument.destroy();
        return fullText.trim();
    } catch (error: any) {
        if (error?.message === 'EXTRACTION_ABORTED') throw error;
        console.error("[PDFExtractor] CRITICAL ERROR:", error);
        throw new Error(`Failed to extract PDF: ${error.message || String(error)}`);
    }
}
