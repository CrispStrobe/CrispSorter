import { describe, it, expect, vi } from 'vitest';
import { ProjectAutosave } from './autosave';

const receipt = (revision: number) => ({ revision, saved_at: '2026-11-08T10:00:00Z', path: 'project.json' });

describe('durable project autosave', () => {
    it('serializes edits made while a save is in flight and advances the revision lease', async () => {
        const calls: Array<[number, number]> = [];
        let release!: () => void;
        const gate = new Promise<void>(resolve => { release = resolve; });
        const saver = new ProjectAutosave<{ value: number }>(async (snapshot, rev) => {
            calls.push([snapshot.value, rev]);
            if (calls.length === 1) await gate;
            return receipt(rev + 1);
        }, () => {}, 0, 60_000);
        saver.schedule({ value: 1 });
        const first = saver.flush();
        saver.schedule({ value: 2 });
        saver.schedule({ value: 3 });
        release();
        await first;
        expect(calls).toEqual([[1, 0], [3, 1]]);
        expect(saver.hasUnsavedChanges()).toBe(false);
        await saver.close();
    });

    it('keeps later edits after a failed write and retries without inventing a revision', async () => {
        let fail = true;
        const writer = vi.fn(async (_snapshot: { value: number }, rev: number) => {
            if (fail) throw new Error('Disk full');
            return receipt(rev + 1);
        });
        const states: string[] = [];
        const saver = new ProjectAutosave(writer, s => states.push(s.state), 5, 60_000);
        saver.schedule({ value: 1 });
        await expect(saver.flush()).rejects.toThrow('Disk full');
        expect(saver.hasUnsavedChanges()).toBe(true);
        saver.schedule({ value: 2 });
        fail = false;
        await saver.flush();
        expect(writer.mock.calls[1]).toEqual([{ value: 2 }, 5]);
        expect(saver.getRevision()).toBe(6);
        expect(states).toContain('error');
        await saver.close();
    });

    it('snapshots caller data and does not double-write concurrent flushes', async () => {
        const writer = vi.fn(async (_snapshot: { value: number }, rev: number) => receipt(rev + 1));
        const saver = new ProjectAutosave(writer, () => {}, 0, 60_000);
        const input = { value: 1 };
        saver.schedule(input);
        input.value = 9;
        await Promise.all([saver.flush(), saver.flush()]);
        expect(writer).toHaveBeenCalledTimes(1);
        expect(writer.mock.calls[0][0]).toEqual({ value: 1 });
        await saver.close();
    });
    it('close drains an edit queued by the saved-status microtask', async () => {
        const calls: Array<[number, number]> = [];
        let queued = false;
        let saver!: ProjectAutosave<{ value: number }>;
        saver = new ProjectAutosave(async (snapshot, revision) => {
            calls.push([snapshot.value, revision]);
            return receipt(revision + 1);
        }, status => {
            if (status.state === 'saved' && !queued) {
                queued = true;
                queueMicrotask(() => saver.schedule({ value: 2 }));
            }
        }, 0, 60_000);
        saver.schedule({ value: 1 });
        await saver.close();
        expect(calls).toEqual([[1, 0], [2, 1]]);
        expect(saver.hasUnsavedChanges()).toBe(false);
        expect(saver.getRevision()).toBe(2);
    });

    it('a failed close keeps the saver open and retains the latest edit for retry', async () => {
        let fail = true;
        const writer = vi.fn(async (_snapshot: { value: number }, revision: number) => {
            if (fail) throw new Error('No space left');
            return receipt(revision + 1);
        });
        const saver = new ProjectAutosave(writer, () => {}, 0, 60_000);
        saver.schedule({ value: 1 });
        await expect(saver.close()).rejects.toThrow('No space left');
        saver.schedule({ value: 2 });
        fail = false;
        await saver.close();
        expect(writer.mock.calls[1]).toEqual([{ value: 2 }, 0]);
        expect(saver.hasUnsavedChanges()).toBe(false);
    });

});
