import { describe, expect, it } from 'vitest';
import {
    addDocument, applyRecognition, approvePage, commitHistory, createHistory, createProject,
    editRegion, parseProject, prepareExport, serializeProject, undoHistory, updatePage,
    type OcrProject, type RecognitionResult,
} from './model';
import { applyZoneReview, captureZoneReview, prepareZoneReview, type ZoneReviewRequest } from './zoneReview';

function fixture(): OcrProject {
    let project = addDocument(createProject('Passage review'), {
        source_uri: '/source.pdf', name: 'Source',
        pages: [1, 2].map(number => ({ image_path: `/page-${number}.png`, width: 1000, height: 1000 })),
    });
    for (const page of project.pages) {
        project = applyRecognition(project, page.id, {
            image_path: page.imagePath, width: 1000, height: 1000,
            regions: [
                { text: 'Heading', x: 100, y: 50, w: 300, h: 40, confidence: 0.98, confidence_basis: 'recognition' },
                { text: 'Original passage.\nSecond line.', x: 100, y: 150, w: 400, h: 100, confidence: 0.9, confidence_basis: 'recognition' },
                { text: 'Unaffected passage', x: 100, y: 300, w: 400, h: 100, confidence: 0.96, confidence_basis: 'recognition' },
            ],
        });
    }
    return project;
}

function capture(project: OcrProject): ZoneReviewRequest {
    return captureZoneReview(project, project.pages[0].id, { x: 110, y: 180, w: 20, h: 20 });
}

function result(request: ZoneReviewRequest, patch: Partial<RecognitionResult> = {}): RecognitionResult {
    return {
        image_path: request.page.imagePath, width: request.page.width, height: request.page.height,
        elapsed_ms: 30, engine: 'tesseract', diagnostics: { effective_zone: { ...request.zone } },
        regions: [{ text: 'Recognized passage.\nSecond paragraph.', ...request.zone, confidence: 0.97, confidence_basis: 'recognition' }],
        ...patch,
    };
}

