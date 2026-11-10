import type { BatchItem } from '../types';

/** A stopped extraction is resumable without resetting completed review work. */
export function canProcessBatchItem(item: Pick<BatchItem, 'status'>): boolean {
    return item.status === 'queued' || item.status === 'error' || item.status === 'unfinished';
}

/** The destination copy exists, but removing the original still needs attention. */
export function isCopyFallback(item: Pick<BatchItem, 'status' | 'errorMessage'>): boolean {
    return item.status === 'done' && item.errorMessage === 'COPY_FALLBACK';
}
