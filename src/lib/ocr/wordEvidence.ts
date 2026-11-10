/** Native evidence refers to immutable original text and working-image pixels. */
export interface WordBounds { x: number; y: number; w: number; h: number; }
export interface NativeWordAlternative { text: string; confidence: number | null; }
export interface NativeWordEvidence extends WordBounds {
    text: string;
    /** Half-open UTF-16 offsets into the owning region's originalText. */
    start: number;
    end: number;
    confidence: number | null;
    confidence_basis: 'recognition' | 'detection' | 'unknown';
    alternatives?: NativeWordAlternative[];
}
export interface WordEvidenceEnvelope {
    version: 1;
    offset_unit: 'utf16';
    source: string;
    words: NativeWordEvidence[];
}

const score = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1;
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const unicodeBoundary = (text: string, offset: number): boolean => offset === 0 || offset === text.length ||
    !(text.charCodeAt(offset - 1) >= 0xd800 && text.charCodeAt(offset - 1) <= 0xdbff &&
      text.charCodeAt(offset) >= 0xdc00 && text.charCodeAt(offset) <= 0xdfff);
const contains = (outer: WordBounds, inner: WordBounds): boolean =>
    inner.x >= outer.x - 0.01 && inner.y >= outer.y - 0.01 &&
    inner.x + inner.w <= outer.x + outer.w + 0.01 && inner.y + inner.h <= outer.y + outer.h + 0.01;

/**
 * Validate the boundary once on recognition/import. Missing evidence is allowed;
 * a contradictory supplied envelope fails instead of manufacturing alignment.
 * Region containment is required for new OCR, while restored original evidence
 * may outlive a manually adjusted region rectangle on the same source image.
 */
export function readWordEvidence(value: unknown, originalText: string, page: { width: number; height: number }, region?: WordBounds): WordEvidenceEnvelope | undefined {
    if (value === undefined || value === null) return undefined;
    const invalid = (reason: string): never => { throw new Error(`Invalid OCR word evidence: ${reason}. Existing text was preserved.`); };
    if (!object(value) || value.version !== 1 || value.offset_unit !== 'utf16' ||
        typeof value.source !== 'string' || !value.source.trim() || value.source.length > 200 || value.source.includes('\0') ||
        !Array.isArray(value.words) || value.words.length > 100000) invalid('unsupported metadata');
    const envelope = value as unknown as WordEvidenceEnvelope;
    let previousEnd = 0;
    const words = envelope.words.map((entry): NativeWordEvidence => {
        if (!object(entry) || typeof entry.text !== 'string' || !entry.text.trim() || entry.text.includes('\0') ||
            !Number.isInteger(entry.start) || !Number.isInteger(entry.end) || entry.start < previousEnd ||
            entry.end <= entry.start || entry.end > originalText.length ||
            !unicodeBoundary(originalText, entry.start) || !unicodeBoundary(originalText, entry.end) ||
            originalText.slice(entry.start, entry.end) !== entry.text) invalid('word offsets do not match the original text');
        if (![entry.x, entry.y, entry.w, entry.h].every(number => typeof number === 'number' && Number.isFinite(number)) ||
            entry.x < 0 || entry.y < 0 || entry.w <= 0 || entry.h <= 0 ||
            entry.x + entry.w > page.width || entry.y + entry.h > page.height ||
            (region && !contains(region, entry))) invalid('word geometry is outside its source');
        if (!['recognition', 'detection', 'unknown'].includes(entry.confidence_basis) ||
            (entry.confidence !== null && !score(entry.confidence))) invalid('word score has no valid provenance');
        let alternatives: NativeWordAlternative[] | undefined;
        if (entry.alternatives !== undefined) {
            if (!Array.isArray(entry.alternatives) || entry.alternatives.length > 50) invalid('too many alternatives');
            alternatives = entry.alternatives.map(alternative => {
                if (!object(alternative) || typeof alternative.text !== 'string' || !alternative.text.trim() ||
                    alternative.text.length > 10000 || alternative.text.includes('\0') ||
                    (alternative.confidence !== null && !score(alternative.confidence))) invalid('invalid alternative');
                return { text: alternative.text, confidence: alternative.confidence };
            });
        }
        previousEnd = entry.end;
        return {
            text: entry.text, start: entry.start, end: entry.end,
            x: entry.x, y: entry.y, w: entry.w, h: entry.h,
            confidence: entry.confidence, confidence_basis: entry.confidence_basis,
            ...(alternatives ? { alternatives } : {}),
        };
    });
    return { version: 1, offset_unit: 'utf16', source: envelope.source, words };
}
