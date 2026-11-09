import { describe, expect, it } from 'vitest';
import { addDocument, applyPageImage, applyRecognition, createProject, editRegion, parseProject, prepareExport, serializeProject, updateRegion } from './model';
import { applyPassagePreview, capturePassageSplit } from './passages';
import { joinRegionText, hasUnresolvedSharedGeometry, sourceOffsetForTextarea } from './text';
import { applyZoneReview, captureZoneReview, prepareZoneReview } from './zoneReview';
import { getWordEvidence } from './words';

function sourceProject() {
    let p=addDocument(createProject(),{source_uri:'/original.png',name:'Original',pages:[{image_path:'/page.png',width:600,height:800}]});
    const pageId=p.pages[0].id;
    p=applyRecognition(p,pageId,{width:600,height:800,regions:[
        {text:'One two',x:20,y:40,w:200,h:30,word_evidence:{version:1,offset_unit:'utf16',source:'fixture',words:[
            {text:'One',start:0,end:3,x:20,y:40,w:60,h:30,confidence:0.42,confidence_basis:'recognition'},
        ]}},
        {text:'Other passage',x:20,y:300,w:200,h:30,confidence:0.99,confidence_basis:'recognition'},
    ]});
    return p;
}
function splitProject() {
    let p=sourceProject(),pageId=p.pages[0].id,regionId=p.pages[0].regions[0].id;
    p=editRegion(p,pageId,regionId,'ONE two');
    return applyPassagePreview(p,capturePassageSplit(p,pageId,regionId,3,{mode:'shared'}));
}

describe('explicit passage text boundaries and source persistence',()=>{
    it('maps a textarea caret through CRLF and CR without modifying the source or accepting stale text',()=>{
        const source='One\r\n😀\rtwo',visible='One\n😀\ntwo';
        expect(sourceOffsetForTextarea(source,visible,0)).toBe(0);
        expect(sourceOffsetForTextarea(source,visible,3)).toBe(3);
        expect(sourceOffsetForTextarea(source,visible,4)).toBe(5);
        expect(sourceOffsetForTextarea(source,visible,6)).toBe(7);
        expect(sourceOffsetForTextarea(source,visible,visible.length)).toBe(source.length);
        expect(sourceOffsetForTextarea(source,'Changed',3)).toBeNull();
        for(const invalid of [-1,1.5,NaN,visible.length+1]) expect(sourceOffsetForTextarea(source,visible,invalid)).toBeNull();
    });
    it('keeps legacy separators and preserves an explicit split without adding text',()=>{
        expect(joinRegionText([{text:'First'},{text:'Second'}])).toBe('First\nSecond');
        expect(joinRegionText([{text:'First'},{text:'Second',separatorBefore:''},{text:'Third',separatorBefore:' '}])).toBe('FirstSecond Third');
        const p=splitProject();
        expect(prepareExport(p).text).toBe('ONE two\nOther passage');
        expect(prepareExport(p).pages[0].regions[1].separator_before).toBe('');
        expect(p.pages[0].regions[0].sourceComponents![0]).toMatchObject({originalText:'One two',textAtCapture:'ONE two'});
        expect(getWordEvidence(p.pages[0].regions[0]).every(word=>word.basis==='edited')).toBe(true);
    });
    it('blocks only unresolved spatial output and keeps source records after manual coverage repair',()=>{
        let p=splitProject(),page=p.pages[0];
        expect(hasUnresolvedSharedGeometry(page.regions)).toBe(true);
        for(const format of ['pdf','hocr','alto','XML']) expect(prepareExport(p,{format}).errors[0]).toMatch(/overlapping/);
        expect(prepareExport(p,{format:'json'}).errors).toEqual([]);
        p=updateRegion(p,page.id,page.regions[0].id,{w:100});
        p=updateRegion(p,page.id,page.regions[1].id,{x:120,w:100});
        expect(hasUnresolvedSharedGeometry(p.pages[0].regions)).toBe(false);
        expect(prepareExport(p,{format:'pdf'}).errors).toEqual([]);
        expect(parseProject(serializeProject(p)).pages[0].regions[0].sourceComponents![0].boundsAtCapture.w).toBe(200);
    });
    it('retains historical components and the new raw result through accepted partial OCR',()=>{
        const p=splitProject(),page=p.pages[0],untouched=page.regions[2];
        const request=captureZoneReview(p,page.id,page.regions[0]);
        const preview=prepareZoneReview(p,request,{image_path:page.imagePath,width:600,height:800,regions:[{
            text:'One two',x:20,y:40,w:200,h:30,word_evidence:{version:1,offset_unit:'utf16',source:'new-fixture',words:[
                {text:'One',start:0,end:3,x:20,y:40,w:60,h:30,confidence:0.95,confidence_basis:'recognition'},
            ]},
        }]});
        expect(p.pages[0].regions).toHaveLength(3);
        const next=parseProject(serializeProject(applyZoneReview(p,preview))),recognized=next.pages[0].regions[0];
        expect(next.pages[0].regions[1]).toEqual(untouched);
        expect(recognized.sourceComponents).toHaveLength(2);
        expect(recognized.sourceComponents![0].wordEvidence!.words[0].confidence).toBe(0.42);
        expect(recognized.sourceComponents![1].wordEvidence!.words[0].confidence).toBe(0.95);
        expect(getWordEvidence(recognized)[0].confidence).toBe(0.95);
        expect(prepareExport(next).pages[0].regions[0].source_components).toEqual(recognized.sourceComponents);
    });
    it('keeps a divided split boundary when only its second half is recognized again',()=>{
        let p=sourceProject(),page=p.pages[0];
        p=applyPassagePreview(p,capturePassageSplit(p,page.id,page.regions[0].id,3,{mode:'divide',axis:'x',position:120,firstSide:'low'}));
        page=p.pages[0];
        const second=page.regions[1],request=captureZoneReview(p,page.id,second);
        expect(request.affectedRegionIds).toEqual([second.id]);
        const preview=prepareZoneReview(p,request,{width:600,height:800,regions:[{text:' TWO',x:120,y:40,w:100,h:30}]});
        const next=applyZoneReview(p,preview);
        expect(prepareExport(next).text).toBe('One TWO\nOther passage');
        expect(parseProject(serializeProject(next)).pages[0].regions[1].separatorBefore).toBe('');
    });
    it('archives structural originals through empty re-OCR and changed images then links new recognition',()=>{
        const p=splitProject(),page=p.pages[0];
        const empty=applyRecognition(p,page.id,{width:600,height:800,regions:[]});
        expect(empty.pages[0].regions).toEqual([]);
        expect((empty.pages[0].provenance.passageSources as unknown[])).toHaveLength(1);
        const changed=applyPageImage(p,page.id,{image_path:'/rotated.png',width:800,height:600},'rotate');
        const archived=parseProject(serializeProject(changed));
        expect((archived.pages[0].provenance.passageSources as {imagePath:string}[])[0].imagePath).toBe('/page.png');
        const next=applyRecognition(archived,page.id,{width:800,height:600,regions:[{text:'New',x:10,y:10,w:80,h:30}]});
        expect(next.pages[0].provenance.passageSources).toBeUndefined();
        expect(next.pages[0].regions[0].sourceComponents!.map(component=>component.imagePath)).toEqual(['/page.png','/rotated.png']);
        expect(next.pages[0].regions[0].wordEvidence).toBeUndefined();
    });
});
