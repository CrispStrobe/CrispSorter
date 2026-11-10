import { describe, expect, it } from 'vitest';
import {
    deriveStructure, readPageStructure, reconcilePageStructure, projectStructureForRegions,
    structureSegments, type PageStructure, type StructurePage,
} from './structureData';

function page(text='😀 Cafe\u0301 alpha [1]'):StructurePage {
    return {id:'page',regions:[{id:'text',text,kind:'text'},{id:'figure',text:'',kind:'picture'},
        {id:'caption',text:'A figure',kind:'text'},{id:'note',text:'A note',kind:'text'}]};
}
function structured():StructurePage {
    const source=page(),structure=deriveStructure(source);
    structure.blocks[2].role='caption';structure.blocks[2].anchorBlockId=structure.blocks[1].id;
    structure.blocks[3].role='footnote';structure.blocks[0].noteReferences=[{start:15,end:18,targetBlockId:structure.blocks[3].id}];
    structure.blocks[0].runs=[{start:3,end:8,bold:true}];
    return {...source,structure:readPageStructure(structure,source)!};
}

describe('versioned structure boundary',()=>{
    it('derives deterministic bounded IDs and default roles without editing source text or requiring OCR scores',()=>{
        const source=page(),before=JSON.stringify(source),first=deriveStructure(source);
        expect(deriveStructure(source)).toEqual(first);expect(JSON.stringify(source)).toBe(before);
        expect(first.blocks.map(block=>block.sourceRegionId)).toEqual(source.regions.map(region=>region.id));
        expect(first.blocks.map(block=>block.role)).toEqual(['paragraph','figure','paragraph','paragraph']);
        expect(first.blocks.every(block=>!('text' in block)&&!('html' in block))).toBe(true);
        const unicode={id:'長'.repeat(250),regions:[{id:'識'.repeat(150),text:'Text',kind:'title' as const}]};
        const migrated=deriveStructure(unicode);expect(migrated.blocks[0].id.length).toBeLessThan(200);
        expect(migrated.sections[0].id.length).toBeLessThan(200);expect(migrated.blocks[0]).toMatchObject({role:'heading',headingLevel:1});
        expect(readPageStructure(undefined,source)).toBeUndefined();
        expect(deriveStructure({id:'empty',regions:[]})).toEqual({version:1,sections:[],blocks:[]});
    });

    it('rejects unknown versions, HTML copies, duplicate identities and mismatched authoritative order',()=>{
        const source=page(),baseline=deriveStructure(source);
        for(const raw of [{...baseline,version:2},{...baseline,html:'copied'},null,{...baseline,sections:[...baseline.sections,...baseline.sections]}])expect(()=>readPageStructure(raw,source)).toThrow(/structure/);
        for(const change of [(s:PageStructure)=>{s.blocks[1].id=s.blocks[0].id;},(s:PageStructure)=>{s.blocks.reverse();},
            (s:PageStructure)=>{s.blocks[0].sourceRegionId='missing';},(s:PageStructure)=>{(s.blocks[0] as unknown as Record<string,unknown>).text='copied';}]) {
            const value=structuredClone(baseline);change(value);expect(()=>readPageStructure(value,source)).toThrow();
        }
    });

    it('requires canonical marks on complete Unicode graphemes and accepts exact UTF16 offsets',()=>{
        const source=page(),baseline=deriveStructure(source);
        for(const runs of [[{start:1,end:2,bold:true}],[{start:3,end:7,bold:true}],
            [{start:3,end:6,bold:true},{start:6,end:8,bold:true}],[{start:3,end:8,bold:true},{start:6,end:10,italic:true}],
            [{start:3,end:8,bold:false}],[{start:3,end:8}]]) {
            const value=structuredClone(baseline);value.blocks[0].runs=runs as never;expect(()=>readPageStructure(value,source)).toThrow(/marks/);
        }
        const valid=structuredClone(baseline);valid.blocks[0].runs=[{start:0,end:2,italic:true},{start:3,end:8,bold:true}];
        expect(readPageStructure(valid,source)?.blocks[0].runs).toEqual(valid.blocks[0].runs);
    });

    it('validates role-specific fields and coarse region compatibility',()=>{
        const source=page(),baseline=deriveStructure(source);
        const changes=[(s:PageStructure)=>{s.blocks[0].role='figure';},(s:PageStructure)=>{s.blocks[1].role='paragraph';},
            (s:PageStructure)=>{s.blocks[0].headingLevel=2;},(s:PageStructure)=>{s.blocks[0].role='heading';},
            (s:PageStructure)=>{s.blocks[0].role='list-item';s.blocks[0].list={kind:'bullet',level:0,start:1};},
            (s:PageStructure)=>{s.blocks[0].role='list-item';s.blocks[0].list={kind:'number',level:6};},
            (s:PageStructure)=>{s.blocks[0].altText='misplaced';},(s:PageStructure)=>{s.blocks[1].noteReferences=[];}];
        for(const change of changes){const value=structuredClone(baseline);change(value);expect(()=>readPageStructure(value,source)).toThrow();}
        const list=structuredClone(baseline);list.blocks[0].role='list-item';list.blocks[0].list={kind:'number',level:5,start:1000000};
        list.blocks[1].altText='A diagram';list.blocks[1].decorative=false;
        expect(readPageStructure(list,source)).toEqual(list);
    });

    it('requires contiguous sections and a nonempty monotonic two-column partition',()=>{
        const source=page(),baseline=deriveStructure(source);
        const valid=structuredClone(baseline);valid.sections[0].columns=2;valid.blocks[2].column=2;valid.blocks[3].column=2;
        expect(readPageStructure(valid,source)).toEqual(valid);
        for(const columns of [[1,2,1,2],[1,1,1,1],[2,2,2,2]]) {
            const value=structuredClone(valid);value.blocks.forEach((block,index)=>block.column=columns[index] as 1|2);
            expect(()=>readPageStructure(value,source)).toThrow(/column/);
        }
        const interleaved=structuredClone(baseline);interleaved.sections.push({id:'second',columns:1});
        interleaved.blocks[1].sectionId='second';expect(()=>readPageStructure(interleaved,source)).toThrow(/contiguous/);
        const empty=structuredClone(baseline);empty.sections.push({id:'empty',columns:1});expect(()=>readPageStructure(empty,source)).toThrow(/empty sections/);
    });

    it('rejects dangling, wrong-role, overlapping or cyclic note/caption links',()=>{
        const source=structured(),baseline=source.structure!;
        for(const change of [(s:PageStructure)=>{s.blocks[2].anchorBlockId='absent';},
            (s:PageStructure)=>{s.blocks[2].anchorBlockId=s.blocks[0].id;},
            (s:PageStructure)=>{s.blocks[0].noteReferences![0].targetBlockId=s.blocks[1].id;},
            (s:PageStructure)=>{s.blocks[0].noteReferences!.push({...s.blocks[0].noteReferences![0]});},
            (s:PageStructure)=>{s.blocks[3].noteReferences=[{start:0,end:1,targetBlockId:s.blocks[3].id}];},
            (s:PageStructure)=>{s.blocks[0].noteReferences=[];}]) {
            const value=structuredClone(baseline);change(value);expect(()=>readPageStructure(value,source)).toThrow();
        }
        const cycle=structuredClone(baseline);cycle.blocks[0].role='footnote';cycle.blocks[3].noteReferences=[{start:0,end:1,targetBlockId:cycle.blocks[0].id}];
        expect(()=>readPageStructure(cycle,source)).toThrow(/cycle/);
    });

    it('bounds imported structure and clones nested metadata',()=>{
        const source=structured(),baseline=source.structure!;
        const big=structuredClone(baseline);big.blocks[1].altText='x'.repeat(8*1024*1024);
        expect(()=>readPageStructure(big,source)).toThrow(/8 MB/);
        const many=structuredClone(baseline);many.blocks[0].runs=Array.from({length:10001},()=>({start:3,end:8,bold:true}));
        expect(()=>readPageStructure(many,source)).toThrow();
        const read=readPageStructure(baseline,source)!;read.blocks[0].runs[0].start=0;
        expect(baseline.blocks[0].runs[0].start).toBe(3);
    });
});

