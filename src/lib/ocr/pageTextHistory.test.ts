import { describe, expect, it } from 'vitest';
import {
    addDocument, applyRecognition, commitHistory, createHistory, createProject,
    editRegion, parseProject, redoHistory, removePage, serializeProject, undoHistory,
    type History, type OcrProject,
} from './model';
import {
    applyPageTextEdit, createPageTextDraft, discardPageTextDraft,
    pageTextDraftSourceIsCurrent, preparePageTextEdit, updatePageTextDraft,
} from './pageTextDraft';
import { initializePageStructure } from './structure';

function fixture(): OcrProject {
    let project = addDocument(createProject('Durable page drafts'), {
        source_uri: '/source.pdf', name: 'Source',
        pages: [{ image_path: '/one.png', width: 600, height: 800 }, { image_path: '/two.png', width: 600, height: 800 }],
    });
    project = applyRecognition(project, project.pages[0].id, {
        width: 600, height: 800,
        regions: [{
            text: 'Original 😀 text.', x: 20, y: 30, w: 500, h: 50,
            confidence: .96, confidence_basis: 'recognition',
            char_conf: Array.from('Original 😀 text.').map(() => .96),
            char_conf_unit: 'unicode_codepoint', char_conf_aligned: true,
            word_evidence: {
                version: 1, offset_unit: 'utf16', source: 'system_tesseract',
                words: [{ text: 'Original', start: 0, end: 8, x: 20, y: 30, w: 100, h: 50, confidence: .96, confidence_basis: 'recognition' }],
            },
        }],
    });
    return initializePageStructure(project, project.pages[0].id);
}
function pending(project: OcrProject, text = 'Revised 😀 text.'): OcrProject {
    return updatePageTextDraft(createPageTextDraft(project, project.pages[0].id), text);
}
function appliedHistory(): History {
    const project = pending(fixture());
    const preview = preparePageTextEdit(project);
    expect(preview).not.toBeNull();
    return commitHistory(createHistory(project), applyPageTextEdit(project, preview!));
}
function sortedJson(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(sortedJson);
    if (value && typeof value === 'object') {
        return Object.fromEntries(Object.entries(value as Record<string, unknown>).sort(([a], [b]) => a.localeCompare(b)).map(([key, item]) => [key, sortedJson(item)]));
    }
    return value;
}

describe('page text drafts and project history', () => {
    it('keeps a newer pending draft through ordinary undo and redo', () => {
        const original = fixture(), page = original.pages[0];
        let history = commitHistory(createHistory(original), editRegion(original, page.id, page.regions[0].id, 'Earlier project edit.'));
        const draftProject = pending(history.present, 'Newer unapplied page text.');
        history = { ...history, present: draftProject };
        const draft = draftProject.pageTextDraft!;
        history = undoHistory(history);
        expect(history.present.pages[0].regions[0].text).toBe('Original 😀 text.');
        expect(history.present.pageTextDraft).toEqual(draft);
        expect(pageTextDraftSourceIsCurrent(history.present, draft)).toBe(false);
        expect(() => preparePageTextEdit(history.present)).toThrow();
        history = redoHistory(history);
        expect(history.present.pages[0].regions[0].text).toBe('Earlier project edit.');
        expect(history.present.pageTextDraft).toEqual(draft);
        expect(preparePageTextEdit(history.present)).not.toBeNull();
    });

    it('applies once and restores the source and exact unapplied draft with undo', () => {
        const project = pending(fixture()), original = project.pages[0].regions[0], draft = project.pageTextDraft!;
        let history = commitHistory(createHistory(project), applyPageTextEdit(project, preparePageTextEdit(project)!));
        expect(history.past).toHaveLength(1);
        expect(history.present.pageTextDraft).toBeUndefined();
        expect(history.present.pages[0].regions[0]).toMatchObject({ id: original.id, text: 'Revised 😀 text.', originalText: original.originalText });
        history = undoHistory(history);
        expect(history.present.pages[0].regions[0]).toEqual(original);
        expect(history.present.pageTextDraft).toEqual(draft);
        history = redoHistory(history);
        expect(history.present.pageTextDraft).toBeUndefined();
        expect(history.present.pages[0].regions[0].text).toBe('Revised 😀 text.');
    });

    it('does not erase a revised draft when redoing an older Apply operation', () => {
        let history = undoHistory(appliedHistory());
        history = { ...history, present: updatePageTextDraft(history.present, 'New draft after Undo.') };
        const newer = history.present.pageTextDraft!;
        history = redoHistory(history);
        expect(history.present.pages[0].regions[0].text).toBe('Revised 😀 text.');
        expect(history.present.pageTextDraft).toEqual(newer);
        expect(pageTextDraftSourceIsCurrent(history.present, newer)).toBe(false);
        expect(() => preparePageTextEdit(history.present)).toThrow();
        expect(history.present.pageTextDraft!.text).toBe('New draft after Undo.');
    });

    it('does not resurrect an explicitly discarded draft through unrelated history', () => {
        const project = fixture(), page = project.pages[0];
        let history = commitHistory(createHistory(project), editRegion(project, page.id, page.regions[0].id, 'Earlier edit.'));
        history = { ...history, present: pending(history.present) };
        history = { ...history, present: discardPageTextDraft(history.present) };
        history = undoHistory(history);
        expect(history.present.pageTextDraft).toBeUndefined();
        history = redoHistory(history);
        expect(history.present.pageTextDraft).toBeUndefined();
        expect(history.present.pages[0].regions[0].text).toBe('Earlier edit.');
    });

    it('reopens a recoverable draft even when its current source page was removed', () => {
        const project = pending(fixture()), draft = project.pageTextDraft!;
        const removed = removePage(project, draft.page.id);
        const restored = parseProject(serializeProject(removed));
        expect(restored.pages.some(page => page.id === draft.page.id)).toBe(false);
        expect(restored.pageTextDraft!.text).toBe(draft.text);
        expect(restored.pageTextDraft!.page.regions[0].originalText).toBe('Original 😀 text.');
        expect(() => preparePageTextEdit(restored)).toThrow();
        expect(restored.pageTextDraft!.text).toBe(draft.text);
    });

    it('recognizes an unchanged native JSON save regardless of object key order when redoing Apply', () => {
        let history = undoHistory(appliedHistory());
        history = { ...history, present: parseProject(sortedJson(JSON.parse(serializeProject(history.present)))) };
        expect(pageTextDraftSourceIsCurrent(history.present, history.present.pageTextDraft!)).toBe(true);
        expect(preparePageTextEdit(history.present)).not.toBeNull();
        history = redoHistory(history);
        expect(history.present.pageTextDraft).toBeUndefined();
        expect(history.present.pages[0].regions[0].text).toBe('Revised 😀 text.');
    });
});
