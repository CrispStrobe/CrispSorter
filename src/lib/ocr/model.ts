import { hasAlignedCharacterConfidence, regionReviewReason } from './confidence';
import { readWordEvidence, type WordEvidenceEnvelope } from './wordEvidence';
import { readProofreadingData, type ProofreadingData } from './proofreadingData';
import { readSourceComponents, readSharedSourceGroups, collectSourceComponents, mergeSourceComponents, withSourceComponents, validatePageSourceComponents, type SourceComponent } from './passageData';
import { joinRegionText, hasUnresolvedSharedGeometry, type PassageSeparator } from './text';
import { deriveStructure, readPageStructure, reconcilePageStructure, projectStructureForRegions, type PageStructure } from './structureData';
import { readStructureHistory, archiveReplacedStructure } from './structureHistory';
import { readPageTextDraft, pageTextDraftSourceIsCurrent, type PageTextDraft } from './pageTextDraft';
import { getExtractionObjects, readExtractionObjects, extractionObjectFreshness } from './extractionObjects';
import { MAX_DOCX_PAGE_POINTS, prepareOfficeExport, type OfficeExportOptions } from './officeExport';

/** Versioned, renderer-independent OCR project model. All geometry is image pixels. */
export const PROJECT_VERSION = 1 as const;
/** Raster sources with no physical-size metadata are interpreted at 300 DPI. */
export const DEFAULT_PAGE_DPI = 300;
export const MAX_PAGE_POINTS = 14_400;
export type RecognitionStatus = 'pending' | 'running' | 'recognized' | 'failed';
export type ReviewStatus = 'unreviewed' | 'corrected' | 'approved';
export type RegionKind = 'text' | 'title' | 'table' | 'picture' | 'formula' | 'header' | 'footer';
export type ConfidenceBasis = 'recognition' | 'detection' | 'unknown' | 'manual';
export interface OcrRegion {
    id: string; text: string; originalText: string;
    x: number; y: number; w: number; h: number;
    confidence: number | null; char_conf: number[]; confidenceBasis: ConfidenceBasis;
    charConfidenceUnit: 'unicode_codepoint' | 'token' | 'unknown'; charConfidenceAligned: boolean;
    kind: RegionKind; review: ReviewStatus; excluded: boolean;
    /** Immutable source word evidence, independently versioned for v1 projects. */
    wordEvidence?: WordEvidenceEnvelope;
    sourceComponents?: SourceComponent[];
    separatorBefore?: PassageSeparator;
    sharedSourceGroups?: string[];
}
export interface OcrPage {
    id: string; documentId: string; sourcePage: number; name: string;
    imagePath: string; originalImagePath: string; width: number; height: number;
    /** Physical paper size in 1/72-inch points, independent of raster resolution. */
    widthPoints?: number; heightPoints?: number;
    status: RecognitionStatus; regions: OcrRegion[]; excluded: boolean; approved: boolean;
    error: string | null; elapsedMs: number | null;
    provenance: Record<string, unknown>; profileId: string;
    /** Logical roles and formatting reference current regions; text stays on the region. */
    structure?: PageStructure;
}
export interface OcrDocument { id: string; sourceUri: string; name: string; }
export interface OcrProject {
    version: typeof PROJECT_VERSION; id: string; name: string; createdAt: string; updatedAt: string;
    documents: OcrDocument[]; pages: OcrPage[]; confidenceThreshold: number; profileId: string;
    recognitionSettings?: Record<string, unknown>;
    proofreading?: ProofreadingData;
    /** Unapplied page text stays recoverable independently of project undo history. */
    pageTextDraft?: PageTextDraft;
}
export interface ImportedPage { image_path: string; width: number; height: number; page_width_points?: number; page_height_points?: number; warnings?: string[]; }
export interface ImportedDocument { source_uri: string; name: string; pages: ImportedPage[]; }
export interface RecognitionRegion {
    text: string; x: number; y: number; w: number; h: number;
    confidence?: number | null; char_conf?: number[];
    confidence_basis?: string; kind?: string; char_conf_aligned?: boolean; char_conf_unit?: string;
    word_evidence?: unknown;
    [key: string]: unknown;
}
export interface RecognitionResult {
    image_path?: string; width: number; height: number; regions: RecognitionRegion[];
    page_width_points?: number; page_height_points?: number;
    elapsed_ms?: number; [key: string]: unknown;
}
export interface OcrProfile {
    id: string; name: string; description: string; config: Record<string, unknown>;
}
export interface ReviewIssue { pageId: string; regionId: string | null; reason: 'failed' | 'pending' | 'empty' | 'low-confidence' | 'unknown-confidence' | 'changed'; message: string; }
export interface TextMatch { pageId: string; regionId: string; start: number; end: number; text: string; }
export interface ExportOptions { pageIds?: string[]; requireReviewed?: boolean; includeExcluded?: boolean; format?: string; officeOptions?: OfficeExportOptions; }
export interface ExportRegion {
    id: string; text: string; original_text: string;
    x: number; y: number; w: number; h: number;
    /** Original engine score; null means unavailable, even after manual review. */
    confidence: number | null; confidence_basis: ConfidenceBasis;
    char_conf_unit: 'unicode_codepoint' | 'token' | 'unknown';
    /** Character scores are attached to current text only with proven alignment. */
    char_conf_aligned: boolean; char_conf: number[];
    original_char_conf: number[]; original_char_conf_aligned: boolean;
    word_evidence?: WordEvidenceEnvelope; original_word_evidence?: WordEvidenceEnvelope;
    source_components?: SourceComponent[]; separator_before?: PassageSeparator; shared_source_groups?: string[];
    review: ReviewStatus; kind: RegionKind;
}
export interface ExportPage {
    image_path: string; width: number; height: number; regions: ExportRegion[];
    page_width_points?: number; page_height_points?: number;
    id?: string; name?: string; source_uri?: string; source_page?: number;
    approved?: boolean; provenance?: Record<string, unknown>;
    structure?: PageStructure;
    /** Source geometry for included figures, separate from the plain-text projection. */
    figure_regions?: ExportRegion[];
}
export interface ExportPreflight { pages: ExportPage[]; errors: string[]; warnings: string[]; text: string; officeOptions?: OfficeExportOptions; }
export interface ProjectMetrics { pages: number; included: number; recognized: number; pending: number; failed: number; approved: number; regions: number; characters: number; needsReview: number; meanConfidence: number | null; }
export interface History { past: OcrProject[]; present: OcrProject; future: OcrProject[]; limit: number; maxBytes: number; }

