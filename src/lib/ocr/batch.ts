import type { OcrPage, OcrProject } from './model';

/** Persisted intent, independent of the in-memory native invocation queue. */
export type BatchPhase = 'running' | 'paused' | 'stopped' | 'interrupted' | 'completed';
export type BatchPageState = 'pending' | 'running' | 'completed' | 'failed';
export interface BatchOptions {
    profileId: string;
    config: Record<string, unknown>;
    engine: string;
    language: string;
}
export interface BatchPage {
    pageId: string;
    imagePath: string;
    width: number;
    height: number;
    contentRevision: string;
    state: BatchPageState;
    error?: string;
}
export interface RecognitionBatch extends BatchOptions {
    version: 1;
    id: string;
    phase: BatchPhase;
    jobs: BatchPage[];
    createdAt: string;
    updatedAt: string;
}
export interface BatchResumePlan {
    project: OcrProject;
    pageIds: string[];
    options: BatchOptions | null;
    conflicts: string[];
    completed: number;
    failed: number;
}

const copy = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
const phases: readonly BatchPhase[] = ['running', 'paused', 'stopped', 'interrupted', 'completed'];
const states: readonly BatchPageState[] = ['pending', 'running', 'completed', 'failed'];
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const nonempty = (value: unknown): value is string => typeof value === 'string' && value.length > 0;
const timestamp = () => new Date().toISOString();

/** Legacy pages receive their revision when a project is parsed or imported. */
function sourceRevision(page: OcrPage): string {
    if (!page.contentRevision) throw new Error('Reload this project before starting a resumable recognition batch.');
    return page.contentRevision;
}

function replaceBatch(project: OcrProject, batch: RecognitionBatch): OcrProject {
    const stamp = timestamp();
    return { ...project, updatedAt: stamp, batch: { ...batch, updatedAt: stamp } };
}

/** Freeze the exact selected order, source revisions, and recognition settings. */
export function beginBatch(project: OcrProject, pageIds: readonly string[], options: BatchOptions): OcrProject {
    if (!pageIds.length || new Set(pageIds).size !== pageIds.length) throw new Error('Select unique pages for the recognition batch.');
    if (!nonempty(options.profileId) || !object(options.config) || typeof options.engine !== 'string' || typeof options.language !== 'string') throw new Error('Recognition batch settings are invalid.');
    const pages = new Map(project.pages.map(page => [page.id, page]));
    const jobs: BatchPage[] = pageIds.map(pageId => {
        const page = pages.get(pageId);
        if (!page || page.excluded) throw new Error('A selected page is missing or excluded. Review the selection before recognition.');
        return { pageId, imagePath: page.imagePath, width: page.width, height: page.height, contentRevision: sourceRevision(page), state: 'pending' };
    });
    const stamp = timestamp();
    return replaceBatch(project, {
        ...copy(options), version: 1, id: globalThis.crypto?.randomUUID?.() ?? `batch-${Date.now()}-${Math.random().toString(36).slice(2)}`,
        phase: 'running', jobs, createdAt: stamp, updatedAt: stamp,
    });
}

export function updateBatchPhase(project: OcrProject, phase: BatchPhase): OcrProject {
    if (!project.batch || project.batch.phase === phase) return project;
    return replaceBatch(project, { ...project.batch, phase });
}

/** Call after applying the page result/error so retries retain the current evidence. */
export function updateBatchPage(project: OcrProject, pageId: string, state: BatchPageState, error?: string): OcrProject {
    if (!project.batch || !project.batch.jobs.some(job => job.pageId === pageId)) return project;
    const page = project.pages.find(item => item.id === pageId);
    return replaceBatch(project, {
        ...project.batch,
        jobs: project.batch.jobs.map(job => job.pageId === pageId ? {
            ...job, state, error: state === 'failed' ? (error || 'Recognition failed.') : undefined,
            // A completed/failed result may have changed review state. A retry
            // authorizes that result, never subsequent edits made by the user.
            contentRevision: state === 'running' || !page ? job.contentRevision : sourceRevision(page),
            imagePath: state === 'running' || !page ? job.imagePath : page.imagePath,
            width: state === 'running' || !page ? job.width : page.width,
            height: state === 'running' || !page ? job.height : page.height,
        } : job),
    });
}

