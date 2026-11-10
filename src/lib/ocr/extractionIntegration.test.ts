import { describe, expect, it } from 'vitest';
import {
    addDocument, applyPageImage, applyRecognition, approvePage, commitHistory,
    createHistory, createProject, editRegion, getReviewIssues, parseProject,
    prepareExport, serializeProject, splitPage, undoHistory, updatePage, type OcrProject,
} from './model';
import {
    appendExtractionObject, captureExtractionRequest, editExtractionCell,
    editExtractionField, editExtractionFormula, extractionObjectFreshness,
    getExtractionObjects, updateExtractionObject, type ExtractionObject,
} from './extractionObjects';
import { applyPassagePreview, capturePassageSplit } from './passages';
import { applyZoneReview, captureZoneReview, prepareZoneReview } from './zoneReview';
import { captureExtractionSource, readExtraction } from './extraction';

function fixture(): OcrProject {
    let project=addDocument(createProject('Independent extraction'),{
        source_uri:'/source.pdf',name:'Source',pages:[{image_path:'/page.png',width:800,height:1000}],
    });
    project=applyRecognition(project,project.pages[0].id,{width:800,height:1000,regions:[
        {text:'Account 0007',kind:'text',x:20,y:20,w:500,h:50},
        {text:'Code Amount\n0007 -12.50',kind:'table',x:20,y:150,w:500,h:200},
        {text:'x^2',kind:'formula',x:20,y:400,w:400,h:100},
    ]});
    return project;
}
function objects(project: OcrProject, pageIndex=0): ExtractionObject[] {
    return getExtractionObjects(project.pages[pageIndex]).collection.objects;
}
function populated(): OcrProject {
    let project=fixture();
    const page=project.pages[0];
    for (let index=0;index<2;index++) {
        const table=captureExtractionRequest(project,page.id,page.regions[1].id,'pixels');
        project=appendExtractionObject(project,table,'table',{
            html:'<table><tr><th>Code</th><th>Amount</th></tr><tr><td>0007</td><td>-12.50</td></tr></table>',rows:2,cols:2,
        });
        const formula=captureExtractionRequest(project,page.id,page.regions[2].id,'pixels');
        project=appendExtractionObject(project,formula,'formula',index===0?'x^2':'y^3');
    }
    const fields=captureExtractionRequest(project,page.id,page.regions[0].id,'text');
    return appendExtractionObject(project,fields,'fields',[{label:'Account',value:'0007',score:.94}],{labels:['Account']});
}

