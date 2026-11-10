import { describe, expect, it } from 'vitest';
import { readWordEvidence, type WordEvidenceEnvelope } from './wordEvidence';
import { getWordConfidenceBand, getWordEvidence } from './words';
import { regionReviewReason } from './confidence';
import {
    addDocument, applyRecognition, createProject, editRegion, parseProject,
    prepareExport, serializeProject, updateRegion,
} from './model';

const text = '😀 Inv0ice\tNS-2026-1042\nCafe\u0301 中文';
const envelope = (): WordEvidenceEnvelope => ({
    version: 1, offset_unit: 'utf16', source: 'tesseract_tsv', words: [
        { text: 'Inv0ice', start: 3, end: 10, x: 10, y: 10, w: 80, h: 20,
            confidence: 0.42, confidence_basis: 'recognition', alternatives: [{ text: 'Invoice', confidence: null }] },
        { text: 'NS-2026-1042', start: 11, end: 23, x: 100, y: 10, w: 140, h: 20,
            confidence: null, confidence_basis: 'recognition' },
    ],
});
const page = { width: 500, height: 200 };
function project() {
    const initial = addDocument(createProject(), { source_uri: '/source.png', name: 'Source',
        pages: [{ image_path: '/page.png', ...page }] });
    return applyRecognition(initial, initial.pages[0].id, { ...page, regions: [{
        text, x: 0, y: 0, w: 500, h: 100, confidence: 0.99, confidence_basis: 'recognition',
        char_conf: Array.from(text, () => 0.99), char_conf_unit: 'unicode_codepoint', char_conf_aligned: true,
        word_evidence: envelope(),
    }] });
}

describe('native word evidence boundary', () => {
    it('retains exact Unicode offsets, native punctuation and independent geometry', () => {
        const original = envelope();
        const validated = readWordEvidence(original, text, page)!;
        expect(validated).toEqual(original);
        expect(validated).not.toBe(original);
        validated.words[0].alternatives![0].text = 'changed';
        expect(original.words[0].alternatives![0].text).toBe('Invoice');
        const extra = { ...envelope(), words: [...envelope().words, {
            text: 'Cafe\u0301 中文', start: 24, end: text.length, x: 10, y: 35, w: 200, h: 25,
            confidence: 1, confidence_basis: 'recognition' as const,
        }] };
        expect(readWordEvidence(extra, text, page)).toEqual(extra);
    });

    it.each([
        { version: 2 }, { offset_unit: 'unicode_codepoint' }, { source: '' }, { words: null },
    ])('rejects an unsupported supplied envelope: %j', patch => {
        expect(() => readWordEvidence({ ...envelope(), ...patch }, text, page)).toThrow(/word evidence/);
    });

    it.each([
        { start: -1 }, { start: 2 }, { end: 9 }, { start: 3.5 }, { text: 'Invoice' },
        { x: -1 }, { x: Infinity }, { y: 199 }, { w: 0 }, { h: NaN },
        { confidence: -0.1 }, { confidence: 1.1 }, { confidence: NaN }, { confidence: undefined },
        { confidence_basis: 'manual' }, { alternatives: [{ text: '', confidence: null }] },
    ])('rejects malformed native ranges, geometry or confidence: %j', patch => {
        const value = envelope();
        value.words[0] = { ...value.words[0], ...patch } as typeof value.words[0];
        expect(() => readWordEvidence(value, text, page)).toThrow(/word evidence/);
    });

    it('rejects overlapping ranges and split surrogate pairs, even when their slices match', () => {
        const value = envelope();
        value.words.push({ ...value.words[0] });
        expect(() => readWordEvidence(value, text, page)).toThrow(/offsets/);
        const split = { ...envelope(), words: [{ ...envelope().words[0], text: '\ud83d', start: 0, end: 1 }] };
        expect(() => readWordEvidence(split, text, page)).toThrow(/offsets/);
    });

    it('requires new word boxes inside their region but retains original page coordinates after manual resizing', () => {
        expect(() => readWordEvidence(envelope(), text, page, { x: 0, y: 0, w: 50, h: 40 })).toThrow(/geometry/);
        const p = project(), source = p.pages[0], r = source.regions[0];
        const resized = updateRegion(p, source.id, r.id, { x: 300, y: 100, w: 100, h: 50 });
        expect(parseProject(serializeProject(resized)).pages[0].regions[0].wordEvidence).toEqual(envelope());
    });

    it('accepts absent evidence in old projects and rejects supplied corrupt evidence without mutation', () => {
        expect(readWordEvidence(undefined, text, page)).toBeUndefined();
        const p = project(), saved = JSON.stringify(p);
        const invalid = envelope(); invalid.words[0].end = 20;
        expect(() => applyRecognition(p, p.pages[0].id, { ...page, regions: [{
            text, x: 0, y: 0, w: 500, h: 100, word_evidence: invalid,
        }] })).toThrow(/word evidence/);
        expect(JSON.stringify(p)).toBe(saved);
        const old = structuredClone(p); delete old.pages[0].regions[0].wordEvidence;
        expect(parseProject(old).pages[0].regions[0].wordEvidence).toBeUndefined();
    });
});

