import { describe, expect, it } from 'vitest';
import { buildEvidenceContext, retrieveSelected, tokenCount } from './evidence';

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
    });
    it('rejects a question too long to leave context for an answer', () => {
        expect(() => buildEvidenceContext('question '.repeat(2000), '', [], 1024)).toThrow(/exceed/);
    });
});