describe('reviewable partial OCR', () => {
    it('captures the complete affected passage and its immutable source snapshot', () => {
        const project = fixture(), request = capture(project);
        expect(request.zone).toEqual({ x: 100, y: 150, w: 400, h: 100 });
        expect(request.affectedRegionIds).toEqual([project.pages[0].regions[1].id]);
        expect(request.previousText).toBe('Original passage.\nSecond line.');
        project.pages[0].regions[1].text = 'Mutated after capture';
        expect(request.page.regions[1].text).toBe('Original passage.\nSecond line.');
    });

    it('previews corrected and approved text without replacing or unsaving anything', () => {
        let project = fixture();
        const page = project.pages[0];
        project = editRegion(project, page.id, page.regions[1].id, 'Reviewed passage.\n\nA complete second paragraph.');
        project = approvePage(project, page.id);
        project = updatePage(project, page.id, { provenance: { ...project.pages[0].provenance, extraction: { fields: [{ value: 'Reviewed' }] } } });
        const before = JSON.stringify(project), request = capture(project);
        const preview = prepareZoneReview(project, request, result(request), 'accurate');
        expect(preview.request.previousText).toBe('Reviewed passage.\n\nA complete second paragraph.');
        expect(preview.replacementText).toBe('Recognized passage.\nSecond paragraph.');
        expect(preview.provenance.profileId).toBe('accurate');
        expect(JSON.stringify(project)).toBe(before);
        expect(project.pages[0].approved).toBe(true);
        expect(project.pages[0].provenance.extraction).toBeDefined();
    });

    it('applies one undoable replacement while preserving unrelated IDs, evidence, order and page geometry', () => {
        const project = fixture(), page = project.pages[0], request = capture(project);
        const preview = prepareZoneReview(project, request, result(request));
        const updated = applyZoneReview(project, preview);
        expect(updated.pages[0].regions.map(region => region.text)).toEqual(['Heading', preview.replacementText, 'Unaffected passage']);
        expect(updated.pages[0].regions[0]).toBe(page.regions[0]);
        expect(updated.pages[0].regions[2]).toBe(page.regions[2]);
        expect(updated.pages[1]).toBe(project.pages[1]);
        expect(updated.pages[0].regions[1].id).not.toBe(page.regions[1].id);
        expect(updated.pages[0].regions[1].originalText).toBe(preview.replacementText);
        expect(updated.pages[0].approved).toBe(false);
        expect([updated.pages[0].imagePath, updated.pages[0].width, updated.pages[0].height, updated.pages[0].widthPoints, updated.pages[0].heightPoints])
            .toEqual([page.imagePath, page.width, page.height, page.widthPoints, page.heightPoints]);
        const history = commitHistory(createHistory(project), updated);
        expect(undoHistory(history).present).toEqual(project);
        const restored = parseProject(serializeProject(updated));
        expect(prepareExport(restored).pages[0].regions[1].text).toBe(preview.replacementText);
    });

    it('invalidates extraction only after accepting a valid replacement and retains warnings', () => {
        let project = fixture();
        const pageId = project.pages[0].id;
        project = updatePage(project, pageId, { provenance: { warnings: ['Old warning'], extraction: { formula: 'x' } } });
        const request = capture(project);
        const preview = prepareZoneReview(project, request, result(request, { warnings: ['New warning', 'Old warning'] }));
        expect(project.pages[0].provenance.extraction).toEqual({ formula: 'x' });
        const updated = applyZoneReview(project, preview);
        expect(updated.pages[0].provenance.extraction).toBeUndefined();
        expect(updated.pages[0].provenance.warnings).toEqual(['Old warning', 'New warning']);
        expect(updated.pages[0].provenance.lastZoneRecognition).toEqual(preview.provenance);
    });

    it('keeps existing text when the recognizer returns no nonempty text', () => {
        const project = fixture(), request = capture(project), before = JSON.stringify(project);
        for (const regions of [[], [{ ...result(request).regions[0], text: ' \n\t' }]]) {
            expect(() => prepareZoneReview(project, request, result(request, { regions }))).toThrow(/No text was found.*Existing text was preserved/);
        }
        expect(JSON.stringify(project)).toBe(before);
    });

    it('rejects results from a changed image, different page dimensions or a different crop', () => {
        const project = fixture(), request = capture(project), before = JSON.stringify(project);
        for (const patch of [{ image_path: '/different.png' }, { width: 999 }, { height: 999 }]) {
            expect(() => prepareZoneReview(project, request, result(request, patch))).toThrow(/page image or dimensions/);
        }
        for (const crop of [
            { ...request.zone, w: request.zone.w - 1 },
            { ...request.zone, x: request.zone.x + 1 },
        ]) {
            expect(() => prepareZoneReview(project, request, result(request, { diagnostics: { effective_zone: crop } }))).toThrow(/different crop/);
        }
        expect(JSON.stringify(project)).toBe(before);
    });

    it('rejects boxes outside the crop even when they are valid inside the whole page', () => {
        const project = fixture(), request = capture(project);
        const outside = { ...result(request).regions[0], x: 600, y: 150, w: 50, h: 50 };
        expect(() => prepareZoneReview(project, request, result(request, { regions: [outside] }))).toThrow(/outside the selected area/);
        expect(project.pages[0].regions[1].text).toBe(request.previousText);
    });

    it('rechecks the captured source when a result arrives and again when the preview is accepted', () => {
        const project = fixture(), request = capture(project), preview = prepareZoneReview(project, request, result(request));
        const changed = editRegion(project, request.page.id, request.affectedRegionIds[0], 'A newer manual correction');
        expect(() => prepareZoneReview(changed, request, result(request))).toThrow(/source page changed/);
        expect(() => applyZoneReview(changed, preview)).toThrow(/source page changed/);
        expect(() => applyZoneReview(approvePage(project, request.page.id), preview)).toThrow(/source page changed/);
        expect(() => applyZoneReview({ ...project, id: 'another-project' }, preview)).toThrow(/source page changed/);
        expect(changed.pages[0].regions[1].text).toBe('A newer manual correction');
        expect(() => applyZoneReview(applyZoneReview(project, preview), preview)).toThrow(/source page changed/);
    });

    it('allows unrelated page edits without changing the captured replacement target', () => {
        const project = fixture(), request = capture(project), preview = prepareZoneReview(project, request, result(request));
        const second = project.pages[1];
        const changed = editRegion(project, second.id, second.regions[1].id, 'Other page correction');
        const updated = applyZoneReview(changed, preview);
        expect(updated.pages[0].regions[1].text).toBe(preview.replacementText);
        expect(updated.pages[1]).toBe(changed.pages[1]);
        expect(updated.pages[1].regions[1].text).toBe('Other page correction');
    });

    it('adds a newly recognized area without removing existing passages', () => {
        const project = fixture(), page = project.pages[0];
        const request = captureZoneReview(project, page.id, { x: 100, y: 260, w: 400, h: 30 });
        expect(request.affectedRegionIds).toEqual([]);
        const updated = applyZoneReview(project, prepareZoneReview(project, request, result(request)));
        expect(updated.pages[0].regions.map(region => region.text)).toEqual([
            'Heading', 'Original passage.\nSecond line.', 'Recognized passage.\nSecond paragraph.', 'Unaffected passage',
        ]);
    });

    it('does not recognize excluded or already-running pages', () => {
        const project = fixture(), page = project.pages[0], box = { x: 100, y: 150, w: 400, h: 100 };
        expect(() => captureZoneReview(updatePage(project, page.id, { excluded: true }), page.id, box)).toThrow(/Include this page/);
        expect(() => captureZoneReview(updatePage(project, page.id, { status: 'running' }), page.id, box)).toThrow(/current recognition/);
    });
});
