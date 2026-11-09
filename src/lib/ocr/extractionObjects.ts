import type { OcrPage, OcrProject } from './model';
import { captureExtractionSource, type ExtractionKind, type ExtractionSource } from './extraction';
import { joinRegionText } from './text';
import { readTableData, tableDataFromRows, MAX_EXTRACTION_TEXT, type TableData } from './tableData';
import { parseTableHtml } from './tableHtml';
export type { TableData, TableCell } from './tableData';
export type { ExtractionKind } from './extraction';

export type JsonValue=null|boolean|number|string|JsonValue[]|{[key:string]:JsonValue};
export type ExtractionReview='unreviewed'|'corrected'|'approved';
export interface ExtractionCapture extends ExtractionSource {
    id:string; pageId:string; documentId:string; width:number; height:number;
}
export interface ExtractionFieldValue { id:string; label:string; value:string; score:number|null; }
export interface ExtractionFieldsData { version:1; labels:string[]; fields:ExtractionFieldValue[]; }
export interface ExtractionFormulaData { version:1; latex:string; }
interface ExtractionBase<K extends ExtractionKind,D> {
    version:1; id:string; kind:K; name:string; createdAt:string; updatedAt:string;
    excluded:boolean; review:ExtractionReview; captures:ExtractionCapture[];
    originalOutput:JsonValue; originalOutputKind:'model'|'legacy-record';
    originalData:D; currentData:D;
}
export type ExtractionObject=
    |ExtractionBase<'fields',ExtractionFieldsData>
    |ExtractionBase<'table',TableData>
    |ExtractionBase<'formula',ExtractionFormulaData>;
export interface ExtractionObjects { version:1; objects:ExtractionObject[]; }
export interface ExtractionRequest { projectId:string; objectId:string; createdAt:string; capture:ExtractionCapture; }
export interface ExtractionObjectPatch { name?:string; excluded?:boolean; review?:ExtractionReview; }
export interface ExtractionReadResult { collection:ExtractionObjects; diagnostics:string[]; migrated:boolean; }
export interface ExtractionFreshness { current:boolean; reasons:string[]; }

const MAX_COLLECTION_BYTES=16*1024*1024,MAX_RAW_BYTES=4*1024*1024,MAX_OBJECTS=128,MAX_CAPTURES=128;
const encoder=new TextEncoder();
const object=(value:unknown):value is Record<string,unknown>=>!!value&&typeof value==='object'&&!Array.isArray(value);
const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const validId=(value:unknown):value is string=>typeof value==='string'&&!!value.trim()&&value.length<=200&&!value.includes('\0');
const string=(value:unknown,maximum=MAX_EXTRACTION_TEXT):value is string=>typeof value==='string'&&value.length<=maximum&&!value.includes('\0');
const finite=(value:unknown):value is number=>typeof value==='number'&&Number.isFinite(value);
const score=(value:unknown):value is number|null=>value===null||finite(value)&&value>=0&&value<=1;
const date=(value:unknown):value is string=>string(value,64)&&Number.isFinite(Date.parse(value));
const fail=(reason:string):never=>{throw new Error('Invalid extraction object: '+reason+'. Existing extraction data was preserved.');};
const bytes=(value:unknown):number=>{try{return encoder.encode(JSON.stringify(value)).byteLength;}catch{return fail('data must be serializable JSON');}};
const now=():string=>new Date().toISOString();
const identity=():string=>'extraction-'+(globalThis.crypto?.randomUUID?.()??Date.now().toString(36)+'-'+Math.random().toString(36).slice(2));
const pageText=(page:OcrPage):string=>joinRegionText(page.regions.filter(region=>!region.excluded&&region.kind!=='picture'));
const kindName=(kind:ExtractionKind):string=>({fields:'Fields',table:'Table',formula:'Formula'})[kind];

