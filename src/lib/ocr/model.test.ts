import { describe, expect, it } from 'vitest';
import {
    addDocument, addRegion, applyPageImage, applyRecognition, approvePage, commitHistory,
    createHistory, createProject, deleteRegion, editRegion, findMatches, getMetrics,
    getReviewIssues, movePage, moveRegion, nextMatch, normalizeRectangle, parseProject,
    prepareExport, redoHistory, removePage, replaceMatches, serializeProject, splitPage, undoHistory,
    updatePage, updateRegion, type OcrProject,
} from './model';

const imported = (name='invoice.pdf', count=2) => ({source_uri:`/sources/${name}`,name,pages:Array.from({length:count},(_,i)=>({image_path:`/cache/${name}-${i}.png`,width:1000,height:1400}))});
function project(count=2): OcrProject { return addDocument(createProject('Test'),imported('invoice.pdf',count)); }
function recognize(p:OcrProject, index=0, text='Original text', confidence:number|null=0.93):OcrProject {
    return applyRecognition(p,p.pages[index].id,{image_path:p.pages[index].imagePath,width:1000,height:1400,elapsed_ms:120,engine:'surya',regions:[{text,x:100,y:150,w:500,h:50,confidence,confidence_basis:'recognition',char_conf:[]}]});
}

