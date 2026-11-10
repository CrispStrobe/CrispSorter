import { describe, expect, it } from 'vitest';
import { addDocument, createProject, type OcrProject } from './model';
import {
    appendExtractionObject, appendJoinedTableObject, captureExtractionRequest, editExtractionCell,
    extractionObjectFreshness, getExtractionObjects, readExtractionObjects, readTableJoinReceipt, replaceExtractionTableData,
    type ExtractionObjects,
} from './extractionObjects';
import { prepareContinuationJoin, applyContinuationJoin, type ContinuationJoinRequest, type ContinuationJoinPreview } from './continuationJoin';
import { tableDataFromRows, tableDataToRows } from './tableData';
import type { TableExtractionObject } from './tableOperations';

const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const a='<table><tr><th>Item</th><th>Qty</th></tr><tr><td>Screw</td><td>0012</td></tr></table>';
const b='<table><tr><th>Item</th><th>Qty</th></tr><tr><td>Nut</td><td>7</td></tr></table>';
const c='<table><tr><th>Qty</th><th>Item</th><th>Reference</th></tr><tr><td>5</td><td>Bolt</td><td>Ref-3</td></tr></table>';
function setup(html=[a,b,c]) {
    let project=addDocument(createProject(),{source_uri:'/tables.pdf',name:'Tables',
        pages:html.map((_,index)=>({image_path:'/page-'+index+'.png',width:600,height:800}))});
    const ids:string[]=[];
    for(let index=0;index<html.length;index++) {
        const request=captureExtractionRequest(project,project.pages[index].id,null,'pixels');
        project=appendExtractionObject(project,request,'table',{html:html[index],rows:2,cols:2});
        ids.push(request.objectId);
    }
    return {project,ids};
}
const table=(project:OcrProject,page:number,id:string)=>getExtractionObjects(project.pages[page]).collection.objects.find(object=>object.id===id)! as TableExtractionObject;
function request(state:ReturnType<typeof setup>,indices=[0,1]):ContinuationJoinRequest {
    return {name:'Joined table',inputs:indices.map((index,position)=>({pageId:state.project.pages[index].id,objectId:state.ids[index],dropHeaderRows:position?1:0}))};
}
const run=(state:ReturnType<typeof setup>,join=request(state))=>prepareContinuationJoin(state.project,join);