function readOutput(value:unknown):JsonValue {
    if(bytes(value)>MAX_RAW_BYTES)return fail('one original model/legacy output exceeds 4 MB');
    let nodes=0;
    function visit(entry:unknown,depth:number):void {
        if(++nodes>100_000||depth>32)return fail('original output is too deeply nested or complex');
        if(entry===null||typeof entry==='string'||typeof entry==='boolean'||finite(entry))return;
        if(Array.isArray(entry)){for(const child of entry)visit(child,depth+1);return;}
        if(object(entry)){for(const child of Object.values(entry))visit(child,depth+1);return;}
        return fail('original output must contain only JSON values');
    }
    visit(value,0);return clone(value) as JsonValue;
}

function readCapture(raw:unknown):ExtractionCapture {
    if(!object(raw)||Object.keys(raw).some(key=>!['id','pageId','documentId','width','height','imagePath','regionId','bounds','method','text'].includes(key))||
        !validId(raw.id)||!validId(raw.pageId)||!validId(raw.documentId)||
        !Number.isSafeInteger(raw.width)||!Number.isSafeInteger(raw.height)||(raw.width as number)<=0||(raw.height as number)<=0||
        (raw.width as number)>100_000||(raw.height as number)>100_000||(raw.width as number)*(raw.height as number)>100_000_000||
        !string(raw.imagePath,10_000)||!raw.imagePath||
        !(raw.regionId===null||validId(raw.regionId))||!['text','pixels'].includes(raw.method as string)||
        (raw.method==='text'?!string(raw.text):raw.text!==null))return fail('source capture metadata is invalid');
    let bounds:ExtractionCapture['bounds']=null;
    if(raw.regionId===null) {if(raw.bounds!==null)return fail('a full-page source cannot claim an unnamed region crop');}
    else {
        if(!object(raw.bounds)||Object.keys(raw.bounds).some(key=>!['x','y','w','h'].includes(key))||
            !finite(raw.bounds.x)||!finite(raw.bounds.y)||!finite(raw.bounds.w)||!finite(raw.bounds.h)||
            raw.bounds.x<0||raw.bounds.y<0||raw.bounds.w<=0||raw.bounds.h<=0||
            raw.bounds.x+raw.bounds.w>(raw.width as number)+0.01||raw.bounds.y+raw.bounds.h>(raw.height as number)+0.01)
            return fail('source crop lies outside its measured page');
        bounds={x:raw.bounds.x,y:raw.bounds.y,w:raw.bounds.w,h:raw.bounds.h};
    }
    return {id:raw.id,pageId:raw.pageId,documentId:raw.documentId,width:raw.width as number,height:raw.height as number,
        imagePath:raw.imagePath,regionId:raw.regionId as string|null,bounds,method:raw.method as 'text'|'pixels',text:raw.text as string|null};
}

function readFields(raw:unknown):ExtractionFieldsData {
    if(!object(raw)||Object.keys(raw).some(key=>!['version','labels','fields'].includes(key))||raw.version!==1||
        !Array.isArray(raw.labels)||raw.labels.length>1000||raw.labels.some(label=>!string(label,10_000))||
        !Array.isArray(raw.fields)||raw.fields.length>1000)return fail('field data is invalid or too large');
    let textUnits=(raw.labels as string[]).reduce((sum,label)=>sum+label.length,0);
    const ids=new Set<string>();
    const fields=raw.fields.map((entry):ExtractionFieldValue=>{
        if(!object(entry)||Object.keys(entry).some(key=>!['id','label','value','score'].includes(key))||
            !validId(entry.id)||ids.has(entry.id)||!string(entry.label,10_000)||!string(entry.value)||!score(entry.score))
            return fail('field identity, value or original score is invalid');
        ids.add(entry.id);textUnits+=entry.label.length+entry.value.length;
        return {id:entry.id,label:entry.label,value:entry.value,score:entry.score};
    });
    if(textUnits>MAX_EXTRACTION_TEXT)return fail('field text exceeds 1,000,000 units');
    return {version:1,labels:[...raw.labels] as string[],fields};
}

function readFormula(raw:unknown):ExtractionFormulaData {
    if(!object(raw)||Object.keys(raw).some(key=>!['version','latex'].includes(key))||raw.version!==1||!string(raw.latex))
        return fail('formula data must be bounded literal LaTeX');
    return {version:1,latex:raw.latex};
}

