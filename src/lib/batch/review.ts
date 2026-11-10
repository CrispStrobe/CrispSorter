import type { BatchItem } from '../types';

/** Bulk operations always use the visible intersection, never hidden selections. */
export function reviewSelection(selectedIds: readonly string[], visible: readonly Pick<BatchItem, 'id'>[], all: readonly Pick<BatchItem, 'id'>[]) {
    const existing = new Set(all.map(item => item.id));
    const selected = new Set(selectedIds.filter(id => existing.has(id)));
    const visibleSet = new Set(visible.map(item => item.id));
    const visibleIds = visible.filter(item => selected.has(item.id)).map(item => item.id);
    const hiddenIds = [...selected].filter(id => !visibleSet.has(id));
    return {
        visibleIds, hiddenIds,
        allVisible: visible.length > 0 && visibleIds.length === visible.length,
        someVisible: visibleIds.length > 0 && visibleIds.length < visible.length,
    };
}

export function toggleVisibleSelection(selectedIds: readonly string[], visible: readonly Pick<BatchItem, 'id'>[]) {
    const selected = new Set(selectedIds);
    const remove = visible.length > 0 && visible.every(item => selected.has(item.id));
    for (const item of visible) {
        if (remove) selected.delete(item.id);
        else selected.add(item.id);
    }
    return [...selected];
}

export interface ReviewMetadata { title: string; author: string; year: string }
export interface ReviewDraft extends ReviewMetadata {
    originalPath: string;
    base: ReviewMetadata;
}
export type ReviewDrafts = Record<string, ReviewDraft>;

export function itemMetadata(item: Pick<BatchItem, 'suggestedTitle' | 'suggestedAuthor' | 'suggestedYear'>): ReviewMetadata {
    return { title: item.suggestedTitle ?? '', author: item.suggestedAuthor ?? '', year: item.suggestedYear ?? '' };
}

export function sameMetadata(a: ReviewMetadata, b: ReviewMetadata) {
    return a.title === b.title && a.author === b.author && a.year === b.year;
}

/** Treat saved settings as untrusted; never attach a draft to a different source. */
export function restoreReviewDrafts(value: unknown): ReviewDrafts {
    const result: ReviewDrafts = {};
    if (!value || typeof value !== 'object' || Array.isArray(value)) return result;
    const metadata = (entry: any): entry is ReviewMetadata => !!entry &&
        ['title', 'author', 'year'].every(key => typeof entry[key] === 'string');
    for (const [id, entry] of Object.entries(value)) {
        if (['__proto__', 'constructor', 'prototype'].includes(id)) continue;
        const draft = entry as ReviewDraft;
        if (metadata(draft) && typeof draft.originalPath === 'string' && metadata(draft.base)) {
            result[id] = { originalPath: draft.originalPath, title: draft.title, author: draft.author, year: draft.year,
                base: { title: draft.base.title, author: draft.base.author, year: draft.base.year } };
        }
    }
    return result;
}
