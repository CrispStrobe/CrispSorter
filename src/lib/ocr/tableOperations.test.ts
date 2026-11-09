import { describe, expect, it } from 'vitest';
import { addDocument, applyRecognition, approvePage, commitHistory, createHistory, createProject, parseProject, serializeProject, undoHistory, type OcrProject } from './model';
import { appendExtractionObject, captureExtractionRequest, editExtractionCell, editExtractionFormula, getExtractionObjects, replaceExtractionTableData, type ExtractionObjects } from './extractionObjects';
import { prepareTableOperation, applyTableOperation, type TableExtractionObject, type TableOperationRequest, type TableOperationPreview } from './tableOperations';
import { tableDataFromRows, tableDataToRows } from './tableData';

const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const plain='<table><tr><td>A</td><td>B</td><td>C</td></tr><tr><td>D</td><td>E</td><td>F</td></tr></table>';
const spanning='<table><tr><th rowspan="2">A</th><th colspan="2">B</th></tr><tr><td>C</td><td>D</td></tr></table>';
function setup(html=plain,boxes=true,method:'pixels'|'text'='pixels') {
    let project=addDocument(createProject(),{source_uri:'/source.pdf',name:'Source',
        pages:[{image_path:'/page.png',width:600,height:800},{image_path:'/other.png',width:600,height:800}]});
    project=applyRecognition(project,project.pages[0].id,{width:600,height:800,regions:[{text:'Source text',x:10,y:10,w:300,h:100}]});
    const request=captureExtractionRequest(project,project.pages[0].id,null,method);
    project=appendExtractionObject(project,request,'table',{html,rows:2,cols:3});
    const collection=getExtractionObjects(project.pages[0]).collection,table=collection.objects[0] as TableExtractionObject;
    if(boxes)for(const data of [table.originalData,table.currentData])for(const cell of data.cells)
        cell.box={captureId:request.capture.id,bounds:{x:cell.column*100,y:cell.row*50,w:cell.columnSpan*100,h:cell.rowSpan*50}};
    project.pages[0].provenance.extractionObjects=collection;
    return {project,pageId:project.pages[0].id,objectId:request.objectId};
}
const table=(project:OcrProject,id:string)=>getExtractionObjects(project.pages[0]).collection.objects.find(object=>object.id===id)! as TableExtractionObject;
const preview=(state:ReturnType<typeof setup>,request:TableOperationRequest)=>prepareTableOperation(state.project,state.pageId,state.objectId,request)!;
function sorted(value:unknown):unknown {
    if(Array.isArray(value))return value.map(sorted);
    if(value&&typeof value==='object')return Object.fromEntries(Object.keys(value).sort().map(key=>[key,sorted((value as Record<string,unknown>)[key])]));
    return value;
}

