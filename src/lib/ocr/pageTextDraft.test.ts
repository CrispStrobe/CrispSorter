import { describe, expect, it } from 'vitest';
import {
    addDocument, applyRecognition, approvePage, commitHistory, createHistory, createProject, editRegion,
    parseProject, prepareExport, serializeProject, undoHistory, type OcrProject, type RecognitionRegion,
} from './model';
import {
    applyPageTextEdit, createPageTextDraft, discardPageTextDraft, pageTextDraftSourceIsCurrent,
    preparePageTextEdit, projectPageText, readPageTextDraft, updatePageTextDraft,
} from './pageTextDraft';
import { collectSourceComponents } from './passageData';
import { deriveStructure, setBlockProperties, setInlineMarks, setFootnoteReference, startSection } from './structure';

const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
function source(texts=['Alpha','Beta','Gamma']):OcrProject {
    const height=Math.max(1000,texts.length*60+50);
    const imported=addDocument(createProject(),{source_uri:'/source.pdf',name:'Source',
        pages:[{image_path:'/page.png',width:600,height}]});
    return applyRecognition(imported,imported.pages[0].id,{width:600,height,
        regions:texts.map((text,index):RecognitionRegion=>({text,x:10,y:10+index*60,w:200,h:40,
            confidence:0.8,confidence_basis:'recognition',char_conf:Array.from(text,()=>0.8),
            char_conf_unit:'unicode_codepoint',char_conf_aligned:true}))});
}
const draft=(project:OcrProject,text?:string):OcrProject=>{
    const created=createPageTextDraft(project,project.pages[0].id);
    return text===undefined?created:updatePageTextDraft(created,text);
};
const review=(project:OcrProject)=>preparePageTextEdit(project)!;
function sortKeys(value:unknown):unknown {
    if(Array.isArray(value))return value.map(sortKeys);
    if(value&&typeof value==='object') {
        const record=value as Record<string,unknown>;
        return Object.fromEntries(Object.keys(record).sort().map(key=>[key,sortKeys(record[key])]));
    }
    return value;
}

