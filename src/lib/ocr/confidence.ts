import type { OcrRegion } from './model';

/** A bounded engine score is a review signal, not a calibrated probability. */
export function isConfidenceScore(value: unknown): value is number {
    return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1;
}

/**
 * Character evidence always describes originalText. Neither a matching array
 * length nor an alignment flag can turn engine tokens into Unicode characters.
 */
export function hasAlignedCharacterConfidence(region: OcrRegion): boolean {
    return region.confidenceBasis === 'recognition' &&
        region.charConfidenceUnit === 'unicode_codepoint' && region.charConfidenceAligned &&
        region.char_conf.length > 0 && region.char_conf.length === Array.from(region.originalText).length &&
        region.char_conf.every(isConfidenceScore);
}

export type RegionReviewReason = 'changed' | 'unknown-confidence' | 'low-confidence';

/** Shared by the review queue and image overlays so neither hides an issue. */
export function regionReviewReason(region: OcrRegion, threshold: number): RegionReviewReason | null {
    if (region.excluded || region.kind === 'picture' || region.review === 'approved') return null;
    if (region.review === 'corrected' || region.confidenceBasis === 'manual') return 'changed';
    const words = region.wordEvidence?.words ?? [];
    if (words.length) {
        if (words.some(word => word.confidence_basis !== 'recognition' || !isConfidenceScore(word.confidence))) return 'unknown-confidence';
        let low = words.some(word => word.confidence !== null && word.confidence < threshold);
        const aligned = hasAlignedCharacterConfidence(region);
        let offset = 0, characterIndex = 0, wordIndex = 0;
        for (const character of region.originalText) {
            while (wordIndex < words.length && words[wordIndex].end <= offset) wordIndex++;
            const covered = wordIndex < words.length && words[wordIndex].start <= offset;
            if (!covered && /[\p{L}\p{N}\p{M}]/u.test(character)) {
                if (!aligned) return 'unknown-confidence';
                if (region.char_conf[characterIndex] < threshold) low = true;
            }
            offset += character.length;
            characterIndex++;
        }
        // Native words override conflicting character/region scores only for
        // their actual spans. Uncovered text still needs its own evidence.
        return low ? 'low-confidence' : null;
    }
    if (region.confidenceBasis !== 'recognition' || !isConfidenceScore(region.confidence)) return 'unknown-confidence';
    if ((region.confidenceBasis === 'recognition' && isConfidenceScore(region.confidence) && region.confidence < threshold) ||
        (hasAlignedCharacterConfidence(region) && region.char_conf.some(score => score < threshold))) return 'low-confidence';
    return null;
}
