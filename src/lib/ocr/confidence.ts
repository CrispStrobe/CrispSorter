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
    if (region.review === 'corrected') return 'changed';
    if (region.confidenceBasis !== 'recognition' || !isConfidenceScore(region.confidence)) return 'unknown-confidence';
    if (region.confidence < threshold ||
        (hasAlignedCharacterConfidence(region) && region.char_conf.some(score => score < threshold))) return 'low-confidence';
    return null;
}