describe('durable single-page text drafts',()=>{
    it('captures independent source data and preserves text, approvals and scores',()=>{
        let p=source(['First\r\nsecond','Last']);p=approvePage(p,p.pages[0].id);
        const before=clone(p),next=draft(p),saved=next.pageTextDraft!;
        expect(saved).toMatchObject({version:1,revision:1,projectId:p.id,text:'First\nsecond\nLast'});
        expect(saved.page).toEqual(p.pages[0]);expect(saved.page).not.toBe(p.pages[0]);
        expect(saved.document).toEqual(p.documents[0]);expect(next.pages).toBe(p.pages);
        expect(p).toEqual(before);expect(next.pages[0].approved).toBe(true);
        expect(readPageTextDraft(saved)).toEqual(saved);
        expect(readPageTextDraft(saved)!.page).not.toBe(saved.page);
        expect(preparePageTextEdit(next)).toBeNull();
        expect(next.pageTextDraft).toBe(saved);
    });

    it('updates only actual normalized text changes and protects the existing draft',()=>{
        let p=draft(source(['Original']));
        expect(updatePageTextDraft(p,'Original')).toBe(p);
        p=updatePageTextDraft(p,'One\r\nTwo\rThree');
        expect(p.pageTextDraft!.text).toBe('One\nTwo\nThree');expect(p.pageTextDraft!.revision).toBe(2);
        expect(updatePageTextDraft(p,'One\nTwo\nThree')).toBe(p);
        expect(createPageTextDraft(p,p.pages[0].id)).toBe(p);
        expect(()=>createPageTextDraft(p,'another-page')).toThrow(/existing page draft/);
        const next=discardPageTextDraft(p);
        expect(next.pageTextDraft).toBeUndefined();expect(next.pages).toBe(p.pages);
        expect(discardPageTextDraft(next)).toBe(next);
    });

    it('round-trips the draft and validates a recovered snapshot without its current page',()=>{
        const p=draft(source(['Source']),'Recovered text'),saved=p.pageTextDraft!;
        expect(parseProject(serializeProject(p)).pageTextDraft).toEqual(saved);
        const recovered={...p,pages:[],documents:[]};
        expect(parseProject(recovered).pageTextDraft).toEqual(saved);
        expect(()=>preparePageTextEdit(recovered)).toThrow(/source page or document changed/);
        expect(updatePageTextDraft(recovered,'Copyable recovered text').pageTextDraft!.text).toBe('Copyable recovered text');
    });

    it('survives native JSON key sorting while rejecting actual source changes',()=>{
        let p=source();
        const page=p.pages[0],block=deriveStructure(page).blocks[0];
        page.regions[0].sourceComponents=collectSourceComponents(page,[page.regions[0]]);
        p=setInlineMarks(p,page.id,block.id,{start:0,end:5},{bold:true});
        p=draft(p,'Alphx\nBeta\nGamma');
        // serde_json::Value may store object keys in sorted order. Project readers
        // rebuild current evidence/structure fields, while the source snapshot is raw.
        const restored=parseProject(JSON.stringify(sortKeys(JSON.parse(serializeProject(p)))));
        expect(pageTextDraftSourceIsCurrent(restored,restored.pageTextDraft!)).toBe(true);
        const preview=review(restored);
        expect(preview.afterText).toBe('Alphx\nBeta\nGamma');
        const next=applyPageTextEdit(restored,sortKeys(preview) as typeof preview);
        expect(next.pages[0].regions[0].text).toBe('Alphx');
        expect(next.pages[0].regions[0].sourceComponents).toEqual(restored.pages[0].regions[0].sourceComponents);
        for(const mutate of [
            (value:OcrProject)=>{value.pages[0].regions[0].text='Changed';},
            (value:OcrProject)=>{value.pages[0].regions[0].x+=1;},
            (value:OcrProject)=>{value.pages[0].structure!.blocks[0].alignment='center';},
            (value:OcrProject)=>{value.pages[0].regions.reverse();},
            (value:OcrProject)=>{value.documents[0].name='Renamed';},
        ]) {
            const changed=clone(restored);mutate(changed);
            expect(pageTextDraftSourceIsCurrent(changed,changed.pageTextDraft!)).toBe(false);
            expect(()=>preparePageTextEdit(changed)).toThrow(/source page or document changed/);
        }
    });

    it('rejects unsupported metadata, malformed geometry, duplicate identities and recursion',()=>{
        const valid=draft(source()).pageTextDraft!;
        for(const patch of [{version:2},{unknown:true},{revision:0},{revision:1.5},{id:''},{id:'x'.repeat(181)},{updatedAt:'bad date'},{text:'a\r\nb'}])
            expect(()=>readPageTextDraft({...valid,...patch})).toThrow();
        const geometry=clone(valid);geometry.page.regions[0].w=900;
        expect(()=>readPageTextDraft(geometry)).toThrow(/invalid region/);
        const identity=clone(valid);identity.document.id=identity.projectId;identity.page.documentId=identity.projectId;
        expect(()=>readPageTextDraft(identity)).toThrow(/duplicate/);
        expect(()=>readPageTextDraft({...valid,page:{...valid.page,pageTextDraft:valid}})).toThrow(/unsupported source fields/);
        expect(readPageTextDraft(undefined)).toBeUndefined();
    });

    it('enforces bounded draft text and source storage before changing the project',()=>{
        const p=draft(source(['Small'])),before=JSON.stringify(p);
        expect(()=>updatePageTextDraft(p,'x'.repeat(1_000_001))).toThrow(/1,000,000/);
        expect(()=>updatePageTextDraft(p,'NUL\0text')).toThrow(/NUL/);
        const tooLarge=clone(p.pageTextDraft!);tooLarge.page.provenance.unbounded='x'.repeat(16*1024*1024);
        expect(()=>readPageTextDraft(tooLarge)).toThrow(/16 MB/);
        expect(JSON.stringify(p)).toBe(before);
    });
});