function checkBoxes(data:TableData,captures:readonly ExtractionCapture[]):void {
    const sources=new Map(captures.map(capture=>[capture.id,capture]));
    for(const cell of data.cells)if(cell.box) {
        const source=sources.get(cell.box.captureId),box=cell.box.bounds;
        if(!source||source.method!=='pixels')return fail('a measured cell box needs its actual pixel capture');
        const limit=source.bounds??{x:0,y:0,w:source.width,h:source.height};
        if(box.x<limit.x||box.y<limit.y||box.x+box.w>limit.x+limit.w+0.01||box.y+box.h>limit.y+limit.h+0.01)
            return fail('a measured cell box lies outside its captured source coverage');
    }
}

function readObject(raw:unknown):ExtractionObject {
    const fields=['version','id','kind','name','createdAt','updatedAt','excluded','review','captures','originalOutput','originalOutputKind','originalData','currentData'];
    if(!object(raw)||Object.keys(raw).some(key=>!fields.includes(key))||raw.version!==1||!validId(raw.id)||
        !['fields','table','formula'].includes(raw.kind as string)||!string(raw.name,1000)||!date(raw.createdAt)||!date(raw.updatedAt)||
        typeof raw.excluded!=='boolean'||!['unreviewed','corrected','approved'].includes(raw.review as string)||
        !['model','legacy-record'].includes(raw.originalOutputKind as string)||
        !Array.isArray(raw.captures)||!raw.captures.length||raw.captures.length>MAX_CAPTURES)return fail('unsupported object metadata');
    const captures=raw.captures.map(readCapture);
    if(new Set(captures.map(capture=>capture.id)).size!==captures.length)return fail('source capture identities repeat');
    const base={version:1 as const,id:raw.id,name:raw.name,createdAt:raw.createdAt,updatedAt:raw.updatedAt,
        excluded:raw.excluded,review:raw.review as ExtractionReview,captures,originalOutput:readOutput(raw.originalOutput),
        originalOutputKind:raw.originalOutputKind as 'model'|'legacy-record'};
    if(raw.kind==='fields') {
        const originalData=readFields(raw.originalData),currentData=readFields(raw.currentData);
        const originalById=new Map(originalData.fields.map(field=>[field.id,field]));
        for(const field of currentData.fields)if(field.score!==null) {
            const original=originalById.get(field.id);
            if(!original||field.label!==original.label||field.value!==original.value||field.score!==original.score)
                return fail('edited field values cannot retain an unrelated model score');
        }
        return {...base,kind:'fields',originalData,currentData};
    }
    if(raw.kind==='formula')return {...base,kind:'formula',originalData:readFormula(raw.originalData),currentData:readFormula(raw.currentData)};
    const originalData=readTableData(raw.originalData),currentData=readTableData(raw.currentData);
    checkBoxes(originalData,captures);checkBoxes(currentData,captures);
    return {...base,kind:'table',originalData,currentData};
}

/** Independent persisted boundary: stale sources are kept, never silently filtered. */
export function readExtractionObjects(raw:unknown):ExtractionObjects|undefined {
    if(raw===undefined)return undefined;
    if(!object(raw)||Object.keys(raw).some(key=>!['version','objects'].includes(key))||raw.version!==1||
        !Array.isArray(raw.objects)||raw.objects.length>MAX_OBJECTS||bytes(raw)>MAX_COLLECTION_BYTES)
        return fail('unsupported collection version or 128-object/16 MB limit exceeded');
    const objects=raw.objects.map(readObject);
    if(new Set(objects.map(entry=>entry.id)).size!==objects.length)return fail('object identities repeat');
    return {version:1,objects};
}

function canonical(value:JsonValue):string {
    if(Array.isArray(value))return '['+value.map(canonical).join(',')+']';
    if(object(value))return '{'+Object.keys(value).sort().map(key=>JSON.stringify(key)+':'+canonical(value[key] as JsonValue)).join(',')+'}';
    return JSON.stringify(value);
}
function stableId(value:string):string {
    let a=2166136261,b=0x9e3779b9;
    for(let index=0;index<value.length;index++){a=Math.imul(a^value.charCodeAt(index),16777619)>>>0;b=Math.imul(b^(value.charCodeAt(index)+index),2246822519)>>>0;}
    return 'legacy-extraction-'+a.toString(16).padStart(8,'0')+b.toString(16).padStart(8,'0');
}

