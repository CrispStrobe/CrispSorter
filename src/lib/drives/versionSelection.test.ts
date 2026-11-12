import { describe, expect, it, vi } from 'vitest';
import { createVersionSelection, emptyVersionState, type VersionState, type VersionTarget } from './versionSelection';

type Stat = { is_dir: boolean; path: string };
type Cloud = { id: string; path: string };
type Local = { id: number; path: string };
const target = (name = 'A', driveId = 'drive', directory = '/folder'): VersionTarget =>
    ({ driveId, directory, path: `${directory}/${name}`, name });
const caps = { create_dir: false, rename: false, move_path: false, copy: false, delete: false, versions: true };
function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (error: Error) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}
function fixture() {
    let selected: VersionTarget | null = target();
    let state: VersionState<Stat, Cloud, Local> = emptyVersionState();
    const deps = {
        current: () => selected,
        changed: vi.fn((next: typeof state) => { state = next; }),
        stat: vi.fn(async (item: VersionTarget): Promise<Stat> => ({ is_dir: false, path: item.path })),
        capabilities: vi.fn(async (_driveId: string) => caps),
        cloud: vi.fn(async (item: VersionTarget): Promise<Cloud[]> => [{ id: 'same-version-id', path: item.path }]),
        local: vi.fn(async (item: VersionTarget): Promise<Local[]> => [{ id: 1, path: item.path }]),
        confirm: vi.fn((_version: Cloud, _target: VersionTarget) => true),
        restore: vi.fn(async (_target: VersionTarget, _versionId: string): Promise<unknown> => undefined),
    };
    const controller = createVersionSelection(deps);
    return { controller, deps, get state() { return state; }, select(item: VersionTarget | null) { selected = item; } };
}

