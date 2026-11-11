import { describe, expect, it } from 'vitest';
import { evidenceKindLabel, evidenceFieldLabel, evidenceRanges, explanationClauseRows, readPassageExplanation,
    type DisplayedPassage, type EvidenceSpan, type ExplanationRequest, type PassageExplanation } from './explanation';

const request = (): ExplanationRequest => ({ docId: 'shared', ownerId: 'reader', chunkIndex: 5,
    query: 'résume~1 AND "carbon dioxide"', mode: 'text', queryIntent: 'advanced', fuzzy: false, synonyms: false });
const passage = (): DisplayedPassage => ({ doc_id: 'shared', owner_id: 'reader', chunk_index: 5,
    text: '😀 résumé carbon dioxide. <source>', start_char: 100, end_char: 138,
    offset_unit: 'utf8_bytes', truncated: false, source_hash: 'stored-source', indexed_at: 12 });
function span(text: string, needle: string, kind: EvidenceSpan['kind'], clause = needle): EvidenceSpan {
    const start = text.indexOf(needle), encode = (value: string) => new TextEncoder().encode(value).byteLength;
    return { start: encode(text.slice(0, start)), end: encode(text.slice(0, start + needle.length)), kind, clause };
}
function response(): PassageExplanation {
    const selected = passage(), query = request();
    return { explanation_version: 1, query: query.query, lexical_query: query.query, query_intent: 'advanced', mode: 'text',
        fuzzy: false, synonyms: false, passage: { ...selected, doc_id: 'shared', owner_id: 'reader', chunk_index: 5 },
        lexical_document_match: true, semantic_evidence: false,
        clauses: { kind: 'and', label: 'AND', matched: true, children: [
            { kind: 'term', label: 'résume~1', matched: true, children: [] },
            { kind: 'phrase', label: 'carbon dioxide', matched: true, field: 'body', children: [] },
        ] },
        spans: [span(selected.text, 'résumé', 'fuzzy', 'résume~1'), span(selected.text, 'carbon dioxide', 'phrase')],
        spans_truncated: false, scope: 'document_query_with_passage_spans', notes: ['Current indexed document.'],
    };
}