describe('structure reconciliation and projection',()=>{
    it('keeps legacy structure absent and preserves marked prefix/suffix around plain inserted text',()=>{
        const plain=page();expect(reconcilePageStructure(plain,{...plain,regions:[]}).structure).toBeUndefined();
        const source=structured(),next={...source,regions:source.regions.map((region,index)=>index?region:{...region,text:'😀 Cafe\u0301 NEW alpha [1]'})};
        source.structure!.blocks[0].runs=[{start:3,end:14,bold:true}];
        const result=reconcilePageStructure(source,next),block=result.structure!.blocks[0];
        expect(block.runs).toEqual([{start:3,end:9,bold:true},{start:13,end:18,bold:true}]);
        expect(block.noteReferences).toEqual([{start:19,end:22,targetBlockId:source.structure!.blocks[3].id}]);
        expect(structureSegments(next.regions[0].text,block).find(segment=>segment.text==='NEW ')).toMatchObject({start:9,end:13});
        expect(structureSegments(next.regions[0].text,block).find(segment=>segment.text==='NEW ')?.bold).toBeUndefined();
        expect(source.structure!.blocks[0].runs).toEqual([{start:3,end:14,bold:true}]);
    });

    it('drops changed grapheme marks and intersected note references without inventing alignment',()=>{
        for(const [oldText,newText,end] of [['e\u0301x','e\u0300x',2],['😀x','😁x',2],['🇩🇪x','🇩🇫x',4]] as const) {
            const source:StructurePage={id:'p',regions:[{id:'r',kind:'text',text:oldText}]};source.structure=deriveStructure(source);source.structure.blocks[0].runs=[{start:0,end,bold:true}];
            const result=reconcilePageStructure(source,{...source,regions:[{...source.regions[0],text:newText}]});
            expect(result.structure?.blocks[0].runs).toEqual([]);
        }
        const source=structured(),next={...source,regions:source.regions.map((region,index)=>index?region:{...region,text:region.text.replace('[1]','[2]')})};
        const result=reconcilePageStructure(source,next);expect(result.structure!.blocks[0].noteReferences).toBeUndefined();
        expect(result.notices.some(notice=>notice.includes('note reference'))).toBe(true);
    });

    it('resets replacement IDs to defaults and prunes links to missing sources',()=>{
        const source=structured(),next={...source,regions:source.regions.filter(region=>region.id!=='figure').map(region=>region.id==='note'?{...region,id:'new-note'}:region)};
        const result=reconcilePageStructure(source,next);
        expect(result.structure?.blocks.map(block=>block.sourceRegionId)).toEqual(next.regions.map(region=>region.id));
        expect(result.structure!.blocks.find(block=>block.sourceRegionId==='new-note')).toMatchObject({role:'paragraph',runs:[]});
        expect(result.structure!.blocks[0].noteReferences).toBeUndefined();
        expect(result.structure!.blocks.find(block=>block.role==='caption')?.anchorBlockId).toBeUndefined();
        expect(result.notices.some(notice=>notice.includes('default roles'))).toBe(true);
    });

    it('regroups reordered columns into valid contiguous sections without reordering text',()=>{
        const source=structured();source.structure!.sections[0].columns=2;source.structure!.blocks[2].column=2;source.structure!.blocks[3].column=2;
        const next={...source,regions:[source.regions[0],source.regions[2],source.regions[1],source.regions[3]]};
        const result=reconcilePageStructure(source,next);
        expect(result.structure!.blocks.map(block=>block.sourceRegionId)).toEqual(['text','caption','figure','note']);
        expect(result.structure!.sections).toHaveLength(2);
        expect(result.notices.some(notice=>notice.includes('regrouped'))).toBe(true);
        expect(readPageStructure(result.structure,next)).toEqual(result.structure);
    });

    it('projects an included subset, preserving figure anchors and explicitly dropping excluded note links',()=>{
        const source=structured(),regions=source.regions.filter(region=>region.id!=='note');
        const result=projectStructureForRegions(source.structure!,regions,source.id);
        expect(result.structure!.blocks).toHaveLength(3);
        expect(result.structure!.blocks[2].anchorBlockId).toBe(result.structure!.blocks[1].id);
        expect(result.structure!.blocks[0].noteReferences).toBeUndefined();
        expect(result.structure!.blocks[0].runs).toEqual(source.structure!.blocks[0].runs);
        expect(result.notices.join(' ')).toMatch(/outside the export/);
        expect(source.structure!.blocks[0].noteReferences).toHaveLength(1);
    });

    it('resets incompatible semantics after a coarse-kind change and supports an empty destination',()=>{
        const source=structured(),next={...source,regions:source.regions.map(region=>region.id==='text'?{...region,kind:'picture' as const}:region)};
        const result=reconcilePageStructure(source,next);
        expect(result.structure!.blocks[0]).toMatchObject({role:'figure',runs:[]});
        expect(result.structure!.blocks[0].noteReferences).toBeUndefined();
        expect(result.notices.some(notice=>notice.includes('source kinds'))).toBe(true);
        expect(reconcilePageStructure(source,{id:source.id,regions:[]}).structure).toEqual({version:1,sections:[],blocks:[]});
    });
});