export const PROFILES: readonly OcrProfile[] = [
    { id: 'balanced', name: 'Balanced document', description: 'Automatic routing with classical scan cleanup.', config: { enabled: true, router: true, cleanup_enabled: true } },
    { id: 'accurate', name: 'Precision review', description: 'Layout-aware reading order with a stricter acceptance gate.', config: { enabled: true, router: true, cleanup_enabled: true, layout: true, min_confidence: 0.85 } },
    { id: 'scan', name: 'Degraded scan', description: 'Denoise, dewarp, restore and upscale small scans.', config: { enabled: true, router: true, cleanup_enabled: true, denoise: true, dewarp: true, restore: true, sr: true } },
    { id: 'book', name: 'Book and columns', description: 'Layout analysis, curved-page correction and header filtering.', config: { enabled: true, router: true, cleanup_enabled: true, layout: true, dewarp: true, drop_headers_footers: true } },
    { id: 'screenshot', name: 'Screenshots', description: 'Preserve crisp screen text without geometric cleanup.', config: { enabled: true, router: false, cleanup_enabled: false } },
];

const kinds: readonly RegionKind[] = ['text', 'title', 'table', 'picture', 'formula', 'header', 'footer'];
const reviews: readonly ReviewStatus[] = ['unreviewed', 'corrected', 'approved'];
const statuses: readonly RecognitionStatus[] = ['pending', 'running', 'recognized', 'failed'];
const bases: readonly ConfidenceBasis[] = ['recognition', 'detection', 'unknown', 'manual'];
const textKinds = new Set<RegionKind>(['text', 'title', 'table', 'formula', 'header', 'footer']);
let idCounter = 0;
const id = (prefix: string) => `${prefix}-${globalThis.crypto?.randomUUID?.() ?? `${Date.now().toString(36)}-${++idCounter}-${Math.random().toString(36).slice(2)}`}`;
const now = () => new Date().toISOString();
const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const validDimension = (value: unknown): value is number => finite(value) && value > 0 && value <= 100_000;
const validPagePoints = (value: unknown): value is number => finite(value) && value > 0 && value <= MAX_PAGE_POINTS;
interface PhysicalSize { widthPoints: number; heightPoints: number; }
const rasterPhysicalSize = (width: number, height: number): PhysicalSize => ({ widthPoints: width * 72 / DEFAULT_PAGE_DPI, heightPoints: height * 72 / DEFAULT_PAGE_DPI });
const pagePhysicalSize = (page: Pick<OcrPage, 'width'|'height'|'widthPoints'|'heightPoints'>): PhysicalSize => ({
    widthPoints: page.widthPoints ?? page.width * 72 / DEFAULT_PAGE_DPI,
    heightPoints: page.heightPoints ?? page.height * 72 / DEFAULT_PAGE_DPI,
});
/** Only a complete valid metadata pair replaces a known physical paper size. */
const importedPhysicalSize = (image: {page_width_points?: number;page_height_points?: number}, fallback: PhysicalSize): PhysicalSize =>
    validPagePoints(image.page_width_points) && validPagePoints(image.page_height_points)
        ? {widthPoints:image.page_width_points,heightPoints:image.page_height_points} : fallback;
const confidence = (value: unknown): number | null => finite(value) && value >= 0 && value <= 1 ? value : null;
const touch = (project: OcrProject, pages: OcrPage[]): OcrProject => ({ ...project, pages, updatedAt: now() });
const activeRegions = (page: OcrPage) => page.regions.filter((region) => !region.excluded && textKinds.has(region.kind));
const hasBox = (region: Pick<OcrRegion, 'x'|'y'|'w'|'h'>, page: Pick<OcrPage,'width'|'height'>) =>
    finite(region.x) && finite(region.y) && finite(region.w) && finite(region.h) &&
    region.x >= 0 && region.y >= 0 && region.w > 0 && region.h > 0 &&
    region.x + region.w <= page.width + 0.01 && region.y + region.h <= page.height + 0.01;

export function createProject(name = 'Untitled OCR project'): OcrProject {
    const stamp = now();
    return { version: PROJECT_VERSION, id: id('project'), name, createdAt: stamp, updatedAt: stamp, documents: [], pages: [], confidenceThreshold: 0.85, profileId: 'balanced' };
}

