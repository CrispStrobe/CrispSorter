import { describe, expect, it } from 'vitest';
import { addDocument, applyRecognition, approvePage, createProject, parseProject, serializeProject, type OcrProject } from './model';
import {
    appendExtractionObject, captureExtractionRequest, editExtractionCell, editExtractionField, editExtractionFormula,
    extractionObjectFreshness, getExtractionObjects, readExtractionObjects, serializeExtractionObject, updateExtractionObject,
} from './extractionObjects';
import { captureExtractionSource } from './extraction';

const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
function project():OcrProject {
    let p=addDocument(createProject(),{source_uri:'/source.pdf',name:'Source',
        pages:[{image_path:'/page.png',width:600,height:800},{image_path:'/second.png',width:600,height:800}]});
    return applyRecognition(p,p.pages[0].id,{width:600,height:800,regions:[
        {text:'Total 125',x:10,y:10,w:200,h:40},{text:'Next passage',x:10,y:100,w:200,h:40},
    ]});
}
const tableOutput={html:'<table><tr><th colspan="2">Amount</th></tr><tr><td>Total</td><td>125</td></tr></table>',rows:2,cols:2};
const fieldsOutput=[{label:'total',value:'125',score:0.9}];
function append(p:OcrProject,kind:'table'|'fields'|'formula',output:unknown=kind==='table'?tableOutput:kind==='fields'?fieldsOutput:'x^2') {
    const request=captureExtractionRequest(p,p.pages[0].id,null,kind==='fields'?'text':'pixels');
    return {project:appendExtractionObject(p,request,kind,output,{labels:['total']}),id:request.objectId,request};
}
const entries=(p:OcrProject)=>getExtractionObjects(p.pages[0]).collection.objects;
const entry=(p:OcrProject,id:string)=>entries(p).find(value=>value.id===id)!;
function legacyPage() {
    const p=project(),page=p.pages[0];
    page.provenance.extraction={fields:[{label:'total',value:'125',original_value:'100',score:0.9,review:'corrected'}],
        tableRows:[['A','B'],['C','D']],formula:'\\alpha + 1',labels:'total',
        sourceImagePath:page.imagePath,updatedAt:'2026-11-09T12:00:00.000Z',sources:{
            fields:captureExtractionSource(page,page.regions[0],'text'),
            table:captureExtractionSource(page,undefined,'pixels'),formula:captureExtractionSource(page,undefined,'pixels'),
        }};
    return page;
}
function sorted(value:unknown):unknown {
    if(Array.isArray(value))return value.map(sorted);
    if(value&&typeof value==='object')return Object.fromEntries(Object.keys(value).sort().map(key=>[key,sorted((value as Record<string,unknown>)[key])]));
    return value;
}

