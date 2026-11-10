import { describe, expect, it } from 'vitest';
import { normalizeSearchRecipe } from './recipe';
import { buildSearchReport, groupResults, highlightedSnippet, occurrenceRanges, plainSnippet,
    queryTerms, searchReportCsv, searchReportHtml, sortGroups, type InspectableHit } from './inspection';

const hit = (doc_id: string, patch: Partial<InspectableHit> = {}): InspectableHit => ({
    doc_id, chunk_index: 0, score: .04, location_uri: `/library/${doc_id}.txt`,
    snippet: 'A <literal> & source excerpt.', filename: `${doc_id}.txt`, ...patch,
});

describe('result inspection', () => {
    it('deduplicates repeated chunks and keeps the strongest passage without changing inputs', () => {
        const rows = [hit('one', { chunk_index: 2, score: .8 }), hit('one', { score: .2 }), hit('one', { chunk_index: 2, score: .1 })];
        const before = JSON.stringify(rows);
        const [group] = groupResults(rows);
        expect(group.chunks.map(item => item.chunk_index)).toEqual([0, 2]);
        expect(group.best.chunk_index).toBe(2);
        expect(JSON.stringify(rows)).toBe(before);
    });
    it('sorts years in either direction with missing values last and stable ties', () => {
        const groups = groupResults([hit('unknown'), hit('old', { year: 2001 }), hit('new', { year: 2026 })]);
        expect(sortGroups(groups, 'year-desc').map(item => item.doc_id)).toEqual(['new', 'old', 'unknown']);
        expect(sortGroups(groups, 'year-asc').map(item => item.doc_id)).toEqual(['old', 'new', 'unknown']);
        expect(sortGroups(groupResults([hit('b'), hit('a')]), 'relevance').map(item => item.doc_id)).toEqual(['b', 'a']);
    });
    it('does not compare a synthetic filename score as if it were a content relevance score', () => {
        const groups = groupResults([hit('catalog', { score: .4, catalog_source: '/drive.caf' }), hit('content', { score: .01 })]);
        expect(sortGroups(groups, 'relevance').map(item => item.doc_id)).toEqual(['content', 'catalog']);
    });
    it('sorts titles naturally and preserves result arrays', () => {
        const groups = groupResults([hit('b', { title: 'Report 10' }), hit('a', { title: 'report 2' })]);
        expect(sortGroups(groups, 'title').map(item => item.doc_id)).toEqual(['a', 'b']);
        expect(groups.map(item => item.doc_id)).toEqual(['b', 'a']);
    });
    it('escapes source markup before rendering and never matches inside escaped entities', () => {
        expect(highlightedSnippet('<script>alert(1)</script> & annual', 'amp annual'))
            .toBe('&lt;script&gt;alert(1)&lt;/script&gt; &amp; <mark>annual</mark>');
        expect(highlightedSnippet('An <mark>annual</mark> &amp; report', 'annual', true)).toBe('An <mark>annual</mark> &amp; report');
        expect(highlightedSnippet('<img src=x onerror=alert(1)>', 'img')).not.toContain('<img');
    });
    it('retains literal markup and decodes marked server snippets exactly once', () => {
        expect(plainSnippet('Literal &amp; text')).toBe('Literal &amp; text');
        expect(plainSnippet('<mark>term</mark> &lt;mark&gt; &amp;lt;', true)).toBe('term <mark> &lt;');
        expect(plainSnippet('<mark>literal document markup</mark>')).toBe('<mark>literal document markup</mark>');
    });
    it('finds literal Unicode occurrences with source offsets and bounded output', () => {
        expect(occurrenceRanges('😀 Straße Straße', ['Straße'])).toEqual([{ start: 3, end: 9 }, { start: 10, end: 16 }]);
        expect(occurrenceRanges('a+b a.b', ['a+b'])).toEqual([{ start: 0, end: 3 }]);
        expect(occurrenceRanges('x'.repeat(1200), ['x'])).toHaveLength(1000);
    });
    it('omits syntax and simple excluded operands from visual query terms', () => {
        expect(queryTerms('title:report AND NOT draft w/5 policy~1')).toEqual(['report', 'policy']);
        expect(queryTerms('"annual report" OR warranty')).toEqual(['annual', 'report', 'warranty']);
        expect(queryTerms('title:"annual report" AND NOT (draft OR obsolete)')).toEqual(['annual', 'report']);
        expect(queryTerms('text:report OR h:"NOT text" OR body_translated:review')).toEqual(['report', 'not', 'text', 'review']);
    });
    it('supports case-sensitive and Unicode whole-word find without changing source offsets', () => {
        const text = 'War warranty war WAR _war war2 aWar e\u0301 e 東京 東京都';
        expect(occurrenceRanges(text, ['war'], 1000, { wholeWords: true }).map(range => text.slice(range.start, range.end)))
            .toEqual(['War', 'war', 'WAR']);
        expect(occurrenceRanges(text, ['war'], 1000, { wholeWords: true, caseSensitive: true }).map(range => text.slice(range.start, range.end)))
            .toEqual(['war']);
        expect(occurrenceRanges(text, ['e'], 1000, { wholeWords: true })).toHaveLength(1);
        expect(occurrenceRanges(text, ['東京'], 1000, { wholeWords: true })).toHaveLength(1);
        expect(occurrenceRanges('zero', ['zero'], 0)).toEqual([]);
        expect(occurrenceRanges('zero', ['zero'], Number.NaN)).toEqual([]);
    });
});

