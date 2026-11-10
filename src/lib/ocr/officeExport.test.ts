import { describe, expect, it } from 'vitest';
import { addDocument, applyRecognition, createProject, prepareExport } from './model';
import { appendExtractionObject, captureExtractionRequest, getExtractionObjects, updateExtractionObject, type ExtractionKind } from './extractionObjects';
import { captureExtractionSource } from './extraction';
import { applyContinuationJoin, prepareContinuationJoin } from './continuationJoin';
import { initializePageStructure, setBlockProperties } from './structure';
import { emptyOfficeExportOptions, prepareOfficeExport, readOfficeExportOptions, type OfficeObjectChoice, type OfficeExportOptions } from './officeExport';

const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
const tableOutput = { html: '<table><tr><th>Item</th><th>Qty</th></tr><tr><td>Valve</td><td>0012</td></tr></table>', rows: 2, cols: 2 };
const choices = (...objects: OfficeObjectChoice[]): OfficeExportOptions => ({ version: 1, objects });
const appendChoice = (pageId: string, objectId: string): OfficeObjectChoice => ({ pageId, objectId, placement: 'append' });

function setup() {
    let project = addDocument(createProject('Word selection'), { source_uri: '/source.pdf', name: 'Source', pages: [
        { image_path: '/first.png', width: 800, height: 1000 }, { image_path: '/second.png', width: 800, height: 1000 },
    ] });
    for (const page of project.pages) project = applyRecognition(project, page.id, { width: 800, height: 1000, regions: [
        { text: 'Exact source text  ', x: 10, y: 10, w: 500, h: 40, kind: 'text' },
        { text: 'Item\tQty\nValve\t0012', x: 10, y: 80, w: 500, h: 150, kind: 'table' },
        { text: 'x^2', x: 10, y: 260, w: 500, h: 60, kind: 'formula' },
        { text: '', x: 10, y: 360, w: 200, h: 200, kind: 'picture' },
    ] });
    const pageId = project.pages[0].id, otherPageId = project.pages[1].id;
    const ids: string[] = [];
    for (const entry of [
        { page: 0, region: 1, kind: 'table', output: tableOutput, name: 'First table' },
        { page: 0, region: 1, kind: 'table', output: tableOutput, name: 'Second table' },
        { page: 0, region: 2, kind: 'formula', output: '\\frac{a}{b}', name: 'Equation' },
        { page: 0, region: 0, kind: 'fields', output: [{ label: 'Account', value: '0007', score: .91 }], name: 'Fields' },
        { page: 1, region: 1, kind: 'table', output: tableOutput, name: 'Continuation' },
    ]) {
        const page = project.pages[entry.page];
        const request = captureExtractionRequest(project, page.id, page.regions[entry.region].id, entry.kind === 'fields' ? 'text' : 'pixels');
        project = appendExtractionObject(project, request, entry.kind as ExtractionKind, entry.output, { name: entry.name, labels: ['Account'] });
        ids.push(request.objectId);
    }
    return { project, pageId, otherPageId, ids, regionId: project.pages[0].regions[1].id };
}

