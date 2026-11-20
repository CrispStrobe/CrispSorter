import { applyPageImage, updatePage, type OcrPage, type OcrProject, type ImportedPage } from './model';

/** Model-free worksheet recipe. Keep one-pixel rules, handwriting and grids. */
export function blackAndWhiteConfig(): Record<string, unknown> {
    return {
        enabled: true, router: false, denoise: false, dewarp: false, restore: false, sr: false,
        cleanup_enabled: true,
        stages: [{ cleanup: {
            enabled: true, deskew: true, photo_geometry: true, crop_borders: false, whiten_background: false,
            binarize: true, binarize_method: 1, sauvola_k: 0.2, sauvola_window: 25,
            denoise: false, despeckle: false, blackfilter: false, auto_crop: false, page_split: false,
        } }],
    };
}

export function preparationPages(project: OcrProject, selected: readonly string[], current?: string): OcrPage[] {
    const ids = new Set(selected.length ? selected : current ? [current] : []);
    return project.pages.filter(page => ids.has(page.id) && !page.excluded);
}

/** No-op cleanup must retain reviewed text/approval; changed pixels invalidate it. */
export function applyPreparation(project: OcrProject, source: OcrPage, result: ImportedPage): OcrProject {
    const current = project.pages.find(page => page.id === source.id);
    if (!current || current.imagePath !== source.imagePath || current.contentRevision !== source.contentRevision)
        throw new Error('The page changed during preparation. Its current image and review were retained.');
    const pixelsChanged = result.image_path !== source.imagePath || result.width !== source.width || result.height !== source.height;
    const warnings = (result.warnings ?? []).filter((value): value is string => typeof value === 'string');
    const next = pixelsChanged ? applyPageImage(project, source.id, result, 'black and white preparation') : project;
    const page = next.pages.find(page => page.id === source.id)!;
    return updatePage(next, page.id, { provenance: {
        ...page.provenance, preparationWarnings: warnings,
        lastPreparation: { sourceImagePath: source.imagePath, resultImagePath: result.image_path,
            pixelsChanged, warnings, completedAt: new Date().toISOString() },
    } });
}
