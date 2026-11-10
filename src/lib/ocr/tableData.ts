import { tableToCsv } from './extraction';
import type { PixelRectangle } from './zones';

export interface TableCell {
    id:string; row:number; column:number; rowSpan:number; columnSpan:number; text:string;
    header?:boolean;
    /** Only measured source geometry; grid positions never imply pixel boxes. */
    box?:{captureId:string;bounds:PixelRectangle};
}
export interface TableData { version:1; rows:number; columns:number; cells:TableCell[]; }
export const MAX_TABLE_GRID=10_000;
export const MAX_TABLE_DIMENSION=1_000;
export const MAX_EXTRACTION_TEXT=1_000_000;
const object=(value:unknown):value is Record<string,unknown>=>!!value&&typeof value==='object'&&!Array.isArray(value);
const identifier=(value:unknown):value is string=>typeof value==='string'&&!!value.trim()&&value.length<=200&&!value.includes('\0');
const integer=(value:unknown):value is number=>Number.isSafeInteger(value);
const finite=(value:unknown):value is number=>typeof value==='number'&&Number.isFinite(value);
const fail=(reason:string):never=>{throw new Error('Invalid table data: '+reason+'. Existing extraction data was preserved.');};

/** Validate an explicit span grid without filling gaps or guessing geometry. */
export function readTableData(raw:unknown):TableData {
    if(!object(raw)||Object.keys(raw).some(key=>!['version','rows','columns','cells'].includes(key))||raw.version!==1||
        !integer(raw.rows)||!integer(raw.columns)||raw.rows<0||raw.columns<0||
        raw.rows>MAX_TABLE_DIMENSION||raw.columns>MAX_TABLE_DIMENSION||raw.rows*raw.columns>MAX_TABLE_GRID||
        !Array.isArray(raw.cells)||raw.cells.length>MAX_TABLE_GRID)return fail('unsupported version or grid size');
    if((raw.rows===0||raw.columns===0)&&(raw.rows!==0||raw.columns!==0||raw.cells.length))return fail('an empty table must be exactly 0 by 0');
    const rows=raw.rows,columns=raw.columns,occupied=new Uint8Array(rows*columns),ids=new Set<string>();
    let textUnits=0;
    const cells=raw.cells.map((value):TableCell=>{
        if(!object(value)||Object.keys(value).some(key=>!['id','row','column','rowSpan','columnSpan','text','header','box'].includes(key))||
            !identifier(value.id)||ids.has(value.id)||!integer(value.row)||!integer(value.column)||
            !integer(value.rowSpan)||!integer(value.columnSpan)||value.row<0||value.column<0||value.rowSpan<1||value.columnSpan<1||
            value.row+value.rowSpan>rows||value.column+value.columnSpan>columns||
            typeof value.text!=='string'||value.text.includes('\0')||
            (value.header!==undefined&&typeof value.header!=='boolean'))return fail('invalid cell identity, range, text, or header');
        textUnits+=value.text.length;if(textUnits>MAX_EXTRACTION_TEXT)return fail('cell text exceeds 1,000,000 units');
        ids.add(value.id);
        for(let row:number=value.row;row<value.row+value.rowSpan;row++)for(let column:number=value.column;column<value.column+value.columnSpan;column++) {
            const index=row*columns+column;
            if(occupied[index])return fail('cell spans overlap');
            occupied[index]=1;
        }
        let box:TableCell['box'];
        if(value.box!==undefined) {
            if(!object(value.box)||Object.keys(value.box).some(key=>!['captureId','bounds'].includes(key))||
                !identifier(value.box.captureId)||!object(value.box.bounds))return fail('invalid measured cell box');
            const bounds=value.box.bounds;
            if(Object.keys(bounds).some(key=>!['x','y','w','h'].includes(key))||
                !finite(bounds.x)||!finite(bounds.y)||!finite(bounds.w)||!finite(bounds.h)||
                bounds.x<0||bounds.y<0||bounds.w<=0||bounds.h<=0||bounds.x+bounds.w>100_000||bounds.y+bounds.h>100_000)
                return fail('invalid measured cell box');
            box={captureId:value.box.captureId,bounds:{x:bounds.x,y:bounds.y,w:bounds.w,h:bounds.h}};
        }
        return {id:value.id,row:value.row,column:value.column,rowSpan:value.rowSpan,columnSpan:value.columnSpan,text:value.text,
            ...(value.header!==undefined?{header:value.header as boolean}:{}),...(box?{box}:{})};
    });
    if(occupied.some(value=>value!==1))return fail('cell spans do not cover the declared grid');
    return {version:1,rows,columns,cells};
}

/** Legacy rectangular rows have no cell geometry or recovered span evidence. */
export function tableDataFromRows(raw:unknown,idPrefix:string):TableData {
    if(!identifier(idPrefix)||idPrefix.length>170||!Array.isArray(raw)||raw.length>MAX_TABLE_DIMENSION)return fail('invalid legacy rows or cell identity prefix');
    if(!raw.length)return {version:1,rows:0,columns:0,cells:[]};
    if(!Array.isArray(raw[0])||!raw[0].length)return fail('legacy rows must be a nonempty rectangle');
    const columns=raw[0].length;
    if(columns>MAX_TABLE_DIMENSION||raw.length*columns>MAX_TABLE_GRID||
        raw.some(row=>!Array.isArray(row)||row.length!==columns||row.some(text=>typeof text!=='string')))
        return fail('legacy rows must be a bounded rectangle of literal strings');
    return readTableData({version:1,rows:raw.length,columns,cells:raw.flatMap((row,index)=>
        (row as string[]).map((text,column)=>({id:idPrefix+'-r'+index+'c'+column,row:index,column,rowSpan:1,columnSpan:1,text})))});
}

/** Spanning cells occupy one anchor value; covered CSV grid positions stay empty. */
export function tableDataToRows(value:TableData):string[][] {
    const table=readTableData(value),rows=Array.from({length:table.rows},()=>Array.from({length:table.columns},()=>''));
    for(const cell of table.cells)rows[cell.row][cell.column]=cell.text;
    return rows;
}

export function tableDataToCsv(value:TableData):string {
    return tableToCsv(tableDataToRows(value));
}
