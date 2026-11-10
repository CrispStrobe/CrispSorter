import { hasAlignedCharacterConfidence, isConfidenceScore } from './confidence';
import type { OcrRegion } from './model';

export interface WordEvidence {
    text: string;
    /** UTF-16 offsets, matching textarea.setSelectionRange and String.slice. */
    start: number;
    end: number;
    /** Mean of verified character recognition scores; never a region score. */
    confidence: number | null;
    /** Lowest character score keeps a single uncertain glyph visible. */
    minConfidence: number | null;
    basis: 'characters' | 'unavailable' | 'edited';
}

export type WordConfidenceBand = 'high' | 'medium' | 'low' | 'unscored' | 'edited';
export const LOW_WORD_CONFIDENCE = 0.6;

/** Colors use the weakest character, matching the review queue's caution. */
export function getWordConfidenceBand(word: WordEvidence, threshold: number): WordConfidenceBand {
    if (word.basis === 'edited') return 'edited';
    if (word.basis !== 'characters' || !isConfidenceScore(word.minConfidence) || !isConfidenceScore(word.confidence)) return 'unscored';
    const high = isConfidenceScore(threshold) ? threshold : 0.85;
    if (word.minConfidence >= high) return 'high';
    return word.minConfidence < Math.min(LOW_WORD_CONFIDENCE, high) ? 'low' : 'medium';
}

interface WordSpan { text: string; start: number; end: number; }
// Cache the segmenter, not text/results: projects may contain very large pages.
const segmenter = typeof Intl.Segmenter === 'function'
    ? new Intl.Segmenter(undefined, { granularity: 'word' }) : null;

/** Preserve Unicode boundaries and the source's original whitespace exactly. */
function wordSpans(text: string): WordSpan[] {
    if (segmenter) {
        return Array.from(segmenter.segment(text))
            .filter(segment => segment.isWordLike)
            .map(segment => ({ text: segment.segment, start: segment.index, end: segment.index + segment.segment.length }));
    }
    // Older webviews can still select/correct whitespace-delimited words. Do
    // not invent subword boundaries for scripts requiring a language segmenter.
    return Array.from(text.matchAll(/\S+/gu))
        .filter(match => /[\p{L}\p{N}\p{M}]/u.test(match[0]))
        .map(match => ({ text: match[0], start: match.index!, end: match.index! + match[0].length }));
}

/**
 * Current corrections have no model confidence. The original view retains its
 * evidence; undoing/restoring the exact original text makes it usable again.
 * Offsets refer to the requested view, never to a guessed alignment after edits.
 */
export function getWordEvidence(region: OcrRegion, source: 'current' | 'original' = 'current'): WordEvidence[] {
    const text = source === 'original' ? region.originalText : region.text;
    const edited = source === 'current' && region.text !== region.originalText;
    const aligned = !edited && hasAlignedCharacterConfidence(region);
    const words = wordSpans(text);
    if (!aligned) return words.map(word => ({
        ...word, confidence: null, minConfidence: null, basis: edited ? 'edited' : 'unavailable',
    }));

    // Convert the native Unicode-codepoint positions to the browser's UTF-16
    // positions in one pass, including astral characters and combining marks.
    const codepointAt = new Map<number, number>();
    let utf16 = 0, codepoint = 0;
    codepointAt.set(0, 0);
    for (const character of text) {
        utf16 += character.length;
        codepointAt.set(utf16, ++codepoint);
    }
    return words.map(word => {
        const start = codepointAt.get(word.start), end = codepointAt.get(word.end);
        if (start === undefined || end === undefined || end <= start) {
            return { ...word, confidence: null, minConfidence: null, basis: 'unavailable' };
        }
        let sum = 0, minimum = 1;
        for (let index = start; index < end; index++) {
            const score = region.char_conf[index];
            sum += score;
            minimum = Math.min(minimum, score);
        }
        return { ...word, confidence: sum / (end - start), minConfidence: minimum, basis: 'characters' };
    });
}

export function wordNeedsReview(word: WordEvidence, threshold: number): boolean {
    return getWordConfidenceBand(word, threshold) !== 'high';
}
