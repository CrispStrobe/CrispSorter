import { invoke } from '@tauri-apps/api/core';
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

/** Audio + video file extensions handled by the Rust-side audio
 *  extractor (symphonia tier-1 + ffmpeg tier-2 + CrispASR).  Keep in
 *  lockstep with `src-tauri/src/extractors/audio.rs::AUDIO_EXTS` —
 *  drift here means UI-accepts-but-backend-rejects (or vice versa). */
export const AUDIO_EXTENSIONS = [
    // Pure audio (symphonia tier-1)
    'wav', 'mp3', 'm4a', 'flac', 'ogg', 'opus', 'aac',
    'alac', 'caf', 'aiff',
    // Video containers — audio stream demuxed by symphonia tier-1
    'mp4', 'mov', 'mkv', 'webm', 'm4v',
    // Long-tail (tier-2 ffmpeg shell-out)
    'avi', 'wmv', 'flv', 'ts', 'amr', 'ra', '3gp', 'asf',
] as const;

/** Superset that drag-drop + file-picker zones use:
 *  documents + images (handled JS-side via `extractText`) plus
 *  audio/video (dispatched to the Rust `audio_extract_text`
 *  Tauri command).  Use this for any drop filter that should
 *  surface the full P13.5 multimodal capability — passing the
 *  document-only `SUPPORTED_EXTENSIONS` would silently drop
 *  audio files, which was the v0.1.40-era user complaint. */
export const MULTIMODAL_EXTENSIONS = [
    ...SUPPORTED_EXTENSIONS,
    ...AUDIO_EXTENSIONS,
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

/** Set of audio/video extensions for fast dispatch lookup inside
 *  `extractText`.  Mirrors `AUDIO_EXTENSIONS` but as a Set for
 *  O(1) membership checks; kept module-local to avoid surfacing
 *  the Set type in the public exports. */
const AUDIO_EXTS_SET = new Set<string>(AUDIO_EXTENSIONS);

export async function extractText(
    file: File | { name: string, arrayBuffer: ArrayBuffer, path?: string },
    options: ExtractionOptions = {}
): Promise<ExtractionResult> {
    let name: string;
    let arrayBuffer: ArrayBuffer;
    let path: string | undefined;

    if (file instanceof File) {
        name = file.name;
        arrayBuffer = await file.arrayBuffer();
        path = undefined; // Browser File has no host path
    } else {
        name = file.name;
        arrayBuffer = file.arrayBuffer;
        path = file.path;
    }

    const extension = name.split('.').pop()?.toLowerCase();

    // Audio / video: dispatch to the Rust-side audio extractor via
    // the `audio_extract_text` Tauri command.  Keeps the large PCM
    // buffer entirely in Rust.  Requires the caller to pass `path`
    // alongside `arrayBuffer` (drag-drop / file-picker call sites
    // have a host path; pasted/in-memory bytes don't, and would
    // error with a clear message).
    if (extension && AUDIO_EXTS_SET.has(extension)) {
        if (!path) {
            logWarn(`Audio extraction requires a host path: ${name}`);
            throw new Error(
                `Audio/video files need a file-system path. The dropped path was not forwarded; please re-open via "Add Files" instead of pasting.`
            );
        }
        const pickedTool = `crispasr (.${extension})`;
        logInfo(`Extracting ${name} with ${pickedTool} (path-based)`);
        // Tauri command returns { text, language? } — the second
        // field is the whisper-detected ISO 639-1 source language,
        // surfaced into ExtractionResult.metadata so callers (Stapel
        // language column, IndexIngest snippet routing) can use it.
        const res = await invoke<{ text: string; language: string | null }>('audio_extract_text', { path });
        logDebug(`Audio extraction finished for ${name}: ${res.text.length.toLocaleString()} chars (via ${pickedTool}, lang=${res.language ?? 'unknown'})`);
        return {
            text: res.text,
            markdownText: undefined,
            headings: [],
            metadata: res.language ? { language: res.language } : undefined,
        };
    }

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
