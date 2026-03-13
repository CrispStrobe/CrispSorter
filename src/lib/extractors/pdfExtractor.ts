import * as pdfjsLib from 'pdfjs-dist';

// Use Vite's worker loading for better compatibility
import pdfjsWorker from 'pdfjs-dist/build/pdf.worker.mjs?url';

console.log(`[PDFExtractor] Loading worker from: ${pdfjsWorker}`);
pdfjsLib.GlobalWorkerOptions.workerSrc = pdfjsWorker;

export async function extractPdf(arrayBuffer: ArrayBuffer): Promise<string> {
    console.log(`[PDFExtractor] Starting extraction, buffer size: ${arrayBuffer.byteLength}`);
    try {
        const loadingTask = pdfjsLib.getDocument({ 
            data: arrayBuffer,
            useSystemFonts: true,
            disableFontFace: true 
        });
        
        console.log("[PDFExtractor] Document loading task created");
        const pdfDocument = await loadingTask.promise;
        
        let fullText = '';
        const numPages = pdfDocument.numPages;
        console.log(`[PDFExtractor] Document loaded, pages: ${numPages}`);

        for (let pageNum = 1; pageNum <= numPages; pageNum++) {
            console.log(`[PDFExtractor] Processing page ${pageNum}/${numPages}...`);
            const page = await pdfDocument.getPage(pageNum);
            const textContent = await page.getTextContent();
            
            const pageText = textContent.items
                .map((item) => {
                    if ('str' in item) return item.str;
                    return '';
                })
                .join(' ');
            
            fullText += pageText + '\n';
        }

        console.log(`[PDFExtractor] Extraction complete, total text length: ${fullText.length}`);
        return fullText.trim();
    } catch (error: any) {
        console.error("[PDFExtractor] CRITICAL ERROR:", error);
        throw new Error(`Failed to extract PDF: ${error.message || String(error)}`);
    }
}