describe('multiple independent extraction objects',()=>{
    it('appends two results of every kind without overwriting original model output',()=>{
        let p=project();const before=clone(p);
        for(const kind of ['table','formula','fields'] as const)for(let index=0;index<2;index++)p=append(p,kind).project;
        expect(entries(p).map(value=>value.kind)).toEqual(['table','table','formula','formula','fields','fields']);
        expect(new Set(entries(p).map(value=>value.id)).size).toBe(6);
        expect(entries(p)[0].originalOutput).toEqual(tableOutput);
        expect(entries(p)[4].originalOutput).toEqual(fieldsOutput);
        expect(entries(p).every(value=>value.originalOutputKind==='model')).toBe(true);
        expect(before.pages[0].provenance.extractionObjects).toBeUndefined();
        expect(p.pages[0].regions).toEqual(before.pages[0].regions);
    });

    it('edits a selected table cell and metadata without changing its spans, originals or another table',()=>{
        const first=append(project(),'table'),second=append(first.project,'table');
        const original=entry(second.project,first.id),other=clone(entry(second.project,second.id));
        expect(original.kind).toBe('table');if(original.kind!=='table')throw new Error();
        const p=editExtractionCell(second.project,second.project.pages[0].id,first.id,original.currentData.cells[2].id,'175');
        const changed=entry(p,first.id);if(changed.kind!=='table')throw new Error();
        expect(changed.currentData.cells[2].text).toBe('175');
        expect(changed.originalData).toEqual(original.originalData);expect(changed.originalOutput).toEqual(tableOutput);
        expect(changed.currentData.cells.map(({text:_text,...cell})=>cell)).toEqual(original.currentData.cells.map(({text:_text,...cell})=>cell));
        expect(entry(p,second.id)).toEqual(other);
        const renamed=updateExtractionObject(p,p.pages[0].id,first.id,{name:'Checked table',excluded:true,review:'approved'});
        expect(entry(renamed,first.id)).toMatchObject({name:'Checked table',excluded:true,review:'approved'});
        expect(entry(renamed,first.id).originalData).toEqual(original.originalData);
        expect(()=>updateExtractionObject(p,p.pages[0].id,first.id,{originalData:{}} as never)).toThrow(/unsupported/);
    });

    it('keeps original field scores but unsets confidence for changed values or labels',()=>{
        const initial=append(project(),'fields'),original=entry(initial.project,initial.id);
        if(original.kind!=='fields')throw new Error();
        const id=original.currentData.fields[0].id,pageId=initial.project.pages[0].id;
        let p=editExtractionField(initial.project,pageId,initial.id,id,{value:'175'});
        const corrected=entry(p,initial.id);if(corrected.kind!=='fields')throw new Error();
        expect(corrected.currentData.fields[0]).toMatchObject({value:'175',score:null});
        expect(corrected.originalData.fields[0]).toMatchObject({value:'125',score:0.9});
        p=editExtractionField(p,pageId,initial.id,id,{value:'125',label:'different'});
        const relabeled=entry(p,initial.id);if(relabeled.kind!=='fields')throw new Error();
        expect(relabeled.currentData.fields[0].score).toBeNull();
        p=editExtractionField(p,pageId,initial.id,id,{label:'total'});
        const restored=entry(p,initial.id);if(restored.kind!=='fields')throw new Error();
        expect(restored.currentData.fields[0].score).toBe(0.9);
        expect(restored.originalOutput).toEqual(fieldsOutput);
    });

    it('keeps formulas literal and independently editable, with no empty model result substituted',()=>{
        const first=append(project(),'formula','\\frac{1}{2}'),second=append(first.project,'formula','x^2');
        const p=editExtractionFormula(second.project,second.project.pages[0].id,first.id,'\\text{<img src=x>} + 3');
        expect(entry(p,first.id).currentData).toEqual({version:1,latex:'\\text{<img src=x>} + 3'});
        expect(entry(p,first.id).originalOutput).toBe('\\frac{1}{2}');
        expect(entry(p,second.id).currentData).toEqual({version:1,latex:'x^2'});
        expect(()=>append(p,'formula',null)).toThrow(/no formula|JSON values/);
        expect(editExtractionFormula(p,p.pages[0].id,first.id,'').pages[0]).toBeDefined();
    });

    it('reopens page approval after accepted edits while retaining immutable source text',()=>{
        const initial=append(project(),'formula'),approved=approvePage(initial.project,initial.project.pages[0].id);
        expect(approved.pages[0].approved).toBe(true);
        const changed=editExtractionFormula(approved,approved.pages[0].id,initial.id,'y^2');
        expect(changed.pages[0].approved).toBe(false);expect(changed.pages[0].regions).toEqual(approved.pages[0].regions);
        expect(entry(changed,initial.id).originalOutput).toBe('x^2');
        expect(editExtractionFormula(changed,changed.pages[0].id,initial.id,'y^2')).toBe(changed);
        expect(updateExtractionObject(approved,approved.pages[0].id,initial.id,{excluded:true}).pages[0].approved).toBe(false);
    });

    it('saves, restores and exports all provenance despite native JSON key sorting',()=>{
        let p=append(project(),'fields').project;p=append(p,'table').project;p=append(p,'formula').project;
        const restored=parseProject(sorted(JSON.parse(serializeProject(p))));
        expect(entries(restored)).toEqual(entries(p));
        for(const object of entries(restored)) {
            expect(JSON.parse(serializeExtractionObject(object))).toEqual(object);
            expect(extractionObjectFreshness(restored,object)).toEqual({current:true,reasons:[]});
        }
        const isolated=getExtractionObjects(restored.pages[0]).collection;isolated.objects[0].name='Mutation';
        expect(entries(restored)[0].name).not.toBe('Mutation');
    });
});

