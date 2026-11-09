import { describe, expect, it } from 'vitest';
import {
    addDocument, applyPageImage, applyRecognition, approvePage, commitHistory,
    createHistory, createProject, parseProject, prepareExport, redoHistory,
    removePage, serializeProject, splitPage, undoHistory, type OcrProject,
} from './model';
import {
    appendExtractionObject, captureExtractionRequest, editExtractionCell,
    extractionObjectFreshness, getExtractionObjects, serializeExtractionObject,
    type ExtractionObject,
} from './extractionObjects';
import { prepareContinuationJoin, applyContinuationJoin } from './continuationJoin';
import { prepareExtractionExport } from './extractionExport';
import { tableDataToRows } from './tableData';

type TableObject=Extract<ExtractionObject,{kind:'table'}>;
function table(project:OcrProject,pageId:string,id?:string):TableObject {
    const page=project.pages.find(page=>page.id===pageId)!;
    const found=getExtractionObjects(page).collection.objects.find(object=>object.kind==='table'&&(!id||object.id===id));
    if(!found||found.kind!=='table')throw new Error('Missing fixture table');
    return found;
}
function fixture() {
    let project=addDocument(createProject('Continuation lifecycle'),{
        source_uri:'/source.pdf',name:'Source',pages:[1,2,3].map(index=>({image_path:'/page-'+index+'.png',width:800,height:1000})),
    });
    const values=[['0012','EUR 1,250.00'],['-7','=SUM(A1)'],['@identifier','+15']];
    const inputs:{pageId:string;objectId:string}[]=[];
    for(let index=0;index<project.pages.length;index++) {
        const pageId=project.pages[index].id;
        project=applyRecognition(project,pageId,{width:800,height:1000,regions:[
            {kind:'table',text:'Code Amount\n'+values[index].join(' '),x:20,y:100,w:700,h:250},
        ]});
        const request=captureExtractionRequest(project,pageId,project.pages[index].regions[0].id,'pixels');
        project=appendExtractionObject(project,request,'table',{
            html:'<table><tr><th>Code</th><th>Amount</th></tr><tr><td>'+values[index][0]+'</td><td>'+values[index][1]+'</td></tr></table>',rows:2,cols:2,
        });
        inputs.push({pageId,objectId:request.objectId});
        project=approvePage(project,pageId);
    }
    return {project,inputs};
}
function capture(project:OcrProject,inputs:{pageId:string;objectId:string}[],name='Combined table') {
    return prepareContinuationJoin(project,{name,inputs:inputs.map((input,index)=>({...input,dropHeaderRows:index?1:0}))});
}
function sorted(value:unknown):unknown {
    if(Array.isArray(value))return value.map(sorted);
    if(value&&typeof value==='object')return Object.fromEntries(Object.keys(value).sort().map(key=>[key,sorted((value as Record<string,unknown>)[key])]));
    return value;
}