describe('native score precedence and persistence', () => {
    it('shows direct word scores, keeps null unscored, and fills uncovered text from aligned characters', () => {
        const r = project().pages[0].regions[0];
        const words = getWordEvidence(r);
        expect(words[0]).toMatchObject({ text: 'Inv0ice', start: 3, end: 10, confidence: 0.42,
            minConfidence: null, basis: 'word', bounds: { x: 10, y: 10, w: 80, h: 20 } });
        expect(words[1]).toMatchObject({ text: 'NS-2026-1042', confidence: null, basis: 'word' });
        expect(words.slice(2).map(word => word.text).join('')).toBe('Cafe\u0301中文');
        expect(words.slice(2).every(word => word.basis === 'characters')).toBe(true);
        expect(words.map(word => getWordConfidenceBand(word, 0.85)).slice(0, 3)).toEqual(['low', 'unscored', 'high']);
        expect(regionReviewReason(r, 0.85)).toBe('unknown-confidence');
    });

    it('does not conceal uncovered text using a region score or contradict direct high scores with character scores', () => {
        const r = project().pages[0].regions[0];
        r.charConfidenceAligned = false;
        r.wordEvidence!.words.forEach(word => word.confidence = 0.99);
        expect(regionReviewReason(r, 0.85)).toBe('unknown-confidence');
        r.text = r.originalText = 'Inv0ice';
        r.wordEvidence!.words = [{ ...envelope().words[0], start: 0, end: 7, confidence: 0.99 }];
        r.char_conf = Array.from(r.text, () => 0.01); r.charConfidenceAligned = true;
        expect(getWordConfidenceBand(getWordEvidence(r)[0], 0.85)).toBe('high');
        expect(regionReviewReason(r, 0.85)).toBeNull();
        r.wordEvidence!.words[0].confidence_basis = 'detection';
        expect(getWordEvidence(r)[0].confidence).toBeNull();
        expect(regionReviewReason(r, 0.85)).toBe('unknown-confidence');
    });

    it('preserves original evidence across corrections, save and export without assigning it to edited words', () => {
        const p = project(), source = p.pages[0], r = source.regions[0];
        const unchanged = prepareExport(p).pages[0].regions[0];
        expect(unchanged.word_evidence).toEqual(envelope());
        const edited = parseProject(serializeProject(editRegion(p, source.id, r.id, text.replace('Inv0ice', 'Invoice'))));
        const corrected = edited.pages[0].regions[0];
        expect(getWordEvidence(corrected).every(word => word.basis === 'edited' && !word.bounds)).toBe(true);
        expect(getWordEvidence(corrected, 'original')[0].confidence).toBe(0.42);
        const output = prepareExport(edited).pages[0].regions[0];
        expect(output.word_evidence).toBeUndefined();
        expect(output.original_word_evidence).toEqual(envelope());
        const protectedSource = updateRegion(edited, source.id, r.id, { wordEvidence: undefined });
        expect(protectedSource.pages[0].regions[0].wordEvidence).toEqual(envelope());
    });
});
