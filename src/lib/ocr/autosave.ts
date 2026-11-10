/** Serializes autosaves so an older request can never overwrite a newer edit. */
export interface SaveReceipt { revision: number; saved_at: number | string; path: string; }
export type SaveState = 'saved' | 'pending' | 'saving' | 'error';
export interface SaveStatus { state: SaveState; error?: string; savedAt?: number | string; }

export class ProjectAutosave<T> {
    private pending: T | undefined;
    private timer: ReturnType<typeof setTimeout> | undefined;
    private active: Promise<void> | undefined;
    private revision: number;
    private closed = false;
    private status: SaveStatus = { state: 'saved' };

    constructor(
        private readonly writer: (project: T, expectedRevision: number) => Promise<SaveReceipt>,
        private readonly onStatus: (status: SaveStatus) => void,
        revision = 0,
        private readonly delay = 600,
    ) { this.revision = revision; }

    private report(status: SaveStatus) {
        this.status = status;
        // An observer cannot own the write lock or make a committed revision
        // look like a failed write. The pending snapshot survives UI errors.
        try { this.onStatus({ ...status }); } catch { /* writer state remains authoritative */ }
    }

    schedule(project: T): void {
        if (this.closed) return;
        // Snapshot at the edit boundary. Svelte proxies are deliberately not retained.
        this.pending = JSON.parse(JSON.stringify(project)) as T;
        clearTimeout(this.timer);
        this.report({ ...this.status, state: 'pending', error: undefined });
        this.timer = setTimeout(() => { void this.flush().catch(() => {}); }, this.delay);
    }

    async flush(): Promise<void> {
        clearTimeout(this.timer);
        if (this.active) {
            await this.active;
            if (this.pending !== undefined) await this.flush();
            return;
        }
        if (this.pending === undefined) return;
        const first = this.pending;
        this.pending = undefined;
        // Install the lock before any observer/writer can re-enter flush().
        this.active = Promise.resolve().then(() => this.drain(first));
        try { await this.active; } finally { this.active = undefined; }
    }

    private async drain(first: T): Promise<void> {
        let snapshot = first;
        while (true) {
            this.report({ ...this.status, state: 'saving', error: undefined });
            try {
                const result = await this.writer(snapshot, this.revision);
                if (!Number.isSafeInteger(result?.revision) || result.revision <= this.revision) throw new Error('Project save returned an invalid revision. The latest edits remain pending.');
                this.revision = result.revision;
                this.report({ state: this.pending === undefined ? 'saved' : 'pending', savedAt: result.saved_at });
            } catch (error) {
                // Keep the latest edit; if there was no later edit, retain this snapshot for retry.
                if (this.pending === undefined) this.pending = snapshot;
                const message = error instanceof Error ? error.message : String(error);
                this.report({ ...this.status, state: 'error', error: message });
                throw error;
            }
            if (this.pending === undefined) return;
            snapshot = this.pending;
            this.pending = undefined;
        }
    }

    hasUnsavedChanges(): boolean { return this.pending !== undefined || this.active !== undefined; }
    getRevision(): number { return this.revision; }
    getStatus(): SaveStatus { return { ...this.status }; }
    /** Flush before replacing the project or closing; callers surface a failure. */
    async close(): Promise<void> {
        // A saved-status observer can enqueue an edit in a microtask after the
        // current drain resolves. Recheck after each flush before closing so
        // clearing the debounce timer never abandons that last snapshot.
        while (this.hasUnsavedChanges()) await this.flush();
        this.closed = true;
        clearTimeout(this.timer);
    }
}