describe('captured extraction source freshness',()=>{
    it('captures the actual selected crop, measured page and exact text projection independently',()=>{
        const p=project(),page=p.pages[0];page.regions[1].separatorBefore='';
        const crop=captureExtractionRequest(p,page.id,page.regions[0].id,'pixels');
        expect(crop.capture).toMatchObject({pageId:page.id,documentId:page.documentId,width:600,height:800,
            imagePath:'/page.png',regionId:page.regions[0].id,bounds:{x:10,y:10,w:200,h:40},method:'pixels',text:null});
        const text=captureExtractionRequest(p,page.id,null,'text');
        expect(text.capture.text).toBe('Total 125Next passage');
        page.regions[0].x=20;expect(crop.capture.bounds!.x).toBe(10);
    });

    it('distinguishes changed transcription from unchanged pixels, and checks geometry for both methods',()=>{
        let p=project();const page=p.pages[0];
        const pixels=captureExtractionRequest(p,page.id,page.regions[0].id,'pixels');
        const text=captureExtractionRequest(p,page.id,page.regions[0].id,'text');
        p=appendExtractionObject(p,pixels,'formula','x');
        p=appendExtractionObject(p,text,'fields',fieldsOutput);
        const corrected=clone(p);corrected.pages[0].regions[0].text='Total 175';
        expect(extractionObjectFreshness(corrected,entry(p,pixels.objectId)).current).toBe(true);
        expect(extractionObjectFreshness(corrected,entry(p,text.objectId)).current).toBe(false);
        for(const change of [
            (value:OcrProject)=>{value.pages[0].regions[0].x=11;},
            (value:OcrProject)=>{value.pages[0].regions[0].id='replacement';},
            (value:OcrProject)=>{value.pages[0].imagePath='/changed.png';},
            (value:OcrProject)=>{value.pages[0].width=601;},
            (value:OcrProject)=>{value.pages.shift();},
        ]) {
            const changed=clone(p);change(changed);
            expect(extractionObjectFreshness(changed,entry(p,pixels.objectId)).current).toBe(false);
            expect(extractionObjectFreshness(changed,entry(p,text.objectId)).current).toBe(false);
        }
    });

    it('retains delayed results on the captured page as historical without attaching to another page',()=>{
        const p=project(),request=captureExtractionRequest(p,p.pages[0].id,null,'pixels');
        const changed=clone(p);changed.pages[0].imagePath='/new.png';
        const stored=appendExtractionObject(changed,request,'table',tableOutput),saved=entry(stored,request.objectId);
        expect(saved.captures[0].imagePath).toBe('/page.png');
        expect(extractionObjectFreshness(stored,saved).current).toBe(false);
        expect(getExtractionObjects(stored.pages[1]).collection.objects).toEqual([]);
        expect(()=>appendExtractionObject({...p,id:'another-project'},request,'table',tableOutput)).toThrow(/another project/);
        expect(()=>appendExtractionObject({...p,pages:p.pages.slice(1)},request,'table',tableOutput)).toThrow(/removed/);
        expect(()=>appendExtractionObject(stored,request,'table',tableOutput)).toThrow(/already stored/);
    });

    it('supports several explicit captures and retains stale objects through the data boundary',()=>{
        const initial=append(project(),'table'),collection=getExtractionObjects(initial.project.pages[0]).collection;
        const other=captureExtractionRequest(initial.project,initial.project.pages[1].id,null,'pixels').capture;
        collection.objects[0].captures.push(other);
        const restored=readExtractionObjects(JSON.parse(JSON.stringify(collection)))!;
        const changed=clone(initial.project);changed.pages[1].imagePath='/changed-second.png';
        expect(extractionObjectFreshness(changed,restored.objects[0]).current).toBe(false);
        expect(restored.objects).toHaveLength(1);expect(restored.objects[0].captures).toHaveLength(2);
    });
});

