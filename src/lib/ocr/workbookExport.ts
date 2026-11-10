import type { OcrPage, OcrProject } from './model';
import { extractionObjectFreshness, getExtractionObjects, type ExtractionObject } from './extractionObjects';

export const MAX_WORKBOOK_SHEETS = 512;
export const MAX_WORKBOOK_GRID_POSITIONS = 1_000_000;
export const MAX_WORKBOOK_TEXT_BYTES = 64 * 1024 * 1024;
export const MAX_WORKBOOK_CELL_UNITS = 32_767;
export const MAX_WORKBOOK_CELL_LINE_FEEDS = 253;
export interface WorkbookSheetChoice { pageId: string; objectId: string; sheetName: string; }
export interface WorkbookOptions { version: 1; sheets: WorkbookSheetChoice[]; }
export type WorkbookTableObject = Extract<ExtractionObject, { kind: 'table' }>;
export interface WorkbookSelectionStatus {
    index: number; choice: WorkbookSheetChoice; label: string;
    page?: OcrPage; object?: WorkbookTableObject; errors: string[]; warnings: string[];
}
export interface WorkbookExportCheck {
    options: WorkbookOptions; selections: WorkbookSelectionStatus[]; ownerPages: OcrPage[];
    errors: string[]; warnings: string[];
}

export const emptyWorkbookOptions = (): WorkbookOptions => ({ version: 1, sheets: [] });
export const workbookObjectKey = (pageId: string, objectId: string): string => JSON.stringify([pageId, objectId]);
/** This matches the native Unicode uppercase comparison, without normalizing names. */
export const worksheetNameKey = (name: string): string => name.toUpperCase();
const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const identifier = (value: unknown): value is string => typeof value === 'string' && !!value.trim() && value.length <= 200 && !value.includes('\0');
const invalidXml = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\ufffe\uffff]/u;

export function worksheetNameError(name: string): string | null {
    if (!name.trim()) return 'Enter a worksheet name.';
    if (name.length > 31) return 'Worksheet names can contain at most 31 characters. Some Unicode characters count as two. Shorten this name; it will not be truncated.';
    if (/[\u0000-\u001f\u007f-\u009f\ufffe\uffff]/u.test(name)) return 'Worksheet names cannot contain control characters or XML-invalid characters.';
    if (/[\/\\?*:\[\]]/u.test(name)) return 'Worksheet names cannot contain / \\ ? * : [ or ].';
    if (name.startsWith("'") || name.endsWith("'")) return 'Worksheet names cannot begin or end with an apostrophe.';
    if (worksheetNameKey(name) === 'HISTORY') return 'History is a reserved worksheet name. Enter another name.';
    return null;
}

/** Only propose a starter after an explicit checkbox choice; never repair an entered name. */
export function nextWorksheetName(options: WorkbookOptions): string {
    const used = new Set(options.sheets.map(sheet => worksheetNameKey(sheet.sheetName)));
    for (let number = 1; ; number++) {
        const candidate = `Table ${number}`;
        if (!used.has(worksheetNameKey(candidate))) return candidate;
    }
}

export function readWorkbookOptions(raw: unknown): WorkbookOptions {
    if (raw === undefined) return emptyWorkbookOptions();
    if (!record(raw) || raw.version !== 1 || Object.keys(raw).some(key => !['version', 'sheets'].includes(key)) ||
        !Array.isArray(raw.sheets) || raw.sheets.length > MAX_WORKBOOK_SHEETS) {
        throw new Error('Workbook choices must use version 1 and contain at most 512 worksheets.');
    }
    return { version: 1, sheets: raw.sheets.map((value): WorkbookSheetChoice => {
        if (!record(value) || Object.keys(value).some(key => !['pageId', 'objectId', 'sheetName'].includes(key)) ||
            !identifier(value.pageId) || !identifier(value.objectId) || typeof value.sheetName !== 'string') {
            throw new Error('A worksheet choice has an invalid source identity or name.');
        }
        // Keep invalid entered names visible so the user can correct them.
        return { pageId: value.pageId, objectId: value.objectId, sheetName: value.sheetName };
    }) };
}

export function workbookScopedPages(project: OcrProject, pageIds?: readonly string[]): OcrPage[] {
    const scope = pageIds ? new Set(pageIds) : null;
    return project.pages.filter(page => !page.excluded && (!scope || scope.has(page.id)));
}

