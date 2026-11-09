import { describe, expect, it, vi } from 'vitest';
import {
    addDocument, applyRecognition, approvePage, commitHistory, createHistory, createProject,
    editRegion, parseProject, prepareExport, serializeProject, undoHistory, updatePage, updateRegion,
    type OcrProject,
} from './model';
import {
    applyPassagePreview, applyPassageRepair, capturePassageMerge, capturePassageRepair,
    capturePassageSplit, isPassageBoundary, passageEligibility, previewRepairText,
} from './passages';
import { hasUnresolvedSharedGeometry, joinRegionText } from './text';

function project(texts=['alpha beta','gamma delta','unrelated']):OcrProject {
    let p=addDocument(createProject(),{source_uri:'/source.pdf',name:'Source',pages:[{image_path:'/page.png',width:600,height:800}]});
    p=applyRecognition(p,p.pages[0].id,{width:600,height:800,regions:texts.map((text,i)=>({
        text,x:10+i*180,y:20,w:160,h:100,confidence:0.42,confidence_basis:'recognition',
    }))});
    return p;
}
function split(p:OcrProject,index=0,offset=5) {
    return capturePassageSplit(p,p.pages[0].id,p.pages[0].regions[index].id,offset,{mode:'shared'});
}

describe('passage split and merge',()=>{
    it('preserves exact text/separators, stable replacement IDs, raw originals, undo and reopening',()=>{
        let p=project(['😀 Cafe\u0301 alpha\nbeta','next']);
        const page=p.pages[0],source=page.regions[0];
        source.wordEvidence={version:1,offset_unit:'utf16',source:'fixture',words:[{
            text:'Cafe\u0301',start:3,end:8,x:20,y:30,w:50,h:20,confidence:0.31,confidence_basis:'recognition',
        }]};
        p=approvePage(p,page.id);
        const before=prepareExport(p).text,preview=capturePassageSplit(p,page.id,source.id,8,{mode:'shared'});
        expect(preview.beforeText).toBe(preview.afterText);
        expect(preview.regions.map(region=>region.text)).toEqual(['😀 Cafe\u0301',' alpha\nbeta']);
        expect(preview.regions[1].separatorBefore).toBe('');
        expect(preview.regions[0]).toMatchObject({confidence:null,confidenceBasis:'manual',char_conf:[],charConfidenceAligned:false,review:'corrected'});
        expect(preview.regions[0].wordEvidence).toBeUndefined();
        expect(preview.regions[0].sourceComponents?.[0]).toMatchObject({sourceRegionId:source.id,originalText:source.text,textAtCapture:source.text,confidence:0.42,wordEvidence:source.wordEvidence});
        const next=applyPassagePreview(p,preview);
        expect(next.pages[0].regions.slice(0,2).map(region=>region.id)).toEqual(preview.replacementIds);
        expect(next.pages[0].approved).toBe(false);
        expect(next.pages[0].regions[2]).toEqual(p.pages[0].regions[1]);
        expect(prepareExport(next).text).toBe(before);
        expect(parseProject(serializeProject(next)).pages).toEqual(next.pages);
        expect(undoHistory(commitHistory(createHistory(p),next)).present).toEqual(p);
        expect(hasUnresolvedSharedGeometry(next.pages[0].regions)).toBe(true);
    });

    it('requires a nonempty grapheme-safe caret, including combining marks, flags and CRLF',()=>{
        for(const [text,offset] of [['😀x',1],['e\u0301x',1],['👩‍👩‍👧‍👦x',2],['🇩🇪x',2],['a\r\nb',2]] as const) {
            expect(isPassageBoundary(text,offset)).toBe(false);
            const p=project([text]); expect(()=>split(p,0,offset)).toThrow(/caret/);
        }
        for(const offset of [0,1,4]) {
            const p=project([' a  ']); expect(()=>split(p,0,offset)).toThrow(/caret/);
        }
        expect(isPassageBoundary('e\u0301 x',2)).toBe(true);
        const p=project(['a b']); expect(split(p,0,1).regions[1].text).toBe(' b');
    });

    it('uses safe ASCII boundaries when the webview has no Unicode segmenter',async()=>{
        const fallbackIntl=Object.create(Intl);fallbackIntl.Segmenter=undefined;
        vi.stubGlobal('Intl',fallbackIntl);vi.resetModules();
        try {
            const fallback=await import('./passages');
            expect(fallback.isPassageBoundary('alpha beta',5)).toBe(true);
            expect(fallback.isPassageBoundary('a\r\nb',2)).toBe(false);
            expect(fallback.isPassageBoundary('क् क',1)).toBe(false);
            expect(fallback.isPassageBoundary('क्क',2)).toBe(false);
            expect(fallback.isPassageBoundary('\u{1f3f4}\u{e0067}\u{e0062}\u{e007f}x',2)).toBe(false);
        } finally { vi.unstubAllGlobals();vi.resetModules(); }
    });

    it('uses only an explicit source cut, including right-side-first order',()=>{
        const p=project(),page=p.pages[0],source=page.regions[0];
        const preview=capturePassageSplit(p,page.id,source.id,5,{mode:'divide',axis:'x',position:100,firstSide:'high'});
        expect(preview.regions.map(({x,y,w,h})=>({x,y,w,h}))).toEqual([{x:100,y:20,w:70,h:100},{x:10,y:20,w:90,h:100}]);
        expect(preview.regions[0].sharedSourceGroups).toBeUndefined();
        expect(preview.regions[0].sourceComponents?.[0].boundsAtCapture).toEqual({x:10,y:20,w:160,h:100});
        for(const position of [10,170,NaN]) expect(()=>capturePassageSplit(p,page.id,source.id,5,{mode:'divide',axis:'x',position,firstSide:'low'})).toThrow(/cut/);
        expect(()=>capturePassageSplit(p,page.id,source.id,5,undefined as never)).toThrow(/coverage/);
        expect(prepareExport(applyPassagePreview(p,preview)).text).toBe(prepareExport(p).text);
    });

    it('merges adjacent equal-kind regions using an explicit separator and preserves untouched regions',()=>{
        const p=project(),page=p.pages[0],ids=page.regions.slice(0,2).map(region=>region.id);
        for(const separator of ['',' ','\n','\n\n'] as const) {
            const preview=capturePassageMerge(p,page.id,ids,separator),next=applyPassagePreview(p,preview),merged=next.pages[0].regions[0];
            expect(merged.text).toBe(`alpha beta${separator}gamma delta`);
            expect(merged.originalText).toBe(merged.text);
            expect(merged.sourceComponents?.map(source=>source.sourceRegionId)).toEqual(ids);
            expect(next.pages[0].regions[1]).toEqual(page.regions[2]);
        }
        expect(passageEligibility(page,ids[0]).nextMergeId).toBe(ids[1]);
    });

    it('rejects intervening excluded/nontext regions, reversed order, mixed kinds and empty passages',()=>{
        const p=project(),page=p.pages[0],ids=page.regions.map(region=>region.id);
        expect(()=>capturePassageMerge(p,page.id,[ids[0],ids[2]],' ')).toThrow(/intervening/);
        expect(()=>capturePassageMerge(p,page.id,[ids[1],ids[0]],' ')).toThrow(/reading order/);
        for(const patch of [{excluded:true},{kind:'table'},{kind:'picture'},{kind:'formula'},{text:'  '}]) {
            const changed=updateRegion(p,page.id,ids[1],patch as never);
            expect(passageEligibility(changed.pages[0],ids[0]).nextMergeId).toBeNull();
            expect(()=>capturePassageMerge(changed,page.id,ids.slice(0,2),' ')).toThrow();
        }
        const title=updateRegion(p,page.id,ids[1],{kind:'title'});
        expect(()=>capturePassageMerge(title,page.id,ids.slice(0,2),' ')).toThrow(/same kind/);
        expect(()=>capturePassageMerge(p,page.id,[ids[0]],' ')).toThrow(/Merge/);
        expect(()=>capturePassageMerge(p,page.id,[ids[0],ids[0]],' ')).toThrow(/distinct/);
    });

    it('keeps flat originals through repeated split/merge and clears consumed shared groups',()=>{
        let p=project(['alpha beta']);const original=p.pages[0].regions[0];
        for(let i=0;i<4;i++) {
            p=applyPassagePreview(p,split(p));
            const preview=capturePassageMerge(p,p.pages[0].id,p.pages[0].regions.map(region=>region.id),'');
            p=applyPassagePreview(p,preview);
            expect(p.pages[0].regions[0].text).toBe(original.text);
            expect(p.pages[0].regions[0].sourceComponents).toHaveLength(1);
            expect(p.pages[0].regions[0].sourceComponents?.[0].sourceRegionId).toBe(original.id);
            expect(p.pages[0].regions[0].sharedSourceGroups).toBeUndefined();
        }
    });

    it('retains multiple unconsumed shared families through a bridging merge',()=>{
        let p=project(['alpha beta','gamma delta']);
        p=applyPassagePreview(p,split(p));
        p=applyPassagePreview(p,split(p,2));
        const page=p.pages[0],groups=[page.regions[0].sharedSourceGroups![0],page.regions[2].sharedSourceGroups![0]];
        p=applyPassagePreview(p,capturePassageMerge(p,page.id,page.regions.slice(1,3).map(region=>region.id),' '));
        expect(p.pages[0].regions[1].sharedSourceGroups).toEqual(groups);
        expect(hasUnresolvedSharedGeometry(p.pages[0].regions)).toBe(true);
        p=applyPassagePreview(p,capturePassageMerge(p,page.id,p.pages[0].regions.map(region=>region.id),' '));
        expect(p.pages[0].regions[0].sourceComponents).toHaveLength(2);
        expect(p.pages[0].regions[0].sharedSourceGroups).toBeUndefined();
    });

    it('rejects stale source/order/approval and changed preview payloads atomically',()=>{
        const p=project(),page=p.pages[0],preview=split(p);
        for(const changed of [editRegion(p,page.id,page.regions[2].id,'changed elsewhere'),updatePage(p,page.id,{imagePath:'/other.png'}),
            updatePage(p,page.id,{regions:[...page.regions].reverse()}),approvePage(p,page.id),updateRegion(p,page.id,page.regions[0].id,{x:11})]) {
            expect(()=>applyPassagePreview(changed,preview)).toThrow(/changed/);
        }
        const forged=structuredClone(preview);forged.regions[0].text='not previewed';
        expect(()=>applyPassagePreview(p,forged)).toThrow(/preview changed/);
        const badSource=structuredClone(preview);badSource.regions[0].sourceComponents![0].originalText='forged';
        expect(()=>applyPassagePreview(p,badSource)).toThrow(/preview changed/);
        expect(joinRegionText(p.pages[0].regions)).toBe('alpha beta\ngamma delta\nunrelated');
    });

    it('invalidates extraction but leaves the untouched source project unchanged',()=>{
        let p=project();p=updatePage(p,p.pages[0].id,{provenance:{extraction:{fields:['old']},engine:'fixture'}});
        const before=JSON.stringify(p),next=applyPassagePreview(p,split(p));
        expect(next.pages[0].provenance.extraction).toBeUndefined();
        expect(next.pages[0].provenance.engine).toBe('fixture');
        expect(JSON.stringify(p)).toBe(before);
    });
});

