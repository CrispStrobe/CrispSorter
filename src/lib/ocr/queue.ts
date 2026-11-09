/** A sequential OCR queue. Pause and cancellation stop between native calls. */
export type QueuePhase = 'idle' | 'running' | 'pausing' | 'paused' | 'cancelling' | 'completed' | 'cancelled';
export interface QueueJob<R> { id: string; run: () => Promise<R>; }
export interface QueueState {
    phase: QueuePhase; total: number; completed: number; failed: number;
    currentId: string | null; remainingIds: string[];
}
export interface QueueCallbacks<R> {
    onState?: (state: Readonly<QueueState>) => void;
    onStart?: (job: QueueJob<R>) => void | Promise<void>;
    onResult?: (job: QueueJob<R>, result: R) => void | Promise<void>;
    onError?: (job: QueueJob<R>, error: unknown) => void | Promise<void>;
    /** A lifecycle checkpoint failed, not the recognition operation. The run
     * stops with its current/pending IDs retained; a saved batch decides which
     * pages need retry after the writer recovers. Never replace a good result
     * with a recognition error merely because saving it failed. */
    onCheckpointError?: (error: unknown, job: QueueJob<R>, stage: 'start' | 'result' | 'error') => void | Promise<void>;
    /** Checked immediately before starting a job and again before applying results. */
    isCurrent?: (job: QueueJob<R>) => boolean;
    /** Synchronous gate; false prevents an entire run from starting. */
    canStart?: () => boolean;
}

/**
 * Exactly one native invocation is allowed at a time, including after invalidate().
 * completed counts successful applied results; failed counts failed jobs. Progress
 * is therefore (completed + failed) / total. Cancelled pending IDs are retained.
 */
export class SequentialQueue<R> {
    private value: QueueState = { phase: 'idle', total: 0, completed: 0, failed: 0, currentId: null, remainingIds: [] };
    private jobs: QueueJob<R>[] = [];
    private next = 0;
    private generation = 0;
    private active: Promise<void> | null = null;
    private claimed = false;

    constructor(private readonly callbacks: QueueCallbacks<R> = {}) {}

    get state(): Readonly<QueueState> { return { ...this.value, remainingIds: [...this.value.remainingIds] }; }
    get canStart(): boolean {
        return !this.claimed && ['idle', 'completed', 'cancelled'].includes(this.value.phase) && (this.callbacks.canStart?.() ?? true);
    }

    private publish(): void {
        // UI observers do not own the queue's state or the native-call lock.
        // An observer exception must not leave an inference invocation orphaned.
        try { this.callbacks.onState?.(this.state); } catch { /* state remains inspectable */ }
    }

    /** Claims the run synchronously; returns false if another run is still active. */
    start(jobs: QueueJob<R>[]): boolean {
        if (!this.canStart) return false;
        if (jobs.some((job) => !job.id || typeof job.run !== 'function') || new Set(jobs.map((job) => job.id)).size !== jobs.length) {
            throw new Error('Every queue job must have a unique nonempty ID and a run function.');
        }
        this.jobs = jobs.map((job) => Object.freeze({ id: job.id, run: job.run }));
        this.next = 0;
        this.generation += 1;
        this.value = { phase: 'running', total: jobs.length, completed: 0, failed: 0, currentId: null, remainingIds: jobs.map((job) => job.id) };
        this.launch(this.generation);
        return true;
    }

    /** Pause after the current native operation and its result handler settle. */
    pause(): void {
        if (this.value.phase !== 'running') return;
        this.value = { ...this.value, phase: 'pausing' };
        this.publish();
    }

    /** Continue the same pending job list. False means the queue is not paused. */
    resume(): boolean {
        if (this.value.phase === 'pausing') {
            this.value = { ...this.value, phase: 'running' };
            this.publish();
            return true;
        }
        if (this.value.phase !== 'paused' || this.claimed || !(this.callbacks.canStart?.() ?? true)) return false;
        this.value = { ...this.value, phase: 'running' };
        this.launch(this.generation);
        return true;
    }