describe('reproducible result reports', () => {
    const report = () => buildSearchReport({
        recipe: normalizeSearchRecipe({ query: 'warranty', mode: 'text', filters: { language: 'de' }, docIdScope: ['one'] }),
        executedQuery: 'warranty', searchedAt: '2026-11-10T12:00:00Z', exportedAt: '2026-11-10T12:01:00Z',
        elapsedMs: 31, sort: 'title', displayTags: ['reviewed'], scope: 'selected',
        returnedDocuments: 5, returnedPassages: 8,
        groups: groupResults([hit('one', { title: '=HYPERLINK("https://invalid")', source_hash: 'abc', indexed_at: 123, author: '@SUM(1,2)' }), hit('one', { chunk_index: 1, snippet: '<mark>warranty</mark> &amp; term', snippet_format: 'marked' })]),
    });
    it('records the executed recipe, selected source identities, passages, and honest window coverage', () => {
        const value = report();
        expect(value.recipe.filters.language).toBe('de');
        expect(value.recipe.docIdScope).toEqual(['one']);
        expect(value.coverage).toMatchObject({ completeMatchSet: false, exportedDocuments: 1, returnedDocuments: 5, returnedPassages: 8 });
        expect(value.documents[0]).toMatchObject({ sourceHash: 'abc', indexedAt: 123 });
        expect(value.documents[0].passages[1].excerpt).toBe('warranty & term');
        expect(value.scope).toBe('selected');
    });
    it('protects spreadsheet readers from formula cells while JSON stays lossless', () => {
        const value = report(), csv = searchReportCsv(value);
        expect(csv).toContain('"\'=HYPERLINK(');
        expect(csv).toContain('"\'@SUM(1,2)"');
        expect(csv).toContain('\r\n');
        expect(value.documents[0].title).toBe('=HYPERLINK("https://invalid")');
    });
    it('writes standalone HTML with escaped source text, complete recipe, and no active document markup', () => {
        const html = searchReportHtml(report());
        expect(html).toContain('<!doctype html>');
        expect(html).toContain('&lt;literal&gt; &amp; source excerpt.');
        expect(html).toContain('Complete search recipe');
        expect(html).not.toContain('<literal>');
        expect(html).not.toContain('<script');
    });
    it('does not retain mutable arrays or filters from the live recipe', () => {
        const recipe = normalizeSearchRecipe({ query: 'original', filters: { language: 'de' } });
        const groups = groupResults([hit('one')]);
        const value = buildSearchReport({ recipe, groups, executedQuery: 'original', searchedAt: '', exportedAt: '', elapsedMs: 0,
            sort: 'relevance', displayTags: ['one'], scope: 'displayed', returnedDocuments: 1, returnedPassages: 1 });
        recipe.filters.language = 'en'; groups[0].best.snippet = 'mutated';
        expect(value.recipe.filters.language).toBe('de');
        expect(value.documents[0].passages[0].excerpt).toBe('A <literal> & source excerpt.');
    });
});
