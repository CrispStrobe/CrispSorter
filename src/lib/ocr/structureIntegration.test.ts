import { describe,expect,it } from 'vitest';
import { addDocument,applyPageImage,applyRecognition,approvePage,commitHistory,createHistory,createProject,deleteRegion,editRegion,getReviewIssues,moveRegion,parseProject,prepareExport,replaceMatches,serializeProject,splitPage,undoHistory,updateRegion,type OcrProject } from './model';
import { initializePageStructure,setBlockProperties,setCaptionAnchor,setFootnoteReference,setInlineMarks,setSectionColumns,startSection } from './structure';
import { readStructureHistory } from './structureHistory';
import { applyPassagePreview,capturePassageSplit } from './passages';

function fixture():OcrProject {
    let p=addDocument(createProject('Structure integration'),{source_uri:'/source.png',name:'Source',pages:[{image_path:'/page.png',width:600,height:800}]});
    p=applyRecognition(p,p.pages[0].id,{width:600,height:800,regions:[
        {text:'😀 Heading [1]',kind:'title',x:10,y:20,w:400,h:60},
        {text:'Body text and stable suffix.',kind:'text',x:10,y:120,w:300,h:40},
        {text:'',kind:'picture',x:400,y:120,w:100,h:100},
        {text:'Figure 1: Circuit.',kind:'text',x:10,y:230,w:300,h:40},
        {text:'The source note.',kind:'text',x:10,y:600,w:300,h:40},
    ].map(region=>({...region,confidence:0.94,confidence_basis:'recognition',char_conf:Array.from(region.text).map(()=>0.94),char_conf_unit:'unicode_codepoint',char_conf_aligned:!!region.text}))});
    return approvePage(p,p.pages[0].id);
}
function structured():OcrProject { const p=fixture();return initializePageStructure(p,p.pages[0].id); }
function linked():OcrProject {
    let p=structured(),page=p.pages[0],blocks=page.structure!.blocks;
    p=setBlockProperties(p,page.id,blocks[3].id,{role:'caption'});
    p=setBlockProperties(p,page.id,blocks[4].id,{role:'footnote'});
    p=setCaptionAnchor(p,page.id,blocks[3].id,blocks[2].id);
    p=setFootnoteReference(p,page.id,blocks[0].id,{start:11,end:14},blocks[4].id);
    return p;
}

