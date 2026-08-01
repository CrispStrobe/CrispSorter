import { describe, expect, it } from 'vitest';
import { decodeDuplicateAudit, encodeDuplicateAudit } from './duplicateAudit';

describe('duplicate decision audit persistence', () => {
    it('round-trips valid records and rejects malformed entries', () => {
        const raw = encodeDuplicateAudit([
            { groupId: 'g1', previous: 'review', next: 'keep_source', at: 123 },
        ]);
        expect(decodeDuplicateAudit(raw)).toEqual([
            { groupId: 'g1', previous: 'review', next: 'keep_source', at: 123 },
        ]);
        expect(decodeDuplicateAudit(JSON.stringify([
            null,
            { groupId: 'g2', previous: 'bad', next: 'keep_both', at: 1 },
            { groupId: 'g3', previous: 'review', next: 'keep_both', at: 'now' },
        ]))).toEqual([]);
    });

    it('keeps only the newest bounded audit entries', () => {
        const entries = Array.from({ length: 205 }, (_, at) => ({
            groupId: `g${at}`,
            previous: 'review' as const,
            next: 'keep_both' as const,
            at,
        }));
        const decoded = decodeDuplicateAudit(encodeDuplicateAudit(entries));
        expect(decoded).toHaveLength(200);
        expect(decoded[0].groupId).toBe('g5');
    });
});
