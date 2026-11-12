import { describe, expect, it } from 'vitest';
import {
    decodeDuplicateAudit,
    decodeDuplicateMutationAudit,
    encodeDuplicateAudit,
    latestDuplicateDecision,
    recordDuplicateRestore,
    type DuplicateMutationAudit,
} from './duplicateAudit';

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

    it('restores the latest decision for a reopened group', () => {
        const entries = [
            { groupId: 'g1', previous: 'review' as const, next: 'keep_source' as const, at: 1 },
            { groupId: 'g2', previous: 'review' as const, next: 'keep_both' as const, at: 2 },
            { groupId: 'g1', previous: 'keep_source' as const, next: 'keep_destination' as const, at: 3 },
        ];
        expect(latestDuplicateDecision(entries, 'g1')).toBe('keep_destination');
        expect(latestDuplicateDecision(entries, 'missing')).toBeNull();
    });

    it('validates and bounds cloud mutation audit records', () => {
        const records = Array.from({ length: 202 }, (_, at) => ({
            groupId: `g${at}`,
            driveId: 'drive-1',
            operation: 'move' as const,
            from: `/old/${at}`,
            to: `/new/${at}`,
            at,
        }));
        const decoded = decodeDuplicateMutationAudit(JSON.stringify(records));
        expect(decoded).toHaveLength(200);
        expect(decoded[0].groupId).toBe('g2');
        expect(decodeDuplicateMutationAudit(JSON.stringify([
            { groupId: 'bad', driveId: 'd', operation: 'move', from: '/a', to: '/b', at: 'now' },
            { groupId: 'bad', driveId: 'd', operation: 'unknown', from: '/a', to: '/b', at: 1 },
        ]))).toEqual([]);
    });

    it('preserves the trash event and records its successful restore across persistence', () => {
        const deleted: DuplicateMutationAudit = {
            groupId: 'g', driveId: 'd', operation: 'delete', from: '/docs/paper.pdf', to: null, at: 10,
        };
        const original = [deleted];
        const restored = recordDuplicateRestore(original, deleted, 20);
        expect(original).toEqual([deleted]);
        expect(decodeDuplicateMutationAudit(JSON.stringify(restored))).toEqual([
            deleted,
            { ...deleted, operation: 'restore', at: 20 },
        ]);
        // The last event is terminal: the UI must not offer the same trash restore again.
        expect(restored.at(-1)?.operation).toBe('restore');
    });

    it('bounds restore history without removing unrelated recent mutations', () => {
        const mutations: DuplicateMutationAudit[] = Array.from({ length: 200 }, (_, at) => ({
            groupId: 'g', driveId: 'd', operation: 'move', from: `/old/${at}`, to: `/new/${at}`, at,
        }));
        const restored = recordDuplicateRestore(mutations, mutations[199], 201);
        expect(restored).toHaveLength(200);
        expect(restored.slice(0, -1)).toEqual(mutations.slice(1));
        expect(restored.at(-1)).toEqual({ ...mutations[199], operation: 'restore', to: null, at: 201 });
    });

    it('preserves restore mutation records for durable history', () => {
        const decoded = decodeDuplicateMutationAudit(JSON.stringify([{
            groupId: 'g', driveId: 'd', operation: 'restore', from: '/a', to: null, at: 123,
        }]));
        expect(decoded).toHaveLength(1);
        expect(decoded[0].operation).toBe('restore');
    });
});
