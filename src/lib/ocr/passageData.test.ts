import { describe, expect, it } from 'vitest';
import { addDocument, applyRecognition, createProject, type OcrRegion } from './model';
import { collectSourceComponents, mergeSourceComponents, readSharedSourceGroups, readSourceComponents, validatePageSourceComponents, withSourceComponents } from './passageData';

function fixture() {
    let project=addDocument(createProject(),{source_uri:'/original.png',name:'Source',pages:[{image_path:'/working.png',width:300,height:400}]});
    project=applyRecognition(project,project.pages[0].id,{width:300,height:400,regions:[{
        text:'😀 Hi',x:10,y:20,w:100,h:40,confidence_basis:'recognition',confidence:0.7,char_conf:[0.8,0.7,0.6,0.5],char_conf_unit:'unicode_codepoint',char_conf_aligned:true,
        word_evidence:{version:1,offset_unit:'utf16',source:'fixture',words:[{text:'Hi',start:3,end:5,x:40,y:25,w:30,h:15,confidence:0.6,confidence_basis:'recognition'}]},
    }]});
    return {project,page:project.pages[0],region:project.pages[0].regions[0]};
}

describe('durable flat passage source history',()=>{
    it('captures raw source text/evidence and separately labels current text/coverage',()=>{
        const {page,region}=fixture();
        region.text='😀 Hello';region.x=80;region.w=100;
        const [source]=collectSourceComponents(page,[region]);
        expect(source).toMatchObject({originalText:'😀 Hi',textAtCapture:'😀 Hello',boundsAtCapture:{x:80,y:20,w:100,h:40},imagePath:'/working.png'});
        // Original word evidence need not fit a later manually adjusted box.
        expect(source.wordEvidence?.words[0].x).toBe(40);
        region.char_conf[0]=0;
        expect(source.char_conf[0]).toBe(0.8);
        expect(readSourceComponents([source])).toEqual([source]);
    });

    it('deduplicates repeated originals without recursive manual snapshots and rejects conflicting identities',()=>{
        const {page,region}=fixture(),components=collectSourceComponents(page,[region]);
        const left={...region,id:'left',sourceComponents:components},right={...region,id:'right',sourceComponents:structuredClone(components)};
        expect(collectSourceComponents(page,[left,right])).toEqual(components);
        expect(mergeSourceComponents(components,components)).toEqual(components);
        const changed=structuredClone(components);changed[0].textAtCapture='different';
        expect(()=>mergeSourceComponents(components,changed)).toThrow(/conflicting/);
        expect(()=>readSourceComponents([{...components[0],sourceComponents:components}])).toThrow(/metadata/);
    });

    it('retains old-image components while adding only the new recognition result as current evidence',()=>{
        const {page,region}=fixture(),old=collectSourceComponents(page,[region]);
        const nextPage={...page,imagePath:'/prepared.png',width:600,height:800};
        const nextRegion:OcrRegion={...region,id:'new-native',text:'New OCR',originalText:'New OCR',wordEvidence:undefined,char_conf:[],charConfidenceAligned:false};
        const next=withSourceComponents(nextPage,nextRegion,old);
        expect(next.sourceComponents).toHaveLength(2);
        expect(next.sourceComponents?.map(component=>component.imagePath)).toEqual(['/working.png','/prepared.png']);
        expect(next.wordEvidence).toBeUndefined();expect(next.confidence).toBe(0.7);
        expect(readSourceComponents(next.sourceComponents)).toEqual(next.sourceComponents);
        expect(collectSourceComponents(nextPage,[next])).toEqual(next.sourceComponents);
        expect(withSourceComponents(nextPage,next,old).sourceComponents).toEqual(next.sourceComponents);
    });

    it('validates source dimensions, geometry, offsets, confidence, group IDs and strict flat metadata',()=>{
        const {page,region}=fixture(),source=collectSourceComponents(page,[region])[0];
        expect(readSourceComponents(undefined)).toBeUndefined();
        for(const patch of [{version:2},{width:1.5},{width:100001},{width:100000,height:100000},{confidence:NaN},
            {boundsAtCapture:{x:0,y:0,w:301,h:2}},{char_conf:[0.3],charConfidenceAligned:true},{imagePath:'bad\0path'}]) {
            expect(()=>readSourceComponents([{...source,...patch}])).toThrow(/source history/);
        }
        const bad=structuredClone(source);bad.wordEvidence!.words[0].start=1;
        expect(()=>readSourceComponents([bad])).toThrow(/word evidence/);
        expect(()=>readSourceComponents([source,source])).toThrow(/metadata/);
        expect(()=>readSourceComponents(null)).toThrow(/flat/);
        expect(readSharedSourceGroups(['one','two'])).toEqual(['one','two']);
        expect(()=>readSharedSourceGroups(['one','one'])).toThrow(/groups/);
        expect(()=>readSharedSourceGroups(Array.from({length:129},(_,i)=>String(i)))).toThrow(/groups/);
    });

    it('enforces component and serialized-size limits without dropping old history',()=>{
        const {page,region}=fixture(),source=collectSourceComponents(page,[region])[0],before=JSON.stringify(source);
        const many=Array.from({length:129},(_,i)=>({...region,id:`region-${i}`}));
        expect(()=>collectSourceComponents(page,many)).toThrow(/128/);
        expect(()=>readSourceComponents([{...source,textAtCapture:'x'.repeat(8*1024*1024)}])).toThrow(/8 MB/);
        expect(JSON.stringify(source)).toBe(before);
    });

    it('validates archived sources independently and counts duplicated snapshots toward the page limit',()=>{
        const {page,region}=fixture(),source=collectSourceComponents(page,[region])[0];
        const large={...source,textAtCapture:'x'.repeat(6*1024*1024)};
        const archived={...page,provenance:{passageSources:[large]},regions:[{...region,sourceComponents:[large]}]};
        expect(()=>validatePageSourceComponents(archived)).not.toThrow();
        expect(()=>validatePageSourceComponents({...archived,regions:[...archived.regions,{...region,id:'second',sourceComponents:[large]}]})).toThrow(/16 MB/);
        expect(()=>validatePageSourceComponents({...page,provenance:{passageSources:[{...source,width:0}]}})).toThrow(/metadata/);
    });
});
