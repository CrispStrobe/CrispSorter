import type { OcrProject, OcrRegion } from './model';
import { getExtractionObjects, extractionObjectFreshness, replaceExtractionTableData, type ExtractionObject } from './extractionObjects';
import { readTableData, MAX_TABLE_DIMENSION, MAX_TABLE_GRID, type TableData, type TableCell } from './tableData';
import { joinRegionText } from './text';
export type { TableData, TableCell } from './tableData';

export type TableMergeSeparator=''|' '|'\n'|'\n\n';
/** Grid indices are zero-based; insertion also accepts the dimension's end. */
export type TableOperationRequest=
    |{kind:'insert-row'|'delete-row'|'insert-column'|'delete-column';index:number}
    |{kind:'merge';cellIds:string[];separator:TableMergeSeparator}
    |{kind:'split';cellId:string}
    |{kind:'header';cellIds:string[];header:boolean};
export type TableExtractionObject=Extract<ExtractionObject,{kind:'table'}>;
export interface TableSourceState {
    captureId:string; pageId:string; documentPresent:boolean;
    page:null|{
        documentId:string;imagePath:string;width:number;height:number;
        region:Pick<OcrRegion,'id'|'x'|'y'|'w'|'h'>|null;text:string|null;
    };
}
export interface TableOperationPreview {
    version:1;projectId:string;pageId:string;objectId:string;operationId:string;request:TableOperationRequest;
    sourceObject:TableExtractionObject;sourceStates:TableSourceState[];
    before:TableData;after:TableData;affectedCellIds:string[];createdCellIds:string[];
    deletedCells:TableCell[];mergedCells:TableCell[];mergeText?:string;warnings:string[];
}
const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const record=(value:unknown):value is Record<string,unknown>=>!!value&&typeof value==='object'&&!Array.isArray(value);
const fail=(reason:string):never=>{throw new Error(reason+' Existing table data was preserved.');};
const operationId=():string=>'table-op-'+(globalThis.crypto?.randomUUID?.()??Date.now().toString(36)+'-'+Math.random().toString(36).slice(2));
const rowMajor=(left:TableCell,right:TableCell):number=>left.row-right.row||left.column-right.column;
const noBox=(cell:TableCell):TableCell=>{const {box:_box,...rest}=cell;return rest;};

/** JSON object-key order may change in native storage; array order remains exact. */
function same(left:unknown,right:unknown):boolean {
    if(left===right)return true;
    if(Array.isArray(left)||Array.isArray(right)) {
        return Array.isArray(left)&&Array.isArray(right)&&left.length===right.length&&left.every((value,index)=>same(value,right[index]));
    }
    if(!record(left)||!record(right))return false;
    const leftKeys=Object.keys(left).filter(key=>left[key]!==undefined),rightKeys=Object.keys(right).filter(key=>right[key]!==undefined);
    return leftKeys.length===rightKeys.length&&leftKeys.every(key=>Object.prototype.hasOwnProperty.call(right,key)&&same(left[key],right[key]));
}

function sourceStates(project:OcrProject,object:TableExtractionObject):TableSourceState[] {
    const textCache=new Map<string,string>();
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
                if(!textCache.has(page.id))textCache.set(page.id,joinRegionText(page.regions.filter(region=>!region.excluded&&region.kind!=='picture')));
                text=textCache.get(page.id)!;
            }
        }
        state.page={documentId:page.documentId,imagePath:page.imagePath,width:page.width,height:page.height,
            region:region?{id:region.id,x:region.x,y:region.y,w:region.w,h:region.h}:null,text};
        return state;
    });
}

function selectedCells(table:TableData,ids:unknown,minimum=1):TableCell[] {
    if(!Array.isArray(ids)||ids.length<minimum||ids.length>MAX_TABLE_GRID||
        ids.some(id=>typeof id!=='string')||new Set(ids).size!==ids.length)return fail('Choose distinct whole table cells.');
    const selected=new Set(ids),cells=table.cells.filter(cell=>selected.has(cell.id));
    if(cells.length!==ids.length)return fail('A selected cell no longer exists.');
    return cells.sort(rowMajor);
}

function validRequest(raw:unknown):TableOperationRequest {
    if(!record(raw)||typeof raw.kind!=='string')return fail('Choose a supported table repair.');
    const keys=raw.kind==='merge'?['kind','cellIds','separator']:raw.kind==='split'?['kind','cellId']:
        raw.kind==='header'?['kind','cellIds','header']:['kind','index'];
    if(Object.keys(raw).some(key=>!keys.includes(key)))return fail('The table repair request contains unsupported fields.');
    if(['insert-row','delete-row','insert-column','delete-column'].includes(raw.kind)) {
        if(!Number.isSafeInteger(raw.index))return fail('Choose a whole row or column index.');
    } else if(raw.kind==='merge') {
        if(!['',' ','\n','\n\n'].includes(raw.separator as string))return fail('Choose no separator, a space, a newline, or a blank line.');
    } else if(raw.kind==='split') {
        if(typeof raw.cellId!=='string')return fail('Choose the spanned cell to split.');
    } else if(raw.kind==='header') {
        if(typeof raw.header!=='boolean')return fail('Choose a header state.');
    } else return fail('Choose a supported table repair.');
    return clone(raw) as unknown as TableOperationRequest;
}

