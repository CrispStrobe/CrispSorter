import { describe, expect, it } from 'vitest';
import { normalizeSearchRecipe } from './recipe';
import { canCountLexicalMatches, corpusScopeForBackend, lexicalGenerationArguments, lexicalSummaryArguments, lexicalExportArguments, readCorpusReportReceipt, readLexicalSummary, recipeWithCorpusFacet, isCorpusCancellation } from './summary';

const facet = { values: [{ value: 'reviewed', documents: 633 }], distinctValues: 31, omittedValues: 30, missingDocuments: 0 };
const response = () => ({ version: 1, scope: 'local_indexed_documents', query: 'needle', effectiveQuery: 'needle',
    generation: 'snapshot-1', matchingDocuments: 633, scannedRows: 900, facetValueLimit: 25,
    facets: { extensions: facet, languages: facet, years: facet, tags: facet } });

describe('exhaustive lexical summary contracts', () => {
    it('classifies only native cancellation receipts, preserving failed paths and backend causes', () => {
        for (const message of ['The matching-document scan was cancelled', 'The report was cancelled before publication']) {
            expect(isCorpusCancellation(message)).toBe(true);
            expect(isCorpusCancellation(new Error(message))).toBe(true);
        }
        expect(isCorpusCancellation(new DOMException('Stopped', 'AbortError'))).toBe(true);
        for (const cause of ['Cannot write cancelled-report.csv', 'Destination: The report was cancelled before publication',
            new Error('A backend request failed: cancelled connection'), {}, null]) expect(isCorpusCancellation(cause)).toBe(false);
    });
    it('retains exact counts beyond the returned window and explicit omitted facet values', () => {
        const summary = readLexicalSummary(response(), 'needle');
        expect(summary.matchingDocuments).toBe(633);
        expect(summary.facets.tags.omittedValues).toBe(30);
        expect(summary.facets.tags.values[0].documents).toBe(633);
    });

    it('rejects absent counts, wrong query/scope, duplicate facets and impossible values', () => {
        for (const altered of [null, {}, { ...response(), query: 'old' }, { ...response(), scope: 'semantic' },
            { ...response(), matchingDocuments: undefined }, { ...response(), matchingDocuments: -1 },
            { ...response(), matchingDocuments: 3.5 }, { ...response(), scannedRows: 10 },
            { ...response(), facets: { ...response().facets, tags: { ...facet, missingDocuments: 1000 } } },
            { ...response(), facets: { ...response().facets, tags: { ...facet, omittedValues: 0 } } },
            { ...response(), facets: { ...response().facets, tags: { ...facet, values: [...facet.values, ...facet.values], omittedValues: 29 } } },
        ]) expect(() => readLexicalSummary(altered, 'needle')).toThrow();
    });

    it('offers complete counts only for lexical modes and uses full exact request filters', () => {
        const recipe = normalizeSearchRecipe({ query: 'smart original', mode: 'text', limit: 20, ownerId: 'reader',
            docIdScope: ['a', 'b'], filters: { tag: 'reviewed', folderPrefix: '/research', fuzzy: true, includeUnmounted: true } });
        expect(canCountLexicalMatches(recipe, 'Local')).toBe(true);
        expect(canCountLexicalMatches(recipe, 'Remote')).toBe(true);
        for (const backend of [undefined, 'unknown']) expect(canCountLexicalMatches(recipe, backend)).toBe(false);
        for (const mode of ['hybrid', 'vector']) for (const backend of ['local', 'remote'])
            expect(canCountLexicalMatches(normalizeSearchRecipe({ ...recipe, mode }), backend)).toBe(false);
        const args = lexicalSummaryArguments(recipe, 'parsed query', 'request-1');
        expect(args).toMatchObject({ query: 'parsed query', mode: 'text', ownerId: 'reader', docIdScope: ['a', 'b'],
            tag: 'reviewed', fuzzy: true, includeUnmounted: true, requestId: 'request-1', searchFilters: { parent_dir_prefix: '/research' } });
        expect(args).not.toHaveProperty('limit');
        const { requestId: _requestId, ...generationScope } = args;
        expect(lexicalGenerationArguments(recipe, 'parsed query')).toEqual(generationScope);
    });

    it('binds remote count and report receipts to the captured backend scope', () => {
        const remote = { ...response(), scope: 'remote_indexed_documents' };
        expect(corpusScopeForBackend('remote')).toBe('remote_indexed_documents');
        expect(corpusScopeForBackend('hybrid')).toBe('local_indexed_documents');
        expect(corpusScopeForBackend('unknown')).toBeNull();
        expect(readLexicalSummary(remote, 'needle', 'remote_indexed_documents').matchingDocuments).toBe(633);
        expect(() => readLexicalSummary(remote, 'needle', 'local_indexed_documents')).toThrow();
        expect(() => readLexicalSummary(response(), 'needle', 'remote_indexed_documents')).toThrow();
        const receipt = { path: '/reports/all.json', documents: 633, scope: 'remote_indexed_documents',
            generation: 'remote-v1:connection:snapshot', format: 'json', sort: 'year-desc' };
        expect(readCorpusReportReceipt(receipt, 'json', 'year-desc', 'remote_indexed_documents').documents).toBe(633);
        expect(() => readCorpusReportReceipt(receipt, 'json', 'year-desc', 'local_indexed_documents')).toThrow();
        expect(() => readCorpusReportReceipt(receipt, 'json', 'year-desc', 'remote_indexed_documents', 'different-generation')).toThrow();
    });

    it('turns corpus facet clicks into real backend filters without retaining display tag hiding', () => {
        const recipe = normalizeSearchRecipe({ query: 'needle', mode: 'text', ownerId: 'reader', docIdScope: ['a'],
            filters: { folderPrefix: '/research' }, view: { displayTags: ['hidden'], sort: 'year-desc' } });
        for (const [key, value, filters] of [
            ['extensions', 'pdf', { ext: 'pdf' }], ['languages', 'de', { language: 'de' }],
            ['tags', "reader's", { tag: "reader's" }], ['years', '2026', { yearMin: 2026, yearMax: 2026 }],
        ] as const) {
            const next = recipeWithCorpusFacet(recipe, key, value);
            expect(next.filters).toMatchObject({ ...filters, folderPrefix: '/research' });
            expect(next).toMatchObject({ ownerId: 'reader', docIdScope: ['a'], view: { displayTags: [], sort: 'year-desc' } });
        }
        expect(() => recipeWithCorpusFacet(recipe, 'years', 'unknown')).toThrow();
        expect(recipe.view.displayTags).toEqual(['hidden']);
    });

    it('freezes complete-export query, owner and filters without an interactive limit or display-tag constraint', () => {
        const recipe = normalizeSearchRecipe({ query: 'draft', mode: 'text', ownerId: 'reader', docIdScope: ['source'],
            filters: { tag: 'reviewed' }, view: { displayTags: ['window-only'] } });
        const args = lexicalExportArguments(recipe, 'executed', { requestId: 'export-1', path: '/reports/all.csv',
            format: 'csv', sort: 'year-desc', expectedGeneration: 'snapshot-1' });
        expect(args.request).toMatchObject({ query: 'executed', ownerId: 'reader', tag: 'reviewed', docIdScope: ['source'],
            expectedGeneration: 'snapshot-1', format: 'csv', sort: 'year-desc' });
        expect(args.request).not.toHaveProperty('limit');
        expect(args.request).not.toHaveProperty('displayTags');
    });

    it('validates success receipts without fabricating missing complete-export counts', () => {
        const receipt = { path: '/reports/all.csv', documents: 0, scope: 'local_indexed_documents', generation: 'one', format: 'csv', sort: 'filename' };
        expect(readCorpusReportReceipt(receipt, 'csv', 'filename').documents).toBe(0);
        for (const invalid of [null, {}, { ...receipt, documents: undefined }, { ...receipt, documents: -1 },
            { ...receipt, scope: 'returned_window' }, { ...receipt, format: 'json' }, { ...receipt, sort: 'title' }]) {
            expect(() => readCorpusReportReceipt(invalid, 'csv', 'filename')).toThrow();
        }
    });
});