describe('page draft mapping and evidence',()=>{
    it('edits one passage without changing its source identity or original OCR evidence',()=>{
        let p=source(['😀 First\r\nsecond','Unchanged']);
        p.pages[0].provenance.extraction={value:'stale'};p=approvePage(p,p.pages[0].id);
        p=draft(p,'😀 Fixed\nsecond\nUnchanged');
        const old=p.pages[0].regions[0],preview=review(p),before=JSON.stringify(p);
        expect(preview.requiresCombinedCoverage).toBe(false);
        expect(preview.regionIds).toEqual([old.id]);expect(preview.replacementIds).toEqual([old.id]);
        expect(preview.afterText).toBe('😀 Fixed\r\nsecond\nUnchanged');
        const next=applyPageTextEdit(p,preview);
        expect(next.pages[0].regions[0]).toEqual({...old,text:'😀 Fixed\r\nsecond',review:'corrected'});
        expect(next.pages[0].regions[1]).toEqual(p.pages[0].regions[1]);
        expect(next.pages[0].approved).toBe(false);expect(next.pageTextDraft).toBeUndefined();
        expect(next.pages[0].provenance.extraction).toBeUndefined();
        expect(next.pages[0].provenance.lastPageTextEdit).toMatchObject({draftId:p.pageTextDraft!.id,draftRevision:2,combined:false});
        expect(JSON.stringify(p)).toBe(before);
    });

    it.each(['failed','pending'] as const)('makes explicitly accepted manual repairs usable after %s recognition',status=>{
        for(const replacement of ['Originxl\nNeighbor','Combined']) {
            const original=source(['Original','Neighbor']);
            original.pages[0].status=status;original.pages[0].error='Earlier recognition did not complete.';
            const p=draft(original,replacement),preview=review(p),next=applyPageTextEdit(p,preview,true);
            expect(next.pages[0].status).toBe('recognized');expect(next.pages[0].error).toBeNull();
            expect(next.pages[0].approved).toBe(false);expect(next.pageTextDraft).toBeUndefined();
            const approved=approvePage(next,next.pages[0].id);
            expect(approved.pages[0].approved).toBe(true);
            expect(prepareExport(approved,{requireReviewed:true,format:'json'}).errors).toEqual([]);
        }
    });

    it('keeps outside CRLF bytes and discloses LF normalization inside a changed envelope',()=>{
        const p=draft(source(['zero\r\nalpha\r\nmiddle\r\nomega\r\nlast']),'zero\nALPHA\nmiddle\nOMEGA\nlast');
        const preview=review(p);
        expect(preview.afterText).toBe('zero\r\nALPHA\nmiddle\nOMEGA\r\nlast');
        expect(preview.warnings).toContain('Line endings inside the changed range use LF. Unchanged text outside that range keeps its original line endings.');
        const next=applyPageTextEdit(p,preview);
        expect(next.pages[0].regions[0].text).toBe(preview.afterText);
        expect(next.pages[0].regions[0].originalText).toBe('zero\r\nalpha\r\nmiddle\r\nomega\r\nlast');
    });

    it('handles combining text and emoji while preserving unchanged formatting only',()=>{
        let p=source(['A🇺🇸Z Cafe\u0301']);
        const page=p.pages[0],block=deriveStructure(page).blocks[0];
        p=setInlineMarks(p,page.id,block.id,{start:1,end:5},{bold:true});
        p=draft(p,'A🇺🇳Z Cafe\u0301');
        const next=applyPageTextEdit(p,review(p));
        expect(next.pages[0].regions[0].text).toBe('A🇺🇳Z Cafe\u0301');
        expect(next.pages[0].structure!.blocks[0].runs).toEqual([]);
        expect(next.pages[0].regions[0].originalText).toBe('A🇺🇸Z Cafe\u0301');
    });

    it('combines consecutive passages with stable manual identity and preserved flat originals',()=>{
        const original=source(['Alpha','Beta','Gamma']);
        original.pages[0].regions[0].separatorBefore=' ';
        const p=draft(original,'ALPHA and BETA\nGamma'),preview=review(p);
        expect(preview.requiresCombinedCoverage).toBe(true);
        expect(preview.regionIds).toEqual(original.pages[0].regions.slice(0,2).map(region=>region.id));
        expect(preview.affectedBeforeText).toBe('Alpha\nBeta');expect(preview.affectedAfterText).toBe('ALPHA and BETA');
        expect(preview.replacementIds).toEqual([p.pageTextDraft!.id+'-combined']);
        expect(review(p)).toEqual(preview);
        const saved=JSON.stringify(p);
        expect(()=>applyPageTextEdit(p,preview)).toThrow(/Accept the combined/);expect(JSON.stringify(p)).toBe(saved);
        const next=applyPageTextEdit(p,preview,true),combined=next.pages[0].regions[0];
        expect(combined).toMatchObject({text:'ALPHA and BETA',originalText:'ALPHA and BETA',kind:'text',
            confidence:null,confidenceBasis:'manual',char_conf:[],charConfidenceAligned:false,separatorBefore:' ',
            x:10,y:10,w:200,h:100,review:'corrected'});
        expect(combined.wordEvidence).toBeUndefined();
        expect(combined.sourceComponents!.map(component=>component.originalText)).toEqual(['Alpha','Beta']);
        expect(next.pages[0].regions[1]).toEqual(original.pages[0].regions[2]);
        expect(projectPageText(next.pages[0])).toBe('ALPHA and BETA\nGamma');
        expect(next.pages[0].provenance.lastPageTextEdit).toMatchObject({combined:true,sourceRegionIds:preview.regionIds});
        expect(parseProject(serializeProject(next)).pages[0].regions).toEqual(next.pages[0].regions);
    });

    it.each([' ','\n\n',''])('assigns separator-only edits to both neighbors (%j)',separator=>{
        const p=draft(source(['Alpha','Beta']),'Alpha'+separator+'Beta'),preview=review(p);
        expect(preview.requiresCombinedCoverage).toBe(true);
        expect(preview.affectedAfterText).toBe('Alpha'+separator+'Beta');
        expect(preview.warnings.some(warning=>warning.includes('ambiguous boundary'))).toBe(true);
        expect(projectPageText(applyPageTextEdit(p,preview,true).pages[0])).toBe(p.pageTextDraft!.text);
    });

    it('combines an insertion at a passage edge and an edit touching a zero-length separator',()=>{
        let p=source(['Alpha','Beta']);p.pages[0].regions[1].separatorBefore='';
        p=draft(p,'ALPHABeta');
        expect(review(p).regionIds).toHaveLength(2);
        expect(projectPageText(applyPageTextEdit(p,review(p),true).pages[0])).toBe('ALPHABeta');
        const inserted=draft(source(['Alpha','Beta']),'Alpha!\nBeta');
        expect(review(inserted).requiresCombinedCoverage).toBe(true);
    });

    it('retains an empty manual passage and its sources after whole-page deletion',()=>{
        const p=draft(source(['Alpha','Beta']),''),preview=review(p),next=applyPageTextEdit(p,preview,true);
        expect(next.pages[0].regions).toHaveLength(1);
        expect(next.pages[0].regions[0]).toMatchObject({text:'',originalText:'',confidenceBasis:'manual',review:'corrected'});
        expect(next.pages[0].regions[0].sourceComponents!.map(component=>component.originalText)).toEqual(['Alpha','Beta']);
        expect(projectPageText(next.pages[0])).toBe('');
        const one=draft(source(['Only']),''),onePreview=review(one);
        expect(onePreview.requiresCombinedCoverage).toBe(false);
        expect(applyPageTextEdit(one,onePreview).pages[0].regions[0].sourceComponents![0].originalText).toBe('Only');
    });

    it('can add text to an existing empty passage without inventing its source',()=>{
        const p=draft(source(['']),'Typed text'),preview=review(p);
        const next=applyPageTextEdit(p,preview);
        expect(next.pages[0].regions[0]).toMatchObject({id:p.pages[0].regions[0].id,text:'Typed text',originalText:''});
    });

    it('flattens repeated source history and consumes resolved shared coverage families',()=>{
        const original=source(['Alpha','Beta']),page=original.pages[0],components=collectSourceComponents(page,page.regions);
        page.regions[0].sourceComponents=components;page.regions[1].sourceComponents=components;
        page.regions[0].sharedSourceGroups=['shared'];page.regions[1].sharedSourceGroups=['shared'];
        const p=draft(original,'Combined'),next=applyPageTextEdit(p,review(p),true);
        expect(next.pages[0].regions[0].sourceComponents).toHaveLength(2);
        expect(next.pages[0].regions[0].sharedSourceGroups).toBeUndefined();
    });
});

