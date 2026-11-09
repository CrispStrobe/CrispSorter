import { applyRecognition, updatePage, type OcrPage, type OcrProject, type OcrRegion, type RecognitionResult } from './model';
import { effectiveRecognitionZone, expandRecognitionZone, mergeZoneRegions, regionsInRecognitionZone, zoneContains, type PixelRectangle } from './zones';
import { collectSourceComponents, withSourceComponents, validatePageSourceComponents } from './passageData';
import { joinRegionText } from './text';
import { reconcilePageStructure } from './structureData';

export interface ZoneReviewRequest {
    projectId: string;
    /** Immutable source snapshot: navigation never changes the target. */
    page: OcrPage;
    zone: PixelRectangle;
    affectedRegionIds: string[];
    previousText: string;
}

export interface ZoneReviewPreview {
    request: ZoneReviewRequest;
    regions: OcrRegion[];
    provenance: Record<string, unknown>;
    replacementText: string;
    warnings: string[];
}

const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
const preserved = 'Existing text was preserved.';
const warningStrings = (value: unknown): string[] => Array.isArray(value)
    ? value.filter((warning): warning is string => typeof warning === 'string') : [];

/** Expand before running OCR so replacement never cuts an existing passage. */
export function captureZoneReview(project: OcrProject, pageId: string, rectangle: PixelRectangle): ZoneReviewRequest {
    const page = project.pages.find(candidate => candidate.id === pageId);
    if (!page) throw new Error('The source page is no longer in this project.');
    if (page.excluded) throw new Error('Include this page before recognizing an area.');
    if (page.status === 'running') throw new Error('Wait for the current recognition to finish.');
    const zone = expandRecognitionZone(page, rectangle);
    const affected = regionsInRecognitionZone(page, zone);
    return {
        projectId: project.id, page: clone(page), zone,
        affectedRegionIds: affected.map(region => region.id),
        previousText: joinRegionText(affected),
    };
}

function currentPage(project: OcrProject, request: ZoneReviewRequest): OcrPage {
    const page = project.pages.find(candidate => candidate.id === request.page.id);
    if (project.id !== request.projectId || !page || JSON.stringify(page) !== JSON.stringify(request.page)) {
        throw new Error(`The source page changed while this area was being recognized. ${preserved} Run partial OCR again.`);
    }
    return page;
}

function validateReplacement(request: ZoneReviewRequest, regions: OcrRegion[]) {
    if (!regions.some(region => !region.excluded && region.kind !== 'picture' && region.text.trim())) {
        throw new Error(`No text was found in this area. ${preserved} Try another area or recognition setting.`);
    }
    if (regions.some(region => !zoneContains(request.zone, region))) {
        throw new Error(`Recognition returned text outside the selected area. ${preserved}`);
    }
}

/** Validate a candidate for inspection without modifying the saved project. */
export function prepareZoneReview(project: OcrProject, request: ZoneReviewRequest, result: RecognitionResult, profileId = project.profileId): ZoneReviewPreview {
    const page = currentPage(project, request);
    if (result.width !== page.width || result.height !== page.height ||
        (result.image_path !== undefined && result.image_path !== page.imagePath)) {
        throw new Error(`Recognition changed the page image or dimensions. ${preserved}`);
    }
    const effective = effectiveRecognitionZone(page, request.zone,
        (result.diagnostics as { effective_zone?: unknown } | undefined)?.effective_zone);
    if (effective.x !== request.zone.x || effective.y !== request.zone.y ||
        effective.w !== request.zone.w || effective.h !== request.zone.h) {
        throw new Error(`Recognition returned a different crop than requested. ${preserved}`);
    }
    // Reuse the normal geometry/evidence boundary, but keep its result local
    // until the user chooses the concrete replacement shown in the preview.
    const recognized = applyRecognition(project, page.id, result, profileId, {preserveSourceHistory:false}).pages.find(candidate => candidate.id === page.id)!;
    const previousSources = collectSourceComponents(page,page.regions.filter(region=>request.affectedRegionIds.includes(region.id) && region.sourceComponents?.length));
    if (previousSources.length) recognized.regions=recognized.regions.map(region=>withSourceComponents(page,region,previousSources));
    // The boundary before the covered text belongs to the surrounding document,
    // not to the recognizer. In particular, re-OCR of one split half must not
    // introduce a newline that the split explicitly omitted.
    const boundary = page.regions.find(region=>request.affectedRegionIds.includes(region.id))?.separatorBefore;
    if (boundary !== undefined && recognized.regions.length) recognized.regions[0]={...recognized.regions[0],separatorBefore:boundary};
    const candidatePage={...page,regions:mergeZoneRegions(page,request.zone,recognized.regions)};
    validatePageSourceComponents(candidatePage);
    validateReplacement(request, recognized.regions);
    return {
        request, regions: recognized.regions, provenance: recognized.provenance,
        replacementText: joinRegionText(recognized.regions.filter(region => !region.excluded && region.kind !== 'picture')),
        warnings: [...warningStrings(result.warnings),...reconcilePageStructure(page,candidatePage).notices],
    };
}

/** One atomic, undoable edit; unrelated regions retain all their evidence. */
export function applyZoneReview(project: OcrProject, preview: ZoneReviewPreview): OcrProject {
    const page = currentPage(project, preview.request);
    validateReplacement(preview.request, preview.regions);
    validatePageSourceComponents({...page,regions:mergeZoneRegions(page,preview.request.zone,preview.regions)});
    const { extraction: _staleExtraction, ...provenance } = page.provenance;
    return updatePage(project, page.id, {
        regions: mergeZoneRegions(page, preview.request.zone, preview.regions),
        status: 'recognized', approved: false, error: null,
        provenance: {
            ...provenance,
            warnings: [...new Set([...warningStrings(provenance.warnings), ...preview.warnings])],
            lastZoneRecognition: clone(preview.provenance),
        },
    });
}
