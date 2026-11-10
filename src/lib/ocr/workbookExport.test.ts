import { describe, expect, it } from 'vitest';
import { addDocument, applyRecognition, approvePage, createProject, prepareExport, type OcrProject } from './model';
import { appendExtractionObject, captureExtractionRequest, getExtractionObjects, replaceExtractionTableData, updateExtractionObject, type ExtractionKind } from './extractionObjects';
import { captureExtractionSource } from './extraction';
import { applyContinuationJoin, prepareContinuationJoin } from './continuationJoin';
import { tableDataFromRows, type TableData } from './tableData';
import { emptyWorkbookOptions, nextWorksheetName, prepareWorkbookExport, readWorkbookOptions, worksheetNameError, worksheetNameKey, type WorkbookOptions, type WorkbookSheetChoice, type WorkbookTableObject } from './workbookExport';

const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
const plan = (...sheets: WorkbookSheetChoice[]): WorkbookOptions => ({ version: 1, sheets });
const sheet = (pageId: string, objectId: string, sheetName = 'Table 1'): WorkbookSheetChoice => ({ pageId, objectId, sheetName });
const tableOutput = { html: '<table><tr><th colspan="2">Parts</th></tr><tr><td>0012</td><td>EUR 1,250.00</td></tr></table>', rows: 2, cols: 2 };

function setup() {
    let project = addDocument(createProject('Workbook source'), { source_uri: '/source.pdf', name: 'Source', pages: [
        { image_path: '/first.png', width: 800, height: 1000 },
        { image_path: '/second.png', width: 800, height: 1000 },
        { image_path: '/unselected.png', width: 800, height: 1000 },
    ] });
    for (const page of project.pages.slice(0, 2)) project = applyRecognition(project, page.id, { width: 800, height: 1000, regions: [
        { text: 'Paragraph not included in worksheets', x: 10, y: 10, w: 500, h: 40, kind: 'text' },
        { text: 'Parts\n0012\tEUR 1,250.00', x: 10, y: 80, w: 500, h: 150, kind: 'table' },
        { text: 'x^2', x: 10, y: 260, w: 500, h: 60, kind: 'formula' },
        { text: '', x: 10, y: 360, w: 200, h: 200, kind: 'picture' },
    ] });
    const ids: string[] = [];
    for (const entry of [
        { page: 0, region: 1, kind: 'table', output: tableOutput, name: 'Parts' },
        { page: 0, region: 1, kind: 'table', output: { html: '<table></table>', rows: 0, cols: 0 }, name: 'Empty table' },
        { page: 0, region: 2, kind: 'formula', output: '\\frac{a}{b}', name: 'Equation' },
        { page: 0, region: 0, kind: 'fields', output: [{ label: 'Account', value: '0007', score: .91 }], name: 'Fields' },
        { page: 1, region: 1, kind: 'table', output: tableOutput, name: 'Continuation' },
    ]) {
        const page = project.pages[entry.page];
        const request = captureExtractionRequest(project, page.id, page.regions[entry.region].id, entry.kind === 'fields' ? 'text' : 'pixels');
        project = appendExtractionObject(project, request, entry.kind as ExtractionKind, entry.output, { name: entry.name, labels: ['Account'] });
        ids.push(request.objectId);
    }
    return { project, pageId: project.pages[0].id, otherPageId: project.pages[1].id, ids };
}
function table(project: OcrProject, pageId: string, objectId: string): WorkbookTableObject {
    return getExtractionObjects(project.pages.find(page => page.id === pageId)!).collection.objects.find(object => object.id === objectId)! as WorkbookTableObject;
}
function withRows(state: ReturnType<typeof setup>, rows: string[][]): OcrProject {
    return replaceExtractionTableData(state.project, state.pageId, state.ids[0], tableDataFromRows(rows, 'current-cell'));
}