export function addDocument(project: OcrProject, imported: ImportedDocument): OcrProject {
    if (!imported.source_uri || !imported.pages.length) throw new Error('The document contains no readable pages.');
    if (imported.pages.some((page) => !page.image_path || !validDimension(page.width) || !validDimension(page.height))) throw new Error('Imported page dimensions or image paths are invalid.');
    const document: OcrDocument = { id: id('document'), sourceUri: imported.source_uri, name: imported.name || imported.source_uri.split(/[\\/]/).pop() || 'Document' };
    const pages: OcrPage[] = imported.pages.map((page, index) => ({
        id: id('page'), documentId: document.id, sourcePage: index + 1, name: `${document.name} · ${index + 1}`,
        imagePath: page.image_path, originalImagePath: page.image_path, width: page.width, height: page.height,
        ...importedPhysicalSize(page, rasterPhysicalSize(page.width, page.height)),
        status: 'pending', regions: [], excluded: false, approved: false, error: null, elapsedMs: null,
        provenance: {}, profileId: project.profileId,
    }));
    if (pages.some((page)=>!validPagePoints(page.widthPoints)||!validPagePoints(page.heightPoints))) throw new Error('Imported page exceeds the supported physical size at the default 300 DPI. Supply valid physical page dimensions.');
    return { ...touch(project, [...project.pages, ...pages]), documents: [...project.documents, document] };
}

export function updatePage(project: OcrProject, pageId: string, patch: Partial<OcrPage>): OcrProject {
    return touch(project, project.pages.map((page) => {
        if(page.id !== pageId) return page;
        const next={...page,...patch,id:page.id,documentId:page.documentId};
        // Existing mutation paths may clear the old singleton editor projection.
        // Preserve its raw record and the independent objects for recovery; the
        // extraction reader decides whether each captured source is still current.
        const retained=retainedExtractionProvenance(page);
        for(const key of ['extraction','extractionObjects'] as const) {
            if(retained[key]!==undefined && next.provenance[key]===undefined)
                next.provenance={...next.provenance,[key]:retained[key]};
        }
        if(patch.provenance?.extractionObjects!==undefined)
            next.provenance={...next.provenance,extractionObjects:readExtractionObjects(patch.provenance.extractionObjects)};
        if('structure' in patch) next.structure=readPageStructure(patch.structure,next);
        else if(patch.regions && page.structure) {
            const reconciled=reconcilePageStructure(page,next);
            next.structure=reconciled.structure;
            if(reconciled.notices.length) next.provenance={...next.provenance,
                structureNotices:reconciled.notices,structureChangedAt:now()};
        }
        if(patch.regions) {
            const archive=archiveReplacedStructure(page,next);
            if(archive.history) next.provenance={...next.provenance,structureHistory:archive.history,
                ...(archive.notice?{structureNotices:[...(Array.isArray(next.provenance.structureNotices)?next.provenance.structureNotices:[]),archive.notice]}:{})};
        }
        return next;
    }));
}

export function applyRecognition(project: OcrProject, pageId: string, result: RecognitionResult, profileId = project.profileId, options: {preserveSourceHistory?:boolean} = {}): OcrProject {
    if (!validDimension(result.width) || !validDimension(result.height)) throw new Error('Recognition returned invalid page dimensions.');
    const page = project.pages.find((p) => p.id === pageId);
    if (!page) throw new Error('The page is no longer in this project.');
    const regionBasis = (raw: unknown): ConfidenceBasis => {
        if (bases.includes(raw as ConfidenceBasis)) return raw as ConfidenceBasis;
        // Token likelihoods and detector scores must never masquerade as
        // character-aligned, calibrated recognition confidence.
        return 'unknown';
    };
    const regions: OcrRegion[] = result.regions.map((region) => {
        const score = confidence(region.confidence);
        const chars = (region.char_conf ?? []).filter((c) => confidence(c) !== null);
        const unit = region.char_conf_unit ?? result.char_conf_unit;
        // An explicit Unicode unit is an alignment contract; an explicit false
        // flag overrides it. Region metadata takes precedence over page defaults.
        const alignment = region.char_conf_aligned ?? result.char_conf_aligned;
        const aligned = unit === 'unicode_codepoint' && alignment !== false &&
            chars.length > 0 && chars.length === Array.from(region.text).length && chars.length === (region.char_conf ?? []).length;
        const normalized: OcrRegion = {
            id: id('region'), text: region.text, originalText: region.text,
            x: region.x, y: region.y, w: region.w, h: region.h,
            confidence: score, char_conf: chars,
            confidenceBasis: regionBasis(region.confidence_basis ?? result.confidence_basis),
            charConfidenceUnit: unit === 'token' ? 'token' : unit === 'unicode_codepoint' ? 'unicode_codepoint' : 'unknown', charConfidenceAligned: aligned,
            wordEvidence: readWordEvidence(region.word_evidence, region.text, result, region),
            kind: kinds.includes(region.kind as RegionKind) ? region.kind as RegionKind : 'text',
            review: 'unreviewed', excluded: false,
        };
        if (!hasBox(normalized, result)) throw new Error('Recognition returned an out-of-bounds region. The existing result was preserved.');
        return normalized;
    });
    const { regions: _regions, ...metadata } = result;
    const previousSources = options.preserveSourceHistory === false ? [] : passageSources(page);
    const imagePage = {...page,width:result.width,height:result.height,imagePath:result.image_path || page.imagePath};
    const preservedRegions = previousSources.length ? regions.map(region=>withSourceComponents(imagePage,region,previousSources)) : regions;
    const next = updatePage(project, pageId, {
        width: result.width, height: result.height, imagePath: result.image_path || page.imagePath,
        ...importedPhysicalSize(result, pagePhysicalSize(page)),
        regions:preservedRegions, status: 'recognized', approved: false, error: null,
        elapsedMs: finite(result.elapsed_ms) ? result.elapsed_ms : null,
        provenance: { ...metadata, recognizedAt: now(), profileId,
            ...(!preservedRegions.length && previousSources.length ? {passageSources:previousSources} : {}) }, profileId,
    });
    validatePageSourceComponents(next.pages.find(candidate=>candidate.id===pageId)!);
    return next;
}