describe('table row and column repairs',()=>{
    it('inserts a blank row and shifts existing cells without changing their identities or source boxes',()=>{
        const state=setup(),before=table(state.project,state.objectId),saved=JSON.stringify(state.project);
        const operation=preview(state,{kind:'insert-row',index:1});
        expect(operation.after.rows).toBe(3);expect(operation.createdCellIds).toHaveLength(3);
        expect(tableDataToRows(operation.after)).toEqual([['A','B','C'],['','',''],['D','E','F']]);
        for(const old of before.currentData.cells) {
            const next=operation.after.cells.find(cell=>cell.id===old.id)!;
            expect(next).toEqual({...old,row:old.row>=1?old.row+1:old.row});
        }
        expect(operation.after.cells.filter(cell=>operation.createdCellIds.includes(cell.id)).every(cell=>!cell.box)).toBe(true);
        expect(JSON.stringify(state.project)).toBe(saved);
        const applied=applyTableOperation(state.project,operation),current=table(applied,state.objectId);
        expect(current.originalData).toEqual(before.originalData);expect(current.originalOutput).toEqual(before.originalOutput);
        expect(current.captures).toEqual(before.captures);expect(current.review).toBe('corrected');
    });

    it('grows only spans crossed by a row insertion and removes only their current boxes',()=>{
        const state=setup(spanning),before=table(state.project,state.objectId).currentData,operation=preview(state,{kind:'insert-row',index:1});
        expect(tableDataToRows(operation.after)).toEqual([['A','B',''],['','',''],['','C','D']]);
        const a=operation.after.cells.find(cell=>cell.text==='A')!,b=operation.after.cells.find(cell=>cell.text==='B')!,c=operation.after.cells.find(cell=>cell.text==='C')!;
        expect(a).toMatchObject({id:before.cells[0].id,rowSpan:3,text:'A'});expect(a.box).toBeUndefined();
        expect(b.box).toEqual(before.cells[1].box);expect(c.box).toEqual(before.cells[2].box);
        expect(operation.warnings.join(' ')).toContain('spanning cell(s) extend');
    });

    it('grows column spans and keeps shifted cell source associations',()=>{
        const state=setup(spanning),before=table(state.project,state.objectId).currentData,operation=preview(state,{kind:'insert-column',index:2});
        expect(tableDataToRows(operation.after)).toEqual([['A','B','',''],['','C','','D']]);
        const b=operation.after.cells.find(cell=>cell.text==='B')!,d=operation.after.cells.find(cell=>cell.text==='D')!;
        expect(b.columnSpan).toBe(3);expect(b.box).toBeUndefined();
        expect(d).toEqual({...before.cells[3],column:3});expect(operation.createdCellIds).toHaveLength(1);
    });

    it('retains all text in a partially deleted span and lists exact fully removed values',()=>{
        const state=setup(spanning),before=table(state.project,state.objectId).currentData;
        const operation=preview(state,{kind:'delete-row',index:0});
        expect(tableDataToRows(operation.after)).toEqual([['A','C','D']]);
        expect(operation.deletedCells.map(cell=>cell.text)).toEqual(['B']);
        const a=operation.after.cells.find(cell=>cell.text==='A')!,c=operation.after.cells.find(cell=>cell.text==='C')!;
        expect(a).toMatchObject({id:before.cells[0].id,row:0,rowSpan:1,text:'A',header:true});expect(a.box).toBeUndefined();
        expect(c).toEqual({...before.cells[2],row:0});
        expect(operation.warnings.join(' ')).toContain('top-left surviving area');
    });

    it('deletes a column through spans while preserving every surviving cell value',()=>{
        const state=setup(spanning),before=table(state.project,state.objectId).currentData;
        const operation=preview(state,{kind:'delete-column',index:1});
        expect(tableDataToRows(operation.after)).toEqual([['A','B'],['','D']]);
        expect(operation.deletedCells.map(cell=>cell.text)).toEqual(['C']);
        expect(operation.after.cells.find(cell=>cell.text==='B')).toMatchObject({id:before.cells[1].id,column:1,columnSpan:1,text:'B'});
        expect(operation.after.cells.find(cell=>cell.text==='B')!.box).toBeUndefined();
        expect(operation.after.cells.find(cell=>cell.text==='D')!.box).toEqual(before.cells[3].box);
    });

    it('shows exact whitespace, line endings and empty values that deletion will remove',()=>{
        const state=setup('<table><tr><td>😀 A&#13;\n"B"</td><td></td></tr><tr><td>C</td><td>D</td></tr></table>');
        const operation=preview(state,{kind:'delete-row',index:0});
        expect(operation.deletedCells.map(cell=>cell.text)).toEqual(['😀 A\r\n"B"','']);
        expect(tableDataToRows(operation.after)).toEqual([['C','D']]);
    });

    it.each(['insert-row','insert-column'] as const)('initializes an empty table through %s and never produces a half-empty grid',kind=>{
        const state=setup('<table></table>',false),operation=preview(state,{kind,index:0});
        expect(operation.after).toMatchObject({rows:1,columns:1,cells:[{text:'',row:0,column:0,rowSpan:1,columnSpan:1}]});
        const inserted=applyTableOperation(state.project,operation);
        const deletion=prepareTableOperation(inserted,state.pageId,state.objectId,{kind:kind==='insert-row'?'delete-row':'delete-column',index:0})!;
        expect(deletion.after).toEqual({version:1,rows:0,columns:0,cells:[]});
        expect(deletion.deletedCells).toHaveLength(1);
    });

    it('rejects invalid indices and size limits before changing table data',()=>{
        const state=setup();
        for(const request of [{kind:'insert-row',index:-1},{kind:'insert-column',index:4},{kind:'delete-row',index:2},{kind:'delete-column',index:1.5}] as TableOperationRequest[])
            expect(()=>preview(state,request)).toThrow();
        const large=tableDataFromRows(Array.from({length:1000},()=>Array.from({length:10},()=>'')),'large');
        state.project=replaceExtractionTableData(state.project,state.pageId,state.objectId,large);
        const saved=JSON.stringify(state.project);
        expect(()=>preview(state,{kind:'insert-row',index:0})).toThrow(/limit/);
        expect(()=>preview(state,{kind:'insert-column',index:0})).toThrow(/limit/);
        expect(JSON.stringify(state.project)).toBe(saved);
    });
});