describe('OCR project and recognition',()=>{
    it('rejects malformed native payloads and dimension drift while preserving reviewed work',()=>{
        const recognized=recognize(project(1));
        const p=approvePage(recognized,recognized.pages[0].id);
        const before=JSON.stringify(p), pageId=p.pages[0].id;
        for (const value of [null,{width:1000,height:1400,regions:null},{width:1000,height:1400,regions:[null]},
            {width:1000,height:1400,regions:[{text:99,x:1,y:2,w:20,h:30}]},
            {width:1000,height:1400,regions:[{text:'x',char_conf:42,x:1,y:2,w:20,h:30}]},
            {width:2000,height:2800,regions:[]}]) {
            expect(()=>applyRecognition(p,pageId,value as never)).toThrow(/Recognition/);
            expect(JSON.stringify(p)).toBe(before);
        }
    });
    it('honors explicit unaligned evidence and never promotes token or unspecified scores through a boolean flag',()=>{
        const p=project(1), pageId=p.pages[0].id;
        const result={width:1000,height:1400,regions:[{text:'Hi',x:0,y:0,w:20,h:20,confidence:0.95,char_conf:[0.1,0.99],confidence_basis:'recognition'}]};
        for (const metadata of [
            {char_conf_unit:'token',char_conf_aligned:true},
            {char_conf_aligned:true},
            {char_conf_unit:'unicode_codepoint',char_conf_aligned:false},
        ]) {
            const out=applyRecognition(p,pageId,{...result,...metadata});
            expect(out.pages[0].regions[0].charConfidenceAligned).toBe(false);
            expect(getReviewIssues(out)).toEqual([]);
            expect(prepareExport(out).pages[0].regions[0].char_conf_aligned).toBe(false);
        }
    });
    it('retains import identity and original source order across multiple documents',()=>{
        const p=addDocument(project(),imported('book.tif',3));
        expect(new Set([...p.documents.map(d=>d.id),...p.pages.map(p=>p.id)]).size).toBe(7);
        expect(p.pages.map(p=>p.sourcePage)).toEqual([1,2,1,2,3]);
        expect(p.pages[2].documentId).toBe(p.documents[1].id);
        expect(p.pages[0].status).toBe('pending');
        expect(p.pages[0].originalImagePath).toBe(p.pages[0].imagePath);
    });
    it('preserves native diagnostics and never invents recognition confidence',()=>{
        const p=project(1);
        const out=applyRecognition(p,p.pages[0].id,{width:1000,height:1400,image_path:'/cache/normalized.png',elapsed_ms:25,accepted:false,engine:'vlm',regions:[{text:'Hi',x:0,y:0,w:200,h:50,confidence:0.99}]});
        expect(out.pages[0].provenance.accepted).toBe(false);
        expect(out.pages[0].imagePath).toBe('/cache/normalized.png');
        expect(out.pages[0].originalImagePath).toBe(p.pages[0].imagePath);
        expect(out.pages[0].regions[0].confidenceBasis).toBe('unknown');
        expect(getMetrics(out).meanConfidence).toBeNull();
        expect(getReviewIssues(out)[0].reason).toBe('unknown-confidence');
    });
    it('never treats VLM token probabilities as character-aligned by coincidental length',()=>{
        const p=project(1), pageId=p.pages[0].id;
        const result={width:1000,height:1400,regions:[{text:'hi',x:0,y:0,w:20,h:20,confidence:0.95,char_conf:[0.1,0.99],confidence_basis:'recognition'}]};
        let out=applyRecognition(p,pageId,result);
        expect(out.pages[0].regions[0].charConfidenceAligned).toBe(false);
        expect(getReviewIssues(out)).toHaveLength(0);
        out=applyRecognition(p,pageId,{...result,char_conf_unit:'unicode_codepoint'});
        expect(out.pages[0].regions[0].charConfidenceAligned).toBe(true);
        expect(getReviewIssues(out)[0].reason).toBe('low-confidence');
        out=applyRecognition(p,pageId,{...result,char_conf_unit:'token'});
        expect(out.pages[0].regions[0].charConfidenceAligned).toBe(false);
    });
    it('rejects invalid engine geometry without mutating the prior result',()=>{
        const p=recognize(project(1));
        const original=JSON.stringify(p);
        expect(()=>applyRecognition(p,p.pages[0].id,{width:1000,height:1400,regions:[{text:'bad',x:999,y:0,w:10,h:20}]})).toThrow(/out-of-bounds/);
        expect(JSON.stringify(p)).toBe(original);
    });
    it('split pages preserve source identity and replace the spread in order',()=>{
        const p=recognize(project(2));
        const source=p.pages[0];
        const next=splitPage(p,source.id,[{image_path:'/left.png',width:500,height:1400},{image_path:'/right.png',width:500,height:1400}]);
        expect(next.pages).toHaveLength(3);
        expect(next.pages.map(page=>page.imagePath)).toEqual(['/left.png','/right.png',p.pages[1].imagePath]);
        expect(next.pages[0].documentId).toBe(source.documentId);
        expect(next.pages[1].sourcePage).toBe(source.sourcePage);
        expect(next.pages[1].originalImagePath).toBe(source.originalImagePath);
        expect(next.pages[0].regions).toEqual([]);
        expect(next.pages[1].status).toBe('pending');
        expect(next.pages[1].id).not.toBe(next.pages[0].id);
        expect(()=>splitPage(p,source.id,[])).toThrow(/at least two/);
    });
    it('restoring a rotated or cropped image invalidates all stale geometry',()=>{
        let p=recognize(project(1));
        p=approvePage(p,p.pages[0].id);
        const next=applyPageImage(p,p.pages[0].id,{image_path:'/cache/rotated.png',width:1400,height:1000},'rotate');
        expect(next.pages[0].status).toBe('pending');
        expect(next.pages[0].regions).toEqual([]);
        expect(next.pages[0].approved).toBe(false);
        expect(next.pages[0].originalImagePath).toBe(p.pages[0].originalImagePath);
        expect(next.pages[0].provenance.lastImageOperation).toBe('rotate');
    });
    it('pixel transforms clear extracted values while undo restores the source evidence',()=>{
        let p=recognize(project(1));
        const source=p.pages[0];
        const extraction={fields:[{name:'Invoice total',value:'120.00'}],tableRows:[['Item','Amount'],['Paper','120.00']],formula:'x^2',labels:['invoice'],sourceImagePath:source.imagePath,regionId:source.regions[0].id};
        p=updatePage(p,source.id,{provenance:{...source.provenance,extraction}});
        const history=createHistory(p);
        const next=applyPageImage(p,source.id,{image_path:'/cache/cropped.png',width:500,height:700},'crop');
        expect(next.pages[0].provenance.extraction).toBeUndefined();
        expect(next.pages[0].provenance.engine).toBe('surya');
        expect(next.pages[0].provenance.lastImageOperation).toBe('crop');
        expect(p.pages[0].provenance.extraction).toEqual(extraction);
        const undone=undoHistory(commitHistory(history,next)).present;
        expect(undone.pages[0].imagePath).toBe(source.imagePath);
        expect(undone.pages[0].provenance.extraction).toEqual(extraction);
    });
    it('split children never inherit spread extraction while other pages retain theirs',()=>{
        let p=recognize(project(2));
        const source=p.pages[0], other=p.pages[1];
        const spreadExtraction={tableRows:[['Left','Right']],sourceImagePath:source.imagePath};
        const otherExtraction={fields:[{name:'Reference',value:'ABC'}],sourceImagePath:other.imagePath};
        p=updatePage(p,source.id,{provenance:{...source.provenance,extraction:spreadExtraction}});
        p=updatePage(p,other.id,{provenance:{...other.provenance,extraction:otherExtraction}});
        const next=splitPage(p,source.id,[{image_path:'/left.png',width:500,height:1400},{image_path:'/right.png',width:500,height:1400}]);
        for (const child of next.pages.slice(0,2)) {
            expect(child.provenance.extraction).toBeUndefined();
            expect(child.provenance.engine).toBe('surya');
            expect(child.provenance.splitFromPageId).toBe(source.id);
        }
        expect(next.pages[2].provenance.extraction).toEqual(otherExtraction);
        expect(p.pages[0].provenance.extraction).toEqual(spreadExtraction);
        expect(undoHistory(commitHistory(createHistory(p),next)).present.pages[0].provenance.extraction).toEqual(spreadExtraction);
    });
});

