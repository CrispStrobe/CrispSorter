import { extractPdf } from './pdfExtractor';
import { extractDocx } from './docxExtractor';

export interface ExtractionResult {
    text: string;
    metadata?: Record<string, any>;
}

export async function extractText(file: File | { name: string, arrayBuffer: ArrayBuffer }): Promise<ExtractionResult> {
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

    switch (extension) {
        case 'pdf':
            text = await extractPdf(arrayBuffer);
            break;
        case 'docx':
            text = await extractDocx(arrayBuffer);
            break;
        case 'txt':
        case 'md':
            const decoder = new TextDecoder('utf-8');
            text = decoder.decode(arrayBuffer);
            break;
        default:
            throw new Error(`Unsupported file type: ${extension}`);
    }

    return { text };
}