function fieldsFromOutput(raw:unknown,id:string,labels:string[],legacyOriginal=false):ExtractionFieldsData {
    if(!Array.isArray(raw)||raw.length>1000)return fail('the field model did not return a bounded field array');
    return readFields({version:1,labels,fields:raw.map((entry,index)=>{
        if(!object(entry)||!string(entry.label,10_000)||!string(entry.value)||
            (entry.score!==undefined&&!score(entry.score))||
            (legacyOriginal&&entry.original_value!==undefined&&!string(entry.original_value)))
            return fail('a model field is malformed');
        return {id:id+'-field-'+index,label:entry.label,value:legacyOriginal&&typeof entry.original_value==='string'?entry.original_value:entry.value,
            score:entry.score??null};
    })});
}

/** Retain the raw legacy property. Only trustworthy source dimensions are migrated. */
export function getExtractionObjects(page:OcrPage):ExtractionReadResult {
    let collection=readExtractionObjects(page.provenance.extractionObjects)??{version:1 as const,objects:[]};
    const diagnostics:string[]=[],legacy=page.provenance.extraction;
    let migrated=false;
    if(legacy===undefined)return {collection,diagnostics,migrated};
    if(!object(legacy))return {collection,diagnostics:['The legacy extraction record is malformed. Its raw value remains in the project for recovery.'],migrated};
    let raw:JsonValue,rawKey:string;
    try{raw=readOutput(legacy);rawKey=canonical(raw);}catch(error){
        return {collection,diagnostics:[String(error instanceof Error?error.message:error)+' The legacy record remains unchanged for recovery.'],migrated};
    }
    for(const kind of ['fields','table','formula'] as const) {
        const output=kind==='fields'?legacy.fields:kind==='table'?legacy.tableRows:legacy.formula;
        if(output===undefined||Array.isArray(output)&&output.length===0||kind==='formula'&&output==='')continue;
        if(collection.objects.some(entry=>entry.kind===kind&&entry.originalOutputKind==='legacy-record'&&canonical(entry.originalOutput)===rawKey))continue;
        try {
            if(!date(legacy.updatedAt))returnLegacy('the original timestamp is unavailable');
            const source=object(legacy.sources)?legacy.sources[kind]:undefined;
            if(!object(source))returnLegacy('the original source capture is unavailable');
            if(source.imagePath!==page.imagePath)returnLegacy('the old image dimensions are not known; current dimensions cannot describe that source');
            const id=stableId(page.id+'|'+kind+'|'+rawKey);
            const capture=readCapture({...source,id:id+'-source',pageId:page.id,documentId:page.documentId,width:page.width,height:page.height});
            const base={version:1,id,kind,name:'Legacy '+kindName(kind).toLowerCase(),createdAt:legacy.updatedAt,updatedAt:legacy.updatedAt,
                excluded:false,review:'unreviewed',captures:[capture],originalOutput:raw,originalOutputKind:'legacy-record'};
            let originalData:ExtractionFieldsData|TableData|ExtractionFormulaData,currentData:typeof originalData;
            if(kind==='fields') {
                const labels=typeof legacy.labels==='string'?legacy.labels.split(',').map(label=>label.trim()).filter(Boolean):[];
                const original=fieldsFromOutput(output,id,labels,true),current=fieldsFromOutput(output,id,labels);
                current.fields=current.fields.map((field,index)=>({...field,score:field.value===original.fields[index].value?field.score:null}));
                originalData=original;currentData=current;
                if(current.fields.some((field,index)=>field.value!==original.fields[index].value))base.review='corrected';
            } else if(kind==='table')originalData=currentData=tableDataFromRows(output,id);
            else originalData=currentData=readFormula({version:1,latex:output});
            const entry=readObject({...base,originalData,currentData});
            collection=readExtractionObjects({version:1,objects:[...collection.objects,entry]})!;
            migrated=true;
        } catch(error) {
            diagnostics.push(kindName(kind)+': '+String(error instanceof Error?error.message:error)+' The legacy record remains unchanged for recovery.');
        }
    }
    return {collection,diagnostics,migrated};
}
function returnLegacy(reason:string):never {return fail(reason);}

