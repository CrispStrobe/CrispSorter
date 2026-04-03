import { extractPdf } from './pdfExtractor';
import { extractDocx } from './docxExtractor';
import { extractEpub } from './epubExtractor';

export interface ExtractionResult {
    text: string;
    /** Markdown-formatted version of the document, with headings as `#`/`##` etc. */
    markdownText?: string;
    /** Ordered list of section heading strings extracted from the document. */
    headings?: string[];
    metadata?: Record<string, any>;
}

export interface ExtractionOptions {
    forceOCR?: boolean;
    signal?: AbortSignal;
    onProgress?: (page: number, total: number) => void;
    maxChars?: number;
    maxPages?: number;
}

/** Extract `#`/`##`/`###` headings from a Markdown string. */
function headingsFromMarkdown(md: string): string[] {
    return md.split('\n')
        .filter(line => /^#{1,6}\s/.test(line))
        .map(line => line.replace(/^#{1,6}\s+/, '').trim())
        .filter(Boolean);
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
    let markdownText: string | undefined;
    let headings: string[] | undefined;

    console.log(`[ExtractorIndex] Extension detected: ${extension}`);

    switch (extension) {
        case 'pdf':
            console.log("[ExtractorIndex] Handing off to extractPdf");
            text = await extractPdf(arrayBuffer, options);
            // PDF: build lightweight markdown with heuristic heading detection.
            ({ markdownText, headings } = pdfTextToMarkdown(text));
            break;
        case 'docx':
            console.log("[ExtractorIndex] Handing off to extractDocx");
            ({ text, markdownText, headings } = await extractDocx(arrayBuffer));
            break;
        case 'epub':
            console.log("[ExtractorIndex] Handing off to extractEpub");
            ({ text, markdownText, headings } = await extractEpub(arrayBuffer, name, options));
            break;
        case 'txt': {
            console.log("[ExtractorIndex] Handling txt internally");
            const decoder = new TextDecoder('utf-8');
            text = decoder.decode(arrayBuffer);
            break;
        }
        case 'md': {
            console.log("[ExtractorIndex] Handling md internally");
            const decoder = new TextDecoder('utf-8');
            text = decoder.decode(arrayBuffer);
            markdownText = text;
            headings = headingsFromMarkdown(text);
            break;
        }
        default:
            console.warn(`[ExtractorIndex] Unsupported type: ${extension}`);
            throw new Error(`Unsupported file type: ${extension}`);
    }

    console.log(`[ExtractorIndex] Extraction finished, result length: ${text.length}, headings: ${headings?.length ?? 0}`);
    return { text, markdownText, headings };
}

/**
 * Heuristic heading detection for plain PDF text.
 *
 * A line is treated as a heading if it is:
 *   - Non-empty after trimming
 *   - Shorter than 120 characters
 *   - Does NOT end with a period, comma, or semicolon
 *   - Followed by at least one non-empty line (not a dangling fragment)
 *
 * Returns a simple Markdown string where detected headings become `## ` lines.
 */
function pdfTextToMarkdown(text: string): { markdownText: string; headings: string[] } {
    const lines = text.split('\n');
    const md: string[] = [];
    const headings: string[] = [];

    for (let i = 0; i < lines.length; i++) {
        const line = lines[i].trim();
        if (!line) { md.push(''); continue; }

        const nextLine = (lines[i + 1] ?? '').trim();
        const isHeading =
            line.length < 120 &&
            !/[.,;:?!]$/.test(line) &&
            nextLine.length > 0 &&
            // Avoid treating page numbers as headings
            !/^\d+$/.test(line);

        if (isHeading) {
            md.push(`## ${line}`);
            headings.push(line);
        } else {
            md.push(line);
        }
    }

    return { markdownText: md.join('\n'), headings };
}
