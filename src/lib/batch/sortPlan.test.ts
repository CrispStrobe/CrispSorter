import { describe, expect, it } from 'vitest';
import type { BatchItem } from '../types';
import { createSortPlan, sortPlanMatches, textSidecarPath } from './sortPlan';

function item(id: string, overrides: Partial<BatchItem> = {}): BatchItem {
    return { id, originalPath: `/in/${id}.pdf`, originalName: `${id}.pdf`, extension: 'pdf', size: 10, modifiedAt: 0,
        status: 'review', isAccepted: true, targetPath: `/out/${id}.pdf`, ...overrides };
}

describe('sort preview snapshot', () => {
    it('lists exactly the eligible accepted files, respecting duplicate and status filters', () => {
        const items = [item('one'), item('dupe', { isDuplicatePrimary: false }), item('busy', { status: 'analyzing' }), item('missing', { targetPath: undefined }), item('unchecked', { isAccepted: false })];
        const plan = createSortPlan(items, 'copy', true, true);
        expect(plan.items.map(i => [i.originalPath, i.targetPath])).toEqual([['/in/one.pdf', '/out/one.pdf']]);
        expect(plan.candidates).toHaveLength(4);
        expect(sortPlanMatches(plan, items, true)).toBe(true);
        expect(createSortPlan(items, 'copy', false, true).items).toHaveLength(2);
    });

    it('rejects changed selection, paths, metadata, status, duplicate eligibility, text and settings', () => {
        const original = [item('one', { extractedText: 'text' }), item('two', { isAccepted: false })];
        const plan = createSortPlan(original, 'move', true, true);
        for (const change of [
            { originalPath: '/different.pdf' }, { targetPath: '/elsewhere.pdf' },
            { suggestedTitle: 'changed' }, { status: 'done' as const },
            { isDuplicatePrimary: false }, { extractedText: 'changed' }, { isAccepted: false },
        ]) {
            expect(sortPlanMatches(plan, [{ ...original[0], ...change }, original[1]], true)).toBe(false);
        }
        expect(sortPlanMatches(plan, [original[0], { ...original[1], isAccepted: true }], true)).toBe(false);
        expect(sortPlanMatches(plan, [], true)).toBe(false);
        expect(sortPlanMatches(plan, original, false)).toBe(false);
    });

    it('shows the exact optional text sidecar destination', () => {
        const pdf = createSortPlan([item('one', { extractedText: '' })], 'move', false, true).items[0];
        expect(textSidecarPath(pdf)).toBe('/out/one.txt');
        expect(textSidecarPath({ ...pdf, originalPath: '/in/file.TXT' })).toBeUndefined();
        expect(textSidecarPath({ ...pdf, targetPath: '/out/.hidden' })).toBe('/out/.hidden.txt');
        expect(textSidecarPath({ ...pdf, extractedText: undefined })).toBeUndefined();
    });
});