/** A source check also guards a late native result after the user changes projects. */
export function batchPageIsCurrent(project: OcrProject, pageId: string): boolean {
    const job = project.batch?.jobs.find(item => item.pageId === pageId);
    const page = project.pages.find(item => item.id === pageId);
    return !!job && !!page && !page.excluded && job.imagePath === page.imagePath &&
        job.width === page.width && job.height === page.height && job.contentRevision === page.contentRevision;
}

/**
 * Resuming never picks up unrelated pending pages or current toolbar settings.
 * Any changed source blocks the continuation as a whole; the UI can start a new
 * explicitly reviewed selection instead. Successful pages are never rerun.
 */
export function resumeBatch(project: OcrProject, options: { retryFailed?: boolean } = {}): BatchResumePlan {
    const batch = project.batch;
    const empty = { project, pageIds: [], options: null, conflicts: [], completed: 0, failed: 0 };
    if (!batch) return empty;
    const completed = batch.jobs.filter(job => job.state === 'completed').length;
    const failed = batch.jobs.filter(job => job.state === 'failed').length;
    const candidates = batch.jobs.filter(job => job.state === 'pending' || job.state === 'running' || (options.retryFailed && job.state === 'failed'));
    const conflicts = candidates.filter(job => !batchPageIsCurrent(project, job.pageId)).map(job => {
        const page = project.pages.find(item => item.id === job.pageId);
        return `${page?.name ?? 'Removed page'}: the source, selection, or reviewed text changed after the batch was saved. Start a new recognition selection for this page.`;
    });
    if (conflicts.length) return { ...empty, completed, failed, conflicts };
    const pageIds = candidates.map(job => job.pageId);
    const next = pageIds.length ? replaceBatch(project, {
        ...batch, phase: 'running', jobs: batch.jobs.map(job => pageIds.includes(job.pageId) ? { ...job, state: 'pending', error: undefined } : job),
    }) : project;
    return {
        project: next, pageIds, completed, failed, conflicts: [],
        options: copy({ profileId: batch.profileId, config: batch.config, engine: batch.engine, language: batch.language }),
    };
}

/** Validate a persisted checkpoint without trusting arbitrary queue metadata. */
export function restoreBatch(input: unknown): RecognitionBatch {
    if (!object(input) || input.version !== 1 || !nonempty(input.id) || !phases.includes(input.phase as BatchPhase) ||
        !nonempty(input.profileId) || !object(input.config) || typeof input.engine !== 'string' || typeof input.language !== 'string' ||
        typeof input.createdAt !== 'string' || typeof input.updatedAt !== 'string' || !Array.isArray(input.jobs) || input.jobs.length > 10000) {
        throw new Error('The saved recognition batch is invalid.');
    }
    const ids = new Set<string>();
    for (const job of input.jobs) {
        if (!object(job) || !nonempty(job.pageId) || ids.has(job.pageId) || !nonempty(job.imagePath) || !nonempty(job.contentRevision) ||
            !states.includes(job.state as BatchPageState) || typeof job.width !== 'number' || !Number.isFinite(job.width) || job.width <= 0 ||
            typeof job.height !== 'number' || !Number.isFinite(job.height) || job.height <= 0 ||
            (job.error !== undefined && typeof job.error !== 'string')) throw new Error('The saved recognition batch contains invalid page evidence.');
        ids.add(job.pageId);
    }
    const batch = copy(input) as unknown as RecognitionBatch;
    if (batch.phase === 'running' || batch.jobs.some(job => job.state === 'running')) batch.phase = 'interrupted';
    batch.jobs = batch.jobs.map(job => job.state === 'running' ? { ...job, state: 'pending' } : job);
    return batch;
}
