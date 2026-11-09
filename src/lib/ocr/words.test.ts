import { describe, expect, it, vi } from 'vitest';
import { hasAlignedCharacterConfidence, regionReviewReason } from './confidence';
import {
    addDocument, applyRecognition, approvePage, createProject, editRegion,
    getReviewIssues, parseProject, prepareExport, serializeProject, type OcrRegion,
} from './model';
import { getWordEvidence, wordNeedsReview } from './words';

function region(text = 'Invoice total', patch: Partial<OcrRegion> = {}): OcrRegion {
    return {
        id: 'region-1', text, originalText: text, x: 0, y: 0, w: 500, h: 50,
        confidence: 0.99, confidenceBasis: 'recognition',
        char_conf: Array.from(text, () => 0.98), charConfidenceUnit: 'unicode_codepoint', charConfidenceAligned: true,
        kind: 'text', review: 'unreviewed', excluded: false, ...patch,
    };
}

describe('word confidence and selection', () => {
    it('uses mean character evidence and flags a weak character hidden by a high mean', () => {
        const source = region();
        source.char_conf[3] = 0.2;
        const words = getWordEvidence(source);
        expect(words.map(word => word.text)).toEqual(['Invoice', 'total']);
        expect(words[0].confidence).toBeCloseTo((0.98 * 6 + 0.2) / 7);
        expect(words[0].confidence).toBeGreaterThan(0.85);
        expect(words[0].minConfidence).toBe(0.2);
        expect(wordNeedsReview(words[0], 0.85)).toBe(true);
        expect(wordNeedsReview(words[1], 0.85)).toBe(false);
        expect(source.confidence).toBe(0.99);
    });

    it('selects the exact UTF-16 word after an astral prefix with codepoint scores', () => {
        const source = region('😀 Inv0ice\tEUR\n1,250.00');
        source.char_conf[5] = 0.15; // The zero, counting Unicode codepoints.
        const words = getWordEvidence(source);
        const invoice = words.find(word => word.text === 'Inv0ice')!;
        expect(invoice).toMatchObject({ start: 3, end: 10, minConfidence: 0.15, basis: 'characters' });
        for (const word of words) expect(source.text.slice(word.start, word.end)).toBe(word.text);
        expect(words.find(word => word.text === 'EUR')?.minConfidence).toBe(0.98);
        expect(source.text).toBe('😀 Inv0ice\tEUR\n1,250.00');
    });

    it('preserves combining marks and segments languages without spaces', () => {
        const source = region('Cafe\u0301 中文文本');
        source.char_conf[4] = 0.3;
        const words = getWordEvidence(source);
        expect(words[0]).toMatchObject({ text: 'Cafe\u0301', start: 0, end: 5, minConfidence: 0.3 });
        expect(words.slice(1).map(word => word.text).join('')).toBe('中文文本');
        for (const word of words) expect(source.text.slice(word.start, word.end)).toBe(word.text);
    });

    it.each([
        { charConfidenceUnit: 'token' as const },
        { charConfidenceUnit: 'unknown' as const },
        { charConfidenceAligned: false },
        { confidenceBasis: 'detection' as const },
        { confidenceBasis: 'unknown' as const },
        { confidenceBasis: 'manual' as const },
        { char_conf: [] },
        { char_conf: [0.95, 0.98] },
        { char_conf: Array.from('Invoice total', (_, index) => index === 3 ? NaN : 0.98) },
        { char_conf: Array.from('Invoice total', (_, index) => index === 3 ? -1 : 0.98) },
        { char_conf: Array.from('Invoice total', (_, index) => index === 3 ? Infinity : 0.98) },
        { char_conf: Array.from('Invoice total', (_, index) => index === 3 ? 1.01 : 0.98) },
    ])('never assigns word scores from unavailable or unproven evidence: %j', patch => {
        const source = region('Invoice total', patch);
        expect(hasAlignedCharacterConfidence(source)).toBe(false);
        const words = getWordEvidence(source);
        expect(words).toHaveLength(2);
        for (const word of words) {
            expect(word).toMatchObject({ confidence: null, minConfidence: null, basis: 'unavailable' });
            expect(wordNeedsReview(word, 0.85)).toBe(true);
        }
    });

    it('does not repeat a region score on each word, even for a one-word region', () => {
        const words = getWordEvidence(region('Invoice', { char_conf: [], charConfidenceAligned: false }));
        expect(words[0]).toMatchObject({ text: 'Invoice', confidence: null, basis: 'unavailable' });
    });

    it('keeps zero scores and treats the threshold consistently', () => {
        const zero = getWordEvidence(region('A', { char_conf: [0] }))[0];
        const boundary = getWordEvidence(region('A', { char_conf: [0.85] }))[0];
        expect(zero.confidence).toBe(0);
        expect(wordNeedsReview(zero, 0.85)).toBe(true);
        expect(wordNeedsReview(boundary, 0.85)).toBe(false);
    });

    it('clears current scores after corrections while retaining original evidence', () => {
        const source = region('Inv0ice total', { text: 'Invoice and total', review: 'corrected' });
        expect(getWordEvidence(source).map(word => word.basis)).toEqual(['edited', 'edited', 'edited']);
        expect(getWordEvidence(source).every(word => word.confidence === null)).toBe(true);
        const original = getWordEvidence(source, 'original');
        expect(original[0]).toMatchObject({ text: 'Inv0ice', basis: 'characters', minConfidence: 0.98 });
        expect(original[0].confidence).toBeCloseTo(0.98);
        expect(getWordEvidence({ ...source, text: source.originalText })[0].basis).toBe('characters');
    });

    it('handles empty, whitespace-only and punctuation-only regions without fake words', () => {
        for (const text of ['', ' \t\n', '… — 😀']) expect(getWordEvidence(region(text))).toEqual([]);
    });

    it('keeps exact selection and scoring in webviews without Intl.Segmenter', async () => {
        const olderIntl = Object.create(Intl) as typeof Intl;
        Object.defineProperty(olderIntl, 'Segmenter', { value: undefined });
        vi.stubGlobal('Intl', olderIntl);
        vi.resetModules();
        try {
            const { getWordEvidence: fallbackWords } = await import('./words');
            const source = region('😀\tInv0ice  total\n中文文本 —');
            source.char_conf[5] = 0.2;
            const words = fallbackWords(source);
            expect(words.map(word => word.text)).toEqual(['Inv0ice', 'total', '中文文本']);
            expect(words[0]).toMatchObject({ start: 3, end: 10, minConfidence: 0.2 });
            for (const word of words) expect(source.text.slice(word.start, word.end)).toBe(word.text);
            expect(fallbackWords(region('… — 😀'))).toEqual([]);
        } finally {
            vi.unstubAllGlobals();
            vi.resetModules();
        }
    });
});

