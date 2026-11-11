import { invoke } from '@tauri-apps/api/core';
// Extractors are dynamically imported on first use so mammoth, pdfjs,
// epub-parser, and tesseract.js don't bloat the initial bundle.
import { beginOperation } from '../operationLog';
import type { ContainerCoverage, ContainerEntry, ContainerSourceReceipt } from '../containers';
import type { OcrSelection } from './ocrSettings';

export interface ExtractionResult {
    text: string;
    /** Root/body text indexed separately from the aggregate preview of its members. */
    ownText?: string;
    ext?: string;
    /** Markdown-formatted version of the document, with headings as `#`/`##` etc. */
    markdownText?: string;
    /** Ordered list of section heading strings extracted from the document. */
    headings?: string[];
    metadata?: Record<string, any>;
    containerSource?: ContainerSourceReceipt;
    containerCoverage?: ContainerCoverage;
    /** Flattened leaves; member documents never contain another expansion. */
    members?: { entries: ContainerEntry[]; document: ExtractionResult }[];
}

/** File extensions whose extraction is supported end-to-end. */
export const SUPPORTED_EXTENSIONS = [
    'pdf', 'docx', 'epub', 'txt', 'md',
    'html', 'htm',
    'zip', 'tar', 'gz', 'gzip', 'tgz', '7z', 'eml', 'mbox', 'pptx',
    'webp', 'png', 'jpg', 'jpeg', 'bmp', 'tif', 'tiff',
    'doc',
] as const;

/** Formats whose bytes stay in a bounded native document reader. */
export const ARCHIVE_EXTENSIONS = ['zip', 'tar', 'gz', 'gzip', 'tgz', '7z'] as const;
const NATIVE_PATH_EXTENSIONS = new Set<string>([...ARCHIVE_EXTENSIONS, 'eml', 'mbox', 'pptx']);

export function usesNativePathExtraction(filename: string): boolean {
    return NATIVE_PATH_EXTENSIONS.has(filename.split('.').pop()?.toLowerCase() ?? '');
}

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
 *  audio/video, mail, presentations, and bounded archives (native, path-based
 *  extraction commands). Use this for any drop filter that should
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
    /** Explicit recognition choices override the saved extraction policy. */
    ocr?: OcrSelection;
}

/** Preserve extraction coverage labels through the foreground indexing boundary. */
export function extractionTags(result: ExtractionResult): string[] {
    const tags = result.metadata?.tags;
    return Array.isArray(tags)
        ? [...new Set(tags.filter((tag): tag is string => typeof tag === 'string').map(tag => tag.trim()).filter(Boolean))] : [];
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
    const operation = beginOperation('document.extract', { forced: options.forceOCR === true, format: file.name.split('.').pop()?.toLowerCase() });
    try {
        options.signal?.throwIfAborted();
        const result = await extractDocument(file, options);
        options.signal?.throwIfAborted();
        operation.complete({ characters: result.text.length, count: result.members?.length ?? 0 });
        return result;
    } catch (error) {
        if (options.signal?.aborted && error === options.signal.reason) operation.cancel();
        else operation.fail(error);
        throw error;
    }
}