describe('physical paper size',()=>{
    it('preserves PDF dimensions and assigns an explicit 300 DPI fallback to raster imports',()=>{
        const p=addDocument(createProject(),{source_uri:'/sources/pages.pdf',name:'Pages',pages:[
            {image_path:'/letter.png',width:2550,height:3300,page_width_points:612,page_height_points:792},
            {image_path:'/raster.png',width:1000,height:1400},
            {image_path:'/bad-metadata.png',width:1000,height:1400,page_width_points:0,page_height_points:792},
        ]});
        expect(p.pages.map(page=>[page.widthPoints,page.heightPoints])).toEqual([[612,792],[240,336],[240,336]]);
        expect(parseProject(serializeProject(p)).pages[0].widthPoints).toBe(612);
        expect(()=>addDocument(createProject(),{source_uri:'/huge.png',name:'Huge',pages:[{image_path:'/huge.png',width:100_000,height:1000}]})).toThrow(/physical size/);
    });
    it('recognition preserves paper size through higher pixel resolution and accepts native crop metadata',()=>{
        let p=project(1);
        const id=p.pages[0].id;
        p=updatePage(p,id,{widthPoints:612,heightPoints:792});
        p=applyRecognition(p,id,{image_path:'/upscaled.png',width:2000,height:2800,regions:[]});
        expect([p.pages[0].widthPoints,p.pages[0].heightPoints]).toEqual([612,792]);
        p=applyRecognition(p,id,{image_path:'/cropped.png',width:1000,height:1400,page_width_points:306,page_height_points:396,regions:[]});
        expect([p.pages[0].widthPoints,p.pages[0].heightPoints]).toEqual([306,396]);
        p=applyRecognition(p,id,{width:1000,height:1400,page_width_points:0,page_height_points:100,regions:[]});
        expect([p.pages[0].widthPoints,p.pages[0].heightPoints]).toEqual([306,396]);
    });
    it('rotation swaps physical axes, cropping scales them, and scan repair preserves them',()=>{
        let p=project(1);
        const id=p.pages[0].id;
        p=updatePage(p,id,{widthPoints:500,heightPoints:700});
        p=applyPageImage(p,id,{image_path:'/rotated.png',width:1400,height:1000},'rotate');
        expect([p.pages[0].widthPoints,p.pages[0].heightPoints]).toEqual([700,500]);
        p=applyPageImage(p,id,{image_path:'/crop.png',width:700,height:500},'crop');
        expect([p.pages[0].widthPoints,p.pages[0].heightPoints]).toEqual([350,250]);
        p=applyPageImage(p,id,{image_path:'/repair.png',width:1400,height:1000},'scan repair');
        expect([p.pages[0].widthPoints,p.pages[0].heightPoints]).toEqual([350,250]);
        p=applyPageImage(p,id,{image_path:'/auto-crop.png',width:1200,height:800,page_width_points:300,page_height_points:200},'scan repair');
        expect([p.pages[0].widthPoints,p.pages[0].heightPoints]).toEqual([300,200]);
    });
    it('split pages receive their proportional share of the spread physical width',()=>{
        const initial=project(1),id=initial.pages[0].id;
        const p=updatePage(initial,id,{widthPoints:500,heightPoints:700});
        const next=splitPage(p,id,[{image_path:'/left.png',width:300,height:1400},{image_path:'/right.png',width:700,height:1400}]);
        expect(next.pages.map(page=>[page.widthPoints,page.heightPoints])).toEqual([[150,700],[350,700]]);
        expect([p.pages[0].widthPoints,p.pages[0].heightPoints]).toEqual([500,700]);
    });
    it('legacy recovery derives physical size and rejects malformed persisted point dimensions',()=>{
        const old=project(1);
        delete old.pages[0].widthPoints;delete old.pages[0].heightPoints;
        const restored=parseProject(old);
        expect([restored.pages[0].widthPoints,restored.pages[0].heightPoints]).toEqual([240,336]);
        expect(old.pages[0].widthPoints).toBeUndefined();
        for (const widthPoints of [0,-1,14_400.1,NaN,Infinity]) {
            const bad=updatePage(restored,restored.pages[0].id,{widthPoints});
            expect(()=>parseProject(bad)).toThrow(/physical page dimensions/);
        }
    });
    it('export uses physical page points independently from pixels and blocks invalid physical sizes',()=>{
        let p=recognize(project(1));
        const id=p.pages[0].id;
        p=updatePage(p,id,{widthPoints:612,heightPoints:792});
        const out=prepareExport(p);
        expect(out.errors).toEqual([]);
        expect(out.pages[0]).toMatchObject({width:1000,height:1400,page_width_points:612,page_height_points:792});
        const bad=prepareExport(updatePage(p,id,{heightPoints:0}));
        expect(bad.pages).toEqual([]);
        expect(bad.errors[0]).toMatch(/physical page dimensions/);
    });
});