describe('review evidence consistency', () => {
    it('shares corrected, unknown and low-character decisions with the review queue', () => {
        const source = region('A B', { char_conf: [0.98, 0.98, 0.2] });
        expect(regionReviewReason(source, 0.85)).toBe('low-confidence');
        expect(regionReviewReason({ ...source, review: 'corrected' }, 0.85)).toBe('changed');
        expect(regionReviewReason({ ...source, confidenceBasis: 'detection' }, 0.85)).toBe('unknown-confidence');
        expect(regionReviewReason({ ...source, charConfidenceUnit: 'token' }, 0.85)).toBeNull();
        for (const patch of [{ excluded: true }, { kind: 'picture' as const }, { review: 'approved' as const }]) {
            expect(regionReviewReason({ ...source, ...patch }, 0.85)).toBeNull();
        }
    });

    it('rejects contradictory alignment flags at recognition and restore boundaries', () => {
        const project = addDocument(createProject(), {
            source_uri: '/source.png', name: 'Source', pages: [{ image_path: '/page.png', width: 600, height: 200 }],
        });
        const result = { width: 600, height: 200, char_conf_aligned: true, char_conf_unit: 'unicode_codepoint',
            regions: [{ text: 'Hi', x: 10, y: 10, w: 200, h: 40, confidence: 0.99, confidence_basis: 'recognition', char_conf: [0.1, 0.99] }] };
        for (const patch of [
            { char_conf_unit: 'token', char_conf_aligned: true },
            { char_conf_unit: 'unknown', char_conf_aligned: true },
            { char_conf_aligned: false },
        ]) {
            const out = applyRecognition(project, project.pages[0].id, { ...result, regions: [{ ...result.regions[0], ...patch }] });
            expect(out.pages[0].regions[0].charConfidenceAligned).toBe(false);
            expect(getReviewIssues(out)).toEqual([]);
            expect(prepareExport(out).pages[0].regions[0].char_conf_aligned).toBe(false);
        }
        const legacy = applyRecognition(project, project.pages[0].id, result);
        legacy.pages[0].regions[0].charConfidenceUnit = 'token';
        legacy.pages[0].regions[0].charConfidenceAligned = true;
        const restored = parseProject(legacy);
        expect(restored.pages[0].regions[0].charConfidenceAligned).toBe(false);
        expect(getReviewIssues(restored)).toEqual([]);
    });

    it('persists corrections, revokes approval and exports original evidence separately', () => {
        let project = addDocument(createProject(), {
            source_uri: '/source.png', name: 'Source', pages: [{ image_path: '/page.png', width: 600, height: 200 }],
        });
        const pageId = project.pages[0].id;
        project = applyRecognition(project, pageId, { width: 600, height: 200, regions: [{
            text: 'Inv0ice', x: 10, y: 10, w: 200, h: 40, confidence: 0.97, confidence_basis: 'recognition',
            char_conf: [0.99, 0.99, 0.99, 0.1, 0.99, 0.99, 0.99], char_conf_unit: 'unicode_codepoint', char_conf_aligned: true,
        }] });
        expect(getReviewIssues(project)[0].reason).toBe('low-confidence');
        const regionId = project.pages[0].regions[0].id;
        project = approvePage(project, pageId);
        project = editRegion(project, pageId, regionId, 'Invoice');
        project = parseProject(serializeProject(project));
        expect(project.pages[0].approved).toBe(false);
        expect(getReviewIssues(project)[0].reason).toBe('changed');
        const saved = project.pages[0].regions[0];
        expect(getWordEvidence(saved)[0].basis).toBe('edited');
        expect(getWordEvidence(saved, 'original')[0].minConfidence).toBe(0.1);
        const output = prepareExport(project).pages[0].regions[0];
        expect(output.text).toBe('Invoice');
        expect(output.original_text).toBe('Inv0ice');
        expect(output.char_conf).toEqual([]);
        expect(output.original_char_conf).toEqual(saved.char_conf);
        expect(output.original_char_conf_aligned).toBe(true);
    });
});
