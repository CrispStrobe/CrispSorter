import { getSetting, saveSetting } from '../store';
import type { BatchItem } from '../types';
import { itemMetadata, sameMetadata, restoreReviewDrafts, type ReviewDrafts, type ReviewMetadata } from './review';

const KEY = 'batchMetadataDrafts';

/** Drafts survive component/tab changes and are saved independently of approval. */
class ReviewDraftStore {
    drafts = $state<ReviewDrafts>({});
    ready = $state(false);
    saving = $state(false);
    applyingId = $state<string | null>(null);
    error = $state('');
    private loading: Promise<void> | null = null;
    private timer: ReturnType<typeof setTimeout> | null = null;
    private writes: Promise<void> = Promise.resolve();
    private revision = 0;

    load(): Promise<void> {
        if (this.loading) return this.loading;
        this.loading = (async () => {
            try {
                this.drafts = restoreReviewDrafts(await getSetting(KEY, {}));
                this.ready = true;
                this.error = '';
            } catch (error) {
                this.error = String(error);
                this.loading = null;
            }
        })();
        return this.loading;
    }

    get(item: BatchItem) {
        const draft = this.drafts[item.id];
        return draft?.originalPath === item.originalPath && !sameMetadata(draft, itemMetadata(item)) ? draft : undefined;
    }

    set(item: BatchItem, value: ReviewMetadata) {
        if (!this.ready || this.applyingId) return;
        const previous = this.get(item);
        if (sameMetadata(value, itemMetadata(item))) delete this.drafts[item.id];
        else this.drafts[item.id] = { ...value, originalPath: item.originalPath, base: previous?.base ?? itemMetadata(item) };
        this.schedule();
    }

    discard(ids: readonly string[]) {
        if (!this.ready) return;
        let changed = false;
        for (const id of ids) {
            if (this.drafts[id]) { delete this.drafts[id]; changed = true; }
        }
        if (changed) this.schedule();
    }

    retainItems(items: readonly BatchItem[]) {
        const paths = new Map(items.map(item => [item.id, item.originalPath]));
        const current = new Map(items.map(item => [item.id, item]));
        this.discard(Object.entries(this.drafts).filter(([id, draft]) => paths.get(id) !== draft.originalPath || sameMetadata(draft, itemMetadata(current.get(id)!))).map(([id]) => id));
    }

    private schedule() {
        if (this.timer) clearTimeout(this.timer);
        this.saving = true;
        this.timer = setTimeout(() => { this.timer = null; void this.flush(); }, 250);
    }

    flush(): Promise<void> {
        if (!this.ready) return this.writes;
        if (this.timer) { clearTimeout(this.timer); this.timer = null; }
        const snapshot = $state.snapshot(this.drafts);
        const revision = ++this.revision;
        this.saving = true;
        // Serialize snapshots so an older write cannot replace a newer draft.
        this.writes = this.writes.then(() => saveSetting(KEY, snapshot)).then(() => {
            if (revision === this.revision) this.error = '';
        }).catch(error => {
            if (revision === this.revision) this.error = String(error);
        }).finally(() => {
            if (revision === this.revision && !this.timer) this.saving = false;
        });
        return this.writes;
    }
}

export const reviewDrafts = new ReviewDraftStore();
