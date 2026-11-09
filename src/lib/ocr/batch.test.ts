import { describe, expect, it } from 'vitest';
import {
    batchPageIsCurrent, beginBatch, restoreBatch, resumeBatch, updateBatchPage, updateBatchPhase,
    type BatchOptions,
} from './batch';
import {
    addDocument, applyPageImage, applyRecognition, approvePage, createProject, editRegion,
    parseProject, removePage, replaceMatches, updatePage,
} from './model';

function fixture() {
    return addDocument(createProject('Capture batch'), {
        source_uri: '/source/phone-scan.pdf', name: 'Phone scan',
        pages: Array.from({ length: 4 }, (_, index) => ({ image_path: `/assets/page-${index}.png`, width: 1000, height: 1400 })),
    });
}
function options(): BatchOptions {
    return { profileId: 'accurate', config: { layout: true, cleanup: { deskew: true } }, engine: 'tesseract', language: 'deu' };
}

describe('durable recognition batch intent', () => {
    it('freezes selected page order and deep recognition settings independently of later toolbar edits', () => {
        const p = fixture(), settings = options(), ids = [p.pages[2].id, p.pages[0].id];
        const saved = beginBatch(p, ids, settings);
        (settings.config.cleanup as { deskew: boolean }).deskew = false;
        ids.reverse();
        saved.recognitionSettings = { engine: 'different', language: 'jpn' };
        const plan = resumeBatch(saved);
        expect(plan.pageIds).toEqual([p.pages[2].id, p.pages[0].id]);
        expect(plan.options).toEqual(options());
        expect(plan.options).not.toBe(saved.batch);
        expect(p.batch).toBeUndefined();
    });

    it('recovers an interrupted native invocation without repeating saved successful pages', () => {
        let p = fixture();
        const ids = p.pages.slice(0, 3).map(page => page.id);
        p = beginBatch(p, ids, options());
        p = updateBatchPage(applyRecognition(p, ids[0], { width: 1000, height: 1400, regions: [] }), ids[0], 'completed');
        p = updateBatchPage(updatePage(p, ids[1], { status: 'running' }), ids[1], 'running');
        const restored = parseProject(JSON.stringify(p));
        expect(restored.batch?.phase).toBe('interrupted');
        expect(restored.pages[1].status).toBe('pending');
        expect(restored.batch?.jobs.map(job => job.state)).toEqual(['completed', 'pending', 'pending']);
        const resume = resumeBatch(restored);
        expect(resume.pageIds).toEqual(ids.slice(1));
        expect(resume.completed).toBe(1);
        expect(resume.project.batch?.phase).toBe('running');
        expect(resume.conflicts).toEqual([]);
    });

    it('stopped and paused checkpoints retain the remaining selection and require explicit failed-page retry', () => {
        let p = fixture();
        const ids = p.pages.slice(0, 3).map(page => page.id);
        p = beginBatch(p, ids, options());
        p = updateBatchPage(updatePage(p, ids[0], { status: 'failed', error: 'Model missing' }), ids[0], 'failed', 'Model missing');
        p = updateBatchPage(p, ids[1], 'completed');
        for (const phase of ['stopped', 'paused'] as const) {
            const restored = parseProject(updateBatchPhase(p, phase));
            const pending = resumeBatch(restored);
            expect(pending.pageIds).toEqual([ids[2]]);
            expect(pending.failed).toBe(1);
            const retried = resumeBatch(restored, { retryFailed: true });
            expect(retried.pageIds).toEqual([ids[0], ids[2]]);
            expect(retried.project.batch?.jobs.map(job => job.state)).toEqual(['pending', 'completed', 'pending']);
            expect(restored.batch?.jobs[0].error).toBe('Model missing');
        }
    });

    it('failure captures changed review state so an authorized retry is possible', () => {
        let p = fixture();
        const id = p.pages[0].id;
        p = approvePage(applyRecognition(p, id, { width: 1000, height: 1400, regions: [] }), id);
        p = beginBatch(p, [id], options());
        p = updateBatchPage(updatePage(p, id, { status: 'failed', approved: false }), id, 'failed', 'Unavailable model');
        expect(batchPageIsCurrent(p, id)).toBe(true);
        expect(resumeBatch(p, { retryFailed: true }).pageIds).toEqual([id]);
    });

    it('does not silently rerun corrected, transformed, removed, or excluded pending pages', () => {
        let p = fixture();
        const id = p.pages[0].id;
        p = applyRecognition(p, id, { width: 1000, height: 1400, regions: [{ text: 'Invoice 101', x: 1, y: 2, w: 200, h: 40 }] });
        p = beginBatch(p, [id, p.pages[1].id], options());
        const corrected = editRegion(p, id, p.pages[0].regions[0].id, 'Invoice 107');
        const replaced = replaceMatches(p, '101', '107');
        const cropped = applyPageImage(p, id, { image_path: '/crop.png', width: 500, height: 700 }, 'crop');
        const removed = removePage(p, id);
        const excluded = updatePage(p, id, { excluded: true });
        for (const changed of [corrected, replaced, cropped, removed, excluded]) {
            const resume = resumeBatch(changed);
            expect(resume.pageIds).toEqual([]);
            expect(resume.conflicts).toHaveLength(1);
            expect(resume.project).toBe(changed);
        }
        expect(resumeBatch(p).pageIds).toHaveLength(2);
    });

    it('progress-only updates keep evidence revisions and allow in-flight result checks', () => {
        let p = fixture();
        const id = p.pages[0].id;
        p = beginBatch(p, [id], options());
        const revision = p.pages[0].contentRevision;
        p = updateBatchPage(updatePage(p, id, { status: 'running', error: null, provenance: { diagnostics: [] } }), id, 'running');
        expect(p.pages[0].contentRevision).toBe(revision);
        expect(batchPageIsCurrent(p, id)).toBe(true);
        p = updateBatchPage(applyRecognition(p, id, { width: 1000, height: 1400, regions: [] }), id, 'completed');
        expect(p.pages[0].contentRevision).not.toBe(revision);
        expect(batchPageIsCurrent(p, id)).toBe(true);
        expect(resumeBatch(updateBatchPhase(p, 'completed')).pageIds).toEqual([]);
    });

    it('validates malformed checkpoint boundaries instead of trusting loaded queue metadata', () => {
        const p = fixture();
        const saved = beginBatch(p, [p.pages[0].id], options());
        const batch = saved.batch!;
        for (const invalid of [null, [], { ...batch, version: 99 }, { ...batch, config: [] }, { ...batch, jobs: [batch.jobs[0], batch.jobs[0]] },
            { ...batch, jobs: [null] }, { ...batch, jobs: [{ ...batch.jobs[0], width: -1 }] }, { ...batch, jobs: [{ ...batch.jobs[0], contentRevision: '' }] }]) {
            expect(() => restoreBatch(invalid)).toThrow(/recognition batch/);
        }
        expect(() => beginBatch(p, [p.pages[0].id, p.pages[0].id], options())).toThrow(/unique/);
        expect(() => beginBatch(p, ['missing'], options())).toThrow(/missing/);
    });

    it('leaves projects without a checkpoint alone and cannot mutate settings through a resume plan', () => {
        const p = fixture();
        expect(resumeBatch(p)).toMatchObject({ project: p, pageIds: [], options: null, conflicts: [] });
        const saved = beginBatch(p, [p.pages[0].id], options());
        const resume = resumeBatch(saved);
        (resume.options!.config.cleanup as { deskew: boolean }).deskew = false;
        expect(saved.batch?.config.cleanup).toEqual({ deskew: true });
    });
});