describe('strict bounded extraction-object data',()=>{
    it('rejects unknown versions, duplicate objects, unsupported edits and unrelated field scores',()=>{
        const initial=append(project(),'fields'),collection=getExtractionObjects(initial.project.pages[0]).collection;
        for(const value of [null,{...collection,version:2},{...collection,html:'unsafe'},
            {...collection,objects:[collection.objects[0],collection.objects[0]]}])
            expect(()=>readExtractionObjects(value)).toThrow();
        const invalid=clone(collection),field=invalid.objects[0];if(field.kind!=='fields')throw new Error();
        field.currentData.fields[0].value='Changed';
        expect(()=>readExtractionObjects(invalid)).toThrow(/unrelated model score/);
        expect(()=>editExtractionField(initial.project,initial.project.pages[0].id,initial.id,field.currentData.fields[0].id,{score:1} as never)).toThrow(/unsupported/);
        expect(readExtractionObjects(undefined)).toBeUndefined();
    });

    it('bounds collections, original outputs, field counts and capture counts before saving',()=>{
        const initial=append(project(),'formula'),collection=getExtractionObjects(initial.project.pages[0]).collection,base=collection.objects[0];
        expect(()=>readExtractionObjects({version:1,objects:Array.from({length:129},(_,index)=>({...base,id:'object-'+index}))})).toThrow(/128-object/);
        expect(()=>readExtractionObjects({version:1,objects:[{...base,captures:Array.from({length:129},(_,index)=>({...base.captures[0],id:'source-'+index}))}]})).toThrow(/metadata/);
        expect(()=>readExtractionObjects({version:1,objects:[{...base,originalOutput:'x'.repeat(4*1024*1024+1)}]})).toThrow(/4 MB/);
        const huge={...base,originalOutput:'x'.repeat(1_000_000),originalData:{version:1,latex:'x'.repeat(1_000_000)},currentData:{version:1,latex:'x'.repeat(1_000_000)}};
        expect(()=>readExtractionObjects({version:1,objects:Array.from({length:6},(_,index)=>({...huge,id:'large-'+index}))})).toThrow(/16 MB/);
        expect(()=>append(initial.project,'fields',Array.from({length:1001},()=>fieldsOutput[0]))).toThrow(/bounded field array/);
    });

    it('requires genuine cell boxes to reference a pixel capture and lie inside its real coverage',()=>{
        const p=project(),page=p.pages[0],request=captureExtractionRequest(p,page.id,page.regions[0].id,'pixels');
        const stored=appendExtractionObject(p,request,'table',tableOutput),collection=getExtractionObjects(stored.pages[0]).collection;
        const table=collection.objects[0];if(table.kind!=='table')throw new Error();
        for(const data of [table.originalData,table.currentData])data.cells[0].box={captureId:request.capture.id,bounds:{x:10,y:10,w:200,h:20}};
        expect(readExtractionObjects(collection)!.objects[0]).toEqual(table);
        const missing=clone(collection),missingTable=missing.objects[0];if(missingTable.kind!=='table')throw new Error();
        missingTable.currentData.cells[0].box!.captureId='missing';
        expect(()=>readExtractionObjects(missing)).toThrow(/actual pixel capture/);
        const outside=clone(collection),outsideTable=outside.objects[0];if(outsideTable.kind!=='table')throw new Error();
        outsideTable.currentData.cells[0].box!.bounds.y=60;
        expect(()=>readExtractionObjects(outside)).toThrow(/captured source coverage/);
        table.captures[0].method='text';table.captures[0].text='Text only';
        expect(()=>readExtractionObjects(collection)).toThrow(/actual pixel capture/);
    });

    it('preserves the project on invalid formula, field or source edits',()=>{
        const initial=append(project(),'fields'),before=JSON.stringify(initial.project),field=entry(initial.project,initial.id);
        if(field.kind!=='fields')throw new Error();
        expect(()=>editExtractionField(initial.project,initial.project.pages[0].id,initial.id,field.currentData.fields[0].id,{value:'x'.repeat(1_000_001)})).toThrow();
        expect(()=>appendExtractionObject(initial.project,{...initial.request,objectId:'other',capture:{...initial.request.capture,width:0}},'formula','x')).toThrow(/source capture/);
        expect(()=>append(initial.project,'table',{html:'<table><tr><td>A</td><td>B</td></tr><tr><td>C</td></tr></table>',rows:2,cols:2})).toThrow(/cover/);
        expect(JSON.stringify(initial.project)).toBe(before);
    });
});