async function extractDocument(
    file: File | { name: string, arrayBuffer: ArrayBuffer, path?: string },
    options: ExtractionOptions,
): Promise<ExtractionResult> {
    const name = file.name;
    const path = file instanceof File ? undefined : file.path;
    const extension = name.split('.').pop()?.toLowerCase();

    // Archive members stay in the bounded native extractor. Do not duplicate
    // an entire archive in the webview merely to pass its host path back.
    if (extension && (ARCHIVE_EXTENSIONS as readonly string[]).includes(extension)) {
        const { extractArchive } = await import('./archiveExtractor');
        return extractArchive(name, path, options);
    }
    if (extension && ['eml', 'mbox', 'pptx'].includes(extension)) {
        const { extractNativeText } = await import('./nativeTextExtractor');
        return extractNativeText(name, path, options);
    }

    const arrayBuffer = file instanceof File ? await file.arrayBuffer() : file.arrayBuffer;

    // Audio / video: dispatch to the Rust-side audio extractor via
    // the `audio_extract_text` Tauri command.  Keeps the large PCM
    // buffer entirely in Rust.  Requires the caller to pass `path`
    // alongside `arrayBuffer` (drag-drop / file-picker call sites
    // have a host path; pasted/in-memory bytes don't, and would
    // error with a clear message).
    if (extension && AUDIO_EXTS_SET.has(extension)) {
        if (!path) {
            throw new Error(
                `Audio/video files need a file-system path. The dropped path was not forwarded; please re-open via "Add Files" instead of pasting.`
            );
        }
        // Tauri command returns { text, language? } — the second
        // field is the whisper-detected ISO 639-1 source language,
        // surfaced into ExtractionResult.metadata so callers (Stapel
        // language column, IndexIngest snippet routing) can use it.
        const res = await invoke<{ text: string; language: string | null }>('audio_extract_text', { path });
        return {
            text: res.text,
            markdownText: undefined,
            headings: [],
            metadata: res.language ? { language: res.language } : undefined,
        };
    }

    // An empty file is rejected here, once, rather than by each
    // extractor in its own way — or, in one case, not at all.
    //
    // pdfjs raises "The PDF file is empty, i.e. its size is zero bytes",
    // but `@lingo-reader/epub-parser` handed a zero-byte buffer neither
    // returns nor rejects: it hangs. Because the batch runner awaits this
    // call, one such file wedged an entire 1017-item run at item 200 for
    // eight hours. The per-item AbortController could not help — aborting
    // is cooperative, and a parser that never yields never sees the
    // signal.
    //
    // Zero-byte files are not hypothetical here: a partial download, an
    // interrupted copy, or a cloud-storage placeholder that was never
    // materialised all produce them, and a document library accumulates
    // them silently.
    if (arrayBuffer.byteLength === 0) {
        throw new Error(`File is empty (0 bytes): ${name}`);
    }

    let text = '';
    let markdownText: string | undefined;
    let headings: string[] | undefined;
    let metadata: ExtractionResult['metadata'];

    // The outer receipt owns completion, including formats that do not use
    // OCR. Individual OCR pages and native transports have their own stages.
    switch (extension) {
        case 'pdf': {
            const { extractPdf } = await import('./pdfExtractor');
            text = await extractPdf(arrayBuffer, options);
            // PDF: build lightweight markdown with heuristic heading detection.
            ({ markdownText, headings } = pdfTextToMarkdown(text));
            break;
        }
        case 'docx': {
            const { extractDocx } = await import('./docxExtractor');
            ({ text, markdownText, headings } = await extractDocx(arrayBuffer));
            break;
        }
        case 'epub': {
            const { extractEpub } = await import('./epubExtractor');
            ({ text, markdownText, headings } = await extractEpub(arrayBuffer, name, options));
            break;
        }
        case 'txt': {
            const decoder = new TextDecoder('utf-8');
            text = decoder.decode(arrayBuffer);
            break;
        }
        case 'md': {
            const decoder = new TextDecoder('utf-8');
            text = decoder.decode(arrayBuffer);
            markdownText = text;
            headings = headingsFromMarkdown(text);
            break;
        }
        case 'html':
        case 'htm': {
            const { extractHtml } = await import('./htmlExtractor');
            ({ text, markdownText, headings } = await extractHtml(arrayBuffer));
            break;
        }
        case 'webp':
        case 'png':
        case 'jpg':
        case 'jpeg':
        case 'bmp':
        case 'tif':
        case 'tiff': {
            const { extractImage } = await import('./imageExtractor');
            ({ text, markdownText, headings, metadata } = await extractImage(arrayBuffer, name, options));
            break;
        }
        case 'doc':
            // Legacy MS Word (CFB / OLE2). Browser libraries can't reliably read this.
            // Surface a clear message so the user can convert to .docx.
            throw new Error(
                'Legacy .doc files are not supported in-app. Please convert the file to .docx, .pdf, or .txt and try again.'
            );
        default:
            throw new Error(`Unsupported file type: ${extension}`);
    }

    return { text, markdownText, headings, ...(metadata ? { metadata } : {}) };
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
