import { describe, expect, it } from 'vitest';
import { readTableData, tableDataFromRows, tableDataToRows, tableDataToCsv, type TableData } from './tableData';
import { parseTableHtml } from './tableHtml';

const spans=():TableData=>({version:1,rows:2,columns:3,cells:[
    {id:'a',row:0,column:0,rowSpan:2,columnSpan:1,text:'Row header',header:true},
    {id:'b',row:0,column:1,rowSpan:1,columnSpan:2,text:'Column header',header:true},
    {id:'c',row:1,column:1,rowSpan:1,columnSpan:1,text:'=SUM(A1:A2)'},
    {id:'d',row:1,column:2,rowSpan:1,columnSpan:1,text:'-42'},
]});

describe('bounded span-aware table data',()=>{
    it('preserves explicit spans, literal text and stable identities without invented boxes',()=>{
        const input=spans(),parsed=readTableData(input);
        expect(parsed).toEqual(input);expect(parsed).not.toBe(input);expect(parsed.cells[0]).not.toBe(input.cells[0]);
        expect(parsed.cells.every(cell=>cell.box===undefined)).toBe(true);
        expect(tableDataToRows(parsed)).toEqual([['Row header','Column header',''],['','=SUM(A1:A2)','-42']]);
        expect(tableDataToCsv(parsed)).toBe('"Row header","Column header",""\r\n"","\'=SUM(A1:A2)","-42"');
        expect(input.cells[2].text).toBe('=SUM(A1:A2)');
    });
    it('requires a complete grid with no overlapping or out-of-range spans',()=>{
        const overlap=spans();overlap.cells[3].column=1;
        expect(()=>readTableData(overlap)).toThrow(/overlap/);
        const gap=spans();gap.cells.pop();expect(()=>readTableData(gap)).toThrow(/cover/);
        const outside=spans();outside.cells[0].rowSpan=3;expect(()=>readTableData(outside)).toThrow(/range/);
        const duplicate=spans();duplicate.cells[1].id='a';expect(()=>readTableData(duplicate)).toThrow(/identity/);
    });
    it('rejects unknown versions/fields, inferred numbers and excessive grid or text size',()=>{
        const input=spans();
        for(const patch of [{version:2},{html:'<table/>'},{rows:1001},{columns:1001},{rows:101,columns:100}])
            expect(()=>readTableData({...input,...patch})).toThrow();
        expect(()=>readTableData({...input,cells:input.cells.map((cell,index)=>index?cell:{...cell,text:12})})).toThrow(/text/);
        expect(()=>tableDataFromRows([['x'.repeat(1_000_001)]],'table')).toThrow(/1,000,000/);
        expect(()=>tableDataFromRows([['a','b'],['c']],'table')).toThrow(/rectangle/);
    });
    it('accepts only explicit finite positive measured boxes and preserves their capture linkage',()=>{
        const input=spans();input.cells[0].box={captureId:'captured-page',bounds:{x:10,y:20,w:30,h:40}};
        expect(readTableData(input).cells[0].box).toEqual(input.cells[0].box);
        for(const bounds of [{x:-1,y:0,w:1,h:1},{x:0,y:0,w:0,h:1},{x:0,y:0,w:Infinity,h:1}])
            expect(()=>readTableData({...input,cells:input.cells.map((cell,index)=>index?cell:{...cell,box:{captureId:'captured-page',bounds}})})).toThrow(/box/);
    });
    it('migrates only rectangular literal rows and retains empty/Unicode values exactly',()=>{
        const rows=[['0012','😀 e\u0301'],['','line\r\nbreak']];
        const table=tableDataFromRows(rows,'legacy');
        expect(tableDataToRows(table)).toEqual(rows);
        expect(table.cells.map(cell=>cell.id)).toEqual(['legacy-r0c0','legacy-r0c1','legacy-r1c0','legacy-r1c1']);
        expect(tableDataFromRows([],'empty')).toEqual({version:1,rows:0,columns:0,cells:[]});
        expect(()=>tableDataFromRows([[]],'empty-row')).toThrow(/rectangle/);
        expect(()=>readTableData({version:1,rows:0,columns:1,cells:[]})).toThrow(/empty/);
    });
});