describe('explicit Word extraction selection', () => {
    it('defaults to no objects and preserves an explicitly chosen append order without mutating the project', () => {
        const { project, pageId, ids } = setup(), before = clone(project);
        expect(prepareOfficeExport(project).options).toEqual(emptyOfficeExportOptions());
        const plan = choices(appendChoice(pageId, ids[1]), appendChoice(pageId, ids[0]), appendChoice(pageId, ids[2]));
        const checked = prepareOfficeExport(project, undefined, plan);
        expect(checked.errors).toEqual([]);
        expect(checked.options).toEqual(plan);
        expect(checked.selections.map(item => item.object?.id)).toEqual([ids[1], ids[0], ids[2]]);
        expect(checked.warnings.join(' ')).toContain('editable LaTeX text');
        expect(checked.warnings.join(' ')).toContain('captured OCR passage text');
        expect(project).toEqual(before);
    });

    it('requires an explicit same-kind target and rejects two replacements of the same captured region', () => {
        const { project, pageId, regionId, ids } = setup();
        const implicit = choices({ pageId, objectId: ids[0], placement: 'replace-region' });
        expect(prepareOfficeExport(project, undefined, implicit).errors.join(' ')).toContain('choose one included table passage');
        const duplicate = choices(...ids.slice(0, 2).map(objectId => ({ pageId, objectId, placement: 'replace-region' as const, regionId })));
        expect(prepareOfficeExport(project, undefined, duplicate).errors.join(' ')).toContain('already replaces this passage');
        const wrongKind = choices({ pageId, objectId: ids[2], placement: 'replace-region', regionId });
        expect(prepareOfficeExport(project, undefined, wrongKind).errors.join(' ')).toContain('included formula passage');
        const single = choices({ pageId, objectId: ids[0], placement: 'replace-region', regionId });
        const checked = prepareOfficeExport(project, undefined, single);
        expect(checked.errors).toEqual([]);
        expect(checked.warnings.join(' ')).not.toContain('captured OCR passage text');
        expect(project.pages[0].regions[1].text).toBe('Item\tQty\nValve\t0012');
    });

    it('retains out-of-scope choices as errors instead of silently dropping choices or adding pages', () => {
        const { project, pageId, otherPageId, ids } = setup();
        const plan = choices(appendChoice(pageId, ids[0]), appendChoice(otherPageId, ids[4]));
        const result = prepareExport(project, { pageIds: [pageId], format: 'docx', officeOptions: plan });
        expect(result.errors.join(' ')).toContain('outside the selected export scope');
        expect(result.pages).toEqual([]);
        expect(result.officeOptions).toEqual(plan);
        expect(result.text).toBe('');
    });

    it('blocks excluded objects, excluded pages, missing identities, field sets and duplicate choices', () => {
        const state = setup(), { pageId, ids } = state;
        const excluded = updateExtractionObject(state.project, pageId, ids[0], { excluded: true });
        expect(prepareOfficeExport(excluded, undefined, choices(appendChoice(pageId, ids[0]))).errors.join(' ')).toContain('extraction is excluded');
        const hidden = clone(state.project); hidden.pages[0].excluded = true;
        expect(prepareOfficeExport(hidden, undefined, choices(appendChoice(pageId, ids[0]))).errors.join(' ')).toContain('page is excluded');
        expect(prepareOfficeExport(state.project, undefined, choices(appendChoice('removed-page', ids[0]))).errors.join(' ')).toContain('page no longer exists');
        expect(prepareOfficeExport(state.project, undefined, choices(appendChoice(pageId, 'missing-object'))).errors.join(' ')).toContain('extraction no longer exists');
        expect(prepareOfficeExport(state.project, undefined, choices(appendChoice(pageId, ids[3]))).errors.join(' ')).toContain('field sets are available as JSON');
        expect(prepareOfficeExport(state.project, undefined, choices(appendChoice(pageId, ids[0]), appendChoice(pageId, ids[0]))).errors.join(' ')).toContain('only once');
        const excludedRegion = clone(state.project); excludedRegion.pages[0].regions[1].excluded = true;
        expect(prepareOfficeExport(excludedRegion, undefined, choices({ pageId, objectId: ids[0], placement: 'replace-region', regionId: state.regionId })).errors.join(' ')).toContain('included table passage');
    });

    it('permits historical captured values with a warning and preserves their source snapshots', () => {
        const { project, pageId, ids } = setup();
        const historical = clone(project); historical.pages[0].imagePath = '/rotated.png';
        const source = clone(getExtractionObjects(historical.pages[0]).collection.objects[0]);
        const result = prepareExport(historical, { format: 'docx', officeOptions: choices(appendChoice(pageId, ids[0])) });
        expect(result.errors).toEqual([]);
        expect(result.warnings.join(' ')).toContain('historical source');
        const objects = result.pages[0].provenance!.extractionObjects as { objects: unknown[] };
        expect(objects.objects[0]).toEqual(source);
        expect(historical.pages[0].imagePath).toBe('/rotated.png');
    });

    it('warns when a joined table repeats selected source objects and contains rows outside the page scope', () => {
        const state = setup(), { pageId, otherPageId, ids } = state;
        const preview = prepareContinuationJoin(state.project, { name: 'Complete table', inputs: [
            { pageId, objectId: ids[0], dropHeaderRows: 0 },
            { pageId: otherPageId, objectId: ids[4], dropHeaderRows: 1 },
        ] });
        const project = applyContinuationJoin(state.project, preview);
        const plan = choices(appendChoice(pageId, preview.result.id), appendChoice(pageId, ids[0]));
        const result = prepareExport(project, { pageIds: [pageId], format: 'docx', officeOptions: plan });
        expect(result.errors).toEqual([]);
        expect(result.pages.map(page => page.id)).toEqual([pageId]);
        expect(result.officeOptions).toEqual(plan);
        expect(result.warnings.join(' ')).toContain('also selected');
        expect(result.warnings.join(' ')).toContain('outside the selected page scope');
        expect(result.warnings.join(' ')).toContain('captured OCR passage text');
        expect(getExtractionObjects(project.pages[1]).collection.objects[0].excluded).toBe(false);
    });

    it('validates the bounded versioned plan without accepting hidden placement fields', () => {
        expect(() => readOfficeExportOptions({ version: 2, objects: [] })).toThrow(/version 1/);
        expect(() => readOfficeExportOptions({ version: 1, objects: Array.from({ length: 513 }, () => appendChoice('p', 'o')) })).toThrow(/512/);
        expect(() => readOfficeExportOptions({ version: 1, objects: [{ ...appendChoice('p', 'o'), regionId: 'r' }] })).toThrow(/invalid identity or placement/);
        expect(() => readOfficeExportOptions({ version: 1, objects: [{ ...appendChoice('p', 'o'), guessedLatest: true }] })).toThrow();
        expect(prepareOfficeExport(setup().project, undefined, { version: 7 }).errors).toHaveLength(1);
    });
});