describe('proofreading and geometry',()=>{
    it('preserves approval and evidence version for identical text, geometry, order, and image edits',()=>{
        let p=recognize(project(1));
        p=approvePage(p,p.pages[0].id);
        const page=p.pages[0], region=page.regions[0];
        expect(editRegion(p,page.id,region.id,region.text)).toBe(p);
        expect(updateRegion(p,page.id,region.id,{x:region.x,y:region.y})).toBe(p);
        expect(moveRegion(p,page.id,region.id,-1)).toBe(p);
        expect(applyPageImage(p,page.id,{image_path:page.imagePath,width:page.width,height:page.height},'scan repair')).toBe(p);
        expect(approvePage(p,page.id)).toBe(p);
        expect(updatePage(p,'missing',{approved:false})).toBe(p);
        expect(p.pages[0].approved).toBe(true);
        expect(p.pages[0].contentRevision).toBe(page.contentRevision);
    });
    it('edits preserve raw recognition evidence and revoke page approval',()=>{
        let p=recognize(project(1));
        const pageId=p.pages[0].id, regionId=p.pages[0].regions[0].id;
        p=approvePage(p,pageId);
        expect(getReviewIssues(p)).toEqual([]);
        p=editRegion(p,pageId,regionId,'Corrected text');
        const region=p.pages[0].regions[0];
        expect(region.originalText).toBe('Original text');
        expect(region.confidence).toBe(0.93);
        expect(region.review).toBe('corrected');
        expect(p.pages[0].approved).toBe(false);
        expect(getReviewIssues(p)[0].reason).toBe('changed');
        p=updateRegion(p,pageId,regionId,{review:'approved'});
        expect(getReviewIssues(p)).toEqual([]);
    });
    it('low and unknown confidence are distinct and reviewed regions leave the queue',()=>{
        let p=recognize(project(),0,'one',0.1);
        p=recognize(p,1,'two',null);
        expect(getReviewIssues(p).map(i=>i.reason)).toEqual(['low-confidence','unknown-confidence']);
        p=approvePage(p,p.pages[0].id);
        expect(getReviewIssues(p)).toHaveLength(1);
        p=updatePage(p,p.pages[1].id,{excluded:true});
        expect(getReviewIssues(p)).toEqual([]);
    });
    it('empty pages remain visible to review until explicitly approved',()=>{
        let p=project(1);
        p=applyRecognition(p,p.pages[0].id,{width:1000,height:1400,regions:[]});
        expect(getReviewIssues(p)[0].reason).toBe('empty');
        p=approvePage(p,p.pages[0].id);
        expect(getReviewIssues(p)).toEqual([]);
    });
    it('normalizes reverse drag, clips edges, and rejects degenerate or nonfinite boxes',()=>{
        expect(normalizeRectangle({x:90,y:80},{x:-10,y:20},100,100)).toEqual({x:0,y:20,w:90,h:60});
        expect(normalizeRectangle({x:0,y:0},{x:0,y:10},100,100)).toBeNull();
        expect(normalizeRectangle({x:NaN,y:0},{x:5,y:10},100,100)).toBeNull();
        expect(normalizeRectangle({x:110,y:110},{x:120,y:120},100,100)).toBeNull();
    });
    it('insertion, reading order, region deletion and page order preserve stable IDs',()=>{
        let p=recognize(project());
        const pageId=p.pages[0].id, original=p.pages[0].regions[0].id;
        p=addRegion(p,pageId,{x:50,y:50,w:100,h:100},'title');
        const added=p.pages[0].regions[1].id;
        p=moveRegion(p,pageId,added,-1);
        expect(p.pages[0].regions.map(r=>r.id)).toEqual([added,original]);
        p=deleteRegion(p,pageId,original);
        expect(p.pages[0].regions.map(r=>r.id)).toEqual([added]);
        p=movePage(p,pageId,1);
        expect(p.pages[1].id).toBe(pageId);
        p=removePage(p,pageId);
        expect(p.pages).toHaveLength(1);
        expect(()=>addRegion(p,p.pages[0].id,{x:990,y:0,w:50,h:20})).toThrow(/inside/);
    });
});

