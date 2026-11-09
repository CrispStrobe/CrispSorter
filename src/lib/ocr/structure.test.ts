import { describe, expect, it } from 'vitest';
import { addDocument, applyRecognition, approvePage, commitHistory, createHistory, createProject, editRegion, parseProject, serializeProject, undoHistory, type OcrProject } from './model';
import {
    initializePageStructure, setBlockProperties, setInlineMarks, clearInlineMarks, startSection,
    setSectionColumns, setCaptionAnchor, setFootnoteReference, deriveStructure, structureSegments,
} from './structure';

function project():OcrProject {
    let p=addDocument(createProject(),{source_uri:'/source.pdf',name:'Source',pages:[{image_path:'/page.png',width:600,height:800}]});
    return applyRecognition(p,p.pages[0].id,{width:600,height:800,regions:[
        {text:'😀 Cafe\u0301 alpha [1]',x:10,y:10,w:500,h:40,confidence:0.7,confidence_basis:'recognition'},
        {text:'',x:10,y:80,w:100,h:100,kind:'picture'},
        {text:'Caption',x:10,y:200,w:500,h:40},
        {text:'Footnote text',x:10,y:300,w:500,h:40},
    ]});
}
const source=(p:OcrProject)=>p.pages[0].regions.map(({review:_review,...region})=>region);

