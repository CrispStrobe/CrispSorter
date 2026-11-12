import { describe, expect, it, vi } from 'vitest';
import { completeDuplicateMove } from './duplicateMove';
import { driveSearchUri } from './browser';
import type { DuplicateMutationAudit } from './duplicateAudit';
import { duplicateGroupPanel, localPathPanel, type ContextPanel } from './panels';

const source = '/100%20-日本語.pdf';
const destination = '/moved/100%20-日本語.pdf';
const item = (path: string) => ({ path: driveSearchUri('drive', path), size: 12, mtime: 0, hash: null, role: 'source' as const });
function fixture(undo = false) {
    let panel: ContextPanel | null = duplicateGroupPanel('A', [item(undo ? destination : source)]);
    let resolve!: () => void;
    let reject!: (error: Error) => void;
    const pending = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
    const audit: DuplicateMutationAudit[] = [];
    const deps = {
        current: () => panel,
        changed: vi.fn((next: ContextPanel) => { panel = next; }),
        move: vi.fn(() => pending),
        record: vi.fn((entry: DuplicateMutationAudit) => { audit.push(entry); }),
    };
    const operation = { groupId: 'A', driveId: 'drive', source: undo ? destination : source,
        destination: undo ? source : destination,
        audit: { groupId: 'A', driveId: 'drive', operation: undo ? 'restore' as const : 'move' as const,
            from: source, to: undo ? null : destination, at: 0 } };
    return { deps, audit, operation, resolve, reject, get panel() { return panel; }, select(next: ContextPanel | null) { panel = next; } };
}

describe.each([false, true])('deferred duplicate move (undo=%s)', undo => {
    it.each(['other-group', 'local', 'closed'])('keeps the originating audit and preserves the %s panel', async selection => {
        const f = fixture(undo);
        const work = completeDuplicateMove(f.deps, f.operation);
        expect(f.deps.move).toHaveBeenCalledWith('drive', undo ? destination : source, undo ? source : destination);
        expect(f.audit).toEqual([]);
        const next = selection === 'other-group' ? duplicateGroupPanel('B', [item('/B.pdf')])
            : selection === 'local' ? localPathPanel('/local.pdf') : null;
        f.select(next);
        f.resolve();
        await work;
        expect(f.panel).toBe(next);
        expect(f.deps.changed).not.toHaveBeenCalled();
        expect(f.audit).toEqual([{ ...f.operation.audit, at: expect.any(Number) }]);
    });

    it('updates the current originating group while preserving later decisions and URI bytes', async () => {
        const f = fixture(undo);
        const work = completeDuplicateMove(f.deps, f.operation);
        f.select(duplicateGroupPanel('A', [item(undo ? destination : source), item('/extra.pdf')], 'keep_both', 'Updated title'));
        f.resolve();
        await work;
        expect(f.panel).toEqual(duplicateGroupPanel('A', [item(undo ? source : destination), item('/extra.pdf')], 'keep_both', 'Updated title'));
        expect(f.audit).toHaveLength(1);
    });

    it('leaves the panel and successful audit unchanged when the provider fails', async () => {
        const f = fixture(undo);
        const work = completeDuplicateMove(f.deps, f.operation);
        const original = f.panel;
        f.reject(new Error('provider failed'));
        await expect(work).rejects.toThrow('provider failed');
        expect(f.panel).toBe(original);
        expect(f.deps.changed).not.toHaveBeenCalled();
        expect(f.deps.record).not.toHaveBeenCalled();
    });
});
