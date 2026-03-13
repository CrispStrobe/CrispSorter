import * as pdfjsLib from 'pdfjs-dist';

// Use standard CDN for worker to keep bundle small
pdfjsLib.GlobalWorkerOptions.workerSrc = `https://cdnjs.cloudflare.com/ajax/libs/pdf.js/${pdfjsLib.version}/pdf.worker.min.mjs`;

export async function extractPdf(arrayBuffer: ArrayBuffer): Promise<string> {
    try {
        const loadingTask = pdfjsLib.getDocument({ 
            data: arrayBuffer,
            useSystemFonts: true,
            disableFontFace: true 
        });
        const pdfDocument = await loadingTask.promise;
        
        let fullText = '';
        const numPages = pdfDocument.numPages;

        for (let pageNum = 1; pageNum <= numPages; pageNum++) {
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

        return fullText.trim();
    } catch (error: any) {
        console.error("PDF Extraction Error:", error);
        throw new Error(`Failed to extract PDF: ${error.message}`);
    }
}