export function editRegion(project: OcrProject, pageId: string, regionId: string, text: string): OcrProject {
    return updateRegion(project, pageId, regionId, { text });
}

export function updateRegion(project: OcrProject, pageId: string, regionId: string, patch: Partial<OcrRegion>): OcrProject {
    const page = project.pages.find((p) => p.id === pageId);
    if (!page) return project;
    let changed = false;
    const regions = page.regions.map((region) => {
        if (region.id !== regionId) return region;
        const next = { ...region, ...patch, id: region.id, originalText: region.originalText, confidence: region.confidence, confidenceBasis: region.confidenceBasis, char_conf: region.char_conf, wordEvidence: region.wordEvidence, sourceComponents: region.sourceComponents };
        if (!hasBox(next, page)) throw new Error('Region must remain inside the page.');
        changed = true;
        // Any content/geometry/type edit invalidates approval, including exclusion.
        const contentChanged = ['text','x','y','w','h','kind','excluded','separatorBefore'].some((key) => key in patch && patch[key as keyof OcrRegion] !== region[key as keyof OcrRegion]);
        if (contentChanged) next.review = next.text === next.originalText && region.confidenceBasis !== 'manual' ? 'unreviewed' : 'corrected';
        return next;
    });
    return changed ? updatePage(project, pageId, { regions, approved: false }) : project;
}

export function addRegion(project: OcrProject, pageId: string, rectangle: {x:number;y:number;w:number;h:number}, kind: RegionKind = 'text'): OcrProject {
    const page = project.pages.find((p) => p.id === pageId);
    if (!page) return project;
    if (!hasBox(rectangle, page)) throw new Error('Draw a region inside the page.');
    const region: OcrRegion = { id: id('region'), text: '', originalText: '', ...rectangle, confidence: null, char_conf: [], confidenceBasis: 'manual', charConfidenceUnit: 'unknown', charConfidenceAligned: false, kind, review: 'unreviewed', excluded: false };
    return updatePage(project, pageId, { regions: [...page.regions, region], approved: false, status: 'recognized' });
}

export function deleteRegion(project: OcrProject, pageId: string, regionId: string): OcrProject {
    const page = project.pages.find((p) => p.id === pageId);
    return page ? updatePage(project, pageId, { regions: page.regions.filter((region) => region.id !== regionId), approved: false }) : project;
}

function move<T extends {id:string}>(items:T[], itemId:string, direction:-1|1): T[] {
    const index = items.findIndex((item) => item.id === itemId);
    const target = index + direction;
    if (index < 0 || target < 0 || target >= items.length) return items;
    const next = [...items];
    [next[index], next[target]] = [next[target], next[index]];
    return next;
}
export function movePage(project: OcrProject, pageId: string, direction: -1 | 1): OcrProject { return touch(project, move(project.pages, pageId, direction)); }
export function moveRegion(project: OcrProject, pageId: string, regionId: string, direction: -1 | 1): OcrProject {
    const page = project.pages.find((p) => p.id === pageId);
    return page ? updatePage(project, pageId, { regions: move(page.regions, regionId, direction), approved: false }) : project;
}
export function removePage(project: OcrProject, pageId: string): OcrProject { return touch(project, project.pages.filter((page) => page.id !== pageId)); }
export function approvePage(project: OcrProject, pageId: string, approved = true): OcrProject {
    const page = project.pages.find((p) => p.id === pageId);
    if (!page || page.status !== 'recognized') return project;
    return updatePage(project, pageId, { approved, regions: page.regions.map((region) => ({...region, review: approved ? 'approved' : region.text !== region.originalText || region.confidenceBasis === 'manual' ? 'corrected' : 'unreviewed'})) });
}

/** Pointer coordinates are already converted to image pixels by the view. */
export function normalizeRectangle(start: {x:number;y:number}, end: {x:number;y:number}, width: number, height: number): {x:number;y:number;w:number;h:number} | null {
    if (![start.x,start.y,end.x,end.y,width,height].every(finite) || width <= 0 || height <= 0) return null;
    const x = Math.max(0, Math.min(width, Math.min(start.x, end.x)));
    const y = Math.max(0, Math.min(height, Math.min(start.y, end.y)));
    const right = Math.max(0, Math.min(width, Math.max(start.x, end.x)));
    const bottom = Math.max(0, Math.min(height, Math.max(start.y, end.y)));
    if (right - x < 1 || bottom - y < 1) return null;
    return {x, y, w: right-x, h: bottom-y};
}

/** Migrate before replacing source pixels, while their measured geometry is known. */
function retainedExtractionProvenance(page: OcrPage): Record<string, unknown> {
    if(page.provenance.extractionObjects!==undefined || page.provenance.extraction===undefined) return page.provenance;
    const migrated=getExtractionObjects(page);
    return migrated.collection.objects.length
        ? {...page.provenance,extractionObjects:migrated.collection}
        : page.provenance;
}

function passageSources(page: OcrPage): SourceComponent[] {
    return mergeSourceComponents(readSourceComponents(page.provenance.passageSources) ?? [],
        collectSourceComponents(page,page.regions.filter(region=>region.sourceComponents?.length)));
}
function archivePassageSources(page: OcrPage): Record<string, unknown> {
    const sources=passageSources(page);
    return {...retainedExtractionProvenance(page),...(sources.length ? {passageSources:sources} : {})};
}

