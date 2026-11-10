import type { OcrPage, OcrProject } from './model';
import { getExtractionObjects, extractionObjectFreshness, type ExtractionObject } from './extractionObjects';

export const MAX_OFFICE_OBJECTS = 512;
export const MAX_DOCX_PAGE_POINTS = 1584;
export const MAX_DOCX_TABLE_COLUMNS = 63;
export interface OfficeObjectChoice {
    pageId: string;
    objectId: string;
    placement: 'append' | 'replace-region';
    regionId?: string;
}
export interface OfficeExportOptions { version: 1; objects: OfficeObjectChoice[]; }
export type OfficeExtractionObject = Extract<ExtractionObject, { kind: 'table' | 'formula' }>;
export interface OfficeSelectionStatus {
    index: number; choice: OfficeObjectChoice; label: string;
    page?: OcrPage; object?: OfficeExtractionObject; errors: string[];
}
export interface OfficeExportCheck {
    options: OfficeExportOptions; selections: OfficeSelectionStatus[]; errors: string[]; warnings: string[];
}
export const emptyOfficeExportOptions = (): OfficeExportOptions => ({ version: 1, objects: [] });
export const officeObjectKey = (pageId: string, objectId: string): string => JSON.stringify([pageId, objectId]);
const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const identifier = (value: unknown): value is string => typeof value === 'string' && !!value.trim() && value.length <= 200 && !value.includes('\0');

/** Read a small explicit selection plan; never infer a latest extraction. */
export function readOfficeExportOptions(raw: unknown): OfficeExportOptions {
    if (raw === undefined) return emptyOfficeExportOptions();
    if (!record(raw) || raw.version !== 1 || Object.keys(raw).some(key => !['version', 'objects'].includes(key)) ||
        !Array.isArray(raw.objects) || raw.objects.length > MAX_OFFICE_OBJECTS) {
        throw new Error('DOCX extracted-content choices must use version 1 and contain at most 512 objects.');
    }
    return { version: 1, objects: raw.objects.map((value): OfficeObjectChoice => {
        if (!record(value) || Object.keys(value).some(key => !['pageId', 'objectId', 'placement', 'regionId'].includes(key)) ||
            !identifier(value.pageId) || !identifier(value.objectId) || !['append', 'replace-region'].includes(String(value.placement)) ||
            (value.regionId !== undefined && !identifier(value.regionId)) || (value.placement === 'append' && value.regionId !== undefined)) {
            throw new Error('A DOCX extraction choice has an invalid identity or placement.');
        }
        return { pageId: value.pageId, objectId: value.objectId, placement: value.placement as OfficeObjectChoice['placement'],
            ...(value.regionId !== undefined ? { regionId: value.regionId as string } : {}) };
    }) };
}

export function officeScopedPages(project: OcrProject, pageIds?: readonly string[]): OcrPage[] {
    const selected = pageIds ? new Set(pageIds) : null;
    return project.pages.filter(page => !page.excluded && (!selected || selected.has(page.id)));
}

