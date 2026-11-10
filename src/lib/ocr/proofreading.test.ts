import { describe, expect, it } from 'vitest';
import {
    addDocument, applyRecognition, approvePage, commitHistory, createHistory, createProject,
    editRegion, parseProject, prepareExport, serializeProject, undoHistory, updatePage, updateRegion,
} from './model';
import {
    applyCorrectionPreview, captureCorrectionPreview, createCorrectionRule, createVocabularyEntry,
    emptyProofreadingData, exportProofreadingData, ignoreSuggestion, isSuggestionIgnored,
    mergeProofreadingData, readProofreadingData, suggestCorrections, updateProjectProofreading,
    type CorrectionRequest, type ProofreadingSelection,
} from './proofreading';
import { getWordEvidence } from './words';

function project(text='😀 Inv0ice Inv0ice\nNS-2026-1042') {
    let p=addDocument(createProject(),{source_uri:'/source.pdf',name:'Source',pages:[
        {image_path:'/page1.png',width:600,height:800},{image_path:'/page2.png',width:600,height:800},
    ]});
    for(const page of p.pages) p=applyRecognition(p,page.id,{width:600,height:800,regions:[{
        text,x:10,y:20,w:500,h:150,confidence:0.4,confidence_basis:'recognition',
    }]});
    return p;
}
const empty=()=>emptyProofreadingData('en');
const request=(p:ReturnType<typeof project>,patch:Partial<CorrectionRequest>={}):CorrectionRequest=>({
    from:'Inv0ice',to:'Invoice',language:'en',caseSensitive:true,wholeWord:true,
    scope:'page',pageId:p.pages[0].id,...patch,
});
const selected=(p:ReturnType<typeof project>):ProofreadingSelection=>({
    pageId:p.pages[0].id,regionId:p.pages[0].regions[0].id,start:3,end:10,text:'Inv0ice',
});

