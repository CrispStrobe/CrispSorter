import { extractPdf } from './pdfExtractor';
import { extractDocx } from './docxExtractor';

export interface ExtractionResult {
    text: string;
    metadata?: Record<string, any>;
}

export async function extractText(file: File | { name: string, arrayBuffer: ArrayBuffer }): Promise<ExtractionResult> {
    console.log(`[ExtractorIndex] Routing file: ${file.name}`);
    let name: string;
    let arrayBuffer: ArrayBuffer;

    if (file instanceof File) {
        name = file.name;
        arrayBuffer = await file.arrayBuffer();
    } else {
        name = file.name;
        arrayBuffer = file.arrayBuffer;
    }

    const extension = name.split('.').pop()?.toLowerCase();
    let text = '';

    console.log(`[ExtractorIndex] Extension detected: ${extension}`);

    switch (extension) {
        case 'pdf':
            console.log("[ExtractorIndex] Handing off to extractPdf");
            text = await extractPdf(arrayBuffer);
            break;
        case 'docx':
            console.log("[ExtractorIndex] Handing off to extractDocx");
            text = await extractDocx(arrayBuffer);
            break;
        case 'txt':
        case 'md':
            console.log("[ExtractorIndex] Handling text/md internally");
            const decoder = new TextDecoder('utf-8');
            text = decoder.decode(arrayBuffer);
            break;
        default:
            console.warn(`[ExtractorIndex] Unsupported type: ${extension}`);
            throw new Error(`Unsupported file type: ${extension}`);
    }

    console.log(`[ExtractorIndex] Extraction finished, result length: ${text.length}`);
    return { text };
}