describe('workbook selections and owner-page projection', () => {
    it('starts empty and never infers a sheet from available objects or source regions', () => {
        const { project } = setup(), before = clone(project);
        const result = prepareExport(project, { format: 'xlsx' });
        expect(result.workbookOptions).toEqual(emptyWorkbookOptions());
        expect(result.errors).toContain('Select at least one saved table for this workbook.');
        expect(result.pages).toEqual([]);
        expect(project).toEqual(before);
    });

    it('keeps tab order independent from scoped owner-page order and sends no OCR, figures or structure', () => {
        const { project, pageId, otherPageId, ids } = setup(), before = clone(project);
        const options = plan(sheet(otherPageId, ids[4], 'Last page first'), sheet(pageId, ids[1], 'Blank'), sheet(pageId, ids[0], 'Parts'));
        const result = prepareExport(project, { format: 'xlsx', workbookOptions: options });
        expect(result.errors).toEqual([]);
        expect(result.workbookOptions).toEqual(options);
        expect(result.pages.map(page => page.id)).toEqual([pageId, otherPageId]);
        expect(result.pages.every(page => !page.regions.length && page.structure === undefined && page.figure_regions === undefined)).toBe(true);
        expect(result.text).toBe('');
        expect(result.officeOptions).toBeUndefined();
        expect(result.pages[0].provenance!.extractionObjects).toEqual(project.pages[0].provenance.extractionObjects);
        expect(project).toEqual(before);
    });

    it('gates approval only on selected owners, leaving unselected pending pages outside the payload', () => {
        let { project, pageId, otherPageId, ids } = setup();
        const options = plan(sheet(pageId, ids[0]));
        expect(prepareExport(project, { format: 'xlsx', workbookOptions: options, requireReviewed: true }).errors.join(' ')).toContain('review is required');
        project = approvePage(project, pageId, true);
        const ready = prepareExport(project, { format: 'xlsx', workbookOptions: options, requireReviewed: true });
        expect(ready.errors).toEqual([]);
        expect(ready.pages.map(page => page.id)).toEqual([pageId]);
        const expanded = prepareExport(project, { format: 'xlsx', workbookOptions: plan(...options.sheets, sheet(otherPageId, ids[4], 'Second')), requireReviewed: true });
        expect(expanded.errors).toHaveLength(1);
        expect(expanded.errors[0]).toContain(project.pages[1].name);
    });

    it('exports saved values from a failed owner without requiring current pixels, OCR text or physical size', () => {
        const state = setup(), { pageId, ids } = state;
        const original = clone(table(state.project, pageId, ids[0]));
        state.project.pages[0] = { ...state.project.pages[0], status: 'failed', error: 'Missing page image', imagePath: '', width: 0, height: 0, widthPoints: 20_000, regions: [] };
        const result = prepareExport(state.project, { format: 'xlsx', workbookOptions: plan(sheet(pageId, ids[0])) });
        expect(result.errors).toEqual([]);
        expect(result.warnings.join(' ')).toContain('historical source');
        expect(result.warnings.join(' ')).not.toContain('image page');
        expect(result.pages[0].regions).toEqual([]);
        expect((result.pages[0].provenance!.extractionObjects as { objects: unknown[] }).objects[0]).toEqual(original);
    });

    it('allows an intentionally chosen 0×0 table as a blank named worksheet with a warning', () => {
        const { project, pageId, ids } = setup();
        const result = prepareExport(project, { format: 'xlsx', workbookOptions: plan(sheet(pageId, ids[1], 'Empty data')) });
        expect(result.errors).toEqual([]);
        expect(result.warnings.join(' ')).toContain('empty worksheet named “Empty data”');
        expect(table(project, pageId, ids[1]).currentData).toEqual({ version: 1, rows: 0, columns: 0, cells: [] });
    });

    it('retains invalid scope choices for correction and rejects excluded, missing and non-table selections', () => {
        const { project, pageId, otherPageId, ids } = setup();
        const options = plan(sheet(otherPageId, ids[4]));
        const scoped = prepareExport(project, { pageIds: [pageId], format: 'xlsx', workbookOptions: options });
        expect(scoped.errors.join(' ')).toContain('outside the selected scope');
        expect(scoped.workbookOptions).toEqual(options);
        expect(scoped.pages).toEqual([]);
        const excluded = updateExtractionObject(project, pageId, ids[0], { excluded: true });
        expect(prepareWorkbookExport(excluded, undefined, plan(sheet(pageId, ids[0]))).errors.join(' ')).toContain('table is excluded');
        const hidden = clone(project); hidden.pages[0].excluded = true;
        expect(prepareWorkbookExport(hidden, undefined, plan(sheet(pageId, ids[0]))).errors.join(' ')).toContain('page is excluded');
        for (const id of [ids[2], ids[3]]) expect(prepareWorkbookExport(project, undefined, plan(sheet(pageId, id))).errors.join(' ')).toContain('only saved table objects');
        expect(prepareWorkbookExport(project, undefined, plan(sheet(pageId, 'removed'))).errors.join(' ')).toContain('saved table no longer exists');
        expect(prepareWorkbookExport(project, undefined, plan(sheet('removed-page', ids[0]))).errors.join(' ')).toContain('source page no longer exists');
    });

    it('rejects repeated object selection even under different names and validates the bounded option schema', () => {
        const { project, pageId, ids } = setup();
        expect(prepareWorkbookExport(project, undefined, plan(sheet(pageId, ids[0], 'A'), sheet(pageId, ids[0], 'B'))).errors.join(' ')).toContain('select each saved table only once');
        expect(() => readWorkbookOptions({ version: 2, sheets: [] })).toThrow(/version 1/);
        expect(() => readWorkbookOptions({ version: 1, sheets: Array.from({ length: 513 }, (_, i) => sheet('p', 'o' + i, 'S' + i)) })).toThrow(/512/);
        expect(readWorkbookOptions({ version: 1, sheets: Array.from({ length: 512 }, (_, i) => sheet('p', 'o' + i, 'S' + i)) }).sheets).toHaveLength(512);
        expect(() => readWorkbookOptions({ version: 1, sheets: [{ ...sheet('p', 'o'), placement: 'append' }] })).toThrow();
        expect(() => readWorkbookOptions({ version: 1, sheets: [{ ...sheet('p', 'o'), pageId: '' }] })).toThrow();
    });

    it('materializes deterministic legacy tables in the payload while preserving the original raw record', () => {
        const { project, pageId } = setup(), page = project.pages[0];
        delete page.provenance.extractionObjects;
        page.provenance.extraction = { tableRows: [['0012', '=literal']], updatedAt: '2026-01-01T00:00:00.000Z',
            sources: { table: captureExtractionSource(page, page.regions[1], 'pixels') } };
        const legacy = getExtractionObjects(page), before = clone(project);
        const result = prepareExport(project, { format: 'xlsx', workbookOptions: plan(sheet(pageId, legacy.collection.objects[0].id)) });
        expect(result.errors).toEqual([]);
        expect(result.pages[0].provenance!.extractionObjects).toEqual(legacy.collection);
        expect(result.pages[0].provenance!.extraction).toEqual(page.provenance.extraction);
        expect(project).toEqual(before);
    });
});