describe('draft barriers, stale previews and structural preflight',()=>{
    it.each(['picture','excluded','table','formula'] as const)('rejects changes across a %s boundary',barrier=>{
        const p=source(['Alpha','Barrier','Gamma']);
        if(barrier==='excluded')p.pages[0].regions[1].excluded=true;
        else p.pages[0].regions[1].kind=barrier;
        const text=barrier==='excluded'||barrier==='picture'?'ALPHA\nGAMMA':'ALPHA\nBarrier\nGAMMA';
        const next=draft(p,text),saved=JSON.stringify(next);
        expect(()=>review(next)).toThrow(/table, formula, picture, or excluded/);
        expect(JSON.stringify(next)).toBe(saved);
        const within=draft(p,projectPageText(p.pages[0]).replace('Alpha','Alphx'));
        expect(review(within).regionIds).toEqual([p.pages[0].regions[0].id]);
    });

    it.each(['table','formula'] as const)('shows but does not edit %s text',kind=>{
        const p=source(['Alpha','Barrier']);p.pages[0].regions[1].kind=kind;
        expect(projectPageText(p.pages[0])).toBe('Alpha\nBarrier');
        expect(()=>review(draft(p,'Alpha\nChanged'))).toThrow(/table, formula/);
    });

    it('rejects new combined rectangles overlapping any unselected included region',()=>{
        const p=source(['Alpha','Beta','Gamma']);
        Object.assign(p.pages[0].regions[2],{x:30,y:55,w:100,h:10});
        p.pages[0].regions.forEach(region=>{region.sharedSourceGroups=['guarded'];});
        const editing=draft(p,'ALPHA\nBETA\nGamma'),saved=JSON.stringify(editing);
        expect(()=>review(editing)).toThrow(/overlap another included/);expect(JSON.stringify(editing)).toBe(saved);
        p.pages[0].regions[2].excluded=true;
        expect(review(draft(p,'ALPHA\nBETA')).requiresCombinedCoverage).toBe(true);
    });

    it('rejects stale page, document and project identities while retaining recovery text',()=>{
        const p=draft(source(),'Alphx\nBeta\nGamma'),preview=review(p);
        const changed=editRegion(p,p.pages[0].id,p.pages[0].regions[2].id,'Other text');
        expect(()=>applyPageTextEdit(changed,preview)).toThrow(/source page or document changed/);
        expect(changed.pageTextDraft).toEqual(p.pageTextDraft);
        const changedDocument={...p,documents:[{...p.documents[0],name:'Renamed'}]};
        expect(()=>review(changedDocument)).toThrow(/source page or document changed/);
        expect(()=>review({...p,id:'another-project'})).toThrow(/source page or document changed/);
    });

    it('rejects forged preview contents and a draft edited away and back',()=>{
        const p=draft(source(),'Alphx\nBeta\nGamma'),preview=review(p),saved=JSON.stringify(p);
        for(const mutate of [
            (value:typeof preview)=>{value.regions[0].text='Forged';},
            (value:typeof preview)=>{value.regions[0].x=11;},
            (value:typeof preview)=>{value.afterText='Forged';},
            (value:typeof preview)=>{value.warnings.push('Hidden forged approval');},
            (value:typeof preview)=>{value.requiresCombinedCoverage=true;},
        ]) {
            const forged=clone(preview);mutate(forged);
            expect(()=>applyPageTextEdit(p,forged,true)).toThrow(/preview changed/);
        }
        const changed=updatePageTextDraft(updatePageTextDraft(p,'Temporary'),p.pageTextDraft!.text);
        expect(changed.pageTextDraft!.revision).toBe(p.pageTextDraft!.revision+2);
        expect(()=>applyPageTextEdit(changed,preview)).toThrow(/preview changed/);
        expect(JSON.stringify(p)).toBe(saved);
    });

    it('preflights passage and flat source-count bounds without dropping old evidence',()=>{
        const p=source(Array.from({length:129},(_,index)=>'Line '+index));
        p.pages[0].regions.forEach((region,index)=>Object.assign(region,{y:index*2,h:1}));
        expect(()=>review(draft(p,'Combined'))).toThrow(/128 passages/);
        const limited=source(['Alpha','Beta']),base=collectSourceComponents(limited.pages[0],limited.pages[0].regions);
        limited.pages[0].regions[0].sourceComponents=Array.from({length:65},(_,index)=>({...base[0],id:'left-'+index}));
        limited.pages[0].regions[1].sourceComponents=Array.from({length:65},(_,index)=>({...base[1],id:'right-'+index}));
        const editing=draft(limited,'Combined'),saved=JSON.stringify(editing);
        expect(()=>review(editing)).toThrow(/128 source components/);expect(JSON.stringify(editing)).toBe(saved);
    });

    it('discloses lost roles, marks, links and section placement and archives their original text',()=>{
        let p=source(['Title1','Note']);
        const page=p.pages[0],blocks=deriveStructure(page).blocks;
        p=setBlockProperties(p,page.id,blocks[0].id,{role:'heading',headingLevel:2});
        p=setInlineMarks(p,page.id,blocks[0].id,{start:0,end:5},{bold:true});
        p=setBlockProperties(p,page.id,blocks[1].id,{role:'footnote'});
        p=setFootnoteReference(p,page.id,blocks[0].id,{start:5,end:6},blocks[1].id);
        p=startSection(p,page.id,blocks[1].id);
        p=draft(p,'Combined');const preview=review(p);
        expect(preview.warnings.join(' ')).toContain('heading, footnote; 1 styled range(s), 1 outgoing link(s)');
        expect(preview.warnings.join(' ')).toContain('2 section(s)');
        const next=applyPageTextEdit(p,preview,true);
        expect(next.pages[0].structure!.blocks).toHaveLength(1);
        expect(next.pages[0].structure!.blocks[0]).toMatchObject({role:'paragraph',runs:[]});
        const history=next.pages[0].provenance.structureHistory as {snapshots:{regions:{text:string}[]}[]};
        expect(history.snapshots.at(-1)!.regions.map(region=>region.text)).toEqual(['Title1','Note']);
        expect(undoHistory(commitHistory(createHistory(p),next)).present).toEqual(p);
    });
});
