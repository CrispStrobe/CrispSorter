import type { DuplicateDecision } from './panels';

export type DuplicateDecisionAudit = {
    groupId: string;
    previous: DuplicateDecision;
    next: DuplicateDecision;
    at: number;
};

export type DuplicateMutationAudit = {
    groupId: string;
    driveId: string;
    operation: 'move' | 'delete' | 'restore';
    from: string;
    to: string | null;
    at: number;
};

const STORAGE_KEY = 'crispsorter.duplicate-decision-audit.v1';
const MUTATION_STORAGE_KEY = 'crispsorter.duplicate-mutation-audit.v1';
const decisions = new Set<DuplicateDecision>([
    'review', 'keep_source', 'keep_destination', 'keep_both',
]);

export function decodeDuplicateAudit(raw: string | null): DuplicateDecisionAudit[] {
    if (!raw) return [];
    try {
        const parsed: unknown = JSON.parse(raw);
        if (!Array.isArray(parsed)) return [];
        return parsed.filter((entry): entry is DuplicateDecisionAudit => {
            if (!entry || typeof entry !== 'object') return false;
            const value = entry as Record<string, unknown>;
            return typeof value.groupId === 'string'
                && decisions.has(value.previous as DuplicateDecision)
                && decisions.has(value.next as DuplicateDecision)
                && typeof value.at === 'number' && Number.isFinite(value.at);
        }).slice(-200);
    } catch {
        return [];
    }
}

export function encodeDuplicateAudit(entries: DuplicateDecisionAudit[]): string {
    return JSON.stringify(entries.slice(-200));
}

export function latestDuplicateDecision(
    entries: DuplicateDecisionAudit[],
    groupId: string,
): DuplicateDecision | null {
    for (let index = entries.length - 1; index >= 0; index -= 1) {
        if (entries[index].groupId === groupId) return entries[index].next;
    }
    return null;
}

export function loadDuplicateAudit(): DuplicateDecisionAudit[] {
    try { return decodeDuplicateAudit(localStorage.getItem(STORAGE_KEY)); }
    catch { return []; }
}

export function saveDuplicateAudit(entries: DuplicateDecisionAudit[]): void {
    try { localStorage.setItem(STORAGE_KEY, encodeDuplicateAudit(entries)); }
    catch { /* private browsing or non-browser preview */ }
}

export function decodeDuplicateMutationAudit(raw: string | null): DuplicateMutationAudit[] {
    if (!raw) return [];
    try {
        const parsed: unknown = JSON.parse(raw);
        if (!Array.isArray(parsed)) return [];
        return parsed.filter((entry): entry is DuplicateMutationAudit => {
            if (!entry || typeof entry !== 'object') return false;
            const value = entry as Record<string, unknown>;
            return typeof value.groupId === 'string'
                && typeof value.driveId === 'string'
                && (value.operation === 'move' || value.operation === 'delete' || value.operation === 'restore')
                && typeof value.from === 'string'
                && (value.to === null || typeof value.to === 'string')
                && typeof value.at === 'number' && Number.isFinite(value.at);
        }).slice(-200);
    } catch {
        return [];
    }
}

export function loadDuplicateMutationAudit(): DuplicateMutationAudit[] {
    try { return decodeDuplicateMutationAudit(localStorage.getItem(MUTATION_STORAGE_KEY)); }
    catch { return []; }
}

export function saveDuplicateMutationAudit(entries: DuplicateMutationAudit[]): void {
    try { localStorage.setItem(MUTATION_STORAGE_KEY, JSON.stringify(entries.slice(-200))); }
    catch { /* private browsing or non-browser preview */ }
}