describe('literal worksheet names', () => {
    it('validates characters, reserved names, apostrophes and UTF-16 boundaries without truncating or trimming', () => {
        for (const name of ['', ' \t\ufeff ', 'History', 'hIsToRy', "'Start", "End'", 'Bad/Name', 'Bad\\Name', 'Bad?', 'Bad*', 'Bad:', '[Bad]', 'A\u0001B', 'A\u0085B', 'A\ufffeB', 'A\uffffB', '😀'.repeat(16)]) {
            expect(worksheetNameError(name), JSON.stringify(name)).not.toBeNull();
        }
        for (const name of ['A'.repeat(31), '😀'.repeat(15) + 'A', "O'Brien", '  Spaced name  ', '_x0041_', 'Résumé']) expect(worksheetNameError(name)).toBeNull();
        const { project, pageId, ids } = setup(), invalid = '😀'.repeat(16);
        const checked = prepareWorkbookExport(project, undefined, plan(sheet(pageId, ids[0], invalid)));
        expect(checked.options.sheets[0].sheetName).toBe(invalid);
        expect(checked.selections[0].errors.join(' ')).toContain('31 characters');
    });

    it('uses conservative Unicode uppercase collision keys and generates only an unused starter name', () => {
        expect(worksheetNameKey('straße')).toBe(worksheetNameKey('STRASSE'));
        const { project, pageId, otherPageId, ids } = setup();
        const checked = prepareWorkbookExport(project, undefined, plan(sheet(pageId, ids[0], 'straße'), sheet(otherPageId, ids[4], 'STRASSE')));
        expect(checked.errors.join(' ')).toContain('already used');
        expect(nextWorksheetName(plan(sheet(pageId, ids[0], 'table 1'), sheet(otherPageId, ids[4], 'Table 3')))).toBe('Table 2');
        expect(worksheetNameKey('é')).not.toBe(worksheetNameKey('e\u0301'));
    });
});