describe('conservative passage repair',()=>{
    it('previews exact selected CRLF/CR repairs and keeps all original evidence',()=>{
        const p=project(['alpha\r\nbeta\rgamma']),page=p.pages[0],region=page.regions[0];
        const preview=capturePassageRepair(p,page.id,[region.id],'normalize-line-endings');
        expect(preview.changes.map(change=>change.before)).toEqual(['\r\n','\r']);
        expect(previewRepairText(preview,[])).toBe(region.text);
        expect(previewRepairText(preview,[preview.changes[0].id])).toBe('alpha\nbeta\rgamma');
        const next=applyPassageRepair(p,preview,[preview.changes[0].id]);
        expect(next.pages[0].regions[0].text).toBe('alpha\nbeta\rgamma');
        expect(next.pages[0].regions[0].originalText).toBe(region.originalText);
        expect(next.pages[0].regions[0].confidence).toBe(0.42);
        expect(undoHistory(commitHistory(createHistory(p),next)).present).toEqual(p);
    });

    it('keeps indentation, tabs, NBSP and numeric alignment outside explicit proposals',()=>{
        const p=project(['  alpha   beta\t\tgamma  42  thing\u00a0\u00a0end']),page=p.pages[0];
        const preview=capturePassageRepair(p,page.id,[page.regions[0].id],'collapse-inline-spaces');
        expect(preview.changes).toHaveLength(1);expect(preview.changes[0].risky).toBe(true);
        expect(preview.afterText).toBe('  alpha beta\t\tgamma  42  thing\u00a0\u00a0end');
        const trailing=project(['  alpha \t\r\n beta\u00a0 \n\t']),line=trailing.pages[0];
        const trim=capturePassageRepair(trailing,line.id,[line.regions[0].id],'trim-trailing-spaces');
        expect(trim.afterText).toBe('  alpha\r\n beta\u00a0\n');
    });

    it('proposes lowercase continuations only, leaving lists, identifiers, hyphens and paragraphs intact',()=>{
        const p=project(['alpha\nbeta\n\n1. item\nnext\n- item\nnext\nfield42\nvalue\nname-\nvalue\nJean-\nLuc']),page=p.pages[0];
        const preview=capturePassageRepair(p,page.id,[page.regions[0].id],'join-lines');
        expect(preview.changes).toHaveLength(1);expect(preview.changes[0].risky).toBe(true);
        expect(preview.afterText).toBe(page.regions[0].text.replace('alpha\nbeta','alpha beta'));
        const words=project(['ex-\nample\nwell-\nknown\nJean-\nLuc\n5-\n3\nA1-\npart\n- some-\nword']),wordPage=words.pages[0];
        const hyphen=capturePassageRepair(words,wordPage.id,[wordPage.regions[0].id],'dehyphenate');
        expect(hyphen.changes.map(change=>change.before)).toEqual(['-\n','-\n']);
        expect(previewRepairText(hyphen,[])).toBe(wordPage.regions[0].text);
        expect(hyphen.changes.every(change=>change.risky)).toBe(true);
    });

    it('never repairs across passage boundaries and respects separator metadata',()=>{
        let p=project(['alpha','beta']);p=updateRegion(p,p.pages[0].id,p.pages[0].regions[1].id,{separatorBefore:''});
        const page=p.pages[0],preview=capturePassageRepair(p,page.id,page.regions.map(region=>region.id),'join-lines');
        expect(preview.changes).toEqual([]);expect(preview.afterText).toBe('alphabeta');
        expect(applyPassageRepair(p,preview,[])).toBe(p);
    });

    it('checks selected IDs and rejects stale or forged repair changes without mutation',()=>{
        const p=project(['alpha\nbeta']),page=p.pages[0],preview=capturePassageRepair(p,page.id,[page.regions[0].id],'join-lines'),id=preview.changes[0].id;
        expect(()=>applyPassageRepair(p,preview,['unknown'])).toThrow(/invalid/);
        expect(()=>applyPassageRepair(p,preview,[id,id])).toThrow(/invalid/);
        const forged=structuredClone(preview);forged.changes[0].after='forged';
        expect(()=>applyPassageRepair(p,forged,[id])).toThrow(/preview changed/);
        expect(()=>applyPassageRepair(editRegion(p,page.id,page.regions[0].id,'different'),preview,[id])).toThrow(/source page changed/);
        expect(p.pages[0].regions[0].text).toBe('alpha\nbeta');
    });
});