describe('proofreading vocabulary and rules',()=>{
    it('round-trips scoped language, protection, literal rules and old projects',()=>{
        const p=project(),data=empty();
        data.entries.push(createVocabularyEntry('Café','fra'),createVocabularyEntry('NS-2026-1042','und','protected'));
        data.rules.push(createCorrectionRule({name:'Invoice confusion',from:'Inv0ice',to:'Invoice',language:'eng',caseSensitive:true,wholeWord:true,enabled:true,example:'Inv0ice total'}));
        const updated=updateProjectProofreading(p,data);
        const restored=parseProject(serializeProject(updated));
        expect(restored.proofreading?.entries[0].language).toBe('fr');
        expect(restored.proofreading?.rules[0].language).toBe('en');
        expect(restored.proofreading).toEqual(updated.proofreading);
        expect(parseProject(p).proofreading).toBeUndefined();
        expect(readProofreadingData(undefined)).toEqual(emptyProofreadingData());
        expect(undoHistory(commitHistory(createHistory(p),updated)).present.proofreading).toBeUndefined();
        expect(updated.pages[0].regions).toEqual(p.pages[0].regions);
    });
    it('rejects malformed imported preferences without mutating current data',()=>{
        const data=empty(),before=JSON.stringify(data);
        for(const patch of [{version:2},{language:'en+de'},{entries:[{id:'bad'}]},{rules:[{id:'bad'}]},{ignored:[{}]}]) {
            expect(()=>readProofreadingData({...data,...patch})).toThrow();
        }
        const entry=createVocabularyEntry('Invoice','en');
        expect(()=>readProofreadingData({...data,entries:[entry,entry]})).toThrow(/entry/);
        expect(JSON.stringify(data)).toBe(before);
    });
    it('merges without erasing existing data, duplicates or colliding IDs',()=>{
        const first=empty(),second=empty(),a=createVocabularyEntry('Invoice','en');
        first.entries=[a]; second.entries=[{...a}, {...a,text:'Total'}];
        // Imported IDs must be unique within one file, even for different words.
        expect(()=>mergeProofreadingData(first,second)).toThrow();
        second.entries=[{...a,text:'Total'}];
        const merged=mergeProofreadingData(first,second);
        expect(merged.entries.map(entry=>entry.text)).toEqual(['Invoice','Total']);
        expect(new Set(merged.entries.map(entry=>entry.id)).size).toBe(2);
        expect(mergeProofreadingData(merged,{...empty(),entries:[a]}).entries).toHaveLength(2);
    });
    it('offers only known nearby vocabulary and leaves identifiers/quantities/languages alone',()=>{
        const p=project(),user=empty();
        user.entries.push(createVocabularyEntry('Invoice','en'),createVocabularyEntry('Invoices','fr'));
        expect(suggestCorrections(p,selected(p),user,'eng')).toEqual([expect.objectContaining({text:'Invoice',source:'vocabulary',confidence:null})]);
        expect(suggestCorrections(p,selected(p),user,'de')).toEqual([]);
        for(const text of ['120.00','2026-11-09','NS-2026-1042','XYZ1','unknown']) {
            const sample=project(text),selection={...selected(sample),start:0,end:text.length,text};
            expect(suggestCorrections(sample,selection,user,'en')).toEqual([]);
        }
        expect(getWordEvidence(p.pages[0].regions[0])[0].confidence).toBeNull();
        expect(p.pages[0].regions[0].confidence).toBe(0.4);
    });
    it('uses native alternatives only for a current verified word, not its edited successor',()=>{
        let p=project('Inv0ice'),page=p.pages[0];
        p=applyRecognition(p,page.id,{width:600,height:800,regions:[{text:'Inv0ice',x:0,y:0,w:100,h:30,
            word_evidence:{version:1,offset_unit:'utf16',source:'fixture',words:[{text:'Inv0ice',start:0,end:7,x:0,y:0,w:100,h:30,
                confidence:0.4,confidence_basis:'recognition',alternatives:[{text:'Invoice',confidence:0.6}]}]},
        }]});
        const selection={pageId:page.id,regionId:p.pages[0].regions[0].id,start:0,end:7,text:'Inv0ice'};
        expect(suggestCorrections(p,selection,empty(),'en')[0]).toMatchObject({text:'Invoice',source:'engine',confidence:0.6});
        p.pages[0].regions[0].wordEvidence!.words[0].confidence_basis='detection';
        expect(suggestCorrections(p,selection,empty(),'en')[0]).toMatchObject({text:'Invoice',source:'engine',confidence:null});
        p=editRegion(p,page.id,selection.regionId,'Inv0ice extra');
        expect(suggestCorrections(p,selection,empty(),'en')).toEqual([]);
    });
    it('does not suggest a near neighbor for accepted vocabulary and respects exact-case membership',()=>{
        const p=project('cat'),selection={...selected(p),start:0,end:3,text:'cat'},user=empty();
        user.entries=[createVocabularyEntry('cat','en'),createVocabularyEntry('car','en')];
        expect(suggestCorrections(p,selection,user,'en')).toEqual([]);
        const upper=project('Cat'),upperSelection={...selected(upper),start:0,end:3,text:'Cat'};
        expect(suggestCorrections(upper,upperSelection,user,'en').map(suggestion=>suggestion.text)).toEqual(['cat']);
        user.entries[0].caseSensitive=false;
        expect(suggestCorrections(upper,upperSelection,user,'en')).toEqual([]);
        const accent=project('CafÉ'),accentSelection={...selected(accent),start:0,end:4,text:'CafÉ'};
        user.entries=[createVocabularyEntry('Café','en')];
        expect(suggestCorrections(accent,accentSelection,user,'en')[0]?.text).toBe('Café');
    });
    it('ignores one current occurrence durably without approving it or exporting ignores as vocabulary',()=>{
        const p=project(),selection=selected(p),user=empty();
        user.entries.push(createVocabularyEntry('Invoice','en'));
        const ignored=parseProject(serializeProject(ignoreSuggestion(p,selection)));
        expect(isSuggestionIgnored(ignored,selection)).toBe(true);
        expect(suggestCorrections(ignored,selection,user,'en')).toEqual([]);
        expect(ignored.pages[0].regions[0]).toEqual(p.pages[0].regions[0]);
        expect(JSON.parse(exportProofreadingData(ignored.proofreading!)).ignored).toEqual([]);
        const changed=editRegion(ignored,selection.pageId,selection.regionId,'😀 Inv0ice changed');
        expect(isSuggestionIgnored(changed,selection)).toBe(false);
        expect(suggestCorrections(changed,selection,user,'en')).toHaveLength(1);
    });
});

