import type { OcrProject, OcrRegion } from './model';
import {
    appendJoinedTableObject, extractionObjectFreshness, getExtractionObjects, readExtractionObjects,
    type ExtractionCapture, type JsonValue, type TableSourceSnapshot, type TableJoinReceipt,
} from './extractionObjects';
import { MAX_TABLE_DIMENSION, MAX_TABLE_GRID, readTableData, type TableCell, type TableData } from './tableData';
import type { TableExtractionObject, TableSourceState } from './tableOperations';
import { joinRegionText } from './text';

export interface ContinuationJoinInput { pageId:string;objectId:string;dropHeaderRows:number;columnMap?:number[]; }
export interface ContinuationJoinRequest { inputs:ContinuationJoinInput[];targetColumns?:number;name:string; }
export interface ContinuationJoinPreview {
    version:1;projectId:string;operationId:string;createdAt:string;request:ContinuationJoinRequest;
    inputs:{pageId:string;object:TableExtractionObject;sourceStates:TableSourceState[]}[];
    result:TableExtractionObject;removedHeaderCells:{inputIndex:number;cell:TableCell}[];warnings:string[];
}
const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const record=(value:unknown):value is Record<string,unknown>=>!!value&&typeof value==='object'&&!Array.isArray(value);
const id=(value:unknown):value is string=>typeof value==='string'&&!!value.trim()&&value.length<=200&&!value.includes('\0');
const fail=(reason:string):never=>{throw new Error(reason+' Existing tables were preserved.');};
const operationId=():string=>'table-join-'+(globalThis.crypto?.randomUUID?.()??Date.now().toString(36)+'-'+Math.random().toString(36).slice(2));
const same=(left:unknown,right:unknown):boolean=>{
    if(left===right)return true;
    if(Array.isArray(left)||Array.isArray(right))return Array.isArray(left)&&Array.isArray(right)&&left.length===right.length&&left.every((value,index)=>same(value,right[index]));
    if(!record(left)||!record(right))return false;
    const a=Object.keys(left).filter(key=>left[key]!==undefined),b=Object.keys(right).filter(key=>right[key]!==undefined);
    return a.length===b.length&&a.every(key=>Object.prototype.hasOwnProperty.call(right,key)&&same(left[key],right[key]));
};
const rowMajor=(left:TableCell,right:TableCell):number=>left.row-right.row||left.column-right.column;

function readRequest(raw:unknown):ContinuationJoinRequest {
    if(!record(raw)||Object.keys(raw).some(key=>!['inputs','targetColumns','name'].includes(key))||
        typeof raw.name!=='string'||raw.name.length>1000||raw.name.includes('\0')||
        !Array.isArray(raw.inputs)||raw.inputs.length<2||raw.inputs.length>16||
        (raw.targetColumns!==undefined&&(!Number.isSafeInteger(raw.targetColumns)||(raw.targetColumns as number)<1||(raw.targetColumns as number)>MAX_TABLE_DIMENSION)))
        return fail('Choose 2–16 tables, a bounded name, and a valid destination column count.');
    const inputs=raw.inputs.map((value,index):ContinuationJoinInput=>{
        if(!record(value)||Object.keys(value).some(key=>!['pageId','objectId','dropHeaderRows','columnMap'].includes(key))||
            !id(value.pageId)||!id(value.objectId)||!Number.isSafeInteger(value.dropHeaderRows)||(value.dropHeaderRows as number)<0||
            (value.dropHeaderRows as number)>MAX_TABLE_DIMENSION||(index===0&&value.dropHeaderRows!==0)||
            (value.columnMap!==undefined&&(!Array.isArray(value.columnMap)||value.columnMap.length>MAX_TABLE_DIMENSION||
                value.columnMap.some(column=>!Number.isSafeInteger(column)||column<0||column>=MAX_TABLE_DIMENSION))))
            return fail('Each input needs a valid table identity, zero-based mapping, and header count. The first table keeps all its rows.');
        return {pageId:value.pageId,objectId:value.objectId,dropHeaderRows:value.dropHeaderRows as number,
            ...(value.columnMap!==undefined?{columnMap:[...value.columnMap as number[]]}:{})};
    });
    if(new Set(inputs.map(input=>JSON.stringify([input.pageId,input.objectId]))).size!==inputs.length)return fail('Select each input table only once.');
    return {inputs,name:raw.name,...(raw.targetColumns!==undefined?{targetColumns:raw.targetColumns as number}:{})};
}

function captureSourceStates(project:OcrProject,object:TableExtractionObject):TableSourceState[] {
    const texts=new Map<string,string>();
    return object.captures.map(capture=>{
        const page=project.pages.find(page=>page.id===capture.pageId);
        const state:TableSourceState={captureId:capture.id,pageId:capture.pageId,
            documentPresent:project.documents.some(document=>document.id===capture.documentId),page:null};
        if(!page)return state;
        const region=capture.regionId===null?undefined:page.regions.find(region=>region.id===capture.regionId);
        let text:string|null=null;
        if(capture.method==='text'&&(capture.regionId===null||region)) {
            if(region)text=region.text;
            else {
                if(!texts.has(page.id))texts.set(page.id,joinRegionText(page.regions.filter(region=>!region.excluded&&region.kind!=='picture')));
                text=texts.get(page.id)!;
            }
        }
        const bounds:Pick<OcrRegion,'id'|'x'|'y'|'w'|'h'>|null=region?{id:region.id,x:region.x,y:region.y,w:region.w,h:region.h}:null;
        state.page={documentId:page.documentId,imagePath:page.imagePath,width:page.width,height:page.height,region:bounds,text};
        return state;
    });
}