function buildPreview(project:OcrProject,pageId:string,objectId:string,requestValue:TableOperationRequest,seed:string):TableOperationPreview|null {
    if(typeof seed!=='string'||!/^table-op-[A-Za-z0-9-]{1,100}$/u.test(seed))return fail('Invalid table repair preview identity.');
    const page=project.pages.find(page=>page.id===pageId);
    if(!page)return fail('The table page no longer exists.');
    const sourceObject=getExtractionObjects(page).collection.objects.find(object=>object.id===objectId);
    if(!sourceObject||sourceObject.kind!=='table')return fail('Choose an existing table extraction.');
    const request=validRequest(requestValue),before=readTableData(sourceObject.currentData);
    let after=clone(before),deletedCells:TableCell[]=[],mergedCells:TableCell[]=[],mergeText:string|undefined;
    const warnings:string[]=[],occupiedIds=new Set([...before.cells,...sourceObject.originalData.cells].map(cell=>cell.id));
    const freshId=(suffix:string):string=>{
        const id=seed+'-'+suffix;if(occupiedIds.has(id))return fail('A new cell identity is already in use. Review the repair again.');
        occupiedIds.add(id);return id;
    };
    const freshCell=(row:number,column:number):TableCell=>({id:freshId('r'+row+'c'+column),row,column,rowSpan:1,columnSpan:1,text:''});
    if(request.kind==='insert-row'||request.kind==='insert-column') {
        const axis=request.kind==='insert-row'?'row':'column',extent=axis==='row'?'rows':'columns',span=axis==='row'?'rowSpan':'columnSpan';
        if(request.index<0||request.index>before[extent])return fail('The insertion index is outside this table.');
        if(!before.rows) {
            after={version:1,rows:1,columns:1,cells:[freshCell(0,0)]};
            warnings.push('The empty table becomes one empty cell with unspecified pixel geometry.');
        } else {
            after[extent]++;
            if(after.rows>MAX_TABLE_DIMENSION||after.columns>MAX_TABLE_DIMENSION||after.rows*after.columns>MAX_TABLE_GRID)
                return fail('The insertion exceeds the 1,000-dimension or 10,000-position table limit.');
            let grown=0;
            after.cells=after.cells.map(cell=>{
                if(cell[axis]>=request.index)return {...cell,[axis]:cell[axis]+1};
                if(cell[axis]+cell[span]>request.index){grown++;return {...noBox(cell),[span]:cell[span]+1};}
                return cell;
            });
            const across=axis==='row'?after.columns:after.rows;
            for(let position=0;position<across;position++) {
                const row=axis==='row'?request.index:position,column=axis==='column'?request.index:position;
                if(!after.cells.some(cell=>row>=cell.row&&row<cell.row+cell.rowSpan&&column>=cell.column&&column<cell.column+cell.columnSpan))
                    after.cells.push(freshCell(row,column));
            }
            if(grown)warnings.push(grown+' spanning cell(s) extend across the inserted '+axis+' and retain their complete text.');
        }
    } else if(request.kind==='delete-row'||request.kind==='delete-column') {
        const axis=request.kind==='delete-row'?'row':'column',extent=axis==='row'?'rows':'columns',span=axis==='row'?'rowSpan':'columnSpan';
        if(request.index<0||request.index>=before[extent])return fail('Choose an existing row or column to delete.');
        let partial=0;
        after.cells=after.cells.flatMap(cell=>{
            if(cell[axis]>request.index)return [{...cell,[axis]:cell[axis]-1}];
            if(cell[axis]+cell[span]<=request.index)return [cell];
            if(cell[span]===1){deletedCells.push(clone(cell));return [];}
            partial++;return [{...noBox(cell),[span]:cell[span]-1}];
        });
        after[extent]--;
        if(!after[extent])after={version:1,rows:0,columns:0,cells:[]};
        if(partial)warnings.push('All text remains in the top-left surviving area of '+partial+' partially deleted spanning cell(s).');
        if(deletedCells.length)warnings.push(deletedCells.length+' whole cell(s) will be removed. The preview lists their exact current text; session Undo can restore it.');
    } else if(request.kind==='merge') {
        mergedCells=selectedCells(before,request.cellIds,2);
        const row=Math.min(...mergedCells.map(cell=>cell.row)),column=Math.min(...mergedCells.map(cell=>cell.column));
        const rowSpan=Math.max(...mergedCells.map(cell=>cell.row+cell.rowSpan))-row,columnSpan=Math.max(...mergedCells.map(cell=>cell.column+cell.columnSpan))-column;
        if(mergedCells.reduce((sum,cell)=>sum+cell.rowSpan*cell.columnSpan,0)!==rowSpan*columnSpan)
            return fail('Select complete cells whose combined coverage is one rectangle, with no gaps.');
        const selected=new Set(mergedCells.map(cell=>cell.id)),allHeaders=mergedCells.every(cell=>cell.header===true);
        mergeText=mergedCells.map(cell=>cell.text).join(request.separator);
        const merged:TableCell={id:freshId('merged'),row,column,rowSpan,columnSpan,text:mergeText,...(allHeaders?{header:true}:{})};
        after.cells=[...after.cells.filter(cell=>!selected.has(cell.id)),merged];
        if(!allHeaders&&mergedCells.some(cell=>cell.header))warnings.push('This mixed header/body selection becomes a body cell. You can set its header state explicitly afterward.');
        warnings.push('Complete cell text is combined in row order using the selected separator, including empty values.');
    } else if(request.kind==='split') {
        const selected=selectedCells(before,[request.cellId])[0];
        if(selected.rowSpan===1&&selected.columnSpan===1)return fail('Choose a cell spanning more than one grid position.');
        const pieces:TableCell[]=[];
        for(let row=selected.row;row<selected.row+selected.rowSpan;row++)for(let column=selected.column;column<selected.column+selected.columnSpan;column++) {
            const first=row===selected.row&&column===selected.column;
            pieces.push({id:first?selected.id:freshId('r'+row+'c'+column),row,column,rowSpan:1,columnSpan:1,text:first?selected.text:'',
                ...(selected.header!==undefined?{header:selected.header}:{})});
        }
        after.cells=[...after.cells.filter(cell=>cell.id!==selected.id),...pieces];
        warnings.push('All text stays in the top-left cell. The other split cells are empty and inherit the same header state.');
    } else if(request.kind==='header') {
        const selected=new Set(selectedCells(before,request.cellIds).map(cell=>cell.id));
        after.cells=after.cells.map(cell=>{
            if(!selected.has(cell.id)||(cell.header===true)===request.header)return cell;
            if(request.header)return {...cell,header:true};
            const {header:_header,...body}=cell;return body;
        });
        if(same(before,after))return null;
    } else return fail('Choose a supported table repair.');
    if(request.kind!=='header')after.cells.sort(rowMajor);
    after=readTableData(after);
    // Pure preflight of the complete collection's evidence and byte limits.
    replaceExtractionTableData(project,pageId,objectId,after);
    const oldIds=new Set(before.cells.map(cell=>cell.id)),afterById=new Map(after.cells.map(cell=>[cell.id,cell]));
    const affectedCellIds=before.cells.filter(cell=>!same(cell,afterById.get(cell.id))).map(cell=>cell.id);
    const removedBoxes=before.cells.filter(cell=>cell.box&&!afterById.get(cell.id)?.box).length;
    if(removedBoxes)warnings.push(removedBoxes+' current cell box(es) are removed with the changed grid coverage. Captured original data remains unchanged.');
    const freshness=extractionObjectFreshness(project,sourceObject);
    if(!freshness.current)warnings.unshift('This table refers to historical sources. '+freshness.reasons.join(' '));
    return {version:1,projectId:project.id,pageId,objectId,operationId:seed,request,
        sourceObject,sourceStates:sourceStates(project,sourceObject),before,after,affectedCellIds,
        createdCellIds:after.cells.filter(cell=>!oldIds.has(cell.id)).map(cell=>cell.id),
        deletedCells:deletedCells.sort(rowMajor),mergedCells:clone(mergedCells),...(mergeText!==undefined?{mergeText}:{}),warnings};
}

export function prepareTableOperation(project:OcrProject,pageId:string,objectId:string,request:TableOperationRequest):TableOperationPreview|null {
    return buildPreview(project,pageId,objectId,request,operationId());
}

/** Apply exactly the inspected repair; source/object changes require a fresh preview. */
export function applyTableOperation(project:OcrProject,preview:TableOperationPreview):OcrProject {
    if(!preview||project.id!==preview.projectId)return fail('The table repair belongs to another project.');
    const fresh=buildPreview(project,preview.pageId,preview.objectId,preview.request,preview.operationId);
    if(!fresh||!same(fresh,preview))return fail('The table, source, or repair preview changed. Review the operation again.');
    return replaceExtractionTableData(project,preview.pageId,preview.objectId,fresh.after);
}