/** Validate saved tables only. Current OCR status, pixels and logical structure are not workbook inputs. */
export function prepareWorkbookExport(project: OcrProject, pageIds?: readonly string[], raw?: unknown): WorkbookExportCheck {
    let options: WorkbookOptions;
    try { options = readWorkbookOptions(raw); }
    catch (error) { return { options: emptyWorkbookOptions(), selections: [], ownerPages: [], errors: [error instanceof Error ? error.message : String(error)], warnings: [] }; }
    const errors: string[] = [], warnings: string[] = [], selections: WorkbookSelectionStatus[] = [];
    const pages = workbookScopedPages(project, pageIds), scope = new Set(pages.map(page => page.id));
    if (pageIds?.some(id => !project.pages.some(page => page.id === id))) errors.push('One or more selected pages no longer exist. Update Pages before exporting.');
    if (!options.sheets.length) errors.push('Select at least one saved table for this workbook.');
    const selected = new Set<string>(), names = new Map<string, string>(), owners = new Set<string>();
    const cache = new Map<string, ExtractionObject[]>();
    const encoder = new TextEncoder();
    let gridPositions = 0, textBytes = 0;
    let readerEscapeWarning = false;
    for (const [index, choice] of options.sheets.entries()) {
        const page = project.pages.find(item => item.id === choice.pageId);
        if (page && !cache.has(page.id)) {
            const extraction = getExtractionObjects(page);
            cache.set(page.id, extraction.collection.objects);
            warnings.push(...extraction.diagnostics.map(message => `${page.name}: ${message}`));
        }
        const object = page ? cache.get(page.id)!.find(item => item.id === choice.objectId) : undefined;
        const label = object?.name ?? choice.objectId;
        const status: WorkbookSelectionStatus = { index, choice, label, page, object: object?.kind === 'table' ? object : undefined, errors: [], warnings: [] };
        const problem = (text: string) => { status.errors.push(text); errors.push(text); };
        const warn = (text: string) => { status.warnings.push(text); warnings.push(text); };
        const nameProblem = worksheetNameError(choice.sheetName);
        readerEscapeWarning ||= choice.sheetName.includes('\r') || /_x[0-9a-fA-F]{4}_/.test(choice.sheetName);
        if (nameProblem) problem(`${label}: ${nameProblem}`);
        const nameKey = worksheetNameKey(choice.sheetName);
        if (names.has(nameKey)) problem(`${label}: worksheet name “${choice.sheetName}” is already used. Enter a unique name.`);
        else names.set(nameKey, label);
        const key = workbookObjectKey(choice.pageId, choice.objectId);
        if (selected.has(key)) problem(`${label}: select each saved table only once, even when worksheet names differ.`);
        selected.add(key);
        if (!page) problem(`${label}: its source page no longer exists. Remove this worksheet.`);
        else if (page.excluded) problem(`${label}: its page is excluded. Remove this worksheet or include the page.`);
        else if (!scope.has(page.id)) problem(`${label}: its source page is outside the selected scope. Remove this worksheet or change Pages.`);
        if (!object) problem(`${label}: the saved table no longer exists. Remove this worksheet.`);
        else if (object.kind !== 'table') problem(`${label}: only saved table objects can become worksheets.`);
        else {
            if (object.excluded) problem(`${label}: this table is excluded. Remove this worksheet or include the table in Extract.`);
            if (page && !page.excluded && scope.has(page.id)) owners.add(page.id);
            const freshness = extractionObjectFreshness(project, object);
            if (!freshness.current) warn(`${label}: historical source. ${freshness.reasons.join(' ')} Saved table values will be exported.`);
            const table = object.currentData;
            if (!table.rows && !table.columns) warn(`${label}: this table has no cells. It will create an empty worksheet named “${choice.sheetName}”.`);
            gridPositions += table.rows * table.columns;
            for (const cell of table.cells) {
                const coordinate = `${label}, row ${cell.row + 1}, column ${cell.column + 1}`;
                if (cell.text.length > MAX_WORKBOOK_CELL_UNITS) problem(`${coordinate}: this cell exceeds 32,767 characters. Some Unicode characters count as two. Split or shorten it, or save the complete table as CSV or JSON.`);
                let lineFeeds = 0;
                for (let i = 0; i < cell.text.length; i++) if (cell.text.charCodeAt(i) === 10) lineFeeds++;
                if (lineFeeds > MAX_WORKBOOK_CELL_LINE_FEEDS) problem(`${coordinate}: this cell contains more than 253 line feeds. Split or shorten it, or save the complete table as CSV or JSON.`);
                const invalid = invalidXml.exec(cell.text);
                if (invalid) problem(`${coordinate}: XML-incompatible character U+${invalid[0].charCodeAt(0).toString(16).toUpperCase().padStart(4, '0')} must be corrected before workbook export. No characters will be removed automatically.`);
                readerEscapeWarning ||= cell.text.includes('\r') || /_x[0-9a-fA-F]{4}_/.test(cell.text);
                textBytes += encoder.encode(cell.text).byteLength;
            }
        }
        selections.push(status);
    }
    if (gridPositions > MAX_WORKBOOK_GRID_POSITIONS) errors.push('The selected worksheets exceed 1,000,000 total grid positions. Export fewer tables in this workbook.');
    if (textBytes > MAX_WORKBOOK_TEXT_BYTES) errors.push('The selected worksheets exceed 64 MiB of cell text. Export fewer tables in this workbook; no text will be truncated.');
    if (readerEscapeWarning) warnings.push('This workbook uses standard spreadsheet escaping for carriage returns or text shaped like _xHHHH_. Some spreadsheet readers may display encoded escapes instead of the original text. Check these values in your spreadsheet reader.');
    const valid = selections.filter((item): item is WorkbookSelectionStatus & { page: OcrPage; object: WorkbookTableObject } => !!item.page && !!item.object && !item.errors.length);
    for (const item of valid) {
        if (item.object.originalOutputKind !== 'table-join') continue;
        const snapshots = item.object.tableSources?.snapshots ?? [];
        for (const other of valid) {
            if (other !== item && snapshots.some(snapshot => snapshot.ownerPageId === other.page.id && snapshot.objectId === other.object.id)) {
                warnings.push(`${item.label} includes rows from ${other.label}, which is also selected as a worksheet. Review these choices to avoid repeated table content.`);
            }
        }
        if (snapshots.some(snapshot => !scope.has(snapshot.ownerPageId))) warnings.push(`${item.label} includes joined rows captured from pages outside the selected scope. Only the chosen saved table is exported; no other page is added.`);
    }
    return { options, selections, ownerPages: pages.filter(page => owners.has(page.id)), errors, warnings: [...new Set(warnings)] };
}
