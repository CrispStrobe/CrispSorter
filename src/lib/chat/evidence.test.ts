import { describe, expect, it } from 'vitest';
import { auditCitations, buildEvidenceContext, retrieveSelected, tokenCount, type EvidenceSource } from './evidence';
import panel from '../../../tests/quality/selected-retrieval-panel.json';

describe('document evidence', () => {
    it('finds the answer in the middle instead of retaining only the ends', () => {
        const text = 'Unrelated opening. '.repeat(350) + 'The cobalt contract expires in November 2031. ' + 'Unrelated appendix. '.repeat(350);
        const hits = retrieveSelected('When does the cobalt contract expire?', [{ id: 'a', originalName: 'contract.txt', originalPath: '/a.txt', extractedText: text }]);
        expect(hits[0].text).toContain('November 2031');
        expect(hits[0].startChar).toBeGreaterThan(1600);
    });
    it('retains known page anchors and does not invent them for plain text', () => {
        const docs = [{ id: 'a', originalName: 'a.pdf', originalPath: '/a.pdf', extractedText: 'Introduction\fWarranty expires in May.' }];
        expect(retrieveSelected('warranty', docs)[0].page).toBe(2);
        expect(retrieveSelected('introduction', [{ ...docs[0], extractedText: 'Introduction' }])[0].page).toBeUndefined();
    });
    it('does not supply unrelated passages as evidence', () => {
        expect(retrieveSelected('cobalt warranty', [{ id: 'a', originalName: 'a', originalPath: '/a', extractedText: 'Chocolate cake recipe' }])).toEqual([]);
    });
    it('budgets multilingual source text, labels and question in tokens', () => {
        const sources = retrieveSelected('Vertrag', [{ id: 'a', originalName: '📄 Vertrag.txt', originalPath: '/a', extractedText: 'Vertrag München 中文 😀 '.repeat(2000) }]);
        const context = buildEvidenceContext('Was steht im Vertrag?', 'Answer carefully.', sources, 2048);
        expect(context.sources.length).toBeGreaterThan(0);
        expect(context.inputTokens).toBe(tokenCount(context.prompt));
        expect(context.inputTokens + context.responseTokens + 128).toBeLessThanOrEqual(Math.floor(2048 * 0.8));
        expect(context.prompt).toContain('[S1]');
        for (const source of context.sources) {
            expect(source.text).not.toContain('\uFFFD');
            expect(sources.some(original => original.text.startsWith(source.text))).toBe(true);
        }
    });
    it('rejects a question too long to leave context for an answer', () => {
        expect(() => buildEvidenceContext('question '.repeat(2000), '', [], 1024)).toThrow(/exceed/);
    });
    it.each([
        ['de', 'München Garantie', 'In Mu\u0308nchen endet die Garantie im Jahr 2031.'],
        ['zh', '保修期限', '该设备保修期限为两年。'],
        ['ja', '保証期間', '製品の保証期間は二年間です。'],
        ['ko', '보증기간', '제품 보증기간은 이 년입니다.'],
    ])('retrieves a real middle passage in a long %s document', (_language, question, answer) => {
        const text = 'Unrelated appendix. '.repeat(300) + answer + ' Irrelevant ending. '.repeat(300);
        const hits = retrieveSelected(question, [
            { id: 'answer', originalName: 'answer.txt', originalPath: '/answer.txt', extractedText: text },
            { id: 'distractor', originalName: 'other.txt', originalPath: '/other.txt', extractedText: 'Unrelated appendix. '.repeat(300) },
        ]);
        expect(hits[0].documentId).toBe('answer');
        expect(hits[0].text).toContain(answer);
        expect(hits[0].startChar).toBeGreaterThan(1600);
        expect(hits[0].offsetUnit).toBe('unicode_characters');
    });
    it('does not match a shared single Han character as a multi-character query', () => {
        const docs = [{ id: 'a', originalName: 'a', originalPath: '/a', extractedText: '产品保养说明。' }];
        expect(retrieveSelected('保修期限', docs)).toEqual([]);
    });
    it('audits grouped and unknown citations and distinguishes missing citations', () => {
        const sources = [{ id: 'S1' }, { id: 'S2' }] as EvidenceSource[];
        expect(auditCitations('See [s1, S2] and [S8].', sources)).toMatchObject({ cited: ['S1', 'S2'], unknown: ['S8'] });
        expect(auditCitations('No cited claim.', sources).warning).toMatch(/No available source citations/);
        expect(auditCitations('See [S8].', sources).warning).toMatch(/unavailable citations: S8/);
        expect(auditCitations('See [S1; S2].', sources).warning).toBe('');
    });
    it.each(panel.cases)('ranks the frozen long-document $language case above its near miss', ({ query, answer, distractor }) => {
        const prefix = 'Unrelated introductory material. '.repeat(panel.padding.prefix_repetitions);
        const suffix = ' Unrelated appendix material.'.repeat(panel.padding.suffix_repetitions);
        const hits = retrieveSelected(query, [
            { id: 'near-miss', originalName: 'near.txt', originalPath: '/near.txt', extractedText: prefix + distractor + suffix },
            { id: 'answer', originalName: 'answer.txt', originalPath: '/answer.txt', extractedText: prefix + answer + suffix },
        ]);
        expect(hits[0].documentId).toBe('answer');
        expect(hits[0].text).toContain(answer);
        expect(hits[0].startChar).toBeGreaterThan(9000);
    });
    it('does not fabricate a match for a cross-language paraphrase', () => {
        expect(retrieveSelected('Wann endet die Gewährleistung?', [{ id: 'a', originalName: 'a', originalPath: '/a', extractedText: 'The warranty expires in November.' }])).toEqual([]);
    });
});
