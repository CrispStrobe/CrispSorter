import { describe, expect, it } from 'vitest';
import { validateBatchItems, validateSessions } from './sessionTransfer';

const file = (id = 'book') => ({ id, originalPath: `/docs/${id}.pdf`, originalName: `${id}.pdf`,
    extension: 'pdf', size: 42, modifiedAt: 100, status: 'review', isAccepted: false });

describe('batch replacement validation', () => {
    it('keeps valid raw-array exports, supplied empty/full text, and review metadata', () => {
        const items = [{ ...file(), extractedText: '', suggestedAuthor: 'A', isIgnored: false },
            { ...file('other'), extractedText: 'A complete document', historyMatch: { processedAt: 1, suggestedTitle: 'Earlier' } }];
        expect(validateBatchItems(items)).toEqual(items);
        expect(validateBatchItems([])).toEqual([]);
    });

    it.each([
        null, {}, [{ ...file(), size: -1 }], [{ ...file(), modifiedAt: Infinity }],
        [{ ...file(), isAccepted: 'yes' }], [{ ...file(), status: 'unexpected' }],
        [{ ...file(), extractedText: 42 }], [{ ...file(), audioChannels: 1.5 }],
        [{ ...file(), id: '__proto__' }], [{ ...file(), originalPath: '' }],
        [{ ...file(), historyMatch: { processedAt: 'yesterday' } }],
    ])('rejects malformed input without repairing away a validation error: %j', value => {
        expect(() => validateBatchItems(value)).toThrow();
    });

    it('rejects duplicate IDs and normalised filesystem paths but keeps distinct provider IDs', () => {
        expect(() => validateBatchItems([file(), { ...file(), originalPath: '/elsewhere.pdf' }])).toThrow('duplicate id');
        expect(() => validateBatchItems([file(), { ...file('copy'), originalPath: '/DOCS/book.pdf' }])).toThrow('duplicate source');
        expect(validateBatchItems([{ ...file(), originalPath: 'content://docs/A' },
            { ...file('copy'), originalPath: 'content://docs/a' }])).toHaveLength(2);
    });

    it('turns interrupted snapshots into retryable work without auto-accepting them', () => {
        const input = { ...file(), status: 'moving', statusDetail: 'in progress', isAccepted: true };
        expect(validateBatchItems([input])[0]).toMatchObject({ status: 'unfinished', isAccepted: false });
        expect(validateBatchItems([input])[0].statusDetail).toBeUndefined();
        expect(input.status).toBe('moving');
    });

    it('loads legacy sessions in date order and validates every saved row', () => {
        const old = { id: 'old', startTime: 1, status: 'paused', items: [file()] };
        const recent = { ...old, id: 'new', startTime: 2 };
        expect(validateSessions({ old, new: recent }).map(session => session.id)).toEqual(['new', 'old']);
        expect(validateSessions({ old })[0]).toMatchObject({ providerId: '', modelId: '' });
        expect(() => validateSessions({ old: { ...old, items: [{ ...file(), id: '' }] } })).toThrow('invalid id');
        expect(() => validateSessions({ wrongKey: old })).toThrow('invalid session details');
    });
});
