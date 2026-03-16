import { extractPdf } from './pdfExtractor';
import { extractDocx } from './docxExtractor';

export interface ExtractionResult {
    text: string;
    metadata?: Record<string, any>;
}

export interface ExtractionOptions {
    forceOCR?: boolean;
    signal?: AbortSignal;
    onProgress?: (page: number, total: number) => void;
    maxChars?: number;
    maxPages?: number;
}

export async function extractText(
    file: File | { name: string, arrayBuffer: ArrayBuffer }, 
    options: ExtractionOptions = {}
): Promise<ExtractionResult> {
    console.log(`[ExtractorIndex] Routing file: ${file.name}, forceOCR: ${options.forceOCR}`);
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
            text = await extractPdf(arrayBuffer, options);
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