describe('inert native table HTML parsing',()=>{
    it('keeps rowspan, colspan and header meaning without treating HTML widths as pixel evidence',()=>{
        const table=parseTableHtml('<table><tbody><tr><th rowspan="2" style="width:99px">Name</th><th colspan="2">Values</th></tr><tr><td>A</td><td>B</td></tr></tbody></table>','model');
        expect(table).toEqual({version:1,rows:2,columns:3,cells:[
            {id:'model-r0c0',row:0,column:0,rowSpan:2,columnSpan:1,text:'Name',header:true},
            {id:'model-r0c1',row:0,column:1,rowSpan:1,columnSpan:2,text:'Values',header:true},
            {id:'model-r1c1',row:1,column:1,rowSpan:1,columnSpan:1,text:'A'},
            {id:'model-r1c2',row:1,column:2,rowSpan:1,columnSpan:1,text:'B'},
        ]});
        expect(table.cells.every(cell=>!cell.box)).toBe(true);
    });
    it('uses standard HTML entities and literal text, including explicit line breaks',()=>{
        const table=parseTableHtml('<table><tr><td>Caf&eacute;&nbsp;&amp; &#x1F600;<br>next</td><td>&lt;img src=x onerror=run()&gt;</td></tr></table>','entities');
        expect(table.cells.map(cell=>cell.text)).toEqual(['Café\u00a0& 😀\nnext','<img src=x onerror=run()>']);
        expect(parseTableHtml('<table><tr><td>A<td>B</table>','optional-tags').columns).toBe(2);
    });
    it('resolves explicit rowspan zero to its row group without crossing groups',()=>{
        const table=parseTableHtml('<table><tbody><tr><td rowspan="0">A</td><td>B</td></tr><tr><td>C</td></tr></tbody><tbody><tr><td>D</td><td>E</td></tr></tbody></table>','group');
        expect(table.cells[0].rowSpan).toBe(2);
        expect(tableDataToRows(table)).toEqual([['A','B'],['','C'],['D','E']]);
        expect(()=>parseTableHtml('<table><thead><tr><th rowspan="2">A</th></tr></thead><tbody><tr><td>B</td></tr></tbody></table>','bad')).toThrow(/row-group/);
    });
    it('rejects ragged, nested, multiple and ambiguous tables instead of making up cells',()=>{
        for(const html of [
            '<table><tr><td>A</td></tr><tr><td>B</td><td>C</td></tr></table>',
            '<table><tr><td><table><tr><td>A</td></tr></table></td></tr></table>',
            '<table><tr><td>A</td></tr></table><table><tr><td>B</td></tr></table>',
            '<table><tr><td colspan="2" colspan="3">A</td></tr></table>',
            '<table><tr><td rowspan="-1">A</td></tr></table>',
            '<table><tr><td colspan="10001">A</td></tr></table>',
        ])expect(()=>parseTableHtml(html,'invalid')).toThrow();
    });
    it('omits active subtrees and attributes while retaining literal cell and link text',()=>{
        const table=parseTableHtml('<table><tr><td><img src=bad onerror="run()">Secure cell<script>run()</script></td>'+
            '<td><a href="javascript:run()">EUR 1,250.00</a><iframe src="outside">hidden</iframe></td></tr></table>'+
            '<script>run()</script>','inert');
        expect(table.cells.map(cell=>cell.text)).toEqual(['Secure cell','EUR 1,250.00']);
    });
    it('rejects unmodeled captions, outside text and overly deep trees without dropping visible text',()=>{
        expect(()=>parseTableHtml('Uncaptured text<table><tr><td>A</td></tr></table>','invalid')).toThrow(/outside/);
        expect(()=>parseTableHtml('<table><caption>Unmodeled caption</caption><tr><td>A</td></tr></table>','invalid')).toThrow(/caption/);
        expect(()=>parseTableHtml('<table><tr><td>'+'<span>'.repeat(140)+'A'+'</span>'.repeat(140)+'</td></tr></table>','deep')).toThrow(/deeply nested/);
    });
});
