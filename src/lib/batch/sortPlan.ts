import type { BatchItem } from '../types';
import { isProviderUri } from '../mobileProvider';

export type SortMode = 'move' | 'copy' | 'script_move' | 'script_copy';

// Capture every accepted row, including excluded rows, so changes to the
// selection or eligibility invalidate the review instead of silently changing it.
export type SortPlanItem = Pick<BatchItem, 'id' | 'originalPath' | 'originalName' | 'extension' | 'status' | 'targetPath' | 'isDuplicatePrimary' | 'suggestedTitle' | 'suggestedAuthor' | 'suggestedYear' | 'extractedText'>;
export interface SortPlan {
    mode: SortMode;
    skipNonPrimaryDupes: boolean;
    saveTxt: boolean;
    candidates: SortPlanItem[];
    items: SortPlanItem[];
}

const SORTABLE = new Set(['review', 'error', 'unfinished', 'queued']);

function candidate(item: BatchItem): SortPlanItem {
    const { id, originalPath, originalName, extension, status, targetPath, isDuplicatePrimary, suggestedTitle, suggestedAuthor, suggestedYear, extractedText } = item;
    return { id, originalPath, originalName, extension, status, targetPath, isDuplicatePrimary, suggestedTitle, suggestedAuthor, suggestedYear, extractedText };
}

export function createSortPlan(items: BatchItem[], mode: SortMode, skipNonPrimaryDupes: boolean, saveTxt: boolean): SortPlan {
    const candidates = items.filter(item => item.isAccepted).map(candidate);
    return {
        mode, skipNonPrimaryDupes, saveTxt, candidates,
        items: candidates.filter(item => !isProviderUri(item.originalPath) && item.targetPath && SORTABLE.has(item.status)
            && !(skipNonPrimaryDupes && item.isDuplicatePrimary === false)),
    };
}

export function sortPlanMatches(plan: SortPlan, items: BatchItem[], saveTxt: boolean): boolean {
    const current = createSortPlan(items, plan.mode, plan.skipNonPrimaryDupes, saveTxt);
    return plan.saveTxt === saveTxt && JSON.stringify(plan.candidates) === JSON.stringify(current.candidates)
        && JSON.stringify(plan.items) === JSON.stringify(current.items);
}

/** Destination of the optional extracted-text sidecar, matching Rust's with_extension. */
export function textSidecarPath(item: SortPlanItem): string | undefined {
    if (item.extractedText == null || !item.targetPath) return undefined;
    const sourceName = item.originalPath.split(/[\\/]/).pop() ?? '';
    const sourceDot = sourceName.lastIndexOf('.');
    if (sourceDot > 0 && sourceName.slice(sourceDot + 1).toLowerCase() === 'txt') return undefined;
    const target = item.targetPath;
    const separator = Math.max(target.lastIndexOf('/'), target.lastIndexOf('\\'));
    const dot = target.lastIndexOf('.');
    return (dot > separator + 1 ? target.slice(0, dot) : target) + '.txt';
}