describe('explicit continuation projection',()=>{
    it('joins exact corrected strings in explicit input order and leaves sources untouched',()=>{
        const state=setup();const source=table(state.project,1,state.ids[1]);
        state.project=editExtractionCell(state.project,state.project.pages[1].id,state.ids[1],source.currentData.cells[3].id,'007\n=literal');
        const before=clone(state.project),preview=run(state);
        expect(tableDataToRows(preview.result.currentData)).toEqual([['Item','Qty'],['Screw','0012'],['Nut','007\n=literal']]);
        expect(preview.removedHeaderCells.map(value=>value.cell.text)).toEqual(['Item','Qty']);
        expect(preview.result.originalOutputKind).toBe('table-join');
        expect(preview.result.tableSources!.snapshots).toHaveLength(2);
        expect(preview.result.tableSources!.snapshots[1].currentData.cells[3].text).toBe('007\n=literal');
        expect(preview.result.tableSources!.snapshots[1].originalData.cells[3].text).toBe('7');
        const next=applyContinuationJoin(state.project,preview);
        expect(table(next,0,state.ids[0])).toEqual(table(before,0,state.ids[0]));
        expect(table(next,1,state.ids[1])).toEqual(table(before,1,state.ids[1]));
        expect(getExtractionObjects(next.pages[0]).collection.objects).toHaveLength(2);
        expect(getExtractionObjects(next.pages[1]).collection.objects).toHaveLength(1);
        expect(state.project).toEqual(before);
    });

    it('requires complete explicit unequal-width mappings and adds only empty unmeasured cells',()=>{
        const state=setup(),unequal=request(state,[0,2]);
        expect(()=>run(state,unequal)).toThrow(/explicit target/);
        unequal.targetColumns=3;unequal.inputs[0].columnMap=[0,1];
        expect(()=>run(state,unequal)).toThrow(/complete mapping/);
        unequal.inputs[1].columnMap=[1,0,2];
        const preview=run(state,unequal),receipt=readTableJoinReceipt(preview.result.originalOutput);
        expect(tableDataToRows(preview.result.currentData)).toEqual([['Item','Qty',''],['Screw','0012',''],['Bolt','5','Ref-3']]);
        expect(preview.warnings.join(' ')).toContain("Removed header rows differ from the first table's retained header rows.");
        expect(receipt.blankCellIds).toHaveLength(2);
        for(const id of receipt.blankCellIds)expect(preview.result.currentData.cells.find(cell=>cell.id===id)).toMatchObject({text:'',rowSpan:1,columnSpan:1});
        expect(preview.result.currentData.cells.filter(cell=>receipt.blankCellIds.includes(cell.id)).every(cell=>!cell.box&&cell.header===undefined)).toBe(true);
        for(const map of [[0,0,2],[0,1],[-1,0,2],[0,1,3]]) {
            const invalid=clone(unequal);invalid.inputs[1].columnMap=map;
            expect(()=>run(state,invalid)).toThrow();
        }
    });

    it('permits reordered whole spans but rejects disconnected retained spans and header cuts',()=>{
        const span='<table><tr><th colspan="2">Pair</th><th>Other</th></tr><tr><td>A</td><td>B</td><td>C</td></tr></table>';
        const state=setup([span,span]),join=request(state);
        join.inputs[1].dropHeaderRows=0;join.inputs[1].columnMap=[1,0,2];
        expect(run(state,join).result.currentData.cells.find(cell=>cell.row===2&&cell.columnSpan===2)!.text).toBe('Pair');
        join.inputs[1].columnMap=[0,2,1];
        expect(()=>run(state,join)).toThrow(/consecutive destination/);
        join.inputs[1].dropHeaderRows=1;
        expect(tableDataToRows(run(state,join).result.currentData).at(-1)).toEqual(['A','C','B']);
        const crossing=setup([a,'<table><tr><th rowspan="2">Shared</th><th>Qty</th></tr><tr><td>7</td></tr></table>']);
        expect(()=>run(crossing)).toThrow(/cuts through a spanning cell/);
    });

    it('shows exact removal and warnings even when continuation rows are not headers or all are removed',()=>{
        const state=setup([a,'<table><tr><td>Unmarked 😀</td><td></td></tr><tr><td>A&#13;\nB</td><td>01</td></tr></table>']);
        const join=request(state);join.inputs[1].dropHeaderRows=2;
        const preview=run(state,join);
        expect(preview.removedHeaderCells.map(value=>value.cell.text)).toEqual(['Unmarked 😀','','A\r\nB','01']);
        expect(preview.warnings.join(' ')).toContain('Some removed cells are not marked as headers.');
        expect(preview.warnings.join(' ')).toContain('All rows from this continuation are removed from the joined grid.');
        expect(tableDataToRows(preview.result.currentData)).toEqual([['Item','Qty'],['Screw','0012']]);
        expect(preview.result.tableSources!.snapshots[1].currentData.cells[2].text).toBe('A\r\nB');
    });

    it('keeps genuine source boxes at their capture while logical row positions move',()=>{
        const state=setup(),page=state.project.pages[1],collection=page.provenance.extractionObjects as ExtractionObjects;
        const source=collection.objects[0] as TableExtractionObject;
        for(const data of [source.originalData,source.currentData])data.cells[2].box={captureId:source.captures[0].id,bounds:{x:10,y:50,w:100,h:30}};
        const preview=run(state),cell=preview.result.currentData.cells.find(cell=>cell.text==='Nut')!;
        expect(cell.row).toBe(2);expect(cell.box).toEqual(source.currentData.cells[2].box);
        expect(cell.id).not.toBe(source.currentData.cells[2].id);
        expect(preview.result.captures.some(capture=>capture.id===cell.box!.captureId)).toBe(true);
    });

    it('allows explicitly excluded or historical inputs and keeps their flags unchanged',()=>{
        const state=setup();state.project.pages[0].excluded=true;state.project.pages[1].imagePath='/changed.png';
        (state.project.pages[1].provenance.extractionObjects as ExtractionObjects).objects[0].excluded=true;
        const preview=run(state),before=clone(state.project);
        expect(preview.warnings.join(' ')).toContain('historical sources');
        expect(preview.warnings.join(' ')).toContain('remains excluded');
        expect(preview.result.excluded).toBe(false);
        const next=applyContinuationJoin(state.project,preview);
        expect(next.pages[0].excluded).toBe(true);expect(table(next,1,state.ids[1]).excluded).toBe(true);
        expect(table(next,1,state.ids[1])).toEqual(table(before,1,state.ids[1]));
    });

    it('rejects invalid selection, first-header removal, empty grids and declared size limits',()=>{
        const state=setup(),join=request(state);
        expect(()=>run(state,{...join,inputs:[join.inputs[0]]})).toThrow(/2–16/);
        expect(()=>run(state,{...join,inputs:[join.inputs[0],join.inputs[0]]})).toThrow(/only once/);
        const first=clone(join);first.inputs[0].dropHeaderRows=1;expect(()=>run(state,first)).toThrow(/first table/);
        const empty=setup(['<table></table>',b]);expect(()=>run(empty)).toThrow(/nonempty grid/);
        state.project=replaceExtractionTableData(state.project,state.project.pages[0].id,state.ids[0],tableDataFromRows(Array.from({length:1000},()=>['']),'large'));
        const large=request(state);large.inputs[1].dropHeaderRows=0;large.targetColumns=2;
        large.inputs[0].columnMap=[0];large.inputs[1].columnMap=[0,1];
        expect(()=>run(state,large)).toThrow(/1,000-row/);
        const text=setup(['<table><tr><td>'+ 'x'.repeat(550_000)+'</td></tr></table>','<table><tr><td>'+ 'y'.repeat(550_000)+'</td></tr></table>']);
        const tooLong=request(text);tooLong.inputs[1].dropHeaderRows=0;
        expect(()=>run(text,tooLong)).toThrow(/1,000,000/);
    });
});

