import { describe, expect, it } from 'vitest';
import { addDocument, applyRecognition, createProject } from './model';
import { appendExtractionObject, captureExtractionRequest, editExtractionCell, editExtractionField, editExtractionFormula, getExtractionObjects, type ExtractionKind } from './extractionObjects';
import { prepareExtractionExport } from './extractionExport';

function fixture(kind:ExtractionKind,output:unknown) {
    let project=addDocument(createProject('Selected export'),{source_uri:'/source.png',name:'Source',pages:[{image_path:'/page.png',width:800,height:1000}]});
    project=applyRecognition(project,project.pages[0].id,{width:800,height:1000,regions:[{text:'Source text',x:20,y:20,w:300,h:40}]});
    const request=captureExtractionRequest(project,project.pages[0].id,null,kind==='fields'?'text':'pixels');
    project=appendExtractionObject(project,request,kind,output,{labels:['Account']});
    return {project,object:getExtractionObjects(project.pages[0]).collection.objects[0]};
}

describe('selected extraction file formats',()=>{
    it('exports corrected tables as literal CSV with spans flattened and no object JSON',()=>{
        let {project,object}=fixture('table',{html:'<table><tr><th colspan="2">Accounts</th></tr><tr><td>0007</td><td>Original</td></tr></table>',rows:2,cols:2});
        if(object.kind!=='table') throw new Error('Fixture kind');
        const target=object.currentData.cells.find(cell=>cell.row===1&&cell.column===1)!;
        project=editExtractionCell(project,project.pages[0].id,object.id,target.id,'=1+1\n"quoted"');
        object=getExtractionObjects(project.pages[0]).collection.objects[0];
        const output=prepareExtractionExport(object);
        expect(output.extension).toBe('csv');
        expect(output.text).toBe('"Accounts",""\r\n"0007","\'=1+1\n""quoted"""');
        expect(output.text).not.toContain('originalOutput');
    });
    it('exports the exact corrected LaTeX source as tex without JSON quoting',()=>{
        let {project,object}=fixture('formula','x^2');
        const latex='\\frac{a}{b}\n+ c';
        project=editExtractionFormula(project,project.pages[0].id,object.id,latex);
        object=getExtractionObjects(project.pages[0]).collection.objects[0];
        expect(prepareExtractionExport(object)).toEqual({extension:'tex',text:latex});
    });
    it('exports fields as complete object JSON retaining original evidence and corrected values',()=>{
        let {project,object}=fixture('fields',[{label:'Account',value:'0007',score:.94}]);
        if(object.kind!=='fields') throw new Error('Fixture kind');
        project=editExtractionField(project,project.pages[0].id,object.id,object.currentData.fields[0].id,{value:'0008'});
        object=getExtractionObjects(project.pages[0]).collection.objects[0];
        const output=prepareExtractionExport(object),saved=JSON.parse(output.text);
        expect(output.extension).toBe('json');
        expect(saved).toEqual(object);
        expect(saved.currentData.fields[0]).toMatchObject({value:'0008',score:null});
        expect(saved.originalData.fields[0]).toMatchObject({value:'0007',score:.94});
        expect(saved.captures[0].imagePath).toBe('/page.png');
    });
});
