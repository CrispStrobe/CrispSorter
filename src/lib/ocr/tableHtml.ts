import { parseFragment } from 'parse5';
import { readTableData, MAX_TABLE_DIMENSION, MAX_TABLE_GRID, type TableCell, type TableData } from './tableData';

interface HtmlNode {
    nodeName:string; tagName?:string; value?:string; childNodes?:HtmlNode[];
    attrs?:{name:string;value:string}[];
}
const fail=(message:string):never=>{throw new Error('The table model output cannot be imported: '+message+'. Existing extraction data was preserved.');};
const activeTags=new Set(['script','style','iframe','object','embed','form','input','button','select','textarea',
    'img','svg','math','canvas','link','meta','base','template','audio','video']);

function cellText(node:HtmlNode):string {
    if(node.nodeName==='#text')return node.value??'';
    if(node.nodeName==='#comment')return '';
    if(activeTags.has(node.tagName??''))return '';
    if(node.tagName==='br')return '\n';
    return (node.childNodes??[]).map(cellText).join('');
}

function span(node:HtmlNode,name:'rowspan'|'colspan',remainingRows:number):number {
    const raw=node.attrs?.find(attribute=>attribute.name===name)?.value;
    if(raw===undefined)return 1;
    if(!/^\d+$/u.test(raw.trim()))return fail('invalid '+name);
    const value=Number(raw.trim());
    if(name==='rowspan'&&value===0)return remainingRows;
    if(!Number.isSafeInteger(value)||value<1||value>MAX_TABLE_DIMENSION)return fail('unsupported '+name);
    return value;
}

/** Parse an inert HTML tree. No source HTML, attributes, or styles are rendered. */
export function parseTableHtml(html:string,idPrefix:string):TableData {
    if(typeof html!=='string'||html.length>4*1024*1024||html.includes('\0')||
        typeof idPrefix!=='string'||!idPrefix.trim()||idPrefix.length>170||idPrefix.includes('\0'))
        return fail('invalid HTML size or cell identity prefix');
    const errors:string[]=[];
    const tree=parseFragment(html,{onParseError:error=>{if(error.code==='duplicate-attribute')errors.push(error.code);}}) as unknown as HtmlNode;
    if(errors.length)return fail('duplicate attributes make cell spans ambiguous');
    const tables:HtmlNode[]=[],pending:{node:HtmlNode;depth:number}[]=[{node:tree,depth:0}];
    let nodes=0;
    // Bound the tree before recursive text reads. Active subtrees are inert,
    // omitted entirely, and remain available only in originalOutput.
    while(pending.length) {
        const {node,depth}=pending.pop()!;
        if(++nodes>100_000||depth>128)return fail('the HTML tree is too deeply nested or complex');
        if(activeTags.has(node.tagName??''))continue;
        if(node.tagName==='table')tables.push(node);
        for(const child of node.childNodes??[])pending.push({node:child,depth:depth+1});
    }
    if(tables.length!==1)return fail('exactly one nonnested table is required');
    const table=tables[0];
    function outsideText(node:HtmlNode):string {
        if(node===table||node.nodeName==='#comment'||activeTags.has(node.tagName??''))return '';
        if(node.nodeName==='#text')return node.value??'';
        return (node.childNodes??[]).map(outsideText).join('');
    }
    if(outsideText(tree).trim())return fail('text outside the table would be lost');
    const groups:HtmlNode[][]=[];
    let loose:HtmlNode[]=[];
    for(const child of table.childNodes??[]) {
        if(activeTags.has(child.tagName??''))continue;
        if(child.tagName==='caption'&&cellText(child).trim())return fail('nonempty captions are not yet represented by the cell grid');
        if(child.tagName==='tr'){loose.push(child);continue;}
        if(loose.length){groups.push(loose);loose=[];}
        if(['thead','tbody','tfoot'].includes(child.tagName??'')) {
            const rows:HtmlNode[]=[];
            for(const row of child.childNodes??[]) {
                if(activeTags.has(row.tagName??''))continue;
                if(row.tagName==='tr')rows.push(row);
                else if(row.nodeName!=='#comment'&&(row.nodeName!=='#text'||row.value?.trim()))return fail('unexpected content in a table row group');
            }
            groups.push(rows);
        } else if(!['caption','colgroup'].includes(child.tagName??'')&&child.nodeName!=='#comment'&&
            (child.nodeName!=='#text'||child.value?.trim()))return fail('unexpected content in a table');
    }
    if(loose.length)groups.push(loose);
    const rowCount=groups.reduce((count,group)=>count+group.length,0);
    if(rowCount>MAX_TABLE_DIMENSION)return fail('too many rows');
    const occupied=Array.from({length:rowCount},()=>new Set<number>()),cells:TableCell[]=[];
    let rowIndex=0,columns=0;
    for(const group of groups)for(let localRow=0;localRow<group.length;localRow++,rowIndex++) {
        let column=0;
        for(const node of group[localRow].childNodes??[]) {
            if(activeTags.has(node.tagName??''))continue;
            if(node.nodeName==='#comment'||node.nodeName==='#text'&&!node.value?.trim())continue;
            if(node.tagName!=='td'&&node.tagName!=='th')return fail('a row contains content outside its cells');
            while(occupied[rowIndex].has(column))column++;
            const rowSpan=span(node,'rowspan',group.length-localRow),columnSpan=span(node,'colspan',group.length-localRow);
            if(rowSpan>group.length-localRow)return fail('a cell span crosses a row-group boundary');
            if(column+columnSpan>MAX_TABLE_DIMENSION||rowCount*(column+columnSpan)>MAX_TABLE_GRID)return fail('the grid exceeds 10,000 positions');
            for(let row=rowIndex;row<rowIndex+rowSpan;row++)for(let covered=column;covered<column+columnSpan;covered++) {
                if(occupied[row].has(covered))return fail('cell spans overlap');
                occupied[row].add(covered);
            }
            cells.push({id:idPrefix+'-r'+rowIndex+'c'+column,row:rowIndex,column,rowSpan,columnSpan,text:cellText(node),
                ...(node.tagName==='th'?{header:true}:{})});
            column+=columnSpan;columns=Math.max(columns,column);
        }
    }
    if(!rowCount&&!cells.length)return {version:1,rows:0,columns:0,cells:[]};
    return readTableData({version:1,rows:rowCount,columns,cells});
}
