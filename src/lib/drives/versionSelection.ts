import { supportsCloudVersions, type DriveCapabilities } from './browser';

export interface VersionTarget { driveId: string; directory: string; path: string; name: string | null; }
export interface VersionState<Stat, Cloud, Local> {
    stat: Stat | null;
    cloud: Cloud[];
    local: Local[];
    loading: boolean;
    error: string;
    restoring: Cloud | null;
}
export function emptyVersionState<Stat, Cloud, Local>(): VersionState<Stat, Cloud, Local> {
    return { stat: null, cloud: [], local: [], loading: false, error: '', restoring: null };
}

/** Owns asynchronous inspection and the identity of the versions it publishes. */
export function createVersionSelection<Stat extends { is_dir: boolean }, Cloud extends { id: string }, Local>(deps: {
    current(): VersionTarget | null;
    changed(state: VersionState<Stat, Cloud, Local>): void;
    stat(target: VersionTarget): Promise<Stat>;
    capabilities(driveId: string): Promise<DriveCapabilities>;
    cloud(target: VersionTarget): Promise<Cloud[]>;
    local(target: VersionTarget): Promise<Local[]>;
    confirm(version: Cloud, target: VersionTarget): boolean;
    restore(target: VersionTarget, versionId: string): Promise<unknown>;
}) {
    let generation = 0;
    let disposed = false;
    let restoring = false;
    let state = emptyVersionState<Stat, Cloud, Local>();
    let published: { generation: number; target: VersionTarget; cloud: Cloud[] } | null = null;
    const update = (patch: Partial<typeof state>) => {
        if (disposed) return;
        state = { ...state, ...patch };
        deps.changed(state);
    };
    const current = (revision: number, target: VersionTarget) => {
        const now = deps.current();
        return !disposed && revision === generation && now !== null
            && now.driveId === target.driveId && now.directory === target.directory
            && now.path === target.path && now.name === target.name;
    };
    function invalidate() {
        generation++;
        published = null;
        update({ ...emptyVersionState<Stat, Cloud, Local>(), restoring: state.restoring });
    }
    async function load(selection: VersionTarget) {
        const target = { ...selection };
        invalidate();
        const revision = generation;
        if (!current(revision, target)) return;
        update({ loading: true });
        try {
            const stat = await deps.stat(target);
            if (!current(revision, target)) return;
            update({ stat });
            if (stat.is_dir || target.name === null) return;
            const caps = await deps.capabilities(target.driveId);
            if (!current(revision, target)) return;
            const cloud = supportsCloudVersions(caps) ? await deps.cloud(target).catch(() => []) : [];
            if (!current(revision, target)) return;
            const local = await deps.local(target).catch(() => []);
            if (!current(revision, target)) return;
            published = { generation: revision, target, cloud };
            update({ cloud, local });
        } catch (error) {
            if (current(revision, target)) update({ error: String(error) });
        } finally {
            if (current(revision, target)) update({ loading: false });
        }
    }
    async function restore(version: Cloud) {
        const source = published;
        if (restoring || !source || !source.cloud.includes(version) || !current(source.generation, source.target)) return;
        if (!deps.confirm(version, source.target) || !current(source.generation, source.target)) return;
        restoring = true;
        update({ restoring: version, error: '' });
        try {
            // Restore always uses the target that supplied this exact row.
            await deps.restore(source.target, version.id);
            if (current(source.generation, source.target)) await load(source.target);
        } catch (error) {
            if (current(source.generation, source.target)) update({ error: String(error) });
        } finally {
            restoring = false;
            update({ restoring: null });
        }
    }
    return { load, restore, invalidate, dispose() { disposed = true; generation++; published = null; } };
}