/** Capture before invoking a model; pixels may use the existing real crop helper. */
export function captureExtractionRequest(project:OcrProject,pageId:string,regionId:string|null,method:'text'|'pixels'):ExtractionRequest {
    const page=project.pages.find(candidate=>candidate.id===pageId);
    if(!validId(project.id)||!page||!project.documents.some(document=>document.id===page.documentId))return fail('the source project or page is missing');
    const region=regionId===null?undefined:page.regions.find(candidate=>candidate.id===regionId);
    if(regionId!==null&&!region)return fail('the source region is missing');
    const objectId=identity();
    const capture=readCapture({...captureExtractionSource(page,region,method),id:objectId+'-source',
        pageId:page.id,documentId:page.documentId,width:page.width,height:page.height});
    return {projectId:project.id,objectId,createdAt:now(),capture};
}

/** Every capture must still match; historical objects remain independently inspectable. */
export function extractionObjectFreshness(project:Pick<OcrProject,'pages'|'documents'>,entry:ExtractionObject):ExtractionFreshness {
    const reasons:string[]=[];
    for(const capture of entry.captures) {
        const page=project.pages.find(candidate=>candidate.id===capture.pageId);
        if(!page){reasons.push('A captured source page is no longer in this project.');continue;}
        if(page.documentId!==capture.documentId||!project.documents.some(document=>document.id===capture.documentId))
            reasons.push('The source document identity changed.');
        if(page.imagePath!==capture.imagePath||page.width!==capture.width||page.height!==capture.height)
            reasons.push('The page image or measured dimensions changed.');
        const region=capture.regionId===null?undefined:page.regions.find(candidate=>candidate.id===capture.regionId);
        if(capture.regionId!==null&&!region){reasons.push('The captured source region was removed or replaced.');continue;}
        if(region&&capture.bounds&&(['x','y','w','h'] as const).some(key=>region[key]!==capture.bounds![key]))
            reasons.push('The source crop geometry changed.');
        if(capture.method==='text'&&capture.text!==(region?.text??pageText(page)))reasons.push('The text sent to the model changed.');
    }
    return {current:reasons.length===0,reasons:[...new Set(reasons)]};
}

function storeCollection(project:OcrProject,pageId:string,collection:ExtractionObjects):OcrProject {
    const checked=readExtractionObjects(collection)!;
    return {...project,updatedAt:now(),pages:project.pages.map(page=>page.id!==pageId?page:
        {...page,approved:false,provenance:{...page.provenance,extractionObjects:checked}})};
}

/** A delayed result stays attached to its captured page, explicitly historical if stale. */
export function appendExtractionObject(project:OcrProject,request:ExtractionRequest,kind:ExtractionKind,output:unknown,
    options:{name?:string;labels?:string[]}={}):OcrProject {
    if(project.id!==request.projectId||!validId(request.objectId)||request.objectId.length>160||!date(request.createdAt))
        return fail('the extraction request belongs to another project or is invalid');
    const capture=readCapture(request.capture),page=project.pages.find(candidate=>candidate.id===capture.pageId);
    if(!page)return fail('the captured page was removed; the result was not attached to another page');
    if(!['fields','table','formula'].includes(kind))return fail('unsupported extraction kind');
    const collection=getExtractionObjects(page).collection;
    if(collection.objects.some(entry=>entry.id===request.objectId))return fail('this extraction request was already stored');
    const originalOutput=readOutput(output);
    const base={version:1,id:request.objectId,kind,name:options.name??kindName(kind)+' '+(collection.objects.filter(entry=>entry.kind===kind).length+1),
        createdAt:request.createdAt,updatedAt:now(),excluded:false,review:'unreviewed',captures:[capture],originalOutput,originalOutputKind:'model'};
    let data:ExtractionFieldsData|TableData|ExtractionFormulaData;
    if(kind==='fields')data=fieldsFromOutput(output,request.objectId,options.labels??[]);
    else if(kind==='table') {
        if(!object(output)||typeof output.html!=='string')return fail('the table model did not return HTML');
        // Grid size comes from explicit HTML spans, never from guessed row/col counts.
        data=parseTableHtml(output.html,request.objectId);
    } else {
        if(typeof output!=='string'||!output.trim())return fail('the model returned no formula');
        data=readFormula({version:1,latex:output});
    }
    const entry=readObject({...base,originalData:data,currentData:data});
    return storeCollection(project,page.id,{version:1,objects:[...collection.objects,entry]});
}