describe('native passage evidence receipts', () => {
    it('accepts exact owner/document/query/source evidence and preserves phrase and fuzzy labels', () => {
        const receipt = readPassageExplanation(response(), request(), passage());
        const ranges = evidenceRanges(receipt.passage.text, receipt.spans);
        expect(ranges.map(range => receipt.passage.text.slice(range.start, range.end))).toEqual(['résumé', 'carbon dioxide']);
        expect(ranges.map(range => range.evidence[0].kind)).toEqual(['fuzzy', 'phrase']);
        expect(ranges[0]).toMatchObject({ start: 3, end: 9 });
        expect(explanationClauseRows(receipt.clauses).map(row => [row.depth, row.key, row.clause.label]))
            .toEqual([[0, '0', 'AND'], [1, '0.0', 'résume~1'], [1, '0.1', 'carbon dioxide']]);
        expect(evidenceKindLabel('proximity')).toBe('Proximity match');
        expect(evidenceKindLabel('fuzzy', true)).toBe('Unscharfer Worttreffer');
    });

    it.each([
        ['version', { explanation_version: 2 }], ['scope', { scope: 'snippet' }],
        ['query', { query: 'another query' }], ['intent', { query_intent: 'natural' }],
        ['mode', { mode: 'hybrid' }], ['fuzzy', { fuzzy: true }], ['synonyms', { synonyms: true }],
    ])('rejects changed %s before source highlighting', (_, patch) => {
        expect(() => readPassageExplanation({ ...response(), ...patch }, request(), passage())).toThrow(/different search|version/);
    });

    it.each([
        ['owner', { owner_id: 'other-reader' }], ['document', { doc_id: 'another' }], ['passage', { chunk_index: 6 }],
    ])('rejects a response for another %s even if text is identical', (_, patch) => {
        const value = response(); Object.assign(value.passage, patch);
        expect(() => readPassageExplanation(value, request(), passage())).toThrow(/different owner, document, or passage/);
    });

    it.each([
        ['text', { text: 'Changed source' }], ['fingerprint', { source_hash: 'changed' }],
        ['timestamp', { indexed_at: 13 }], ['offset', { start_char: 101 }],
        ['unit', { offset_unit: 'utf16' }], ['truncation', { truncated: true }],
    ])('rejects changed %s while explanation was in flight', (_, patch) => {
        const value = response(); Object.assign(value.passage, patch);
        expect(() => readPassageExplanation(value, request(), passage())).toThrow(/changed while|current evidence/);
    });

    it('accepts missing fingerprints only as missing and never borrows the search-result hash', () => {
        const selected = passage(); delete selected.source_hash; delete selected.indexed_at;
        const value = response(); value.passage.source_hash = null; value.passage.indexed_at = null;
        expect(readPassageExplanation(value, request(), selected).passage.source_hash).toBeUndefined();
        value.passage.source_hash = 'search-result-hash';
        expect(() => readPassageExplanation(value, request(), selected)).toThrow(/changed while/);
    });

    it('keeps matched document-level conditions with no spans in the selected passage', () => {
        const value = response(); value.spans = [];
        value.clauses = { kind: 'field', field: 'title', label: 'title', matched: true,
            children: [{ kind: 'term', label: 'review', matched: true, children: [] }] };
        const receipt = readPassageExplanation(value, request(), passage());
        expect(receipt.lexical_document_match).toBe(true);
        expect(evidenceRanges(receipt.passage.text, receipt.spans)).toEqual([]);
    });

    it('retains actual matching fields separately from query scope and source-body spans', () => {
        const value = response();
        value.clauses!.children[0].matched_fields = ['title', 'translated'];
        const receipt = readPassageExplanation(value, request(), passage());
        expect(receipt.clauses!.children[0].matched_fields).toEqual(['title', 'translated']);
        value.clauses!.children[0].matched_fields.push('body');
        expect(receipt.clauses!.children[0].matched_fields).toEqual(['title', 'translated']);
        expect(evidenceFieldLabel('translated')).toBe('Translation');
        expect(evidenceFieldLabel('body', true)).toBe('Dokumenttext');
        const invalid = response() as any;
        invalid.clauses.children[0].matched_fields = ['title', 'title'];
        expect(() => readPassageExplanation(invalid, request(), passage())).toThrow(/matching text fields/);
        invalid.clauses.children[0].matched_fields = ['invented'];
        expect(() => readPassageExplanation(invalid, request(), passage())).toThrow(/matching text fields/);
    });

    it.each([
        { kind: 'and', label: 'AND', matched: true, children: [{ kind: 'term', label: 'missing', matched: false, children: [] }] },
        { kind: 'or', label: 'OR', matched: true, children: [] },
        { kind: 'not', label: 'NOT', matched: true, children: [{ kind: 'term', label: 'found', matched: true, children: [] }] },
        { kind: 'field', field: 'title', label: 'title', matched: true, children: [] },
    ])('rejects contradictory clause evaluation %j', clauses => {
        expect(() => readPassageExplanation({ ...response(), clauses }, request(), passage())).toThrow(/clause semantics/);
    });

    it('preserves unmatched alternatives and exclusions without inventing source spans', () => {
        const value = response();
        value.clauses = { kind: 'or', label: 'OR', matched: true, children: [
            { kind: 'term', label: 'missing', matched: false, children: [] },
            { kind: 'not', label: 'NOT', matched: true, children: [
                { kind: 'term', label: 'obsolete', matched: false, children: [] },
            ] },
        ] };
        value.spans = [];
        expect(explanationClauseRows(readPassageExplanation(value, request(), passage()).clauses).map(row => row.clause.matched))
            .toEqual([true, false, true, false]);
    });

    it('requires semantic evidence to have no lexical truth, clauses or word attribution', () => {
        const query = { ...request(), mode: 'vector' as const };
        const value = { ...response(), mode: 'vector' as const, lexical_query: '',
            lexical_document_match: null, clauses: null, semantic_evidence: true, spans: [] };
        expect(readPassageExplanation(value, query, passage()).semantic_evidence).toBe(true);
        expect(() => readPassageExplanation({ ...value, spans: response().spans }, query, passage())).toThrow(/Semantic/);
        expect(() => readPassageExplanation({ ...value, lexical_document_match: true }, query, passage())).toThrow(/Semantic/);
        expect(() => readPassageExplanation({ ...value, spans_truncated: true }, query, passage())).toThrow(/Semantic/);
    });

    it('requires the root condition and document truth to agree', () => {
        const value = response(); value.lexical_document_match = false;
        expect(() => readPassageExplanation(value, request(), passage())).toThrow(/inconsistent lexical/);
        expect(() => readPassageExplanation({ ...response(), clauses: null }, request(), passage())).toThrow(/inconsistent lexical/);
    });

    it('enforces tree, span, note and text limits without silently truncating evidence', () => {
        const value = response();
        expect(() => readPassageExplanation({ ...value, spans: Array(257).fill(value.spans[0]) }, request(), passage())).toThrow(/coverage/);
        expect(() => readPassageExplanation({ ...value, notes: Array(33).fill('note') }, request(), passage())).toThrow(/coverage/);
        expect(() => readPassageExplanation({ ...value, notes: [0] }, request(), passage())).toThrow(/coverage/);
        expect(() => readPassageExplanation({ ...value, clauses: { ...value.clauses, children: Array(1024).fill(value.clauses!.children[0]) } }, request(), passage())).toThrow(/clause limit/);
        let nested = value.clauses!;
        for (let depth = 0; depth < 65; depth++) nested = { kind: 'not', label: 'NOT', matched: true, children: [nested] };
        expect(() => readPassageExplanation({ ...value, clauses: nested }, request(), passage())).toThrow(/clause limit/);
        expect(() => readPassageExplanation({ ...value, lexical_query: '東'.repeat(2731) }, request(), passage())).toThrow(/version/);
        const selected = { ...passage(), text: 'x'.repeat(256 * 1024 + 1) };
        expect(() => readPassageExplanation({ ...value, passage: { ...value.passage, text: selected.text } }, request(), selected)).toThrow(/changed while/);
    });

    it('detaches mutable proof arrays from the incoming receipt', () => {
        const value = response(), receipt = readPassageExplanation(value, request(), passage());
        value.notes.push('later'); value.spans[0].start = 200;
        value.clauses!.children[0].label = 'changed';
        expect(receipt.notes).toEqual(['Current indexed document.']);
        expect(receipt.spans[0].start).toBe(5);
        expect(receipt.clauses!.children[0].label).toBe('résume~1');
    });
});