function headerValues(table:TableData,rows:number,map:number[],columns:number):string[][]|null {
    if(rows>table.rows)return null;
    const result=Array.from({length:rows},()=>Array.from({length:columns},()=>''));
    for(const cell of table.cells)if(cell.row<rows) {
        if(cell.row+cell.rowSpan>rows)return null;
        result[cell.row][Math.min(...map.slice(cell.column,cell.column+cell.columnSpan))]=cell.text;
    }
    return result;
}

function buildPreview(project:OcrProject,requestValue:ContinuationJoinRequest,seed:string,createdAt:string):ContinuationJoinPreview {
    if(typeof seed!=='string'||!/^table-join-[A-Za-z0-9-]{1,100}$/u.test(seed)||typeof createdAt!=='string'||
        createdAt.length>64||!Number.isFinite(Date.parse(createdAt)))return fail('Invalid continuation preview identity or timestamp.');
    const request=readRequest(requestValue),warnings:string[]=[];
    const collections=new Map<string,ReturnType<typeof getExtractionObjects>>();
    const inputs=request.inputs.map((input,index)=>{
        const page=project.pages.find(page=>page.id===input.pageId);
        if(!page)return fail('An input table page no longer exists.');
        if(!collections.has(page.id))collections.set(page.id,getExtractionObjects(page));
        const object=collections.get(page.id)!.collection.objects.find(object=>object.id===input.objectId);
        if(!object||object.kind!=='table'||!object.currentData.rows||!object.currentData.columns)return fail('Every input must be an existing table with a nonempty grid.');
        if(input.dropHeaderRows>object.currentData.rows)return fail('Header removal exceeds an input table.');
        if(page.excluded||object.excluded)warnings.push('Input '+(index+1)+' is excluded in the project; this explicit join leaves its exclusion unchanged.');
        const freshness=extractionObjectFreshness(project,object);
        if(!freshness.current)warnings.push('Input '+(index+1)+' refers to historical sources. '+freshness.reasons.join(' '));
        return {pageId:page.id,object,sourceStates:captureSourceStates(project,object)};
    });
    const different=inputs.some(input=>input.object.currentData.columns!==inputs[0].object.currentData.columns);
    if(different&&(request.targetColumns===undefined||request.inputs.some(input=>input.columnMap===undefined)))
        return fail('Different column counts require an explicit target column count and a complete mapping for every input.');
    const columns=request.targetColumns??inputs[0].object.currentData.columns;
    const maps=request.inputs.map((input,index)=>{
        const count=inputs[index].object.currentData.columns,map=input.columnMap??Array.from({length:count},(_,column)=>column);
        if(map.length!==count||new Set(map).size!==map.length||map.some(column=>column<0||column>=columns))
            return fail('Map every source column exactly once to distinct destination columns; source columns cannot be omitted.');
        return [...map];
    });
    const rowCount=inputs.reduce((sum,input,index)=>sum+input.object.currentData.rows-request.inputs[index].dropHeaderRows,0);
    if(rowCount>MAX_TABLE_DIMENSION||rowCount*columns>MAX_TABLE_GRID)return fail('The joined table exceeds the 1,000-row or 10,000-position limit.');
    const snapshots=new Map<string,TableSourceSnapshot>();
    const addSnapshot=(snapshot:TableSourceSnapshot):void=>{
        const existing=snapshots.get(snapshot.id);
        if(existing&&!same(existing,snapshot))return fail('A source snapshot identity has conflicting content.');
        if(!existing)snapshots.set(snapshot.id,clone(snapshot));
        if(snapshots.size>128)return fail('The join exceeds 128 flat source snapshots. Choose fewer source tables.');
    };
    for(const input of inputs)for(const snapshot of input.object.tableSources?.snapshots??[])addSnapshot(snapshot);
    const receiptInputs:TableJoinReceipt['inputs']=[];
    for(let index=0;index<inputs.length;index++) {
        const input=inputs[index],source=input.object,snapshotId=seed+'-input-'+index;
        addSnapshot({id:snapshotId,objectId:source.id,ownerPageId:input.pageId,name:source.name,capturedAt:createdAt,sourceUpdatedAt:source.updatedAt,
            originalOutputKind:source.originalOutputKind,originalOutput:clone(source.originalOutput),originalData:clone(source.originalData),
            currentData:clone(source.currentData),captures:clone(source.captures)});
        receiptInputs.push({snapshotId,dropHeaderRows:request.inputs[index].dropHeaderRows,columnMap:maps[index]});
    }
    const captures=new Map<string,ExtractionCapture>();
    for(const snapshot of snapshots.values())for(const capture of snapshot.captures) {
        const previous=captures.get(capture.id);
        if(previous&&!same(previous,capture))return fail('A source capture identity has conflicting content.');
        if(!previous)captures.set(capture.id,clone(capture));
        if(captures.size>128)return fail('The join exceeds 128 captured sources.');
    }
    const cells:TableCell[]=[],origins:TableJoinReceipt['cellOrigins']=[],blankCellIds:string[]=[];
    const removedHeaderCells:ContinuationJoinPreview['removedHeaderCells']=[];
    let offset=0,nextCell=0;
    for(let index=0;index<inputs.length;index++) {
        const source=inputs[index].object.currentData,drop=request.inputs[index].dropHeaderRows,map=maps[index];
        const removed:TableCell[]=[];
        for(const cell of [...source.cells].sort(rowMajor)) {
            if(cell.row+cell.rowSpan<=drop){removed.push(clone(cell));removedHeaderCells.push({inputIndex:index,cell:clone(cell)});continue;}
            if(cell.row<drop)return fail('Header removal cuts through a spanning cell. Split the cell or choose a complete header boundary first.');
            const mapped=map.slice(cell.column,cell.column+cell.columnSpan).sort((left,right)=>left-right);
            if(mapped.some((column,index)=>index>0&&column!==mapped[index-1]+1))
                return fail('A spanning cell must map to consecutive destination columns. Split that span before using a disconnected mapping.');
            const cellId=seed+'-cell-'+nextCell++;
            cells.push({...clone(cell),id:cellId,row:offset+cell.row-drop,column:mapped[0]});
            origins.push({cellId,snapshotId:receiptInputs[index].snapshotId,sourceCellId:cell.id});
        }
        if(drop) {
            const prefix='Input '+(index+1)+': ';
            if(removed.some(cell=>cell.header!==true))warnings.push(prefix+'Some removed cells are not marked as headers.');
            if(!same(headerValues(source,drop,map,columns),headerValues(inputs[0].object.currentData,drop,maps[0],columns)))
                warnings.push(prefix+"Removed header rows differ from the first table's retained header rows.");
            if(drop===source.rows)warnings.push(prefix+'All rows from this continuation are removed from the joined grid.');
        }
        offset+=source.rows-drop;
    }
    const filled=new Uint8Array(rowCount*columns);
    for(const cell of cells)for(let row=cell.row;row<cell.row+cell.rowSpan;row++)for(let column=cell.column;column<cell.column+cell.columnSpan;column++) {
        const position=row*columns+column;
        if(filled[position])return fail('Mapped input cells overlap.');
        filled[position]=1;
    }
    for(let row=0;row<rowCount;row++)for(let column=0;column<columns;column++)if(!filled[row*columns+column]) {
        const cellId=seed+'-cell-'+nextCell++;
        cells.push({id:cellId,row,column,rowSpan:1,columnSpan:1,text:''});blankCellIds.push(cellId);
    }
    if(blankCellIds.length)warnings.push(blankCellIds.length+' unmapped destination position(s) become empty cells with no inferred header or pixel geometry.');
    cells.sort(rowMajor);
    const data=readTableData({version:1,rows:rowCount,columns,cells});
    const receipt:TableJoinReceipt={version:1,operation:'continuation-join',targetColumns:columns,inputs:receiptInputs,cellOrigins:origins,blankCellIds};
    const result=readExtractionObjects({version:1,objects:[{version:1,id:seed,kind:'table',name:request.name,createdAt,updatedAt:createdAt,
        excluded:false,review:'corrected',captures:[...captures.values()],originalOutput:receipt as unknown as JsonValue,
        originalOutputKind:'table-join',originalData:data,currentData:data,tableSources:{version:1,snapshots:[...snapshots.values()]}}]})!.objects[0] as TableExtractionObject;
    const destination=project.pages.find(page=>page.id===inputs[0].pageId)!;
    if(destination.excluded)warnings.push('The joined table is stored on the first input page, which remains excluded from document exports.');
    // Preflight complete destination collection size without changing the project.
    appendJoinedTableObject(project,destination.id,result);
    return {version:1,projectId:project.id,operationId:seed,createdAt,request,inputs,result,removedHeaderCells,warnings};
}

export function prepareContinuationJoin(project:OcrProject,request:ContinuationJoinRequest):ContinuationJoinPreview {
    return buildPreview(project,request,operationId(),new Date().toISOString());
}

/** Append only the inspected result; input data, review and exclusion remain untouched. */
export function applyContinuationJoin(project:OcrProject,preview:ContinuationJoinPreview):OcrProject {
    if(!preview||project.id!==preview.projectId)return fail('This continuation preview belongs to another project.');
    const fresh=buildPreview(project,preview.request,preview.operationId,preview.createdAt);
    if(!same(fresh,preview))return fail('An input table, source, or continuation preview changed. Review the join again.');
    return appendJoinedTableObject(project,fresh.inputs[0].pageId,fresh.result);
}