describe('explicit whole-cell merge, split and headers',()=>{
    it.each(['',' ','\n','\n\n'] as const)('merges row-major literal values with explicit separator %j',separator=>{
        const state=setup(),before=table(state.project,state.objectId),ids=before.currentData.cells.slice(0,3).map(cell=>cell.id).reverse();
        const operation=preview(state,{kind:'merge',cellIds:ids,separator});
        expect(operation.mergedCells.map(cell=>cell.text)).toEqual(['A','B','C']);
        expect(operation.mergeText).toBe(['A','B','C'].join(separator));
        expect(operation.createdCellIds).toHaveLength(1);
        const merged=operation.after.cells[0];
        expect(merged).toMatchObject({row:0,column:0,rowSpan:1,columnSpan:3,text:operation.mergeText});
        expect(merged.box).toBeUndefined();expect(ids).not.toContain(merged.id);
        expect(operation.deletedCells).toEqual([]);
        const applied=applyTableOperation(state.project,operation);
        expect(table(applied,state.objectId).originalData).toEqual(before.originalData);
        expect(table(applied,state.objectId).originalOutput).toEqual(before.originalOutput);
    });

    it('requires a whole rectangle and includes empty cell values rather than skipping them',()=>{
        const state=setup(),cells=table(state.project,state.objectId).currentData.cells;
        expect(()=>preview(state,{kind:'merge',cellIds:[cells[0].id,cells[4].id],separator:' '})).toThrow(/rectangle/);
        expect(()=>preview(state,{kind:'merge',cellIds:[cells[0].id],separator:' '})).toThrow(/whole table cells/);
        expect(()=>preview(state,{kind:'merge',cellIds:[cells[0].id,cells[0].id],separator:' '})).toThrow(/distinct/);
        expect(()=>preview(state,{kind:'merge',cellIds:[cells[0].id,cells[1].id],separator:','} as never)).toThrow(/separator/);
        state.project=editExtractionCell(state.project,state.pageId,state.objectId,cells[1].id,'');
        const operation=preview(state,{kind:'merge',cellIds:cells.slice(0,3).map(cell=>cell.id),separator:' '});
        expect(operation.mergeText).toBe('A  C');
    });

    it('keeps all-header merges marked and discloses mixed header/body resets',()=>{
        const state=setup(spanning),cells=table(state.project,state.objectId).currentData.cells;
        const all=preview(state,{kind:'merge',cellIds:cells.map(cell=>cell.id),separator:'\n'});
        expect(all.after.cells[0].header).toBeUndefined();expect(all.warnings.join(' ')).toContain('mixed header/body');
        const plainState=setup('<table><tr><th>A</th><th>B</th></tr></table>');
        const headers=table(plainState.project,plainState.objectId).currentData.cells;
        expect(preview(plainState,{kind:'merge',cellIds:headers.map(cell=>cell.id),separator:' '}).after.cells[0].header).toBe(true);
    });

    it('splits a span with all literal text at top-left and fresh empty cells elsewhere',()=>{
        const state=setup('<table><tr><th colspan="2" rowspan="2">A; B<br>C</th></tr><tr></tr></table>');
        const source=table(state.project,state.objectId).currentData.cells[0],operation=preview(state,{kind:'split',cellId:source.id});
        expect(tableDataToRows(operation.after)).toEqual([['A; B\nC',''],['','']]);
        expect(operation.after.cells[0].id).toBe(source.id);expect(operation.createdCellIds).toHaveLength(3);
        expect(operation.after.cells.every(cell=>cell.header===true&&!cell.box&&cell.rowSpan===1&&cell.columnSpan===1)).toBe(true);
        expect(operation.deletedCells).toEqual([]);expect(operation.mergedCells).toEqual([]);
        const one=setup();
        expect(()=>preview(one,{kind:'split',cellId:table(one.project,one.objectId).currentData.cells[0].id})).toThrow(/spanning more than one/);
    });

    it('toggles only explicit header roles and retains source boxes and text',()=>{
        const state=setup(),before=table(state.project,state.objectId).currentData,cells=before.cells.slice(0,2);
        expect(prepareTableOperation(state.project,state.pageId,state.objectId,{kind:'header',cellIds:cells.map(cell=>cell.id),header:false})).toBeNull();
        const operation=preview(state,{kind:'header',cellIds:cells.map(cell=>cell.id),header:true});
        expect(operation.after.cells.slice(0,2)).toEqual(cells.map(cell=>({...cell,header:true})));
        expect(operation.after.cells.slice(2)).toEqual(before.cells.slice(2));expect(operation.createdCellIds).toEqual([]);
        const applied=applyTableOperation(state.project,operation);
        const cleared=prepareTableOperation(applied,state.pageId,state.objectId,{kind:'header',cellIds:cells.map(cell=>cell.id),header:false})!;
        expect(cleared.after).toEqual(before);
    });
});