describe('drive version selection', () => {
    it.each(['stat', 'capabilities', 'cloud', 'local'] as const)('ignores an A request finishing at %s after B loads', async stage => {
        const f = fixture();
        const pending = deferred<any>();
        f.deps[stage].mockReturnValueOnce(pending.promise);
        const first = f.controller.load(target());
        await vi.waitFor(() => expect(f.deps[stage]).toHaveBeenCalledOnce());
        f.select(target('B'));
        await f.controller.load(target('B'));
        const expected = f.state;
        const values = { stat: { is_dir: false, path: '/folder/A' }, capabilities: caps,
            cloud: [{ id: 'A-version', path: '/folder/A' }], local: [{ id: 2, path: '/folder/A' }] };
        pending.resolve(values[stage]);
        await first;
        expect(f.state).toBe(expected);
        expect(f.state.stat?.path).toBe('/folder/B');
        expect(f.state.cloud[0].path).toBe('/folder/B');
        expect(f.state.local[0].path).toBe('/folder/B');
        expect(f.state.loading).toBe(false);
    });

    it('a stale failure cannot set errors or stop the active loading indicator', async () => {
        const f = fixture();
        const a = deferred<Stat>();
        const b = deferred<Stat>();
        f.deps.stat.mockReturnValueOnce(a.promise).mockReturnValueOnce(b.promise);
        const first = f.controller.load(target());
        f.select(target('B'));
        const second = f.controller.load(target('B'));
        a.reject(new Error('A failed'));
        await first;
        expect(f.state.error).toBe('');
        expect(f.state.loading).toBe(true);
        b.resolve({ is_dir: false, path: '/folder/B' });
        await second;
        expect(f.state.loading).toBe(false);
    });

    it.each([target('A', 'other-drive'), target('A', 'drive', '/other-folder'), null])('guards drive/path/deselection changes without waiting for invalidation (%j)', async changed => {
        const f = fixture();
        const pending = deferred<Stat>();
        f.deps.stat.mockReturnValueOnce(pending.promise);
        const work = f.controller.load(target());
        const before = f.state;
        f.select(changed);
        pending.resolve({ is_dir: false, path: '/folder/A' });
        await work;
        expect(f.state).toBe(before);
        expect(f.deps.capabilities).not.toHaveBeenCalled();
    });

    it('invalidation clears stale rows immediately and blocks late completion', async () => {
        const f = fixture();
        await f.controller.load(target());
        const row = f.state.cloud[0];
        const pending = deferred<Stat>();
        f.deps.stat.mockReturnValueOnce(pending.promise);
        const work = f.controller.load(target());
        f.controller.invalidate();
        expect(f.state).toEqual(emptyVersionState());
        pending.resolve({ is_dir: false, path: '/folder/A' });
        await work;
        await f.controller.restore(row);
        expect(f.deps.restore).not.toHaveBeenCalled();
        expect(f.state).toEqual(emptyVersionState());
    });

    it('never restores a stale row against B, even with the same version ID', async () => {
        const f = fixture();
        await f.controller.load(target());
        const rowA = f.state.cloud[0];
        f.select(target('B'));
        await f.controller.load(target('B'));
        await f.controller.restore(rowA);
        expect(f.deps.confirm).not.toHaveBeenCalled();
        expect(f.deps.restore).not.toHaveBeenCalled();
        await f.controller.restore(f.state.cloud[0]);
        expect(f.deps.restore).toHaveBeenCalledExactlyOnceWith(target('B'), 'same-version-id');
        expect(f.deps.stat).toHaveBeenLastCalledWith(target('B'));
        expect(f.deps.stat).toHaveBeenCalledTimes(3);
    });

    it('an A → B → A round trip does not revive the old A row', async () => {
        const f = fixture();
        await f.controller.load(target());
        const old = f.state.cloud[0];
        f.select(target('B'));
        await f.controller.load(target('B'));
        f.select(target());
        await f.controller.load(target());
        await f.controller.restore(old);
        expect(f.deps.restore).not.toHaveBeenCalled();
    });

    it('rechecks selection after confirmation', async () => {
        const f = fixture();
        await f.controller.load(target());
        f.deps.confirm.mockImplementation(() => { f.select(target('B')); return true; });
        await f.controller.restore(f.state.cloud[0]);
        expect(f.deps.restore).not.toHaveBeenCalled();
    });

    it.each([false, true])('restore completion leaves B selected and its versions intact (failure=%s)', async fail => {
        const f = fixture();
        await f.controller.load(target());
        const pending = deferred<unknown>();
        f.deps.restore.mockReturnValueOnce(pending.promise);
        const restoring = f.controller.restore(f.state.cloud[0]);
        expect(f.deps.restore).toHaveBeenCalledWith(target(), 'same-version-id');
        f.select(target('B'));
        await f.controller.load(target('B'));
        expect(f.state.restoring).not.toBe(f.state.cloud[0]);
        await f.controller.restore(f.state.cloud[0]);
        expect(f.deps.restore).toHaveBeenCalledOnce();
        if (fail) pending.reject(new Error('A restore failed'));
        else pending.resolve(undefined);
        await restoring;
        expect(f.state.stat?.path).toBe('/folder/B');
        expect(f.state.cloud[0].path).toBe('/folder/B');
        expect(f.state.error).toBe('');
        expect(f.state.restoring).toBe(null);
        expect(f.deps.stat).toHaveBeenCalledTimes(2);
    });

    it('does not query unsupported provider versions or versions for search-result inspection', async () => {
        const f = fixture();
        f.deps.capabilities.mockResolvedValue({ ...caps, versions: false });
        await f.controller.load(target());
        expect(f.deps.cloud).not.toHaveBeenCalled();
        expect(f.deps.local).toHaveBeenCalledOnce();
        const search = { ...target('search-result'), name: null };
        f.select(search);
        await f.controller.load(search);
        expect(f.state.stat?.path).toBe(search.path);
        expect(f.deps.capabilities).toHaveBeenCalledOnce();
        expect(f.state.loading).toBe(false);
    });

    it('disposal prevents late updates and restore actions', async () => {
        const f = fixture();
        await f.controller.load(target());
        const old = f.state.cloud[0];
        const pending = deferred<Stat>();
        f.deps.stat.mockReturnValueOnce(pending.promise);
        const work = f.controller.load(target());
        f.controller.dispose();
        const updates = f.deps.changed.mock.calls.length;
        pending.resolve({ is_dir: false, path: '/folder/A' });
        await work;
        await f.controller.restore(old);
        expect(f.deps.changed).toHaveBeenCalledTimes(updates);
        expect(f.deps.restore).not.toHaveBeenCalled();
    });
});
