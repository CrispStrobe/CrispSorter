import { describe, expect, it } from 'vitest';
import { reviewSelection, toggleVisibleSelection, restoreReviewDrafts, itemMetadata, sameMetadata } from './review';

describe('visible batch selection', () => {
    const all = [{ id: 'a' }, { id: 'b' }, { id: 'c' }];
    it('does not confuse equal counts with matching identities', () => {
        expect(reviewSelection(['a'], [{ id: 'b' }], all)).toEqual({ visibleIds: [], hiddenIds: ['a'], allVisible: false, someVisible: false });
    });
    it('limits actions to visible selected rows and exposes hidden selections', () => {
        expect(reviewSelection(['a', 'b', 'deleted'], [{ id: 'b' }, { id: 'c' }], all)).toEqual({ visibleIds: ['b'], hiddenIds: ['a'], allVisible: false, someVisible: true });
    });
    it('selects and deselects visible rows while preserving hidden choices', () => {
        expect(toggleVisibleSelection(['a'], [{ id: 'b' }, { id: 'c' }])).toEqual(['a', 'b', 'c']);
        expect(toggleVisibleSelection(['a', 'b', 'c'], [{ id: 'b' }, { id: 'c' }])).toEqual(['a']);
        expect(reviewSelection([], [], all).allVisible).toBe(false);
    });
});

describe('restored metadata drafts', () => {
    it('preserves deliberately cleared values and original metadata for change detection', () => {
        const base = { title: 'Old title', author: 'Acme', year: '2020' };
        const draft = { title: '', author: 'New author', year: '', originalPath: '/book.pdf', base };
        expect(restoreReviewDrafts({ one: draft })).toEqual({ one: draft });
        expect(sameMetadata(draft, base)).toBe(false);
        expect(itemMetadata({ suggestedAuthor: 'Acme' })).toEqual({ title: '', author: 'Acme', year: '' });
    });
    it('ignores malformed records and prototype keys without discarding valid drafts', () => {
        const draft = { title: 'Draft', author: '', year: '', originalPath: '/book.pdf', base: { title: '', author: '', year: '' } };
        expect(restoreReviewDrafts({ valid: draft, invalid: { title: 4 }, missingPath: { ...draft, originalPath: null } })).toEqual({ valid: draft });
        expect(restoreReviewDrafts(JSON.parse('{"__proto__": {}, "constructor": {}}'))).toEqual({});
        expect(restoreReviewDrafts(null)).toEqual({});
        expect(restoreReviewDrafts([])).toEqual({});
    });
});