    /** Cancel between pages. The in-flight page still completes and is saved. */
    cancel(): void {
        if (['idle', 'completed', 'cancelled'].includes(this.value.phase)) return;
        this.value = { ...this.value, phase: this.claimed ? 'cancelling' : 'cancelled' };
        this.publish();
    }

    /**
     * Discard callbacks for an obsolete project/page generation. This does not
     * interrupt native code; start() remains locked until that call settles.
     */
    invalidate(): void {
        this.generation += 1;
        this.value = { ...this.value, phase: this.claimed ? 'cancelling' : 'cancelled', remainingIds: this.jobs.slice(this.next).map((job) => job.id) };
        this.publish();
    }

    /** Wait only for this processing segment; an intentionally paused queue resolves. */
    whenSettled(): Promise<void> { return this.active ?? Promise.resolve(); }

    private isCurrent(token: number, job: QueueJob<R>): boolean {
        if (token !== this.generation) return false;
        try { return this.callbacks.isCurrent?.(job) ?? true; } catch { return false; }
    }

    private launch(token: number): void {
        // Defer the loop until after the lock and active promise are installed.
        // An onState callback may synchronously pause/cancel but cannot start twice.
        this.claimed = true;
        this.active = Promise.resolve().then(() => this.run(token)).finally(() => {
            this.claimed = false;
            this.active = null;
            this.publish();
        });
        this.publish();
    }

    private async checkpoint(token: number, job: QueueJob<R>, stage: 'start' | 'result' | 'error', action: () => void | Promise<void>): Promise<boolean> {
        try { await action(); return true; }
        catch (error) {
            // A failed notification must not unlock a second native invocation
            // or allow the next page to overtake an uncommitted checkpoint.
            if (this.isCurrent(token, job)) {
                try { await this.callbacks.onCheckpointError?.(error, job, stage); } catch { /* the run still stops */ }
            }
            return false;
        }
    }

    private async run(token: number): Promise<void> {
        let terminal: QueuePhase = 'completed';
        while (true) {
            if (token !== this.generation || this.value.phase === 'cancelling') { terminal = 'cancelled'; break; }
            if (this.next >= this.jobs.length) break;
            if (this.value.phase === 'pausing') { terminal = 'paused'; break; }
            const job = this.jobs[this.next];
            if (!this.isCurrent(token, job)) { terminal = 'cancelled'; break; }
            this.value = { ...this.value, currentId: job.id, remainingIds: this.jobs.slice(this.next + 1).map((item) => item.id) };
            this.publish();
            if (!await this.checkpoint(token, job, 'start', () => this.callbacks.onStart?.(job))) { terminal = 'cancelled'; break; }
            if (!this.isCurrent(token, job) || this.value.phase === 'cancelling') { terminal = 'cancelled'; break; }
            // A user may pause or stop while the start checkpoint is saving.
            // Honor that request before entering the expensive native call.
            if (this.value.phase === 'pausing') { terminal = 'paused'; break; }
            let result: R;
            try {
                result = await job.run();
            } catch (error) {
                if (!this.isCurrent(token, job)) { terminal = 'cancelled'; break; }
                this.value = { ...this.value, failed: this.value.failed + 1 };
                if (!await this.checkpoint(token, job, 'error', () => this.callbacks.onError?.(job, error))) { terminal = 'cancelled'; break; }
                if (!this.isCurrent(token, job)) { terminal = 'cancelled'; break; }
                this.next += 1;
                this.value = { ...this.value, currentId: null, remainingIds: this.jobs.slice(this.next).map((item) => item.id) };
                this.publish();
                continue;
            }
            if (!this.isCurrent(token, job)) { terminal = 'cancelled'; break; }
            if (!await this.checkpoint(token, job, 'result', () => this.callbacks.onResult?.(job, result))) { terminal = 'cancelled'; break; }
            if (!this.isCurrent(token, job)) { terminal = 'cancelled'; break; }
            this.value = { ...this.value, completed: this.value.completed + 1 };
            this.next += 1;
            this.value = { ...this.value, currentId: null, remainingIds: this.jobs.slice(this.next).map((item) => item.id) };
            this.publish();
        }
        this.value = { ...this.value, phase: terminal, currentId: null, remainingIds: this.jobs.slice(this.next).map((job) => job.id) };
    }
}
