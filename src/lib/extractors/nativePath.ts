import { extractionTags, type ExtractionResult } from './index';

/** Native document readers need an absolute host path, never a provider URI. */
export function requireNativePath(path: string | undefined, format: string): string {
    const absolute = path && (/^\//.test(path) || /^[a-z]:[\\/]/i.test(path) || /^\\\\[^\\]/.test(path));
    if (!absolute || path.includes('\0') || /^[a-z][a-z0-9+.-]*:\/\//i.test(path)) {
        throw new Error(`${format} extraction requires a local file-system path. Save the file locally and reopen it with Add Files.`);
    }
    return path;
}

/** Keep native extraction coverage and structural headings at the UI boundary. */
export function nativeExtractionResult(value: unknown, format: string): ExtractionResult {
    const extracted = value as { text?: unknown; headings?: unknown; tags?: unknown } | null;
    if (!extracted || typeof extracted.text !== 'string') throw new Error(`The ${format} extractor returned no usable response.`);
    return {
        text: extracted.text,
        headings: Array.isArray(extracted.headings) ? extracted.headings.filter((heading): heading is string => typeof heading === 'string') : [],
        metadata: { tags: extractionTags({ text: '', metadata: { tags: extracted.tags } }) },
    };
}
