import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf.mjs';
import Tesseract from 'tesseract.js';
import { getSetting } from '../store';

// Point to the local static worker file we just copied
pdfjsLib.GlobalWorkerOptions.workerSrc = '/pdf.worker.min.mjs';

export async function extractPdf(arrayBuffer: ArrayBuffer): Promise<string> {
    console.log(`[PDFExtractor] Starting extraction, buffer size: ${arrayBuffer.byteLength}`);
    const ocrEnabled = await getSetting('ocrEnabled', false);
    
    try {
        const loadingTask = pdfjsLib.getDocument({ 
            data: arrayBuffer,
            useSystemFonts: true,
            disableFontFace: true 
        });
        
        const pdfDocument = await loadingTask.promise;
        let fullText = '';
        const numPages = pdfDocument.numPages;
        console.log(`[PDFExtractor] Document loaded, pages: ${numPages}`);

        for (let pageNum = 1; pageNum <= numPages; pageNum++) {
            console.log(`[PDFExtractor] Processing page ${pageNum}/${numPages}...`);
            const page = await pdfDocument.getPage(pageNum);
            const textContent = await page.getTextContent();
            
            let pageText = textContent.items
                .map((item: any) => item.str || '')
                .join(' ');
            
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