/** Validate placement against the actual scoped pages without changing the project. */
export function prepareOfficeExport(project: OcrProject, pageIds?: readonly string[], raw?: unknown): OfficeExportCheck {
    let options: OfficeExportOptions;
    try { options = readOfficeExportOptions(raw); }
    catch (error) { return { options: emptyOfficeExportOptions(), selections: [], errors: [error instanceof Error ? error.message : String(error)], warnings: [] }; }
    const errors: string[] = [], warnings: string[] = [], selections: OfficeSelectionStatus[] = [];
    const pages = officeScopedPages(project, pageIds), scope = new Set(pages.map(page => page.id));
    const objectCache = new Map<string, ExtractionObject[]>();
    const objectsFor = (page: OcrPage) => {
        let objects = objectCache.get(page.id);
        if (!objects) { objects = getExtractionObjects(page).collection.objects; objectCache.set(page.id, objects); }
        return objects;
    };
    const selectedKeys = new Set<string>(), replacements = new Map<string, string>();
    for (const [index, choice] of options.objects.entries()) {
        const page = project.pages.find(item => item.id === choice.pageId);
        const object = page ? objectsFor(page).find(item => item.id === choice.objectId) : undefined;
        const status: OfficeSelectionStatus = { index, choice, label: object?.name ?? choice.objectId, page,
            object: object?.kind === 'table' || object?.kind === 'formula' ? object : undefined, errors: [] };
        const problem = (text: string) => { status.errors.push(text); errors.push(text); };
        const key = officeObjectKey(choice.pageId, choice.objectId);
        if (selectedKeys.has(key)) problem(`${status.label}: select each extraction object only once.`);
        selectedKeys.add(key);
        if (!page) problem(`${status.label}: its source page no longer exists. Remove this export choice.`);
        else if (page.excluded) problem(`${status.label}: its page is excluded. Remove this export choice or include the page.`);
        else if (!scope.has(page.id)) problem(`${status.label}: its page is outside the selected export scope. Remove this choice or change Pages.`);
        if (!object) problem(`${status.label}: the saved extraction no longer exists. Remove this export choice.`);
        else if (object.kind === 'fields') problem(`${status.label}: field sets are available as JSON; DOCX choices support tables and formulas.`);
        else {
            if (object.excluded) problem(`${object.name}: this extraction is excluded. Remove this choice or include it in Extract.`);
            if (object.kind === 'table' && object.currentData.columns > MAX_DOCX_TABLE_COLUMNS) {
                problem(`${object.name}: this DOCX exporter supports at most 63 table columns. Save this table as CSV instead.`);
            }
            if (object.kind === 'table' && (!object.currentData.rows || !object.currentData.columns)) problem(`${object.name}: this table is empty. Add a row and column in Extract, or remove it from the Word choices.`);
            const freshness = extractionObjectFreshness(project, object);
            if (!freshness.current) warnings.push(`${object.name}: historical source. ${freshness.reasons.join(' ')}`);
            if (object.kind === 'formula') warnings.push(`${object.name}: the formula is exported as editable LaTeX text, not a Word equation.`);
            if (object.kind === 'table' && object.currentData.cells.some(cell => cell.header)) {
                warnings.push(`${object.name}: header cells are bold; repeating Word table-header rows are not created.`);
            }
            if (choice.placement === 'replace-region' && page) {
                const region = page.regions.find(item => item.id === choice.regionId);
                if (!region || region.excluded || region.kind !== object.kind) problem(`${object.name}: choose one included ${object.kind} passage to replace.`);
                else {
                    const target = officeObjectKey(page.id, region.id), previous = replacements.get(target);
                    if (previous) problem(`${object.name}: ${previous} already replaces this passage. Choose a different passage or append this object.`);
                    else replacements.set(target, object.name);
                }
            }
        }
        selections.push(status);
    }

    const selected = selections.filter((item): item is OfficeSelectionStatus & { page: OcrPage; object: OfficeExtractionObject } => !!item.page && !!item.object && !item.errors.length);
    for (const item of selected) {
        const { object } = item;
        if (object.kind === 'table' && object.originalOutputKind === 'table-join') {
            const snapshots = object.tableSources?.snapshots ?? [];
            for (const other of selected) {
                if (other !== item && snapshots.some(snapshot => snapshot.ownerPageId === other.page.id && snapshot.objectId === other.object.id)) {
                    warnings.push(`${object.name} includes source content from ${other.object.name}, which is also selected. Review these choices to avoid repeated rows.`);
                }
            }
            const outside = [...new Set(snapshots.map(snapshot => snapshot.ownerPageId).filter(id => !scope.has(id)))];
            if (outside.length) warnings.push(`${object.name} includes joined rows from ${outside.map(id => project.pages.find(page => page.id === id)?.name ?? 'an earlier page').join(', ')} outside the selected page scope.`);
        }
        const captures = [...object.captures, ...(object.tableSources?.snapshots.flatMap(snapshot => snapshot.captures) ?? [])];
        const retained = new Set<string>();
        for (const capture of captures) {
            if (!scope.has(capture.pageId)) continue;
            const sourcePage = pages.find(page => page.id === capture.pageId)!;
            const sourceRegions = capture.regionId ? sourcePage.regions.filter(region => region.id === capture.regionId)
                : sourcePage.regions.filter(region => region.kind === object.kind);
            if (sourceRegions.some(region => !region.excluded && region.text.length > 0 && !replacements.has(officeObjectKey(sourcePage.id, region.id)))) retained.add(sourcePage.name);
        }
        if (retained.size) warnings.push(`${object.name}: captured OCR passage text on ${[...retained].join(', ')} remains included. The extracted content may repeat it; replacement affects only the passage you explicitly choose.`);
    }
    for (const page of pages) {
        const included = new Set(page.regions.filter(region => !region.excluded).map(region => region.id));
        const blocks = page.structure?.blocks.filter(block => included.has(block.sourceRegionId)) ?? [];
        if (blocks.some(block => block.role === 'list-item')) warnings.push(`${page.name}: Word list numbering preserves literal bullets and numbers in the source text. Remove an existing marker in the text editor if it would produce duplicate visible markers.`);
        if (blocks.some(block => block.role === 'header' || block.role === 'footer') || page.regions.some(region => !region.excluded && ['header', 'footer'].includes(region.kind))) warnings.push(`${page.name}: headers and footers are placed in the document body.`);
        if (blocks.some(block => block.role === 'footnote' || block.noteReferences?.length)) warnings.push(`${page.name}: notes use body text and custom markers rather than Word footnote objects.`);
        if (page.regions.some(region => !region.excluded && region.kind === 'picture')) warnings.push(`${page.name}: figures are included; this Word writer does not encode figure alt text or decorative flags.`);
        if (page.regions.some(region => !region.excluded && region.text.includes('\r'))) warnings.push(`${page.name}: carriage returns are normalized to Word line breaks.`);
    }
    return { options, selections, errors, warnings: [...new Set(warnings)] };
}