describe('find, replace and export',()=>{
    it('literal Unicode search retains original offsets and treats metacharacters literally',()=>{
        const p=recognize(project(1),0,'😀 Invoice A+B. invoice A+B. ÄPFEL');
        const matches=findMatches(p,'a+b.');
        expect(matches.map(m=>m.start)).toEqual([11,24]);
        expect(matches.map(m=>m.text)).toEqual(['A+B.','A+B.']);
        expect(findMatches(p,'äpfel')).toHaveLength(1);
        expect(findMatches(p,'invoice',{caseSensitive:true})).toHaveLength(1);
        expect(nextMatch(matches,matches[1])).toEqual(matches[0]);
        expect(nextMatch(matches,matches[0],-1)).toEqual(matches[1]);
    });
    it('replacement is literal even for dollar syntax and invalidates review atomically',()=>{
        let p=recognize(project(1),0,'A+B A+B');
        p=approvePage(p,p.pages[0].id);
        const next=replaceMatches(p,'A+B','$&');
        expect(next.pages[0].regions[0].text).toBe('$& $&');
        expect(next.pages[0].regions[0].originalText).toBe('A+B A+B');
        expect(next.pages[0].regions[0].review).toBe('corrected');
        expect(next.pages[0].approved).toBe(false);
        expect(p.pages[0].approved).toBe(true);
    });
    it('mixed pending/recognized projects cannot silently omit pages on export',()=>{
        const p=recognize(project());
        const preflight=prepareExport(p);
        expect(preflight.pages).toEqual([]);
        expect(preflight.text).toBe('');
        expect(preflight.errors).toHaveLength(1);
        const selected=prepareExport(p,{pageIds:[p.pages[0].id]});
        expect(selected.pages).toHaveLength(1);
        expect(selected.errors).toEqual([]);
        expect(prepareExport(p,{pageIds:['missing']}).errors.length).toBeGreaterThan(0);
    });
    it('exports selected page order with corrections, excluding picture zones',()=>{
        let p=recognize(project(),0,'first');
        p=recognize(p,1,'second');
        p=editRegion(p,p.pages[0].id,p.pages[0].regions[0].id,'corrected');
        p=addRegion(p,p.pages[0].id,{x:10,y:10,w:30,h:30},'picture');
        p=movePage(p,p.pages[1].id,-1);
        const out=prepareExport(p);
        expect(out.text).toBe('second\fcorrected');
        expect(out.pages[1].regions).toHaveLength(1);
        expect(out.pages[1].regions[0].char_conf).toEqual([]);
    });
    it('structured export retains page/source identities and native provenance',()=>{
        let p=recognize(project(1));
        p=approvePage(p,p.pages[0].id);
        const out=prepareExport(p).pages[0];
        expect(out.id).toBe(p.pages[0].id);
        expect(out.name).toBe(p.pages[0].name);
        expect(out.source_uri).toBe('/sources/invoice.pdf');
        expect(out.source_page).toBe(1);
        expect(out.approved).toBe(true);
        expect(out.provenance?.engine).toBe('surya');
        expect(out.provenance).toEqual(p.pages[0].provenance);
        expect(out.provenance).not.toBe(p.pages[0].provenance);
    });
    it('structured export preserves unavailable confidence and the original evidence identity',()=>{
        const p=recognize(project(1),0,'Unscored text',null);
        const region=p.pages[0].regions[0];
        const out=prepareExport(p).pages[0].regions[0];
        expect(out.id).toBe(region.id);
        expect(out.original_text).toBe('Unscored text');
        expect(out.confidence).toBeNull();
        expect(out.confidence_basis).toBe('recognition');
        expect(out.review).toBe('unreviewed');
        expect(out.kind).toBe('text');
        expect(out.char_conf).toEqual([]);
        expect(out.original_char_conf).toEqual([]);
    });
    it('structured export retains corrected text and its separately aligned original scores',()=>{
        let p=project(1);
        p=applyRecognition(p,p.pages[0].id,{width:1000,height:1400,regions:[{text:'Hi',x:10,y:10,w:50,h:30,confidence:0.6,confidence_basis:'recognition',char_conf:[0.3,0.9],char_conf_unit:'unicode_codepoint'}]});
        const before=prepareExport(p).pages[0].regions[0];
        expect(before.char_conf).toEqual([0.3,0.9]);
        expect(before.char_conf_aligned).toBe(true);
        expect(before.char_conf_unit).toBe('unicode_codepoint');
        p=editRegion(p,p.pages[0].id,p.pages[0].regions[0].id,'Hello');
        const after=prepareExport(p).pages[0].regions[0];
        expect(after.text).toBe('Hello');
        expect(after.original_text).toBe('Hi');
        expect(after.review).toBe('corrected');
        expect(after.char_conf).toEqual([]);
        expect(after.char_conf_aligned).toBe(false);
        expect(after.original_char_conf).toEqual([0.3,0.9]);
        expect(after.original_char_conf_aligned).toBe(true);
        expect(after.confidence).toBe(0.6);
    });
    it('structured export keeps token evidence separate from character scores',()=>{
        let p=project(1);
        p=applyRecognition(p,p.pages[0].id,{width:1000,height:1400,regions:[{text:'Hi',x:10,y:10,w:50,h:30,confidence:0.9,confidence_basis:'detection',char_conf:[0.8,0.9],char_conf_unit:'token'}]});
        const out=prepareExport(p).pages[0].regions[0];
        expect(out.confidence_basis).toBe('detection');
        expect(out.char_conf_unit).toBe('token');
        expect(out.char_conf).toEqual([]);
        expect(out.char_conf_aligned).toBe(false);
        expect(out.original_char_conf).toEqual([0.8,0.9]);
        expect(out.original_char_conf_aligned).toBe(false);
    });
    it('review policy blocks export and preserves approved blank pages',()=>{
        let p=project(1);
        p=applyRecognition(p,p.pages[0].id,{width:1000,height:1400,regions:[]});
        expect(prepareExport(p,{requireReviewed:true}).pages).toEqual([]);
        p=approvePage(p,p.pages[0].id);
        const out=prepareExport(p,{requireReviewed:true});
        expect(out.pages).toHaveLength(1);
        expect(out.pages[0].regions).toEqual([]);
        expect(out.warnings[0]).toContain('retained');
    });
});

