import type { ConfidenceBasis, OcrPage, OcrRegion, RegionKind } from './model';
import { readWordEvidence, type WordBounds, type WordEvidenceEnvelope } from './wordEvidence';

/** Flat, immutable source snapshots; bounds describe coverage at capture. */
export interface SourceComponent {
    version: 1;
    id: string;
    sourceRegionId: string;
    pageId: string;
    imagePath: string;
    width: number;
    height: number;
    boundsAtCapture: WordBounds;
    kind: RegionKind;
    originalText: string;
    textAtCapture: string;
    confidence: number | null;
    confidenceBasis: ConfidenceBasis;
    char_conf: number[];
    charConfidenceUnit: 'unicode_codepoint' | 'token' | 'unknown';
    charConfidenceAligned: boolean;
    wordEvidence?: WordEvidenceEnvelope;
}

export const MAX_SOURCE_COMPONENTS = 128;
export const MAX_SOURCE_BYTES = 8 * 1024 * 1024;
export const MAX_PAGE_SOURCE_BYTES = 16 * 1024 * 1024;
export const MAX_SHARED_SOURCE_GROUPS = 128;
const kinds = ['text','title','table','picture','formula','header','footer'];
const bases = ['recognition','detection','unknown','manual'];
const fields = new Set(['version','id','sourceRegionId','pageId','imagePath','width','height',
    'boundsAtCapture','kind','originalText','textAtCapture','confidence','confidenceBasis',
    'char_conf','charConfidenceUnit','charConfidenceAligned','wordEvidence']);
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const score = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1;
const identifier = (value: unknown): value is string => typeof value === 'string' && !!value.trim() && value.length <= 200 && !value.includes('\0');
const dimension = (value: unknown): value is number => Number.isInteger(value) && (value as number) > 0 && (value as number) <= 100000;
const encoder = new TextEncoder();
const invalid = (reason: string): never => { throw new Error(`Invalid passage source history: ${reason}. Existing text was preserved.`); };
function byteSize(value: unknown): number {
    try { return encoder.encode(JSON.stringify(value)).byteLength; }
    catch { return invalid('source history must be serializable'); }
}

/** Source pages may differ from the current page; validate against their own pixels. */
export function readSourceComponents(value: unknown): SourceComponent[] | undefined {
    if (value === undefined) return undefined;
    if (!Array.isArray(value) || !value.length || value.length > MAX_SOURCE_COMPONENTS) invalid('expected 1–128 flat source components');
    if (byteSize(value) > MAX_SOURCE_BYTES) invalid('one passage exceeds the 8 MB source-history limit');
    const ids = new Set<string>();
    return (value as unknown[]).map(raw => {
        if (!object(raw) || Object.keys(raw).some(key => !fields.has(key)) || raw.version !== 1 ||
            !identifier(raw.id) || ids.has(raw.id) || !identifier(raw.sourceRegionId) || !identifier(raw.pageId) ||
            typeof raw.imagePath !== 'string' || !raw.imagePath || raw.imagePath.length > 10000 || raw.imagePath.includes('\0') ||
            !dimension(raw.width) || !dimension(raw.height) || raw.width * raw.height > 100000000 ||
            typeof raw.originalText !== 'string' || typeof raw.textAtCapture !== 'string' ||
            raw.originalText.length > MAX_SOURCE_BYTES || raw.textAtCapture.length > MAX_SOURCE_BYTES ||
            !kinds.includes(raw.kind as string) || !bases.includes(raw.confidenceBasis as string) ||
            (raw.confidence !== null && !score(raw.confidence)) || !Array.isArray(raw.char_conf) ||
            raw.char_conf.length > 1000000 || raw.char_conf.some(value => !score(value)) ||
            !['unicode_codepoint','token','unknown'].includes(raw.charConfidenceUnit as string) ||
            typeof raw.charConfidenceAligned !== 'boolean') invalid('unsupported component metadata');
        const entry = raw as unknown as SourceComponent;
        const box = entry.boundsAtCapture;
        if (!object(box) || ![box.x,box.y,box.w,box.h].every(value => typeof value === 'number' && Number.isFinite(value)) ||
            box.x < 0 || box.y < 0 || box.w <= 0 || box.h <= 0 ||
            box.x + box.w > entry.width + 0.01 || box.y + box.h > entry.height + 0.01) invalid('source coverage lies outside its captured page');
        if (entry.charConfidenceAligned && (entry.charConfidenceUnit !== 'unicode_codepoint' ||
            !entry.char_conf.length || entry.char_conf.length !== Array.from(entry.originalText).length)) invalid('character scores do not match original text');
        const wordEvidence = readWordEvidence(entry.wordEvidence, entry.originalText, entry);
        ids.add(entry.id);
        return {
            version:1, id:entry.id, sourceRegionId:entry.sourceRegionId, pageId:entry.pageId,
            imagePath:entry.imagePath, width:entry.width, height:entry.height,
            boundsAtCapture:{x:box.x,y:box.y,w:box.w,h:box.h}, kind:entry.kind,
            originalText:entry.originalText, textAtCapture:entry.textAtCapture,
            confidence:entry.confidence, confidenceBasis:entry.confidenceBasis,
            char_conf:[...entry.char_conf], charConfidenceUnit:entry.charConfidenceUnit,
            charConfidenceAligned:entry.charConfidenceAligned,
            ...(wordEvidence ? {wordEvidence} : {}),
        };
    });
}