describe('immutable table repair previews',()=>{
    it('rejects tampered output, request, removed text, source boxes and warnings without mutating data',()=>{
        const state=setup(),operation=preview(state,{kind:'delete-row',index:0}),saved=JSON.stringify(state.project);
        const mutations:((value:TableOperationPreview)=>void)[]=[
            value=>{value.after.cells[0].text='Forged';},
            value=>{value.deletedCells[0].text='Hidden';},
            value=>{(value.request as {index:number}).index=1;},
            value=>{value.after.cells[0].box!.bounds.x=25;},
            value=>{value.warnings=[];},
            value=>{value.sourceObject.originalOutput='Forged raw';},
        ];
        for(const mutate of mutations){const forged=clone(operation);mutate(forged);expect(()=>applyTableOperation(state.project,forged)).toThrow(/preview changed/);}
        expect(JSON.stringify(state.project)).toBe(saved);
    });

    it('rejects selected object edits and project switching but preserves unrelated extraction edits',()=>{
        const state=setup(),operation=preview(state,{kind:'insert-row',index:0});
        const first=table(state.project,state.objectId).currentData.cells[0].id;
        const changed=editExtractionCell(state.project,state.pageId,state.objectId,first,'Changed');
        expect(()=>applyTableOperation(changed,operation)).toThrow(/preview changed/);
        expect(()=>applyTableOperation({...state.project,id:'different'},operation)).toThrow(/another project/);
        const request=captureExtractionRequest(state.project,state.pageId,null,'pixels');
        let other=appendExtractionObject(state.project,request,'formula','x');
        other=editExtractionFormula(other,state.pageId,request.objectId,'y');
        const applied=applyTableOperation(other,operation);
        expect(getExtractionObjects(applied.pages[0]).collection.objects.find(value=>value.id===request.objectId)!.currentData).toEqual({version:1,latex:'y'});
    });

    it('allows historical tables with explicit warnings but rejects further source changes',()=>{
        const state=setup();state.project.pages[0].imagePath='/historical-replacement.png';
        const operation=preview(state,{kind:'delete-column',index:0});
        expect(operation.warnings[0]).toContain('historical sources');
        expect(applyTableOperation(state.project,operation)).toBeDefined();
        const changed=clone(state.project);changed.pages[0].imagePath='/another-replacement.png';
        expect(()=>applyTableOperation(changed,operation)).toThrow(/preview changed/);
        const missing=clone(state.project);missing.pages[0].regions=[];
        // Full-page pixel input is independent of the current OCR transcription.
        expect(applyTableOperation(missing,operation)).toBeDefined();
    });

    it('binds exact current text for text-derived tables and all captures for future multi-page sources',()=>{
        const state=setup(plain,false,'text'),operation=preview(state,{kind:'insert-row',index:0});
        const changed=clone(state.project);changed.pages[0].regions[0].text='Changed';
        expect(()=>applyTableOperation(changed,operation)).toThrow(/preview changed/);
        const collection=state.project.pages[0].provenance.extractionObjects as ExtractionObjects;
        collection.objects[0].captures.push(captureExtractionRequest(state.project,state.project.pages[1].id,null,'pixels').capture);
        const multiple=preview(state,{kind:'insert-column',index:0});
        const changedOther=clone(state.project);changedOther.pages[1].imagePath='/other-changed.png';
        expect(()=>applyTableOperation(changedOther,multiple)).toThrow(/preview changed/);
    });

    it('rejects changed geometry for an actual selected-region pixel capture',()=>{
        const state=setup(),region=state.project.pages[0].regions[0];
        const request=captureExtractionRequest(state.project,state.pageId,region.id,'pixels');
        const project=appendExtractionObject(state.project,request,'table',{html:plain,rows:2,cols:3});
        const operation=prepareTableOperation(project,state.pageId,request.objectId,{kind:'insert-row',index:0})!;
        const moved=clone(project);moved.pages[0].regions[0].x+=1;
        expect(()=>applyTableOperation(moved,operation)).toThrow(/preview changed/);
        expect(table(project,request.objectId).currentData).toEqual(operation.before);
    });

    it('survives native JSON key sorting, applies one undoable edit and restores original sources',()=>{
        const state=setup();state.project=approvePage(state.project,state.pageId);
        const operation=preview(state,{kind:'merge',cellIds:table(state.project,state.objectId).currentData.cells.slice(0,3).map(cell=>cell.id),separator:' '});
        const restored=parseProject(sorted(JSON.parse(serializeProject(state.project))));
        const applied=applyTableOperation(restored,sorted(operation) as TableOperationPreview);
        expect(applied.pages[0].approved).toBe(false);
        expect(table(applied,state.objectId).originalData).toEqual(table(state.project,state.objectId).originalData);
        const saved=parseProject(serializeProject(applied));
        expect(table(saved,state.objectId).currentData).toEqual(operation.after);
        expect(undoHistory(commitHistory(createHistory(restored),applied)).present).toEqual(restored);
    });
});