describe('Word-specific document projection', () => {
    it('derives legacy logical order and included figures without changing the saved project', () => {
        const { project } = setup();
        project.pages[0].regions[0].text = '  literal\r\ntext  ';
        project.pages[0].regions[0].originalText = '  literal\r\ntext  ';
        const before = clone(project), result = prepareExport(project, { format: 'docx' });
        expect(result.errors).toEqual([]);
        expect(result.pages[0].structure!.blocks.map(block => block.sourceRegionId)).toEqual(project.pages[0].regions.map(region => region.id));
        expect(result.pages[0].figure_regions!.map(region => region.id)).toEqual([project.pages[0].regions[3].id]);
        expect(result.pages[0].regions[0].text).toBe('  literal\r\ntext  ');
        expect(result.officeOptions).toEqual(emptyOfficeExportOptions());
        expect(project).toEqual(before);
        expect(project.pages[0].structure).toBeUndefined();
        expect(result.warnings.join(' ')).toContain('carriage returns');
        expect(result.warnings.join(' ')).not.toContain('separate capability');
    });

    it('materializes a deterministic legacy extraction projection and retains the exact saved raw record', () => {
        let project = setup().project;
        project = clone(project);
        const page = project.pages[0]; delete page.provenance.extractionObjects;
        page.provenance.extraction = { tableRows: [['0012', '=literal']], updatedAt: '2026-01-01T00:00:00.000Z',
            sources: { table: captureExtractionSource(page, page.regions[1], 'pixels') } };
        const legacy = getExtractionObjects(page), before = clone(project);
        expect(legacy.migrated).toBe(true);
        const objectId = legacy.collection.objects[0].id;
        const result = prepareExport(project, { format: 'docx', officeOptions: choices(appendChoice(page.id, objectId)) });
        expect(result.errors).toEqual([]);
        expect(result.pages[0].provenance!.extraction).toEqual(page.provenance.extraction);
        expect(result.pages[0].provenance!.extractionObjects).toEqual(legacy.collection);
        expect(project).toEqual(before);
    });

    it('applies Word physical-page and table-column limits only to Word output', () => {
        let { project, pageId } = setup();
        project.pages[0].widthPoints = 1585;
        expect(prepareExport(project, { format: 'docx' }).errors.join(' ')).toContain('1584 points');
        expect(prepareExport(project, { format: 'pdf' }).errors).toEqual([]);
        project.pages[0].widthPoints = 1584;
        const request = captureExtractionRequest(project, pageId, null, 'pixels');
        project = appendExtractionObject(project, request, 'table', { html: '<table><tr>' + Array.from({ length: 64 }, (_, i) => `<td>${i}</td>`).join('') + '</tr></table>', rows: 1, cols: 64 });
        const plan = choices(appendChoice(pageId, request.objectId));
        expect(prepareExport(project, { format: 'docx', officeOptions: plan }).errors.join(' ')).toContain('at most 63 table columns');
        expect(prepareExport(project, { format: 'json', officeOptions: plan }).errors).toEqual([]);
        expect(prepareExport(project, { format: 'json', officeOptions: plan }).officeOptions).toBeUndefined();
    });

    it('does not submit a selected empty table to the Word writer', () => {
        let { project, pageId } = setup();
        const request = captureExtractionRequest(project, pageId, null, 'pixels');
        project = appendExtractionObject(project, request, 'table', { html: '<table></table>', rows: 0, cols: 0 });
        const result = prepareExport(project, { format: 'docx', officeOptions: choices(appendChoice(pageId, request.objectId)) });
        expect(result.errors.join(' ')).toContain('table is empty');
        expect(result.pages).toEqual([]);
    });

    it('discloses real numbering alongside unchanged literal list markers', () => {
        let { project, pageId } = setup();
        project = initializePageStructure(project, pageId);
        const block = project.pages[0].structure!.blocks[0];
        project = setBlockProperties(project, pageId, block.id, { role: 'list-item', list: { kind: 'number', level: 0, start: 1 } });
        const result = prepareExport(project, { format: 'docx' });
        expect(result.errors).toEqual([]);
        expect(result.warnings.join(' ')).toContain('literal bullets and numbers');
        expect(result.pages[0].structure!.blocks[0]).toMatchObject({ role: 'list-item', list: { kind: 'number', level: 0, start: 1 } });
        expect(result.pages[0].regions[0].text).toBe(project.pages[0].regions[0].text);
    });
});