describe('durable project recovery and history',()=>{
    it('rejects null document/page/region records with recovery errors rather than raw property errors',()=>{
        const p=recognize(project(1));
        for (const corrupt of [{...p,documents:[null]}, {...p,pages:[null]}, {...p,pages:[{...p.pages[0],regions:[null]}]}]) {
            expect(()=>parseProject(corrupt)).toThrow(/OCR/);
        }
        const restored=parseProject({...p,pages:[{...p.pages[0],regions:[{...p.pages[0].regions[0],charConfidenceUnit:'token',charConfidenceAligned:true,char_conf:Array.from(p.pages[0].regions[0].text).map(()=>0.1)}]}]});
        expect(restored.pages[0].regions[0].charConfidenceAligned).toBe(false);
    });
    it('round trips arbitrary native provenance and interrupted jobs become retryable',()=>{
        let p=recognize(project(1));
        p=updatePage(p,p.pages[0].id,{status:'running',provenance:{engine:'surya',diagnostics:{stages:[1,2]}}});
        const restored=parseProject(serializeProject(p));
        expect(restored.pages[0].status).toBe('pending');
        expect(restored.pages[0].provenance).toEqual(p.pages[0].provenance);
        expect(restored.pages[0].error).toContain('Interrupted');
        expect(p.pages[0].status).toBe('running');
    });
    it('rejects malformed versions, duplicate IDs, unsafe geometry and invalid scores',()=>{
        const p=recognize(project(1));
        expect(()=>parseProject({...p,version:999})).toThrow(/version/);
        expect(()=>parseProject({...p,pages:[p.pages[0],p.pages[0]]})).toThrow(/identifiers/);
        const corrupt=JSON.parse(JSON.stringify(p));
        corrupt.pages[0].regions[0].confidence=3;
        expect(()=>parseProject(corrupt)).toThrow(/region/);
        expect(()=>parseProject('{broken')).toThrow(/valid/);
    });
    it('history byte limit prunes snapshots independently of its entry count',()=>{
        let p=recognize(project(1));
        const pid=p.pages[0].id,rid=p.pages[0].regions[0].id;
        const singleSize=new TextEncoder().encode(JSON.stringify(p)).length;
        let history=createHistory(p,50,singleSize+100);
        for (const text of ['one','two','three','four']) history=commitHistory(history,editRegion(history.present,pid,rid,text));
        expect(history.past.length).toBeLessThanOrEqual(1);
        history=undoHistory(history);
        expect(history.present.pages[0].regions[0].text).toBe('three');
        history=redoHistory(history);
        expect(history.present.pages[0].regions[0].text).toBe('four');
        expect(commitHistory(createHistory(p,50,0),editRegion(p,pid,rid,'no history')).past).toEqual([]);
    });
    it('recognition settings persist for a reopened project',()=>{
        const p=project(1);
        p.recognitionSettings={engine:'surya',language:'deu',cleanup:{deskew:true}};
        expect(parseProject(serializeProject(p)).recognitionSettings).toEqual(p.recognitionSettings);
    });
    it('undo/redo is bounded, immutable and a new edit discards future history',()=>{
        let p=recognize(project(1));
        const pid=p.pages[0].id,rid=p.pages[0].regions[0].id;
        let history=createHistory(p,2);
        for (const text of ['one','two','three']) history=commitHistory(history,editRegion(history.present,pid,rid,text));
        expect(history.past).toHaveLength(2);
        history=undoHistory(history);
        expect(history.present.pages[0].regions[0].text).toBe('two');
        history=redoHistory(history);
        expect(history.present.pages[0].regions[0].text).toBe('three');
        history=undoHistory(history);
        history=commitHistory(history,editRegion(history.present,pid,rid,'alternative'));
        expect(history.future).toEqual([]);
        expect(p.pages[0].regions[0].text).toBe('Original text');
    });
});
