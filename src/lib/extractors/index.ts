import { extractPdf } from './pdfExtractor';
import { extractDocx } from './docxExtractor';
import { extractEpub } from './epubExtractor';
import { extractHtml } from './htmlExtractor';
import { extractImage } from './imageExtractor';
import { logInfo, logWarn, logDebug } from '../log';

export interface ExtractionResult {
    text: string;
    /** Markdown-formatted version of the document, with headings as `#`/`##` etc. */
    markdownText?: string;
    /** Ordered list of section heading strings extracted from the document. */
    headings?: string[];
    metadata?: Record<string, any>;
}

/** File extensions whose extraction is supported end-to-end. */
export const SUPPORTED_EXTENSIONS = [
    'pdf', 'docx', 'epub', 'txt', 'md',
    'html', 'htm',
    'webp', 'png', 'jpg', 'jpeg', 'bmp', 'tif', 'tiff',
    'doc',
] as const;

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
    let pickedTool = '';

    // info-level: which extractor was picked (visible to users by
    // default). Per-page chatter inside the individual extractors
    // stays at console.log (browser-devtools only); the milestone
    // events the user actually wants to see go through flog.
    switch (extension) {
        case 'pdf':
            pickedTool = 'pdfjs-dist (JS)';
            logInfo(`Extracting ${name} with ${pickedTool} (${(arrayBuffer.byteLength / 1024).toFixed(0)} KB input)`);
            text = await extractPdf(arrayBuffer, options);
            // PDF: build lightweight markdown with heuristic heading detection.
            ({ markdownText, headings } = pdfTextToMarkdown(text));
            break;
        case 'docx':
            pickedTool = 'mammoth (DOCX)';
            logInfo(`Extracting ${name} with ${pickedTool} (${(arrayBuffer.byteLength / 1024).toFixed(0)} KB input)`);
            ({ text, markdownText, headings } = await extractDocx(arrayBuffer));
            break;
        case 'epub':
            pickedTool = '@lingo-reader/epub-parser';
            logInfo(`Extracting ${name} with ${pickedTool} (${(arrayBuffer.byteLength / 1024).toFixed(0)} KB input)`);
            ({ text, markdownText, headings } = await extractEpub(arrayBuffer, name, options));
            break;
        case 'txt': {
            pickedTool = 'TextDecoder utf-8';
            logDebug(`Extracting ${name} as plain text`);
            const decoder = new TextDecoder('utf-8');
            text = decoder.decode(arrayBuffer);
            break;
        }
        case 'md': {
            pickedTool = 'TextDecoder utf-8 (md)';
            logDebug(`Extracting ${name} as markdown`);
            const decoder = new TextDecoder('utf-8');
            text = decoder.decode(arrayBuffer);
            markdownText = text;
            headings = headingsFromMarkdown(text);
            break;
        }
        case 'html':
        case 'htm':
            pickedTool = 'DOMParser (HTML)';
            logInfo(`Extracting ${name} with ${pickedTool} (${(arrayBuffer.byteLength / 1024).toFixed(0)} KB input)`);
            ({ text, markdownText, headings } = await extractHtml(arrayBuffer));
            break;
        case 'webp':
        case 'png':
        case 'jpg':
        case 'jpeg':
        case 'bmp':
        case 'tif':
        case 'tiff':
            pickedTool = `tesseract.js OCR (.${extension})`;
            logInfo(`Extracting ${name} with ${pickedTool} (${(arrayBuffer.byteLength / 1024).toFixed(0)} KB input)`);
            ({ text, markdownText, headings } = await extractImage(arrayBuffer, name));
            break;
        case 'doc':
            // Legacy MS Word (CFB / OLE2). Browser libraries can't reliably read this.
            // Surface a clear message so the user can convert to .docx.
            logWarn(`Legacy .doc rejected (no in-app extractor): ${name}`);
            throw new Error(
                'Legacy .doc files are not supported in-app. Please convert the file to .docx, .pdf, or .txt and try again.'
            );
        default:
            logWarn(`Unsupported file type rejected: ${name} (.${extension})`);
            throw new Error(`Unsupported file type: ${extension}`);
    }

    logDebug(`Extraction finished for ${name}: ${text.length.toLocaleString()} chars, ${headings?.length ?? 0} headings (via ${pickedTool})`);
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