describe('extraction objects across the project lifecycle',()=>{
    it('keeps two tables and two formulas independent through edits, undo, reload and JSON export',()=>{
        const original=populated(),page=original.pages[0],entries=objects(original);
        expect(entries.map(entry=>entry.kind)).toEqual(['table','formula','table','formula','fields']);
        const first=entries[0],second=entries[2],formula=entries[1],fields=entries[4];
        if(first.kind!=='table'||second.kind!=='table'||formula.kind!=='formula'||fields.kind!=='fields') throw new Error('Fixture kinds');
        const cell=first.currentData.cells.find(cell=>cell.row===1&&cell.column===0)!;
        let next=editExtractionCell(original,page.id,first.id,cell.id,'=1+1');
        next=editExtractionFormula(next,page.id,formula.id,'x^{2}+1');
        next=editExtractionField(next,page.id,fields.id,fields.currentData.fields[0].id,{value:'0008'});
        expect(objects(next)[2]).toEqual(second);
        expect(objects(next)[0].originalOutput).toEqual(first.originalOutput);
        expect(objects(next)[0].originalData).toEqual(first.originalData);
        expect(objects(original)).toEqual(entries);
        const restored=parseProject(serializeProject(next));
        expect(objects(restored)).toEqual(objects(next));
        expect(prepareExport(restored,{format:'json'}).pages[0].provenance!.extractionObjects).toEqual(restored.pages[0].provenance.extractionObjects);
        expect(undoHistory(commitHistory(createHistory(original),next)).present).toEqual(original);
    });

    it('retains captured pixels and raw outputs after transforms and marks every old object historical',()=>{
        const original=populated(),page=original.pages[0],before=objects(original);
        const changed=applyPageImage(original,page.id,{image_path:'/cropped.png',width:600,height:800},'crop');
        expect(objects(changed)).toEqual(before);
        expect(objects(changed).every(object=>!extractionObjectFreshness(changed,object).current)).toBe(true);
        expect(objects(changed)[0].captures[0]).toMatchObject({pageId:page.id,imagePath:'/page.png',width:800,height:1000});
        expect(objects(parseProject(serializeProject(changed)))).toEqual(before);
        const undone=undoHistory(commitHistory(createHistory(original),changed)).present;
        expect(objects(undone).every(object=>extractionObjectFreshness(undone,object).current)).toBe(true);
    });

    it('keeps historical objects on split children without relabeling the captured source',()=>{
        const original=populated(),page=original.pages[0],before=objects(original);
        const next=splitPage(original,page.id,[{image_path:'/left.png',width:400,height:1000},{image_path:'/right.png',width:400,height:1000}]);
        for(let index=0;index<2;index++) {
            expect(objects(next,index)).toEqual(before);
            expect(objects(next,index).every(object=>!extractionObjectFreshness(next,object).current)).toBe(true);
            expect(objects(next,index)[0].captures[0].pageId).toBe(page.id);
        }
        expect(objects(parseProject(serializeProject(next)),1)).toEqual(before);
    });

    it('retains extraction history through whole-page recognition, passage splits and partial re-OCR',()=>{
        const original=populated(),page=original.pages[0],before=objects(original);
        const recognized=applyRecognition(original,page.id,{width:800,height:1000,regions:[{text:'New OCR',x:20,y:20,w:500,h:50}]});
        expect(objects(recognized)).toEqual(before);
        expect(objects(recognized).every(object=>!extractionObjectFreshness(recognized,object).current)).toBe(true);
        const split=applyPassagePreview(original,capturePassageSplit(original,page.id,page.regions[0].id,7,{mode:'shared'}));
        expect(objects(split)).toEqual(before);
        expect(extractionObjectFreshness(split,objects(split)[4]).current).toBe(false);
        expect(extractionObjectFreshness(split,objects(split)[0]).current).toBe(true);
        const request=captureZoneReview(original,page.id,{x:20,y:150,w:500,h:200});
        const zone=prepareZoneReview(original,request,{width:800,height:1000,regions:[{text:'Replacement table OCR',kind:'table',x:20,y:150,w:500,h:200}]});
        const replaced=applyZoneReview(original,zone);
        expect(objects(replaced)).toEqual(before);
        expect(extractionObjectFreshness(replaced,objects(replaced)[0]).current).toBe(false);
        expect(extractionObjectFreshness(replaced,objects(replaced)[1]).current).toBe(true);
    });

    it('migrates legacy records before losing their measured source and preserves unsafe raw data',()=>{
        let project=fixture(),page=project.pages[0];
        const legacy={tableRows:[['Code','Amount'],['0007','-12.50']],sourceImagePath:page.imagePath,
            sources:{table:captureExtractionSource(page,page.regions[1],'pixels')},updatedAt:'2026-11-09T00:00:00.000Z'};
        project=updatePage(project,page.id,{provenance:{...page.provenance,extraction:legacy}});
        const before=objects(project);
        expect(before).toHaveLength(1);
        const transformed=applyPageImage(project,page.id,{image_path:'/new.png',width:600,height:800},'crop');
        expect(objects(transformed)).toEqual(before);
        expect(transformed.pages[0].provenance.extraction).toEqual(legacy);
        expect(extractionObjectFreshness(transformed,objects(transformed)[0]).current).toBe(false);
        const unsafe={tableRows:[['Cannot recover its source']],sourceImagePath:'/unknown-old.png'};
        const base=fixture(),stored=updatePage(base,base.pages[0].id,{provenance:{extraction:unsafe}});
        const changed=applyPageImage(stored,stored.pages[0].id,{image_path:'/other.png',width:700,height:900},'crop');
        expect(changed.pages[0].provenance.extraction).toEqual(unsafe);
        expect(objects(changed)).toEqual([]);
        expect(getExtractionObjects(changed.pages[0]).diagnostics.length).toBeGreaterThan(0);
        expect(readExtraction(changed.pages[0]).tableRows).toEqual([]);
    });

    it('queues included current extraction until page approval and reopens review on correction',()=>{
        let project=populated();project=approvePage(project,project.pages[0].id);
        const message='Extracted content needs page approval.';
        expect(getReviewIssues(project).some(issue=>issue.message===message)).toBe(false);
        const page=project.pages[0],formula=objects(project).find(object=>object.kind==='formula')!;
        const corrected=editExtractionFormula(project,page.id,formula.id,'x+1');
        expect(corrected.pages[0].approved).toBe(false);
        expect(getReviewIssues(corrected).filter(issue=>issue.message===message)).toHaveLength(1);
        let excluded=corrected;
        for(const object of objects(excluded)) excluded=updateExtractionObject(excluded,page.id,object.id,{excluded:true});
        expect(getReviewIssues(excluded).some(issue=>issue.message===message)).toBe(false);
        expect(getReviewIssues(approvePage(corrected,page.id)).some(issue=>issue.message===message)).toBe(false);
    });

    it('rejects unsupported collection versions on reopen while preserving the current project',()=>{
        const project=populated(),before=serializeProject(project),raw=JSON.parse(before);
        raw.pages[0].provenance.extractionObjects.version=2;
        expect(()=>parseProject(raw)).toThrow();
        expect(serializeProject(project)).toBe(before);
        const page=project.pages[0],fields=objects(project).find(object=>object.kind==='fields')!;
        const changed=editRegion(project,page.id,page.regions[0].id,'Account 0009');
        expect(extractionObjectFreshness(changed,fields).current).toBe(false);
        expect(objects(changed)).toEqual(objects(project));
    });
});