describe('immutable structure editing',()=>{
    it('initializes legacy defaults without changing text, order, evidence, geometry or approval',()=>{
        let p=project();p=approvePage(p,p.pages[0].id);const before=JSON.stringify(p),next=initializePageStructure(p,p.pages[0].id);
        expect(next.pages[0].approved).toBe(true);expect(next.pages[0].regions).toEqual(p.pages[0].regions);
        expect(next.pages[0].structure).toEqual(deriveStructure(p.pages[0]));expect(JSON.stringify(p)).toBe(before);
        expect(initializePageStructure(next,next.pages[0].id)).toBe(next);
        expect(parseProject(serializeProject(next)).pages[0].structure).toEqual(next.pages[0].structure);
        expect(undoHistory(commitHistory(createHistory(p),next)).present).toEqual(p);
    });

    it('stores heading/list/alignment/page-break choices while leaving source text and coarse kinds intact',()=>{
        let p=project();p=approvePage(p,p.pages[0].id);const before=source(p),page=p.pages[0],block=deriveStructure(page).blocks[0];
        p=setBlockProperties(p,page.id,block.id,{role:'heading',headingLevel:2,alignment:'center',breakBefore:'page'});
        expect(p.pages[0].structure!.blocks[0]).toMatchObject({role:'heading',headingLevel:2,alignment:'center',breakBefore:'page'});
        expect(source(p)).toEqual(before);expect(p.pages[0].approved).toBe(false);
        expect(p.pages[0].regions[0].review).toBe('unreviewed');expect(p.pages[0].regions[2].review).toBe('approved');
        p=setBlockProperties(p,page.id,block.id,{role:'list-item',list:{kind:'number',level:1,start:3}});
        expect(p.pages[0].structure!.blocks[0].headingLevel).toBeUndefined();
        expect(p.pages[0].structure!.blocks[0].list).toEqual({kind:'number',level:1,start:3});
        expect(()=>setBlockProperties(p,page.id,block.id,{role:'figure'})).toThrow(/structure/);
    });

    it('applies and clears overlapping Unicode formatting as canonical nonoverlapping runs',()=>{
        let p=project();const page=p.pages[0],blockId=deriveStructure(page).blocks[0].id,before=source(p);
        p=setInlineMarks(p,page.id,blockId,{start:3,end:8},{bold:true});
        p=setInlineMarks(p,page.id,blockId,{start:6,end:11},{underline:true});
        expect(p.pages[0].structure!.blocks[0].runs).toEqual([{start:3,end:6,bold:true},{start:6,end:8,bold:true,underline:true},{start:8,end:11,underline:true}]);
        p=clearInlineMarks(p,page.id,blockId,{start:6,end:8});
        expect(p.pages[0].structure!.blocks[0].runs).toEqual([{start:3,end:6,bold:true},{start:8,end:11,underline:true}]);
        expect(source(p)).toEqual(before);
        const segments=structureSegments(page.regions[0].text,p.pages[0].structure!.blocks[0]);
        expect(segments.map(segment=>segment.text).join('')).toBe(page.regions[0].text);
        expect(()=>setInlineMarks(p,page.id,blockId,{start:1,end:2},{bold:true})).toThrow(/complete characters/);
        expect(()=>setInlineMarks(p,page.id,blockId,{start:3,end:7},{italic:true})).toThrow(/complete characters/);
    });

    it('uses one shared two-column boundary and explicit contiguous section starts',()=>{
        let p=project();const page=p.pages[0],before=source(p);
        p=initializePageStructure(p,page.id);
        const structure=p.pages[0].structure!,ids=structure.blocks.map(block=>block.id),sectionId=structure.sections[0].id;
        p=setSectionColumns(p,page.id,sectionId,ids[2]);
        expect(p.pages[0].structure!.blocks.map(block=>block.column)).toEqual([1,1,2,2]);
        expect(()=>setSectionColumns(p,page.id,sectionId,ids[0])).toThrow(/after the first/);
        p=startSection(p,page.id,ids[1]);
        expect(p.pages[0].structure!.sections.map(section=>section.columns)).toEqual([1,2]);
        expect(p.pages[0].structure!.blocks.map(block=>block.sourceRegionId)).toEqual(page.regions.map(region=>region.id));
        const second=p.pages[0].structure!.sections[1].id;
        expect(startSection(p,page.id,ids[1])).toBe(p);
        p=setSectionColumns(p,page.id,second,null);
        expect(p.pages[0].structure!.blocks.map(block=>block.column)).toEqual([1,1,1,1]);
        expect(source(p)).toEqual(before);
    });

    it('sets figure descriptions, caption anchors and exact footnote references without inferring meaning',()=>{
        let p=project();const page=p.pages[0],blocks=deriveStructure(page).blocks,before=source(p);
        p=setBlockProperties(p,page.id,blocks[1].id,{altText:'A diagram of the process',decorative:false});
        p=setBlockProperties(p,page.id,blocks[2].id,{role:'caption'});
        p=setCaptionAnchor(p,page.id,blocks[2].id,blocks[1].id);
        p=setBlockProperties(p,page.id,blocks[3].id,{role:'footnote'});
        p=setFootnoteReference(p,page.id,blocks[0].id,{start:15,end:18},blocks[3].id);
        expect(p.pages[0].structure!.blocks[2].anchorBlockId).toBe(blocks[1].id);
        expect(p.pages[0].structure!.blocks[0].noteReferences).toEqual([{start:15,end:18,targetBlockId:blocks[3].id}]);
        expect(source(p)).toEqual(before);
        expect(()=>setFootnoteReference(p,page.id,blocks[3].id,{start:0,end:1},blocks[3].id)).toThrow(/cycle/);
        expect(()=>setFootnoteReference(p,page.id,blocks[0].id,{start:16,end:18},blocks[3].id)).toThrow(/overlapping/);
        p=setBlockProperties(p,page.id,blocks[3].id,{role:'paragraph'});
        expect(p.pages[0].structure!.blocks[0].noteReferences).toBeUndefined();
        expect(p.pages[0].provenance.structureNotices).toEqual([expect.stringContaining('target role changed')]);
        p=setCaptionAnchor(p,page.id,blocks[2].id,null);expect(p.pages[0].structure!.blocks[2].anchorBlockId).toBeUndefined();
    });

    it('rebases plain passage edits through the central model and preserves styles through save/undo',()=>{
        let p=project();const page=p.pages[0],blockId=deriveStructure(page).blocks[0].id;
        p=setInlineMarks(p,page.id,blockId,{start:3,end:14},{bold:true});
        const next=editRegion(p,page.id,page.regions[0].id,'😀 Cafe\u0301 NEW alpha [1]');
        expect(next.pages[0].structure!.blocks[0].runs).toEqual([{start:3,end:9,bold:true},{start:13,end:18,bold:true}]);
        expect(next.pages[0].regions[0].originalText).toBe(page.regions[0].originalText);
        expect(parseProject(serializeProject(next)).pages[0].structure).toEqual(next.pages[0].structure);
        expect(undoHistory(commitHistory(createHistory(p),next)).present).toEqual(p);
    });
});