/** Transforming page pixels invalidates recognition rather than warping stale boxes. */
export function applyPageImage(project: OcrProject, pageId: string, image: ImportedPage, operation: string): OcrProject {
    if (!validDimension(image.width) || !validDimension(image.height) || !image.image_path) throw new Error('Invalid transformed page.');
    const page = project.pages.find((p) => p.id === pageId);
    if (!page) return project;
    const priorSize = pagePhysicalSize(page);
    const fallback = operation === 'rotate' && image.width === page.height && image.height === page.width
        ? {widthPoints:priorSize.heightPoints,heightPoints:priorSize.widthPoints}
        : operation === 'crop'
            ? {widthPoints:priorSize.widthPoints * image.width / page.width,heightPoints:priorSize.heightPoints * image.height / page.height}
            : priorSize;
    return updatePage(project, pageId, { imagePath: image.image_path, width: image.width, height: image.height, ...importedPhysicalSize(image,fallback), regions: [], status: 'pending', approved: false, error: null, elapsedMs: null, provenance: { ...archivePassageSources(page), lastImageOperation: operation, transformedAt: now() } });
}

/** Replace one spread with independently recognizable pages in reading order. */
export function splitPage(project: OcrProject, pageId: string, images: ImportedPage[]): OcrProject {
    const source = project.pages.find((page) => page.id === pageId);
    if (!source) return project;
    if (images.length < 2 || images.some((image) => !image.image_path || !validDimension(image.width) || !validDimension(image.height))) throw new Error('Page split must produce at least two valid images.');
    const priorSize = pagePhysicalSize(source);
    const archive=archiveReplacedStructure(source,{...source,regions:[],structure:undefined});
    const provenance = {...archivePassageSources(source),...(archive.history?{structureHistory:archive.history}:{}),
        ...(source.structure?{structureNotices:['The spread was split into new pages. Previous logical structure is available in structure history. Rebuild roles and links for the new recognition.']}:{}),
        ...(archive.notice?{structureHistoryNotice:archive.notice}:{})};
    const children = images.map((image, index): OcrPage => ({
        ...source, id: id('page'), name: `${source.name} · part ${index+1}`,
        imagePath: image.image_path, width: image.width, height: image.height,
        ...importedPhysicalSize(image,{widthPoints:priorSize.widthPoints * image.width / source.width,heightPoints:priorSize.heightPoints * image.height / source.height}),
        regions: [], status: 'pending', approved: false, error: null, elapsedMs: null,
        structure:undefined,
        provenance: {...provenance, splitFromPageId: source.id, splitPart: index+1, transformedAt: now()},
    }));
    return touch(project, project.pages.flatMap((page) => page.id === pageId ? children : [page]));
}

export function getReviewIssues(project: OcrProject, threshold = project.confidenceThreshold): ReviewIssue[] {
    const issues: ReviewIssue[] = [];
    for (const page of project.pages.filter((p) => !p.excluded)) {
        if (page.status !== 'recognized') {
            issues.push({pageId:page.id,regionId:null,reason:page.status==='failed'?'failed':'pending',message:page.error || 'Page has not been recognized.'});
            continue;
        }
        if (page.approved) continue;
        if (page.structure && typeof page.provenance.structureChangedAt === 'string') {
            issues.push({pageId:page.id,regionId:null,reason:'changed',message:'Document structure changes need page approval.'});
        }
        if ((page.provenance.extractionObjects!==undefined || page.provenance.extraction!==undefined) &&
            getExtractionObjects(page).collection.objects.some(object=>!object.excluded && extractionObjectFreshness(project,object).current)) {
            issues.push({pageId:page.id,regionId:null,reason:'changed',message:'Extracted content needs page approval.'});
        }
        const regions = activeRegions(page);
        if (!regions.length || regions.every((r) => !r.text.trim())) issues.push({pageId:page.id,regionId:null,reason:'empty',message:'No recognized text. Verify this is an intentionally blank page.'});
        for (const region of regions) {
            if (region.review === 'approved') continue;
            const reason = regionReviewReason(region, threshold);
            if (reason) issues.push({ pageId: page.id, regionId: region.id, reason, message: reason === 'changed' ? 'Corrected text needs approval.' : reason === 'unknown-confidence' ? 'Recognition confidence is unavailable; inspect the source.' : 'Recognition confidence is below the review threshold.' });
        }
    }
    return issues;
}

export function getMetrics(project: OcrProject): ProjectMetrics {
    const pages = project.pages.filter((page) => !page.excluded);
    const regions = pages.flatMap(activeRegions);
    const scores = regions.filter((r) => r.confidenceBasis === 'recognition' && r.confidence !== null).map((r) => r.confidence!);
    return {
        pages: project.pages.length, included: pages.length, recognized: pages.filter((p) => p.status==='recognized').length,
        pending: pages.filter((p) => p.status==='pending'||p.status==='running').length,
        failed: pages.filter((p) => p.status==='failed').length, approved: pages.filter((p)=>p.approved).length,
        regions: regions.length, characters: regions.reduce((sum,r)=>sum+Array.from(r.text).length,0),
        needsReview: getReviewIssues(project).length,
        meanConfidence: scores.length ? scores.reduce((sum,v)=>sum+v,0)/scores.length : null,
    };
}