describe('joined tables across the project lifecycle',()=>{
    it('preserves source objects and exact corrected strings through one-step history, reload and exports',()=>{
        let {project,inputs}=fixture();
        const first=table(project,inputs[0].pageId,inputs[0].objectId);
        const value=first.currentData.cells.find(cell=>cell.row===1&&cell.column===1)!;
        project=editExtractionCell(project,inputs[0].pageId,first.id,value.id,'EUR 1,250.00\npaid');
        project=approvePage(project,inputs[0].pageId);
        const original=serializeProject(project),sourceObjects=inputs.map(input=>table(project,input.pageId,input.objectId));
        const preview=capture(project,inputs.slice(0,2));
        expect(serializeProject(project)).toBe(original);
        const joined=applyContinuationJoin(project,preview),owner=joined.pages[0],result=table(joined,owner.id,preview.result.id);
        expect(inputs.map(input=>table(joined,input.pageId,input.objectId))).toEqual(sourceObjects);
        expect(joined.pages[0].approved).toBe(false);
        expect(joined.pages[1].approved).toBe(true);
        expect(result.originalOutputKind).toBe('table-join');
        expect(tableDataToRows(result.currentData)).toEqual([
            ['Code','Amount'],['0012','EUR 1,250.00\npaid'],['-7','=SUM(A1)'],
        ]);
        expect(result.originalData).toEqual(result.currentData);
        const history=commitHistory(createHistory(project),joined);
        expect(undoHistory(history).present).toEqual(project);
        expect(redoHistory(undoHistory(history)).present).toEqual(joined);
        const restored=parseProject(sorted(JSON.parse(serializeProject(joined))));
        expect(table(restored,owner.id,result.id)).toEqual(result);
        expect(JSON.parse(serializeExtractionObject(result))).toEqual(result);
        const exported=prepareExport(restored,{format:'json'});
        expect(exported.errors).toEqual([]);
        expect(exported.pages[0].provenance!.extractionObjects).toEqual(restored.pages[0].provenance.extractionObjects);
        const csv=prepareExtractionExport(result);
        expect(csv.extension).toBe('csv');
        expect(csv.text).toContain('"0012","EUR 1,250.00\npaid"');
        expect(csv.text).toContain("'=SUM(A1)");
    });

    it('keeps joined values recoverable when a contributing table is later corrected or its page is removed',()=>{
        const {project,inputs}=fixture(),preview=capture(project,inputs.slice(0,2));
        const joined=applyContinuationJoin(project,preview),result=table(joined,inputs[0].pageId,preview.result.id);
        expect(extractionObjectFreshness(joined,result).current).toBe(true);
        const source=table(joined,inputs[1].pageId,inputs[1].objectId);
        const edited=editExtractionCell(joined,inputs[1].pageId,source.id,source.currentData.cells[2].id,'9999');
        expect(extractionObjectFreshness(edited,table(edited,inputs[0].pageId,result.id)).current).toBe(false);
        expect(table(edited,inputs[0].pageId,result.id)).toEqual(result);
        const removed=removePage(edited,inputs[1].pageId),restored=parseProject(serializeProject(removed));
        expect(table(restored,inputs[0].pageId,result.id)).toEqual(result);
        expect(extractionObjectFreshness(restored,result).current).toBe(false);
        expect(tableDataToRows(result.currentData)[2]).toEqual(['-7','=SUM(A1)']);
    });

    it('flattens an earlier join while preserving its baseline and all source model outputs',()=>{
        const {project,inputs}=fixture(),firstPreview=capture(project,inputs.slice(0,2));
        const firstJoin=applyContinuationJoin(project,firstPreview),firstResult=table(firstJoin,inputs[0].pageId,firstPreview.result.id);
        const nextPreview=capture(firstJoin,[{pageId:inputs[0].pageId,objectId:firstResult.id},inputs[2]],'Three pages');
        const next=applyContinuationJoin(firstJoin,nextPreview),result=table(next,inputs[0].pageId,nextPreview.result.id);
        expect(table(next,inputs[0].pageId,firstResult.id)).toEqual(firstResult);
        expect(tableDataToRows(result.currentData)).toEqual([
            ['Code','Amount'],['0012','EUR 1,250.00'],['-7','=SUM(A1)'],['@identifier','+15'],
        ]);
        const snapshots=result.tableSources!.snapshots;
        expect(snapshots).toHaveLength(4);
        expect(snapshots.every(snapshot=>!('tableSources' in snapshot))).toBe(true);
        for(const input of inputs) {
            const original=table(project,input.pageId,input.objectId);
            const snapshot=snapshots.find(snapshot=>snapshot.objectId===input.objectId)!;
            expect(snapshot.originalData).toEqual(original.originalData);
            expect(snapshot.originalOutput).toEqual(original.originalOutput);
            expect(snapshot.captures).toEqual(original.captures);
        }
        expect(table(parseProject(serializeProject(next)),inputs[0].pageId,result.id)).toEqual(result);
    });

    it('retains a joined source graph through image changes and page splitting',()=>{
        const {project,inputs}=fixture(),preview=capture(project,inputs.slice(0,2));
        const joined=applyContinuationJoin(project,preview),result=table(joined,inputs[0].pageId,preview.result.id);
        const changed=applyPageImage(joined,inputs[0].pageId,{image_path:'/prepared.png',width:800,height:1000},'prepare');
        expect(table(changed,inputs[0].pageId,result.id)).toEqual(result);
        expect(extractionObjectFreshness(changed,result).current).toBe(false);
        const split=splitPage(joined,inputs[0].pageId,[
            {image_path:'/left.png',width:400,height:1000},{image_path:'/right.png',width:400,height:1000},
        ]);
        for(const child of split.pages.slice(0,2)) {
            expect(table(split,child.id,result.id)).toEqual(result);
            expect(extractionObjectFreshness(split,table(split,child.id,result.id)).current).toBe(false);
        }
        const restored=parseProject(serializeProject(split));
        expect(table(restored,restored.pages[1].id,result.id)).toEqual(result);
    });
});
