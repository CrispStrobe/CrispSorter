import { extractionTags, type ExtractionResult } from './index';
import { readContainerCoverage, readContainerEntries, readContainerSource } from '../containers';

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
    const maximumTextBytes = 16 * 1024 * 1024;
    let indexedBytes = 0;
    const parse = (input: unknown, member: boolean): ExtractionResult => {
        const extracted = input as Record<string, unknown> | null;
        if (!extracted || typeof extracted !== 'object' || Array.isArray(extracted) || typeof extracted.text !== 'string') {
            throw new Error(`The ${format} extractor returned no usable response.`);
        }
        if (new TextEncoder().encode(extracted.text).byteLength > maximumTextBytes) throw new Error('Native extraction exceeds the text limit.');
        const metadata = extracted.metadata && typeof extracted.metadata === 'object' && !Array.isArray(extracted.metadata)
            ? { ...extracted.metadata as Record<string, unknown> } : {};
        const result: ExtractionResult = {
            text: extracted.text,
            headings: Array.isArray(extracted.headings) ? extracted.headings.filter((heading): heading is string => typeof heading === 'string') : [],
            metadata: { ...metadata, tags: extractionTags({ text: '', metadata: { tags: extracted.tags ?? metadata.tags } }) },
        };
        if (extracted.ownText != null) {
            if (typeof extracted.ownText !== 'string') throw new Error('Invalid native root text.');
            result.ownText = extracted.ownText;
        }
        if (extracted.ext != null) {
            if (typeof extracted.ext !== 'string' || !/^[a-z\d]{1,16}$/i.test(extracted.ext)) throw new Error('Invalid native document extension.');
            result.ext = extracted.ext.toLowerCase();
        }
        if (extracted.containerSource != null) result.containerSource = readContainerSource(extracted.containerSource);
        if (extracted.containerCoverage != null) result.containerCoverage = readContainerCoverage(extracted.containerCoverage);
        if (extracted.members != null) {
            if (!Array.isArray(extracted.members) || extracted.members.length > 2048 || (member && extracted.members.length > 0)) {
                throw new Error('Native container members must be bounded, flattened documents.');
            }
            if (extracted.members.length > 0 && (!result.containerSource || result.ownText === undefined)) {
                throw new Error('Native container extraction is missing its root source receipt or separate root text.');
            }
            const identities = new Set<string>();
            result.members = extracted.members.map(item => {
                if (!item || typeof item !== 'object' || Array.isArray(item)) throw new Error('Invalid native container member.');
                const entries = readContainerEntries((item as Record<string, unknown>).entries);
                const identity = JSON.stringify(entries.map(entry => [entry.kind, entry.name, entry.occurrence]));
                if (identities.has(identity)) throw new Error('Native container extraction returned duplicate member identities.');
                identities.add(identity);
                return { entries, document: parse((item as Record<string, unknown>).document, true) };
            });
        }
        indexedBytes += new TextEncoder().encode(result.ownText ?? result.text).byteLength;
        if (indexedBytes > maximumTextBytes) throw new Error('Native extraction exceeds the combined text limit.');
        return result;
    };
    return parse(value, false);
}
