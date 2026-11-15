import { describe, expect, it } from 'vitest';
import { addDocument, applyRecognition, approvePage, createProject, prepareExport, updatePage } from './model';
import { resolveExportFormat } from './export';
import { applyPreparation, blackAndWhiteConfig, preparationPages } from './preparation';

function fixture() {
    return addDocument(createProject(), { source_uri: '/original.pdf', name: 'Worksheets',
        pages: [1, 2, 3].map(i => ({ image_path: `/page-${i}.png`, width: 800, height: 1100 })) });
}

describe('worksheet preparation', () => {
    it('exports unrecognized page images explicitly without relaxing searchable/text exports', () => {
        const project = fixture();
        expect(prepareExport(project, { format: 'pdf' }).errors.length).toBeGreaterThan(0);
        const images = prepareExport(project, { format: 'image-pdf' });
        expect(images.errors).toEqual([]);
        expect(images.pages).toHaveLength(3);
        expect(images.pages.every(page => page.regions.length === 0)).toBe(true);
        expect(prepareExport(project, { format: 'image-pdf', requireReviewed: true }).errors.length).toBeGreaterThan(0);
        expect(resolveExportFormat('image-pdf', { formats: ['pdf'] })).toEqual({ format: 'pdf', pdfa: false });
        expect(() => resolveExportFormat('image-pdf', { formats: ['text'] })).toThrow('unavailable');
    });
    it('uses model-free Sauvola without filters that damage thin rules', () => {
        const cfg = blackAndWhiteConfig();
        expect(cfg).toMatchObject({ denoise: false, dewarp: false, restore: false, sr: false });
        expect(cfg.stages).toEqual([{ cleanup: expect.objectContaining({ deskew: true, binarize: true,
            binarize_method: 1, whiten_background: false, despeckle: false, blackfilter: false }) }]);
        (cfg.stages as { cleanup: { binarize: boolean } }[])[0].cleanup.binarize = false;
        expect((blackAndWhiteConfig().stages as { cleanup: { binarize: boolean } }[])[0].cleanup.binarize).toBe(true);
    });
    it('freezes selected pages in document order, excludes omitted pages and falls back to current', () => {
        let project = fixture();
        const [a, b, c] = project.pages;
        project = updatePage(project, b.id, { excluded: true });
        expect(preparationPages(project, [c.id, b.id, a.id], b.id).map(p => p.id)).toEqual([a.id, c.id]);
        expect(preparationPages(project, [], c.id).map(p => p.id)).toEqual([c.id]);
        expect(preparationPages(project, [b.id], a.id)).toEqual([]);
    });
    it('retains approved recognition when pixels are unchanged', () => {
        let project = fixture();
        const id = project.pages[0].id;
        project = applyRecognition(project, id, { width: 800, height: 1100,
            regions: [{ text: 'Reviewed answer', x: 20, y: 20, w: 200, h: 30 }] });
        project = approvePage(project, id);
        const page = project.pages[0];
        const next = applyPreparation(project, page, { image_path: page.imagePath, width: 800, height: 1100 });
        expect(next.pages[0].approved).toBe(true);
        expect(next.pages[0].regions).toEqual(page.regions);
    });
    it('invalidates changed pixels and rejects stale results without altering other pages', () => {
        const project = fixture(), source = project.pages[0];
        const result = { image_path: '/new.png', width: 820, height: 1120 };
        const next = applyPreparation(project, source, result);
        expect(next.pages[0].imagePath).toBe('/new.png');
        expect(next.pages[0].approved).toBe(false);
        expect(next.pages.slice(1)).toEqual(project.pages.slice(1));
        expect(() => applyPreparation(next, source, result)).toThrow('changed during preparation');
    });
});