interface MatchOptions { caseSensitive?: boolean; includedOnly?: boolean; pageId?: string; regionId?: string; }
/** Literal Unicode search; regex is escaped and indices refer to original UTF-16 text. */
function literalPattern(query: string, caseSensitive = false): RegExp { return new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), caseSensitive ? 'gu' : 'giu'); }
export function findMatches(project: OcrProject, query: string, options: MatchOptions = {}): TextMatch[] {
    if (!query) return [];
    const matches: TextMatch[] = [];
    const pattern = literalPattern(query, options.caseSensitive);
    for (const page of project.pages) {
        if ((options.includedOnly !== false && page.excluded) || (options.pageId && page.id !== options.pageId)) continue;
        for (const region of page.regions) {
            if ((options.includedOnly !== false && region.excluded) || (options.regionId && region.id !== options.regionId)) continue;
            for (const match of region.text.matchAll(pattern)) matches.push({pageId:page.id,regionId:region.id,start:match.index!,end:match.index!+match[0].length,text:match[0]});
        }
    }
    return matches;
}
export function replaceMatches(project: OcrProject, query: string, replacement: string, options: MatchOptions = {}): OcrProject {
    if (!query) return project;
    const targets = new Map<string,Set<string>>();
    for (const match of findMatches(project,query,options)) {
        if (!targets.has(match.pageId)) targets.set(match.pageId,new Set());
        targets.get(match.pageId)!.add(match.regionId);
    }
    if (!targets.size) return project;
    let next=project;
    for(const page of project.pages) if(targets.has(page.id)) next=updatePage(next,page.id,{
        approved:false, regions:page.regions.map((region) => targets.get(page.id)!.has(region.id) ? {
            ...region, text:region.text.replace(literalPattern(query,options.caseSensitive),()=>replacement), review:'corrected',
        } : region),
    });
    return next;
}
export function nextMatch(matches: TextMatch[], current: Pick<TextMatch,'pageId'|'regionId'|'start'> | null, direction: -1|1 = 1): TextMatch | null {
    if (!matches.length) return null;
    const index = current ? matches.findIndex((m) => m.pageId===current.pageId && m.regionId===current.regionId && m.start===current.start) : -1;
    if (index<0) return direction===1 ? matches[0] : matches[matches.length-1];
    return matches[(index+direction+matches.length)%matches.length];
}

function exportRegion(r:OcrRegion):ExportRegion {
    const originalAligned=hasAlignedCharacterConfidence(r),currentAligned=originalAligned && r.text===r.originalText;
    return {
        id:r.id,text:r.text,original_text:r.originalText,x:r.x,y:r.y,w:r.w,h:r.h,
        confidence:r.confidence,confidence_basis:r.confidenceBasis,
        char_conf_unit:r.charConfidenceUnit,char_conf_aligned:currentAligned,
        char_conf:currentAligned?[...r.char_conf]:[],original_char_conf:[...r.char_conf],original_char_conf_aligned:originalAligned,
        ...(r.wordEvidence?{original_word_evidence:clone(r.wordEvidence),...(r.text===r.originalText?{word_evidence:clone(r.wordEvidence)}:{})}:{}),
        ...(r.sourceComponents?.length?{source_components:clone(r.sourceComponents)}:{}),
        ...(r.separatorBefore!==undefined?{separator_before:r.separatorBefore}:{}),
        ...(r.sharedSourceGroups?.length?{shared_source_groups:[...r.sharedSourceGroups]}:{}),
        review:r.review,kind:r.kind,
    };
}