describe('document structure across the project lifecycle',()=>{
    it('queues semantic-only changes until the page is approved',()=>{
        const p=structured(),page=p.pages[0];
        expect(getReviewIssues(p)).toHaveLength(0);
        const changed=setBlockProperties(p,page.id,page.structure!.blocks[1].id,{alignment:'center'});
        expect(changed.pages[0].regions.map(region=>region.text)).toEqual(page.regions.map(region=>region.text));
        expect(getReviewIssues(changed)).toContainEqual({pageId:page.id,regionId:null,reason:'changed',message:'Document structure changes need page approval.'});
        expect(getReviewIssues(approvePage(changed,page.id))).toHaveLength(0);
    });
    it('initializes legacy projects without changing text, geometry, approvals, or raw evidence',()=>{
        const original=fixture(),page=original.pages[0],next=initializePageStructure(original,page.id);
        expect(next.pages[0].approved).toBe(true);
        expect(next.pages[0].regions).toEqual(page.regions);
        expect(next.pages[0].structure!.blocks.map(block=>block.role)).toEqual(['heading','paragraph','figure','paragraph','paragraph']);
        const restored=parseProject(serializeProject(next));
        expect(restored.pages[0].structure).toEqual(next.pages[0].structure);
        const output=prepareExport(restored,{format:'json'});
        expect(output.text).toBe(prepareExport(original).text);
        expect(output.pages[0].figure_regions!.map(region=>region.id)).toEqual([page.regions[2].id]);
        expect(output.pages[0].structure!.blocks).toHaveLength(5);
        expect(output.pages[0].regions).toHaveLength(4);
    });
    it('reconciles exact Unicode text and links through all ordinary text-edit entry points',()=>{
        let p=linked(),page=p.pages[0],heading=page.structure!.blocks[0];
        p=setInlineMarks(p,page.id,heading.id,{start:3,end:10},{bold:true});
        const before=p;
        p=editRegion(p,page.id,page.regions[0].id,'New 😀 Heading [1]');
        expect(p.pages[0].structure!.blocks[0].runs).toEqual([{start:7,end:14,bold:true}]);
        expect(p.pages[0].structure!.blocks[0].noteReferences![0]).toMatchObject({start:15,end:18});
        p=replaceMatches(p,'[1]','[A]',{caseSensitive:true});
        expect(p.pages[0].structure!.blocks[0].noteReferences).toBeUndefined();
        expect(p.pages[0].provenance.structureNotices).toEqual(expect.arrayContaining([expect.stringMatching(/note reference/)]));
        expect(p.pages[0].regions[0].originalText).toBe('😀 Heading [1]');
        expect(undoHistory(commitHistory(createHistory(before),p)).present).toEqual(before);
        expect(parseProject(serializeProject(p)).pages[0].structure).toEqual(p.pages[0].structure);
    });
    it('keeps included figure links in JSON and reports links excluded from an export subset',()=>{
        let p=linked(),page=p.pages[0];
        p=setBlockProperties(p,page.id,page.structure!.blocks[2].id,{altText:'A circuit with a switch',decorative:false});
        const exported=prepareExport(p,{format:'json'});
        expect(exported.pages[0].structure!.blocks[3].anchorBlockId).toBe(page.structure!.blocks[2].id);
        expect(exported.pages[0].figure_regions![0]).toMatchObject({x:400,y:120,w:100,h:100,kind:'picture'});
        const word=prepareExport(p,{format:'docx'});
        expect(word.warnings.some(warning=>warning.includes('plain-text'))).toBe(false);
        expect(word.warnings.some(warning=>warning.includes('does not encode figure alt text'))).toBe(true);
        p=updateRegion(p,page.id,page.regions[2].id,{excluded:true});
        const subset=prepareExport(p,{format:'json'});
        expect(subset.pages[0].figure_regions).toEqual([]);
        expect(subset.pages[0].structure!.blocks.find(block=>block.role==='caption')!.anchorBlockId).toBeUndefined();
        expect(subset.warnings.some(warning=>warning.includes('outside the export'))).toBe(true);
        expect(p.pages[0].structure!.blocks[3].anchorBlockId).toBeDefined();
    });
    it('reorders and repartitions logical columns without changing or dropping text',()=>{
        let p=structured(),page=p.pages[0],blocks=page.structure!.blocks;
        p=startSection(p,page.id,blocks[1].id);
        p=setSectionColumns(p,page.id,p.pages[0].structure!.sections[1].id,blocks[3].id);
        expect(p.pages[0].structure!.blocks.map(block=>block.column)).toEqual([1,1,1,2,2]);
        p=moveRegion(p,page.id,page.regions[3].id,-1);
        expect(p.pages[0].structure!.blocks.map(block=>block.sourceRegionId)).toEqual(p.pages[0].regions.map(region=>region.id));
        expect(parseProject(serializeProject(p)).pages[0].regions.map(region=>region.text)).toEqual(p.pages[0].regions.map(region=>region.text));
        expect(prepareExport(p).text).toBe('😀 Heading [1]\nBody text and stable suffix.\nFigure 1: Circuit.\nThe source note.');
    });
    it('archives replaced structure and announces formatting resets in a passage preview',()=>{
        let p=linked(),page=p.pages[0];
        p=setInlineMarks(p,page.id,page.structure!.blocks[1].id,{start:0,end:4},{italic:true});
        const preview=capturePassageSplit(p,page.id,page.regions[1].id,4,{mode:'divide',axis:'x',position:160,firstSide:'low'});
        expect(preview.warnings.some(warning=>warning.includes('active structure'))).toBe(true);
        const next=applyPassagePreview(p,preview),history=readStructureHistory(next.pages[0].provenance.structureHistory)!;
        expect(history.snapshots).toHaveLength(1);
        expect(history.snapshots[0].structure.blocks[1].runs).toEqual([{start:0,end:4,italic:true}]);
        expect(history.snapshots[0].regions[1].text).toBe('Body text and stable suffix.');
        expect(next.pages[0].structure!.blocks.slice(1,3).every(block=>block.runs.length===0)).toBe(true);
        expect(prepareExport(next).text).toBe(prepareExport(p).text);
        expect(parseProject(serializeProject(next)).pages[0].provenance.structureHistory).toEqual(history);
    });
    it('keeps prior semantic text inspectable through image changes, page splits and re-recognition',()=>{
        const p=linked(),page=p.pages[0];
        const changed=applyPageImage(p,page.id,{image_path:'/new.png',width:600,height:800},'cleanup');
        expect(changed.pages[0].structure!.blocks).toEqual([]);
        expect(readStructureHistory(changed.pages[0].provenance.structureHistory)!.snapshots[0].imagePath).toBe('/page.png');
        const recognized=applyRecognition(changed,page.id,{width:600,height:800,regions:[{text:'New text',x:10,y:10,w:200,h:40}]});
        expect(readStructureHistory(recognized.pages[0].provenance.structureHistory)!.snapshots).toHaveLength(1);
        expect(recognized.pages[0].structure!.blocks[0].role).toBe('paragraph');
        const spread=splitPage(p,page.id,[{image_path:'/left.png',width:300,height:800},{image_path:'/right.png',width:300,height:800}]);
        expect(spread.pages.every(child=>child.structure===undefined)).toBe(true);
        expect(readStructureHistory(spread.pages[1].provenance.structureHistory)!.snapshots[0].pageId).toBe(page.id);
        const removed=deleteRegion(p,page.id,page.regions[4].id);
        expect(removed.pages[0].structure!.blocks[0].noteReferences).toBeUndefined();
        expect(removed.pages[0].provenance.structureNotices).toEqual(expect.arrayContaining([expect.stringMatching(/target disappeared/)]));
    });
    it('rejects unknown saved structure versions and stale historical text ranges without altering the source',()=>{
        const p=linked(),raw=JSON.parse(serializeProject(p));
        raw.pages[0].structure.version=2;
        expect(()=>parseProject(raw)).toThrow(/structure/);
        const next=deleteRegion(p,p.pages[0].id,p.pages[0].regions[4].id),saved=JSON.parse(serializeProject(next));
        saved.pages[0].provenance.structureHistory.snapshots[0].regions[0].text='short';
        expect(()=>parseProject(saved)).toThrow(/structure/);
        expect(p.pages[0].structure!.blocks[0].noteReferences![0].end).toBe(14);
    });
});
