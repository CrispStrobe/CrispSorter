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

export async function extractPdf(arrayBuffer: ArrayBuffer): Promise<string> {
    console.log(`[PDFExtractor] Starting extraction, buffer size: ${arrayBuffer.byteLength}`);
    const ocrEnabled = await getSetting('ocrEnabled', false);
    
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
            console.log(`[PDFExtractor] Processing page ${pageNum}/${numPages}...`);
            const page = await pdfDocument.getPage(pageNum);
            
            let pageText = '';
            try {
                const textContent = await page.getTextContent();
                // PDF.js returns items in internal object order, which often breaks headers/columns.
                // We sort by Y coordinate (top to bottom) and then X coordinate (left to right).
                const items = textContent.items as any[];
                
                // Sort items: higher Y first (top), then lower X (left)
                items.sort((a, b) => {
                    const yDiff = b.transform[5] - a.transform[5];
                    if (Math.abs(yDiff) > 5) return yDiff; // Use a threshold for "same line"
                    return a.transform[4] - b.transform[4];
                });

                let lastY = -1;
                for (const item of items) {
                    const currentY = item.transform[5];
                    if (lastY !== -1 && Math.abs(currentY - lastY) > 5) {
                        pageText += '\n'; // New line if Y changes significantly
                    } else if (lastY !== -1) {
                        pageText += ' '; // Space if on same line
                    }
                    pageText += item.str || '';
                    lastY = currentY;
                }
            } catch (textErr) {
                console.warn(`[PDFExtractor] Text extraction failed for page ${pageNum}:`, textErr);
            }
            
            // If no text was found and OCR is enabled, try OCR on this page
            if (pageText.trim().length < 20 && ocrEnabled) {
                console.log(`[PDFExtractor] Page ${pageNum} seems empty or scanned. Running OCR...`);
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
                        logger: m => console.log(`[OCR] Page ${pageNum} - ${m.status}: ${Math.round(m.progress * 100)}%`)
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
        return fullText.trim();
    } catch (error: any) {
        console.error("[PDFExtractor] CRITICAL ERROR:", error);
        throw new Error(`Failed to extract PDF: ${error.message || String(error)}`);
    }
}