describe('flat continuation evidence and strict preview application',()=>{
    it('retains flat native ancestors and an immutable intermediate join input',()=>{
        const state=setup([a,b,b]),first=run(state),withFirst=applyContinuationJoin(state.project,first);
        const second=prepareContinuationJoin(withFirst,{name:'Three pages',inputs:[
            {pageId:withFirst.pages[0].id,objectId:first.result.id,dropHeaderRows:0},
            {pageId:withFirst.pages[2].id,objectId:state.ids[2],dropHeaderRows:1},
        ]});
        expect(second.result.tableSources!.snapshots).toHaveLength(4);
        expect(second.result.tableSources!.snapshots.every(snapshot=>!('tableSources' in snapshot))).toBe(true);
        const joinedInput=second.result.tableSources!.snapshots.find(snapshot=>snapshot.objectId===first.result.id)!;
        expect(joinedInput.originalOutputKind).toBe('table-join');
        expect(joinedInput.currentData).toEqual(first.result.currentData);
        expect(joinedInput.sourceUpdatedAt).toBe(first.result.updatedAt);
        const standalone=readExtractionObjects({version:1,objects:[JSON.parse(JSON.stringify(second.result))]})!;
        expect(standalone.objects[0]).toEqual(second.result);
    });

    it('rejects malformed snapshot graphs, nested histories, lost cell origins and inferred blank text',()=>{
        const preview=run(setup()),result=preview.result;
        const invalids:TableExtractionObject[]=[];
        const missing=clone(result);(missing.originalOutput as unknown as ReturnType<typeof readTableJoinReceipt>).inputs[0].snapshotId='missing';invalids.push(missing);
        const repeated=clone(result);repeated.tableSources!.snapshots.push(clone(repeated.tableSources!.snapshots[0]));invalids.push(repeated);
        const nested=clone(result);Object.assign(nested.tableSources!.snapshots[0],{tableSources:{version:1,snapshots:[]}});invalids.push(nested);
        const noSources=clone(result);delete noSources.tableSources;invalids.push(noSources);
        const noOrigin=clone(result);(noOrigin.originalOutput as unknown as ReturnType<typeof readTableJoinReceipt>).cellOrigins.pop();invalids.push(noOrigin);
        const changedValue=clone(result);changedValue.originalData.cells[0].text='Changed baseline';invalids.push(changedValue);
        const cycle=clone(result),source=cycle.tableSources!.snapshots[0];source.originalOutputKind='table-join';source.originalOutput=clone(cycle.originalOutput);
        invalids.push(cycle);
        for(const invalid of invalids)expect(()=>readExtractionObjects({version:1,objects:[invalid]})).toThrow();
        expect(()=>readExtractionObjects({version:1,objects:[{...result,kind:'formula'}]})).toThrow(/only a table join/);
        const nonjoin=clone(result);nonjoin.originalOutputKind='model';
        expect(()=>readExtractionObjects({version:1,objects:[nonjoin]})).toThrow(/only a table join/);
    });

    it('rejects source and preview changes even when timestamps collide or an image was already historical',()=>{
        const state=setup(),preview=run(state),saved=JSON.stringify(state.project);
        const altered=clone(state.project),source=(altered.pages[1].provenance.extractionObjects as ExtractionObjects).objects[0] as TableExtractionObject;
        source.currentData.cells[3].text='8'; // Keep the exact original updatedAt intentionally.
        expect(()=>applyContinuationJoin(altered,preview)).toThrow(/preview changed/);
        const mutations:((value:ContinuationJoinPreview)=>void)[]=[
            value=>{value.result.currentData.cells[0].text='Forged';},
            value=>{value.removedHeaderCells[0].cell.text='Hidden';},
            value=>{value.inputs[0].object.originalOutput='Forged native output';},
            value=>{value.warnings.push('Forged warning');},
        ];
        for(const mutate of mutations){const forged=clone(preview);mutate(forged);expect(()=>applyContinuationJoin(state.project,forged)).toThrow(/preview changed/);}
        expect(JSON.stringify(state.project)).toBe(saved);
        state.project.pages[1].imagePath='/already-historical.png';const historical=run(state);
        state.project.pages[1].imagePath='/changed-again.png';
        expect(()=>applyContinuationJoin(state.project,historical)).toThrow(/preview changed/);
    });

    it('detects post-join input data edits even with an unchanged timestamp and preserves standalone snapshots',()=>{
        const state=setup(),preview=run(state),joined=applyContinuationJoin(state.project,preview);
        expect(extractionObjectFreshness(joined,preview.result).current).toBe(true);
        const data=(joined.pages[1].provenance.extractionObjects as ExtractionObjects).objects[0] as TableExtractionObject;
        const updatedAt=data.updatedAt;data.currentData.cells[3].text='Changed';expect(data.updatedAt).toBe(updatedAt);
        expect(extractionObjectFreshness(joined,preview.result)).toMatchObject({current:false});
        expect(extractionObjectFreshness(joined,preview.result).reasons.join(' ')).toContain('source table was edited');
        expect(preview.result.tableSources!.snapshots[1].currentData.cells[3].text).toBe('7');
    });

    it('requires all retained snapshots and refuses conflicting shared ancestry',()=>{
        const state=setup([a,b,b]),first=run(state),joined=applyContinuationJoin(state.project,first);
        const conflicting=clone(first.result);conflicting.id='conflicting-join';
        conflicting.tableSources!.snapshots[0].originalOutput={different:'raw source record'};
        const withConflict=appendJoinedTableObject(joined,joined.pages[2].id,conflicting);
        expect(()=>prepareContinuationJoin(withConflict,{name:'Conflict',inputs:[
            {pageId:withConflict.pages[0].id,objectId:first.result.id,dropHeaderRows:0},
            {pageId:withConflict.pages[2].id,objectId:conflicting.id,dropHeaderRows:1},
        ]})).toThrow(/conflicting content/);
        const unused=clone(first.result),extra=clone(unused.tableSources!.snapshots[0]);extra.id='unused';
        unused.tableSources!.snapshots.push(extra);
        expect(()=>readExtractionObjects({version:1,objects:[unused]})).toThrow(/unreferenced/);
        const tooMany=clone(first.result);
        tooMany.tableSources!.snapshots=Array.from({length:129},(_,index)=>({...extra,id:'snapshot-'+index}));
        expect(()=>readExtractionObjects({version:1,objects:[tooMany]})).toThrow(/128-snapshot/);
    });
});