describe('workbook values and limits', () => {
    it('keeps literal values, span metadata and source evidence unchanged, with no Word column restriction', () => {
        const state = setup(), literals = ['0007', '=SUM(A1)', '+12', '-0', '@literal', '€1,250.00', '  spaces  ', '😀 & < >', 'tab\tline\nnext'];
        const project = withRows(state, [Array.from({ length: 64 }, (_, i) => literals[i % literals.length])]);
        const before = clone(project), options = plan(sheet(state.pageId, state.ids[0], 'Literal values'));
        const workbook = prepareExport(project, { format: 'xlsx', workbookOptions: options });
        expect(workbook.errors).toEqual([]);
        expect(table(project, state.pageId, state.ids[0]).currentData.columns).toBe(64);
        expect((workbook.pages[0].provenance!.extractionObjects as { objects: unknown[] }).objects[0]).toEqual(table(project, state.pageId, state.ids[0]));
        expect(project).toEqual(before);
        expect(prepareExport(project, { pageIds: [state.pageId], format: 'docx', officeOptions: { version: 1, objects: [{ pageId: state.pageId, objectId: state.ids[0], placement: 'append' }] } }).errors.join(' ')).toContain('63 table columns');
    });

    it('rejects cells above the UTF-16 or literal-LF limits at the exact coordinate, without modifying them', () => {
        const state = setup(), options = plan(sheet(state.pageId, state.ids[0]));
        for (const text of ['A'.repeat(32_767), '😀'.repeat(16_383) + 'A', '\r\n'.repeat(253), '\r'.repeat(300)]) {
            expect(prepareExport(withRows(state, [[text]]), { format: 'xlsx', workbookOptions: options }).errors).toEqual([]);
        }
        for (const [text, message] of [['😀'.repeat(16_384), '32,767 characters'], ['\n'.repeat(254), '253 line feeds']] as const) {
            const project = withRows(state, [['ordinary', text]]), before = clone(project);
            const result = prepareExport(project, { format: 'xlsx', workbookOptions: options });
            expect(result.errors.join(' ')).toContain('row 1, column 2');
            expect(result.errors.join(' ')).toContain(message);
            expect(result.pages).toEqual([]);
            expect(project).toEqual(before);
        }
    });

    it('rejects XML-invalid cell characters and warns only for actual CR or lowercase-x escape shapes', () => {
        const state = setup(), options = plan(sheet(state.pageId, state.ids[0]));
        for (const character of ['\u0001', '\ufffe', '\uffff']) expect(prepareExport(withRows(state, [[character]]), { format: 'xlsx', workbookOptions: options }).errors.join(' ')).toContain('XML-incompatible');
        for (const text of ['A\rB', '_x0041_', '_x00dF_']) {
            const result = prepareExport(withRows(state, [[text]]), { format: 'xlsx', workbookOptions: options });
            expect(result.errors).toEqual([]);
            expect(result.warnings.join(' ')).toContain('Some spreadsheet readers');
        }
        expect(prepareExport(withRows(state, [['_X0041_']]), { format: 'xlsx', workbookOptions: options }).warnings.join(' ')).not.toContain('Some spreadsheet readers');
        expect(prepareExport(state.project, { format: 'xlsx', workbookOptions: plan(sheet(state.pageId, state.ids[0], '_x0041_')) }).warnings.join(' ')).toContain('Some spreadsheet readers');
    });

    it('counts spanned grid positions across sheets instead of only counting anchor cells', () => {
        const state = setup(), original = table(state.project, state.pageId, state.ids[0]);
        const objects = Array.from({ length: 101 }, (_, i) => ({ ...original, id: 'grid-table-' + i, name: 'Grid ' + i,
            currentData: { version: 1, rows: 100, columns: 100, cells: [{ id: 'grid-cell-' + i, row: 0, column: 0, rowSpan: 100, columnSpan: 100, text: '' }] } }));
        state.project.pages[0].provenance.extractionObjects = { version: 1, objects };
        const sheets = objects.map((object, i) => sheet(state.pageId, object.id, 'Grid ' + i));
        expect(prepareWorkbookExport(state.project, undefined, plan(...sheets.slice(0, 100))).errors).toEqual([]);
        expect(prepareWorkbookExport(state.project, undefined, plan(...sheets)).errors.join(' ')).toContain('1,000,000 total grid positions');
    });

    it('enforces the cumulative UTF-8 budget independently from per-cell and per-table character limits', () => {
        const state = setup(), original = table(state.project, state.pageId, state.ids[0]), page = state.project.pages[0];
        const full = Array.from({ length: 32 }, () => '界'.repeat(31_250)); // 1M UTF-16 units / 3M UTF-8 bytes.
        const remaining = '界'.repeat(369_621) + 'a'; // 64 MiB minus 22 × 3M bytes.
        const last = Array.from({ length: Math.ceil(remaining.length / 32_767) }, (_, i) => remaining.slice(i * 32_767, (i + 1) * 32_767));
        state.project.pages = Array.from({ length: 23 }, (_, i) => {
            const id = 'budget-page-' + i, objectId = 'budget-table-' + i;
            const currentData = tableDataFromRows((i < 22 ? full : last).map(text => [text]), 'budget-cell-' + i);
            const object = { ...original, id: objectId, name: 'Budget ' + i, currentData,
                captures: original.captures.map(capture => ({ ...capture, pageId: id })) };
            return { ...page, id, provenance: { extractionObjects: { version: 1, objects: [object] } } };
        });
        const options = plan(...state.project.pages.map((owner, i) => sheet(owner.id, 'budget-table-' + i, 'Budget ' + i)));
        expect(prepareWorkbookExport(state.project, undefined, options).errors).toEqual([]);
        const collection = state.project.pages[22].provenance.extractionObjects as { objects: { currentData: TableData }[] };
        collection.objects[0].currentData.cells.at(-1)!.text += 'a';
        expect(prepareWorkbookExport(state.project, undefined, options).errors.join(' ')).toContain('64 MiB');
    });

    it('warns about explicitly repeated joined source content without adding source-page sheets', () => {
        const { project, pageId, otherPageId, ids } = setup();
        const preview = prepareContinuationJoin(project, { name: 'Joined', inputs: [
            { pageId, objectId: ids[0], dropHeaderRows: 0 }, { pageId: otherPageId, objectId: ids[4], dropHeaderRows: 1 },
        ] });
        const joined = applyContinuationJoin(project, preview), options = plan(sheet(pageId, preview.result.id, 'Joined'), sheet(pageId, ids[0], 'Source'));
        const result = prepareExport(joined, { pageIds: [pageId], format: 'xlsx', workbookOptions: options });
        expect(result.errors).toEqual([]);
        expect(result.pages.map(page => page.id)).toEqual([pageId]);
        expect(result.workbookOptions).toEqual(options);
        expect(result.warnings.join(' ')).toContain('also selected as a worksheet');
        expect(result.warnings.join(' ')).toContain('pages outside the selected scope');
    });
});