describe('honest deterministic legacy migration',()=>{
    it('migrates one valid object per kind without hiding stale text or inventing old model output',()=>{
        const page=legacyPage(),raw=clone(page.provenance.extraction);page.regions[0].text='Changed since extraction';
        const read=getExtractionObjects(page);
        expect(read.migrated).toBe(true);expect(read.diagnostics).toEqual([]);
        expect(read.collection.objects.map(value=>value.kind)).toEqual(['fields','table','formula']);
        expect(read.collection.objects.every(value=>value.originalOutputKind==='legacy-record')).toBe(true);
        expect(read.collection.objects.every(value=>JSON.stringify(value.originalOutput)===JSON.stringify(raw))).toBe(true);
        const fields=read.collection.objects[0];if(fields.kind!=='fields')throw new Error();
        expect(fields.originalData.fields[0]).toMatchObject({value:'100',score:0.9});
        expect(fields.currentData.fields[0]).toMatchObject({value:'125',score:null});
        expect(fields.captures[0].text).toBe('Total 125');
        expect(page.provenance.extraction).toEqual(raw);
        page.provenance.extractionObjects=read.collection;
        expect(getExtractionObjects(page).migrated).toBe(false);
        expect(getExtractionObjects(page).collection.objects).toHaveLength(3);
    });

    it('keeps unknown source extents and missing captures as recoverable raw legacy records',()=>{
        const page=legacyPage(),raw=clone(page.provenance.extraction);
        page.imagePath='/transformed.png';page.width=1200;
        const read=getExtractionObjects(page);
        expect(read.collection.objects).toEqual([]);expect(read.diagnostics).toHaveLength(3);
        expect(read.diagnostics.join(' ')).toContain('old image dimensions are not known');
        expect(page.provenance.extraction).toEqual(raw);
        const missing=legacyPage();delete (missing.provenance.extraction as Record<string,unknown>).sources;
        expect(getExtractionObjects(missing).diagnostics.join(' ')).toContain('source capture is unavailable');
        const undated=legacyPage();delete (undated.provenance.extraction as Record<string,unknown>).updatedAt;
        expect(getExtractionObjects(undated).diagnostics.join(' ')).toContain('timestamp is unavailable');
    });

    it('isolates malformed kinds and retains their raw record alongside valid migrations',()=>{
        const page=legacyPage(),raw=page.provenance.extraction as Record<string,unknown>;
        raw.tableRows=[['A','B'],['C']];
        const read=getExtractionObjects(page);
        expect(read.collection.objects.map(value=>value.kind)).toEqual(['fields','formula']);
        expect(read.diagnostics.join(' ')).toContain('rectangle');
        page.provenance.extractionObjects=read.collection;
        expect(getExtractionObjects(page).collection.objects).toHaveLength(2);
        expect(getExtractionObjects(page).diagnostics.join(' ')).toContain('rectangle');
        expect(raw.tableRows).toEqual([['A','B'],['C']]);
    });

    it('keeps migration identities stable through JSON sorting and later image/page changes',()=>{
        const page=legacyPage(),first=getExtractionObjects(page);
        const reordered=clone(page);reordered.provenance.extraction=sorted(reordered.provenance.extraction);
        expect(getExtractionObjects(reordered).collection.objects.map(value=>value.id)).toEqual(first.collection.objects.map(value=>value.id));
        reordered.provenance.extractionObjects=first.collection;reordered.imagePath='/new-image.png';reordered.id='split-child';
        const retained=getExtractionObjects(reordered);
        expect(retained.diagnostics).toEqual([]);expect(retained.migrated).toBe(false);
        expect(retained.collection.objects.map(value=>value.id)).toEqual(first.collection.objects.map(value=>value.id));
        expect(retained.collection.objects[0].captures[0].pageId).toBe(page.id);
    });
});