function mutateObject(project:OcrProject,pageId:string,objectId:string,mutate:(entry:ExtractionObject)=>ExtractionObject):OcrProject {
    const page=project.pages.find(candidate=>candidate.id===pageId);
    if(!page)return fail('the extraction page is missing');
    const read=getExtractionObjects(page),index=read.collection.objects.findIndex(entry=>entry.id===objectId);
    if(index<0)return fail('the selected extraction no longer exists');
    const original=read.collection.objects[index],next=mutate(clone(original));
    if(JSON.stringify(original)===JSON.stringify(next))return read.migrated?storeCollection(project,pageId,read.collection):project;
    const objects=[...read.collection.objects];objects[index]=readObject({...next,updatedAt:now()});
    return storeCollection(project,pageId,{version:1,objects});
}

export function updateExtractionObject(project:OcrProject,pageId:string,objectId:string,patch:ExtractionObjectPatch):OcrProject {
    if(!object(patch)||Object.keys(patch).some(key=>!['name','excluded','review'].includes(key)))return fail('unsupported extraction metadata change');
    return mutateObject(project,pageId,objectId,entry=>({...entry,...patch}));
}

export function editExtractionField(project:OcrProject,pageId:string,objectId:string,fieldId:string,patch:{label?:string;value?:string}):OcrProject {
    if(!object(patch)||Object.keys(patch).some(key=>!['label','value'].includes(key)))return fail('unsupported field edit');
    return mutateObject(project,pageId,objectId,entry=>{
        if(entry.kind!=='fields')return fail('the selected object is not a field extraction');
        const index=entry.currentData.fields.findIndex(field=>field.id===fieldId);
        if(index<0)return fail('the selected field is missing');
        const previous=entry.currentData.fields[index],next={...previous,...patch};
        if(next.label===previous.label&&next.value===previous.value)return entry;
        const original=entry.originalData.fields.find(field=>field.id===fieldId);
        next.score=original&&next.label===original.label&&next.value===original.value?original.score:null;
        entry.currentData.fields[index]=next;entry.review='corrected';return entry;
    });
}

export function editExtractionCell(project:OcrProject,pageId:string,objectId:string,cellId:string,text:string):OcrProject {
    return mutateObject(project,pageId,objectId,entry=>{
        if(entry.kind!=='table')return fail('the selected object is not a table');
        const cell=entry.currentData.cells.find(cell=>cell.id===cellId);
        if(!cell)return fail('the selected cell is missing');
        if(cell.text===text)return entry;
        cell.text=text;entry.review='corrected';return entry;
    });
}

/** Structural repair replaces current grid data; captured originals stay immutable. */
export function replaceExtractionTableData(project:OcrProject,pageId:string,objectId:string,data:TableData):OcrProject {
    const checked=readTableData(data);
    return mutateObject(project,pageId,objectId,entry=>{
        if(entry.kind!=='table')return fail('the selected object is not a table');
        if(JSON.stringify(entry.currentData)===JSON.stringify(checked))return entry;
        entry.currentData=checked;entry.review='corrected';return entry;
    });
}

export function editExtractionFormula(project:OcrProject,pageId:string,objectId:string,latex:string):OcrProject {
    return mutateObject(project,pageId,objectId,entry=>{
        if(entry.kind!=='formula')return fail('the selected object is not a formula');
        if(entry.currentData.latex===latex)return entry;
        entry.currentData.latex=latex;entry.review='corrected';return entry;
    });
}

/** Selected-object JSON includes immutable model output, editable data and captures. */
export function serializeExtractionObject(entry:ExtractionObject):string {
    return JSON.stringify(readObject(entry),null,2);
}