/** Preflight returns no payload on error. A mixed project can never silently lose pages. */
export function prepareExport(project: OcrProject, options: ExportOptions = {}): ExportPreflight {
    const errors: string[] = [], warnings: string[] = [];
    const docx = options.format?.toLowerCase() === 'docx';
    const selectedIds = options.pageIds ? new Set(options.pageIds) : null;
    if (selectedIds && [...selectedIds].some((pageId) => !project.pages.some((page) => page.id===pageId))) errors.push('One or more selected pages no longer exist.');
    const pages = project.pages.filter((p)=>(!p.excluded || options.includeExcluded) && (!selectedIds || selectedIds.has(p.id)));
    if (!pages.length) errors.push('Select at least one included page to export.');
    const office = docx ? prepareOfficeExport(project, pages.map(page => page.id), options.officeOptions) : undefined;
    if (office) { errors.push(...office.errors); warnings.push(...office.warnings); }
    const payload: ExportPage[] = [];
    const sources = new Map(project.documents.map(document => [document.id, document.sourceUri]));
    for (const page of pages) {
        const physicalSize = pagePhysicalSize(page);
        if (page.status!=='recognized') errors.push(`${page.name}: recognition is ${page.status}. Recognize or explicitly exclude this page.`);
        if (!page.imagePath || !validDimension(page.width) || !validDimension(page.height)) errors.push(`${page.name}: page image or dimensions are missing.`);
        if (!validPagePoints(physicalSize.widthPoints) || !validPagePoints(physicalSize.heightPoints)) errors.push(`${page.name}: physical page dimensions must be positive and no greater than ${MAX_PAGE_POINTS} points.`);
        if (docx && (physicalSize.widthPoints > MAX_DOCX_PAGE_POINTS || physicalSize.heightPoints > MAX_DOCX_PAGE_POINTS)) errors.push(`${page.name}: Word pages cannot exceed 1584 points (22 inches) in either dimension. Split or crop this page, or export it as PDF.`);
        if (options.requireReviewed && !page.approved) errors.push(`${page.name}: page review is required.`);
        const regions = activeRegions(page);
        if (options.format && ['pdf','hocr','alto','xml'].includes(options.format.toLowerCase()) && hasUnresolvedSharedGeometry(regions)) {
            errors.push(`${page.name}: passages share overlapping source areas. Divide their boxes or merge them before a spatial export; text and JSON preserve the exact edited text.`);
        }
        if (regions.some((region) => !hasBox(region,page))) errors.push(`${page.name}: an OCR region lies outside the page image.`);
        if (!regions.some((r)=>r.text.trim())) warnings.push(docx ? `${page.name}: this page contains no included OCR text.` : `${page.name}: this page contains no text and will be retained as an image page.`);
        if (!page.approved) warnings.push(`${page.name}: the page has not been approved.`);
        const includedRegions=page.regions.filter(region=>!region.excluded);
        // A transient structure gives legacy figures their proper reading-order
        // position without changing the saved project or its review status.
        const sourceStructure = page.structure ?? (docx ? deriveStructure(page) : undefined);
        const projectedStructure=sourceStructure?projectStructureForRegions(sourceStructure,includedRegions,page.id):undefined;
        if(projectedStructure) warnings.push(...projectedStructure.notices.map(notice=>`${page.name}: ${notice}`));
        if(page.structure && options.format && !['json','docx'].includes(options.format.toLowerCase())) {
            warnings.push(`${page.name}: this output currently uses the plain-text or source-image projection. Logical formatting is retained in the project and JSON; structured document export is a separate capability.`);
        }
        const provenance = clone(page.provenance);
        // Legacy singleton extraction records resolve to deterministic object IDs.
        // Send the validated collection as a projection, retaining the raw record.
        if (docx) {
            const extraction = getExtractionObjects(page);
            if (extraction.collection.objects.length || page.provenance.extractionObjects !== undefined) provenance.extractionObjects = clone(extraction.collection);
            warnings.push(...extraction.diagnostics.map(message => `${page.name}: ${message}`));
        }
        payload.push({
            id:page.id, name:page.name, source_uri:sources.get(page.documentId),
            source_page:page.sourcePage, approved:page.approved, provenance,
            image_path:page.imagePath, width:page.width, height:page.height,
            page_width_points:physicalSize.widthPoints, page_height_points:physicalSize.heightPoints,
            regions:regions.map(exportRegion),
            ...(projectedStructure?.structure?{structure:projectedStructure.structure,
                figure_regions:includedRegions.filter(region=>region.kind==='picture').map(exportRegion)}:{}),
        });
    }
    return {pages:errors.length?[]:payload,errors,warnings:[...new Set(warnings)],text:errors.length?'':pages.map((p)=>joinRegionText(activeRegions(p))).join('\f'),...(office ? {officeOptions:office.options} : {})};
}

/** History is bounded by count and UTF-8 snapshot bytes, excluding the live project. */
export function createHistory(project: OcrProject, limit = 50, maxBytes = 24 * 1024 * 1024): History {
    return {past:[],present:clone(project),future:[],limit:Math.max(1,Math.min(200,Math.floor(limit)||50)),maxBytes:Math.max(0,Math.floor(maxBytes))};
}
function boundedHistory(history: History): History {
    const past = history.past.slice(-history.limit), future = history.future.slice(0,history.limit);
    const encoder = new TextEncoder();
    const size = (project:OcrProject) => encoder.encode(JSON.stringify(project)).byteLength;
    const pastSizes=past.map(size), futureSizes=future.map(size);
    let bytes=[...pastSizes,...futureSizes].reduce((sum,value)=>sum+value,0);
    while (bytes>history.maxBytes && (past.length || future.length)) {
        // Retain the nearest edit on each side for as long as possible.
        if (past.length >= future.length) { past.shift(); bytes-=pastSizes.shift()!; }
        else { future.pop(); bytes-=futureSizes.pop()!; }
    }
    return {...history,past,future};
}
/** Ordinary history cannot overwrite a newer unapplied editor draft. */
function historySnapshot(project: OcrProject, keepDraft = false): OcrProject {
    const snapshot = clone(project);
    if (!keepDraft) delete snapshot.pageTextDraft;
    return snapshot;
}
function appliedDraft(project: OcrProject, draft: PageTextDraft): boolean {
    const marker = project.pages.find(page => page.id === draft.page.id)?.provenance.lastPageTextEdit;
    if (!marker || typeof marker !== 'object' || Array.isArray(marker)) return false;
    const applied = marker as Record<string, unknown>;
    return applied.draftId === draft.id && applied.draftRevision === draft.revision;
}
export function commitHistory(history: History, project: OcrProject): History {
    if (project === history.present || JSON.stringify(project) === JSON.stringify(history.present)) return history;
    const draft = history.present.pageTextDraft;
    // Applying a captured draft is one undoable operation. Its previous snapshot
    // retains that draft, so Undo restores both the OCR text and unapplied edits.
    const consumesDraft = !!draft && !project.pageTextDraft && appliedDraft(project, draft);
    return boundedHistory({...history,past:[...history.past,historySnapshot(history.present,consumesDraft)],present:clone(project),future:[]});
}
export function undoHistory(history: History): History {
    if (!history.past.length) return history;
    const present = clone(history.past[history.past.length - 1]);
    if (history.present.pageTextDraft) present.pageTextDraft = clone(history.present.pageTextDraft);
    return boundedHistory({...history,past:history.past.slice(0,-1),present,future:[historySnapshot(history.present),...history.future]});
}
export function redoHistory(history: History): History {
    if (!history.future.length) return history;
    const present = clone(history.future[0]), draft = history.present.pageTextDraft;
    // Consume only the exact draft applied by this history entry, and only while
    // its captured source is still current. A subsequently edited draft survives.
    const consumesDraft = !!draft && appliedDraft(present,draft) && pageTextDraftSourceIsCurrent(history.present,draft);
    if (draft && !consumesDraft) present.pageTextDraft = clone(draft);
    else delete present.pageTextDraft;
    return boundedHistory({...history,past:[...history.past,historySnapshot(history.present,consumesDraft)],present,future:history.future.slice(1)});
}