describe('source-preserving Unicode evidence ranges', () => {
    it('maps UTF-8 to UTF-16 around astral characters, accents, combining marks and CJK', () => {
        const text = '😀 Straße e\u0301 東京 <source>';
        const values = ['😀', 'Straße', 'e\u0301', '東京', '<source>'];
        expect(evidenceRanges(text, values.map(value => span(text, value, 'term')))
            .map(range => text.slice(range.start, range.end))).toEqual(values);
    });

    it('merges overlapping source intervals without losing their kinds or touching adjacent ranges', () => {
        const text = 'carbon dioxide levels';
        const ranges = evidenceRanges(text, [span(text, 'dioxide', 'term'), span(text, 'carbon dioxide', 'phrase'),
            span(text, 'carbon', 'term'), span(text, 'levels', 'fuzzy'), span(text, 'carbon', 'term')]);
        expect(ranges).toHaveLength(2);
        expect(text.slice(ranges[0].start, ranges[0].end)).toBe('carbon dioxide');
        expect(ranges[0].evidence).toEqual([{ kind: 'term', clause: 'carbon' }, { kind: 'phrase', clause: 'carbon dioxide' }, { kind: 'term', clause: 'dioxide' }]);
        expect(evidenceRanges('ab', [{ start: 0, end: 1, kind: 'term', clause: 'a' }, { start: 1, end: 2, kind: 'term', clause: 'b' }])).toHaveLength(2);
    });

    it.each([
        { start: 1, end: 4 }, { start: 0, end: 3 }, { start: 0, end: 99 },
        { start: -1, end: 4 }, { start: 0, end: 0 }, { start: 0.5, end: 4 },
    ])('rejects non-boundary or invalid ranges %j', offsets => {
        expect(() => evidenceRanges('😀', [{ ...offsets, kind: 'term', clause: 'emoji' }])).toThrow(/offsets|Unicode/);
    });

    it('rejects invalid UTF-16 source strings instead of shifting following spans', () => {
        expect(() => evidenceRanges('\ud800x', [{ start: 3, end: 4, kind: 'term', clause: 'x' }])).toThrow(/invalid Unicode/);
    });
});
