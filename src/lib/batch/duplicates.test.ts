import { describe, expect, it, vi } from 'vitest';
import {
    deleteVerifiedCopies, deletionWasConfirmed, DUPLICATE_MAX_BYTES,
    findVerifiedDuplicates, type DuplicateSource, type VerifiedDuplicateGroup,
} from './duplicates';

const hashA = 'a'.repeat(64);
const hashB = 'b'.repeat(64);
const file = (id: string, overrides: Partial<DuplicateSource> = {}): DuplicateSource => ({
    id, originalPath: `/in/${id}.pdf`, size: 20, status: 'review', ...overrides,
});
const group = (items: DuplicateSource[]): VerifiedDuplicateGroup => ({ size: 20, sha256: hashA, items });

describe('content-verified duplicate review', () => {
    it('does not mistake the same size or filename for identical contents', async () => {
        const items = [file('one'), file('two'), file('other'), file('unique', { size: 30 })];
        const hashFile = vi.fn(async (path: string) => path.endsWith('other.pdf') ? hashB : hashA);
        const scan = await findVerifiedDuplicates(items, hashFile);
        expect(scan.groups.map(value => value.items.map(item => item.id))).toEqual([['one', 'two']]);
        expect(hashFile).toHaveBeenCalledTimes(3);
        expect(scan.groups[0].sha256).toBe(hashA);
    });

    it('never groups hash errors, invalid hashes, provider URIs, or duplicate entries for one path', async () => {
        const items = [file('valid'), file('throws'), file('invalid'), file('alias', { originalPath: '/in/valid.pdf' }),
            file('provider', { originalPath: 'content://provider/123' })];
        const scan = await findVerifiedDuplicates(items, async path => {
            if (path.endsWith('throws.pdf')) throw new Error('Permission denied');
            return path.endsWith('invalid.pdf') ? '__error' : hashA;
        });
        expect(scan.groups).toEqual([]);
        expect(scan.failed.map(issue => issue.item.id).sort()).toEqual(['invalid', 'throws']);
        expect(scan.skipped.map(issue => issue.item.id)).toEqual(['alias', 'provider']);
    });

    it('reports unverified large, completed, and busy files while checking empty files safely', async () => {
        const items = [file('empty-a', { size: 0 }), file('empty-b', { size: 0 }),
            file('large', { size: DUPLICATE_MAX_BYTES + 1 }), file('done', { status: 'done' }), file('moving', { status: 'moving' })];
        const scan = await findVerifiedDuplicates(items, async () => hashA);
        expect(scan.groups[0].items.map(item => item.id)).toEqual(['empty-a', 'empty-b']);
        expect(scan.skipped).toHaveLength(3);
        expect(scan.checked).toBe(2);
    });

    it('bounds concurrent disk reads and preserves batch order', async () => {
        let active = 0;
        let peak = 0;
        const items = Array.from({ length: 13 }, (_, index) => file(String(index)));
        const progress = vi.fn();
        const scan = await findVerifiedDuplicates(items, async () => {
            active++;
            peak = Math.max(peak, active);
            await new Promise(resolve => setTimeout(resolve, 1));
            active--;
            return hashA;
        }, progress);
        expect(peak).toBeLessThanOrEqual(4);
        expect(scan.groups[0].items.map(item => item.id)).toEqual(items.map(item => item.id));
        expect(progress).toHaveBeenLastCalledWith(13, 13);
    });
});

describe('duplicate cleanup', () => {
    it('requires a retained member and refuses two entries pointing at the same source', async () => {
        const items = [file('keeper'), file('copy')];
        const deleteFile = vi.fn();
        await expect(deleteVerifiedCopies(group(items), 'missing', () => items, async () => hashA, deleteFile)).rejects.toThrow('keep');
        const aliases = [items[0], file('alias', { originalPath: items[0].originalPath })];
        await expect(deleteVerifiedCopies(group(aliases), 'keeper', () => aliases, async () => hashA, deleteFile)).rejects.toThrow('distinct');
        expect(deleteFile).not.toHaveBeenCalled();
    });

    it('does not delete a changed target or delete anything when the retained file changed', async () => {
        const items = [file('keeper'), file('copy')];
        for (const changed of ['keeper', 'copy']) {
            const deleteFile = vi.fn();
            const result = await deleteVerifiedCopies(group(items), 'keeper', () => items,
                async path => path.endsWith(`${changed}.pdf`) ? hashB : hashA, deleteFile);
            expect(result.deleted).toEqual([]);
            expect(result.failed).toHaveLength(1);
            expect(deleteFile).not.toHaveBeenCalled();
        }
    });

    it('rechecks membership after hashing and does not delete a row that moved during the check', async () => {
        const items = [file('keeper'), file('copy')];
        const snapshot = group(items.map(item => ({ ...item })));
        const deleteFile = vi.fn();
        const result = await deleteVerifiedCopies(snapshot, 'keeper', () => items, async () => {
            items[1].originalPath = '/elsewhere/changed.pdf';
            return hashA;
        }, deleteFile);
        expect(result.deleted).toEqual([]);
        expect(result.failed).toHaveLength(1);
        expect(deleteFile).not.toHaveBeenCalled();
    });

    it('retains failures while allowing other verified copies to be deleted', async () => {
        const items = [file('keeper'), file('locked'), file('copy')];
        const deleteFile = vi.fn(async (path: string) => {
            if (path.endsWith('locked.pdf')) throw new Error('Permission denied');
        });
        const result = await deleteVerifiedCopies(group(items), 'keeper', () => items, async () => hashA, deleteFile);
        expect(result.deleted.map(item => item.id)).toEqual(['copy']);
        expect(result.failed.map(issue => issue.item.id)).toEqual(['locked']);
        expect(deleteFile).not.toHaveBeenCalledWith('/in/keeper.pdf');
    });

    it('requires explicit native confirmation for the exact deleted path', () => {
        const path = '/in/copy.pdf';
        expect(deletionWasConfirmed(path, [`Deleted: ${path}`])).toBe(true);
        for (const response of [null, [], ['Error deleting /in/copy.pdf: Permission denied'], ['Deleted: /different.pdf'], ['Deleted: /in/copy.pdf', 'Error']]) {
            expect(deletionWasConfirmed(path, response)).toBe(false);
        }
    });
});