export function readSharedSourceGroups(value: unknown): string[] | undefined {
    if (value === undefined) return undefined;
    if (!Array.isArray(value) || value.length > MAX_SHARED_SOURCE_GROUPS ||
        value.some(group => !identifier(group)) || new Set(value).size !== value.length) invalid('invalid shared source groups');
    const groups=value as string[];
    return groups.length ? [...groups] : undefined;
}

function snapshot(page: OcrPage, region: OcrRegion): SourceComponent {
    return readSourceComponents([{
        version:1, id:region.id, sourceRegionId:region.id, pageId:page.id,
        imagePath:page.imagePath, width:page.width, height:page.height,
        boundsAtCapture:{x:region.x,y:region.y,w:region.w,h:region.h}, kind:region.kind,
        originalText:region.originalText, textAtCapture:region.text,
        confidence:region.confidence, confidenceBasis:region.confidenceBasis,
        char_conf:region.char_conf, charConfidenceUnit:region.charConfidenceUnit,
        charConfidenceAligned:region.charConfidenceAligned,
        ...(region.wordEvidence ? {wordEvidence:region.wordEvidence} : {}),
    }])![0];
}

function deduplicate(components: Iterable<SourceComponent>): SourceComponent[] {
    const unique = new Map<string,SourceComponent>();
    for (const component of components) {
        const existing = unique.get(component.id);
        if (existing && JSON.stringify(existing) !== JSON.stringify(component)) invalid('a source identity has conflicting snapshots');
        if (!existing) unique.set(component.id, component);
        if (unique.size > MAX_SOURCE_COMPONENTS) invalid('the operation would exceed 128 source components');
    }
    return unique.size ? readSourceComponents([...unique.values()])! : [];
}

export function mergeSourceComponents(...groups: readonly (readonly SourceComponent[])[]): SourceComponent[] {
    function* sources() {
        for (const group of groups) if (group.length) yield* readSourceComponents(group)!;
    }
    return deduplicate(sources());
}

/** Repeated split/merge flattens the same originals, never a recursive edit tree. */
export function collectSourceComponents(page: OcrPage, regions: readonly OcrRegion[]): SourceComponent[] {
    function* sources() {
        for (const region of regions) yield* readSourceComponents(region.sourceComponents) ?? [snapshot(page,region)];
    }
    return deduplicate(sources());
}

/** Accepted re-OCR retains earlier sources and adds this run's real native result. */
export function withSourceComponents(page: OcrPage, newRegion: OcrRegion, previousComponents: readonly SourceComponent[]): OcrRegion {
    const previous = previousComponents.length ? readSourceComponents(previousComponents)! : [];
    return {...newRegion, sourceComponents:deduplicate([
        ...previous, ...(readSourceComponents(newRegion.sourceComponents) ?? []), snapshot(page,newRegion),
    ])};
}

/** Count repeated inline snapshots too: save/undo serialize each owning region. */
export function validatePageSourceComponents(page: OcrPage): void {
    const archived = readSourceComponents(page.provenance?.passageSources);
    let bytes = archived ? byteSize(archived) : 0;
    for (const region of page.regions) {
        const components = readSourceComponents(region.sourceComponents);
        readSharedSourceGroups(region.sharedSourceGroups);
        if (components) bytes += byteSize(components);
        if (bytes > MAX_PAGE_SOURCE_BYTES) invalid('this page exceeds the 16 MB source-history limit');
    }
}