/** Strict boundary validation; interrupted jobs return to pending on restore. */
export function parseProject(input: unknown): OcrProject {
    let value: unknown;
    try { value=typeof input==='string'?JSON.parse(input):clone(input); } catch { throw new Error('This is not a valid OCR project file.'); }
    if (!value || typeof value!=='object') throw new Error('This is not a valid OCR project.');
    const p=value as OcrProject;
    if (p.version!==PROJECT_VERSION) throw new Error(`Unsupported OCR project version: ${String(p.version)}.`);
    if (typeof p.id!=='string'||!p.id||typeof p.name!=='string'||!Array.isArray(p.pages)||!Array.isArray(p.documents)||p.pages.length>10000) throw new Error('OCR project metadata is invalid.');
    if (!finite(p.confidenceThreshold)||p.confidenceThreshold<0||p.confidenceThreshold>1||typeof p.profileId!=='string') throw new Error('OCR project settings are invalid.');
    if (p.recognitionSettings!==undefined && (!p.recognitionSettings || typeof p.recognitionSettings!=='object' || Array.isArray(p.recognitionSettings))) throw new Error('OCR recognition settings are invalid.');
    if (p.proofreading!==undefined) p.proofreading=readProofreadingData(p.proofreading);
    const ids=new Set<string>();
    const checkId=(candidate:unknown) => { if (typeof candidate!=='string'||!candidate||ids.has(candidate)) throw new Error('OCR project contains duplicate or invalid identifiers.'); ids.add(candidate); };
    checkId(p.id);
    for (const document of p.documents) {
        checkId(document.id);
        if (typeof document.sourceUri!=='string'||typeof document.name!=='string') throw new Error('OCR document metadata is invalid.');
    }
    for (const page of p.pages) {
        checkId(page.id);
        if (!p.documents.some((d)=>d.id===page.documentId)||typeof page.name!=='string'||!Number.isInteger(page.sourcePage)||page.sourcePage<1||typeof page.imagePath!=='string'||!page.imagePath||typeof page.originalImagePath!=='string'||!page.originalImagePath||!validDimension(page.width)||!validDimension(page.height)||!statuses.includes(page.status)||!Array.isArray(page.regions)||page.regions.length>100000||typeof page.excluded!=='boolean'||typeof page.approved!=='boolean') throw new Error('OCR project contains an invalid page.');
        if ((page.widthPoints!==undefined&&!validPagePoints(page.widthPoints))||(page.heightPoints!==undefined&&!validPagePoints(page.heightPoints))) throw new Error('OCR project contains invalid physical page dimensions.');
        const physicalSize=pagePhysicalSize(page);
        if (!validPagePoints(physicalSize.widthPoints)||!validPagePoints(physicalSize.heightPoints)) throw new Error('OCR page exceeds the supported physical size at the default 300 DPI.');
        Object.assign(page,physicalSize);
        if (!page.provenance||typeof page.provenance!=='object'||Array.isArray(page.provenance)) page.provenance={};
        if (page.provenance.extractionObjects!==undefined) page.provenance.extractionObjects=readExtractionObjects(page.provenance.extractionObjects);
        if (typeof page.profileId!=='string') page.profileId=p.profileId;
        for (const region of page.regions) {
            checkId(region.id);
            // Earlier v1 snapshots omitted this metadata; default to no character tint.
            if (!['unicode_codepoint','token','unknown'].includes(region.charConfidenceUnit)) region.charConfidenceUnit='unknown';
            if (region.charConfidenceUnit!=='unicode_codepoint' || region.charConfidenceAligned!==true || region.char_conf?.length!==Array.from(region.originalText ?? '').length) region.charConfidenceAligned=false;
            if (typeof region.text!=='string'||typeof region.originalText!=='string'||!hasBox(region,page)||!kinds.includes(region.kind)||!reviews.includes(region.review)||!bases.includes(region.confidenceBasis)||typeof region.excluded!=='boolean'||!Array.isArray(region.char_conf)||region.char_conf.some((c)=>confidence(c)===null)||(region.confidence!==null&&confidence(region.confidence)===null)) throw new Error('OCR project contains an invalid region.');
            region.wordEvidence=readWordEvidence(region.wordEvidence,region.originalText,page);
            region.sourceComponents=readSourceComponents(region.sourceComponents);
            if (region.separatorBefore!==undefined && !['',' ','\n','\n\n'].includes(region.separatorBefore)) throw new Error('OCR passage separator is invalid.');
            region.sharedSourceGroups=readSharedSourceGroups(region.sharedSourceGroups);
        }
        validatePageSourceComponents(page);
        page.structure=readPageStructure(page.structure,page);
        if(page.provenance.structureHistory!==undefined) page.provenance.structureHistory=readStructureHistory(page.provenance.structureHistory);
        if (page.status==='running') { page.status='pending'; page.error='Interrupted recognition can be resumed.'; page.approved=false; }
    }
    if (p.pageTextDraft !== undefined) p.pageTextDraft = readPageTextDraft(p.pageTextDraft);
    return p;
}
export function serializeProject(project: OcrProject): string { return JSON.stringify(parseProject(project),null,2); }
