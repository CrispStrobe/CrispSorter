import { describe, expect, it } from 'vitest';
import { buildQuery, type QueryBuilderInput } from './queryBuilder';

const input = (patch: Partial<QueryBuilderInput> = {}): QueryBuilderInput => ({
    all: '', any: '', phrase: '', excluded: '', field: '', nearLeft: '', nearRight: '', distance: 5, ordered: false, ...patch,
});

describe('literal query builder', () => {
    it('combines a field phrase, alternatives, exclusions and ordered proximity without losing grouping', () => {
        expect(buildQuery(input({ field: 'body', phrase: 'annual report', any: 'budget forecast', excluded: 'draft',
            nearLeft: 'net income', nearRight: 'growth', distance: 0, ordered: true }))).toEqual({
            query: 'body:(("budget" OR "forecast") AND "annual report" AND ("net income" pre/0 "growth") AND NOT "draft")', error: null,
        });
    });
    it('keeps operators, field separators, wildcards and backslashes entered as literals', () => {
        expect(buildQuery(input({ nearLeft: 'OR', nearRight: 'title:report* "quoted" \\path', distance: 1024 })).query)
            .toBe('("OR" w/1024 "title:report* \\"quoted\\" \\\\path")');
        expect(buildQuery(input({ excluded: 'OR NOT' })).query).toBe('NOT "OR" AND NOT "NOT"');
    });
    it('refuses incomplete operands and invalid distances instead of silently omitting a condition', () => {
        expect(buildQuery(input({ all: 'invoice', nearLeft: 'one' })).error).toBe('operands');
        for (const distance of [null, -1, 1.5, 1025, Number.NaN, Number.POSITIVE_INFINITY]) {
            expect(buildQuery(input({ nearLeft: 'one', nearRight: 'two', distance })).error).toBe('distance');
        }
    });
    it('counts the native UTF-8 byte limit and preserves Unicode phrases', () => {
        expect(buildQuery(input({ phrase: 'München 東京' })).query).toBe('"München 東京"');
        expect(buildQuery(input({ phrase: '東'.repeat(3000) })).error).toBe('length');
        expect(buildQuery(input())).toEqual({ query: '', error: null });
    });
});