describe('scoped literal correction previews',()=>{
    it('shows exact ranges and replaces only selected occurrences in one undo step',()=>{
        const p=project(),preview=captureCorrectionPreview(p,request(p),empty());
        expect(preview.matches).toHaveLength(2);
        expect(preview.matches[0]).toMatchObject({start:3,end:10,before:'Inv0ice',after:'Invoice'});
        const next=applyCorrectionPreview(p,preview,[preview.matches[1].id],empty());
        expect(next.pages[0].regions[0].text).toBe('😀 Inv0ice Invoice\nNS-2026-1042');
        expect(next.pages[1]).toEqual(p.pages[1]);
        expect(next.pages[0].regions[0].originalText).toBe(p.pages[0].regions[0].text);
        expect(undoHistory(commitHistory(createHistory(p),next)).present).toEqual(p);
        expect(prepareExport(next).pages[0].regions[0].text).toContain('Invoice');
    });
    it('preserves literal dollar signs, regex punctuation, backslashes and newlines',()=>{
        const p=project('Cost: [USD] [USD]'),preview=captureCorrectionPreview(p,request(p,{from:'[USD]',to:'$&\\value\nnext',wholeWord:false}),empty());
        const next=applyCorrectionPreview(p,preview,preview.matches.map(match=>match.id),empty());
        expect(next.pages[0].regions[0].text).toBe('Cost: $&\\value\nnext $&\\value\nnext');
    });
    it('maps canonically equivalent Unicode to exact original UTF-16 and protects containing identifiers',()=>{
        let p=project('😀 Cafe\u0301 café Café-42');
        const data=empty();data.entries.push(createVocabularyEntry('Café-42','und','protected'));
        p=updateProjectProofreading(p,data);
        const preview=captureCorrectionPreview(p,request(p,{from:'Café',to:'Coffee',caseSensitive:false}),empty());
        expect(preview.matches.map(match=>[match.start,match.end,match.before])).toEqual([[3,8,'Cafe\u0301'],[9,13,'café']]);
        expect(preview.skippedProtected).toBe(1);
        const next=applyCorrectionPreview(p,preview,preview.matches.map(match=>match.id),empty());
        expect(next.pages[0].regions[0].text).toBe('😀 Coffee Coffee Café-42');
    });
    it('respects case and whole-word choices without offsets from expanded lowercase strings',()=>{
        const p=project('İ I i Inv0ices Inv0ice');
        expect(captureCorrectionPreview(p,request(p),empty()).matches).toHaveLength(1);
        expect(captureCorrectionPreview(p,request(p,{wholeWord:false}),empty()).matches).toHaveLength(2);
        const preview=captureCorrectionPreview(p,request(p,{from:'i',to:'x',caseSensitive:false}),empty());
        expect(preview.matches.map(match=>match.before)).toEqual(['I','i']);
        expect(applyCorrectionPreview(p,preview,preview.matches.map(match=>match.id),empty()).pages[0].regions[0].text).toBe('İ x x Inv0ices Inv0ice');
    });
    it('skips approved text by default, explicit inclusion revokes approval, and scope never widens',()=>{
        const original=project(),approved=approvePage(original,original.pages[0].id);
        const skipped=captureCorrectionPreview(approved,request(approved),empty());
        expect(skipped.matches).toEqual([]);expect(skipped.skippedApproved).toBe(2);
        const preview=captureCorrectionPreview(approved,request(approved,{includeApproved:true}),empty());
        const next=applyCorrectionPreview(approved,preview,preview.matches.map(match=>match.id),empty());
        expect(next.pages[0].approved).toBe(false);expect(next.pages[0].regions[0].review).toBe('corrected');
        expect(next.pages[1]).toEqual(approved.pages[1]);
        const selectedPages=captureCorrectionPreview(original,request(original,{scope:'selected-pages',pageIds:[original.pages[1].id]}),empty());
        expect(new Set(selectedPages.matches.map(match=>match.pageId))).toEqual(new Set([original.pages[1].id]));
    });
    it('blocks protection and stale text, pixels, geometry, approval and policy changes atomically',()=>{
        const p=project(),preview=captureCorrectionPreview(p,request(p),empty()),ids=preview.matches.map(match=>match.id);
        const page=p.pages[0],region=page.regions[0];
        for(const changed of [
            editRegion(p,page.id,region.id,'changed'),updatePage(p,page.id,{imagePath:'/new.png'}),
            updateRegion(p,page.id,region.id,{x:20}),approvePage(p,page.id),
            updatePage(p,page.id,{excluded:true}),updateRegion(p,page.id,region.id,{excluded:true}),
        ]) expect(()=>applyCorrectionPreview(changed,preview,ids,empty())).toThrow(/changed|Preview|preview/);
        const protectedUser=empty();protectedUser.entries.push(createVocabularyEntry('Inv0ice','en','protected'));
        expect(()=>applyCorrectionPreview(p,preview,ids,protectedUser)).toThrow(/Vocabulary/);
        expect(captureCorrectionPreview(p,request(p),protectedUser).skippedProtected).toBe(2);
        expect(suggestCorrections(p,selected(p),protectedUser,'en')).toEqual([]);
        expect(p.pages[0].regions[0].text).toBe('😀 Inv0ice Inv0ice\nNS-2026-1042');
    });
    it('rejects forged selections/replacements and limits a single suggestion to its occurrence',()=>{
        const p=project(),preview=captureCorrectionPreview(p,request(p,{selection:selected(p),scope:'passage',regionId:p.pages[0].regions[0].id}),empty());
        expect(preview.matches).toHaveLength(1);
        expect(()=>applyCorrectionPreview(p,preview,['unknown'],empty())).toThrow(/invalid/);
        expect(()=>applyCorrectionPreview(p,preview,[preview.matches[0].id,preview.matches[0].id],empty())).toThrow(/invalid/);
        const forged=structuredClone(preview);forged.matches[0].after='forged';
        expect(()=>applyCorrectionPreview(p,forged,[forged.matches[0].id],empty())).toThrow(/changed/);
        expect(applyCorrectionPreview(p,preview,[],empty())).toBe(p);
    });
    it('deduplicates protected scans and avoids scanning protection when there are no corrections',()=>{
        const p=project(),user=empty();
        user.entries=Array.from({length:1000},(_,i)=>({...createVocabularyEntry('Inv0ice','en','protected'),id:`protected-${i}`}));
        const preview=captureCorrectionPreview(p,request(p),user);
        expect(preview.skippedProtected).toBe(2);
        expect(preview.matches).toEqual([]);
        const unrelated=project('safe '.repeat(6000));
        expect(captureCorrectionPreview(unrelated,request(unrelated),user).matches).toEqual([]);
    });
});
