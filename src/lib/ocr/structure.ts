import { updatePage, type OcrPage, type OcrProject } from './model';
import {
    canonicalInlineRuns, deriveStructure, isStructureRange, readPageStructure, TEXT_STRUCTURE_ROLES,
    type InlineMarks, type InlineRun, type PageStructure, type StructureBlock, type TextRange,
} from './structureData';
export * from './structureData';

export type BlockProperties=Partial<Pick<StructureBlock,'role'|'headingLevel'|'list'|'alignment'|'breakBefore'|'altText'|'decorative'>>;
export type InlineMarkChanges=Partial<Record<'bold'|'italic'|'underline',boolean>>;
const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const fail=(message:string):never=>{throw new Error(`${message} Existing text and structure were preserved.`);};
const propertyKeys=new Set(['role','headingLevel','list','alignment','breakBefore','altText','decorative']);

function context(project:OcrProject,pageId:string):{page:OcrPage;structure:PageStructure} {
    const page=project.pages.find(page=>page.id===pageId);
    if(!page)return fail('The source page no longer exists.');
    if(page.excluded||page.status==='running')return fail('Include the page and wait for recognition before editing structure.');
    return {page,structure:deriveStructure(page)};
}
function blockFor(structure:PageStructure,blockId:string):StructureBlock {
    const block=structure.blocks.find(block=>block.id===blockId);return block??fail('The selected structure block no longer exists.');
}
function cleanLinks(structure:PageStructure):{structure:PageStructure;notices:string[]} {
    const next=clone(structure),roles=new Map(next.blocks.map(block=>[block.id,block.role]));let count=0;
    for(const block of next.blocks) {
        if(block.anchorBlockId!==undefined&&!['figure','table'].includes(roles.get(block.anchorBlockId)??'')){delete block.anchorBlockId;count++;}
        const references=(block.noteReferences??[]).filter(reference=>roles.get(reference.targetBlockId)==='footnote');
        count+=(block.noteReferences?.length??0)-references.length;
        delete block.noteReferences;if(references.length)block.noteReferences=references;
    }
    return {structure:next,notices:count?[`${count} caption or footnote link${count===1?' was':'s were'} removed because the target role changed.`]:[]};
}
function commit(project:OcrProject,page:OcrPage,structure:PageStructure,notices:string[]=[]):OcrProject {
    const checked=readPageStructure(structure,page)!;
    if(page.structure&&JSON.stringify(page.structure)===JSON.stringify(checked))return project;
    const before=deriveStructure(page);
    if(!page.structure&&JSON.stringify(before)===JSON.stringify(checked))return updatePage(project,page.id,{structure:checked});
    const oldBlocks=new Map(before.blocks.map(block=>[block.sourceRegionId,block]));
    const oldSections=new Map(before.sections.map(section=>[section.id,section.columns]));
    const nextSections=new Map(checked.sections.map(section=>[section.id,section.columns]));
    const affected=new Set(checked.blocks.filter(block=>JSON.stringify(block)!==JSON.stringify(oldBlocks.get(block.sourceRegionId))||
        oldSections.get(block.sectionId)!==nextSections.get(block.sectionId)).map(block=>block.sourceRegionId));
    const regions=page.regions.map(region=>affected.has(region.id)?{...region,review:region.confidenceBasis==='manual'||region.text!==region.originalText?'corrected' as const:'unreviewed' as const}:region);
    return updatePage(project,page.id,{structure:checked,regions,approved:false,
        provenance:{...page.provenance,structureNotices:[...notices],structureChangedAt:new Date().toISOString()}});
}

export function initializePageStructure(project:OcrProject,pageId:string):OcrProject {
    const {page,structure}=context(project,pageId);return page.structure?project:updatePage(project,page.id,{structure});
}

export function setBlockProperties(project:OcrProject,pageId:string,blockId:string,patch:BlockProperties):OcrProject {
    const {page,structure}=context(project,pageId),source=blockFor(structure,blockId);
    if(!patch||typeof patch!=='object'||Array.isArray(patch)||Object.keys(patch).some(key=>!propertyKeys.has(key)))return fail('Choose supported block properties.');
    const next={...source,...clone(patch)};
    if(next.role!==source.role) {
        delete next.headingLevel;delete next.list;delete next.anchorBlockId;delete next.altText;delete next.decorative;
        if(next.role==='heading')next.headingLevel=patch.headingLevel??1;
        if(next.role==='list-item')next.list=patch.list?{...patch.list}:{kind:'bullet',level:0};
        if(next.role==='figure') {
            if(patch.altText!==undefined)next.altText=patch.altText;
            if(patch.decorative!==undefined)next.decorative=patch.decorative;
        }
        if(!TEXT_STRUCTURE_ROLES.has(next.role)){next.runs=[];delete next.noteReferences;}
    }
    const cleaned=cleanLinks({...structure,blocks:structure.blocks.map(block=>block.id===blockId?next:block)});
    return commit(project,page,cleaned.structure,cleaned.notices);
}

export function setInlineMarks(project:OcrProject,pageId:string,blockId:string,range:TextRange,marks:InlineMarkChanges):OcrProject {
    const {page,structure}=context(project,pageId),block=blockFor(structure,blockId),region=page.regions.find(region=>region.id===block.sourceRegionId)!;
    if(!TEXT_STRUCTURE_ROLES.has(block.role)||!isStructureRange(region.text,range))return fail('Select complete characters in a text block before formatting.');
    const changes=Object.keys(marks??{});
    if(!changes.length||changes.some(key=>!['bold','italic','underline'].includes(key)||typeof marks[key as keyof InlineMarkChanges]!=='boolean'))return fail('Choose a supported inline mark.');
    const points=[...new Set([0,region.text.length,range.start,range.end,...block.runs.flatMap(run=>[run.start,run.end])])].sort((a,b)=>a-b);
    const runs:InlineRun[]=[];let runIndex=0;
    for(let index=0;index<points.length-1;index++) {
        const start=points[index],end=points[index+1];
        while(runIndex<block.runs.length&&block.runs[runIndex].end<=start)runIndex++;
        const current=block.runs[runIndex],inside=current&&current.start<=start&&current.end>=end;
        const next:InlineMarks=inside?{...(current.bold?{bold:true}:{}),...(current.italic?{italic:true}:{}),...(current.underline?{underline:true}:{})}:{};
        if(start>=range.start&&end<=range.end)for(const key of changes as (keyof InlineMarks)[]) {
            if(marks[key])next[key]=true;else delete next[key];
        }
        runs.push({start,end,...next});
    }
    return commit(project,page,{...structure,blocks:structure.blocks.map(candidate=>candidate.id===blockId?{...candidate,runs:canonicalInlineRuns(runs)}:candidate)});
}
export function clearInlineMarks(project:OcrProject,pageId:string,blockId:string,range:TextRange):OcrProject {
    return setInlineMarks(project,pageId,blockId,range,{bold:false,italic:false,underline:false});
}

function sectionIdentity(structure:PageStructure):string {
    const existing=new Set([...structure.sections.map(section=>section.id),...structure.blocks.map(block=>block.id)]);
    let value:string;do{value=`section-${globalThis.crypto?.randomUUID?.()??`${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`}`;}while(existing.has(value));
    return value;
}
export function startSection(project:OcrProject,pageId:string,blockId:string):OcrProject {
    const {page,structure}=context(project,pageId),selected=blockFor(structure,blockId);
    const members=structure.blocks.filter(block=>block.sectionId===selected.sectionId),boundary=members.findIndex(block=>block.id===blockId);
    if(boundary===0)return project;
    const newId=sectionIdentity(structure),rightIds=new Set(members.slice(boundary).map(block=>block.id));
    const next=clone(structure);for(const block of next.blocks)if(rightIds.has(block.id))block.sectionId=newId;
    const index=next.sections.findIndex(section=>section.id===selected.sectionId);next.sections.splice(index+1,0,{id:newId,columns:1});
    for(const section of next.sections.filter(section=>section.id===selected.sectionId||section.id===newId)) {
        const blocks=next.blocks.filter(block=>block.sectionId===section.id),both=blocks.some(block=>block.column===1)&&blocks.some(block=>block.column===2);
        section.columns=both?2:1;if(!both)for(const block of blocks)block.column=1;
    }
    return commit(project,page,next);
}
export function setSectionColumns(project:OcrProject,pageId:string,sectionId:string,secondColumnStartBlockId:string|null):OcrProject {
    const {page,structure}=context(project,pageId),next=clone(structure),section=next.sections.find(section=>section.id===sectionId);
    if(!section)return fail('The selected section no longer exists.');
    const members=next.blocks.filter(block=>block.sectionId===sectionId);
    if(secondColumnStartBlockId===null){section.columns=1;for(const block of members)block.column=1;}
    else {
        const boundary=members.findIndex(block=>block.id===secondColumnStartBlockId);
        if(boundary<1)return fail('Choose a second-column boundary after the first block in this section.');
        section.columns=2;members.forEach((block,index)=>block.column=index<boundary?1:2);
    }
    return commit(project,page,next);
}

export function setCaptionAnchor(project:OcrProject,pageId:string,captionBlockId:string,targetBlockId:string|null):OcrProject {
    const {page,structure}=context(project,pageId),block=blockFor(structure,captionBlockId);
    if(block.role!=='caption')return fail('Choose a caption block before attaching it to a figure or table.');
    const next={...block};delete next.anchorBlockId;
    if(targetBlockId!==null) {
        const target=blockFor(structure,targetBlockId);
        if(!['figure','table'].includes(target.role))return fail('A caption can attach only to a figure or table on this page.');
        next.anchorBlockId=target.id;
    }
    return commit(project,page,{...structure,blocks:structure.blocks.map(block=>block.id===captionBlockId?next:block)});
}

export function setFootnoteReference(project:OcrProject,pageId:string,sourceBlockId:string,range:TextRange,targetFootnoteBlockId:string|null):OcrProject {
    const {page,structure}=context(project,pageId),block=blockFor(structure,sourceBlockId),region=page.regions.find(region=>region.id===block.sourceRegionId)!;
    if(!TEXT_STRUCTURE_ROLES.has(block.role)||!isStructureRange(region.text,range)||!region.text.slice(range.start,range.end).trim())return fail('Select the exact visible note marker in a text block.');
    const references=(block.noteReferences??[]).filter(reference=>reference.start!==range.start||reference.end!==range.end);
    if(targetFootnoteBlockId!==null) {
        const target=blockFor(structure,targetFootnoteBlockId);
        if(target.role!=='footnote')return fail('Choose a footnote block on this page.');
        if(references.some(reference=>reference.start<range.end&&reference.end>range.start))return fail('Remove an overlapping note reference before creating this one.');
        references.push({...range,targetBlockId:target.id});references.sort((a,b)=>a.start-b.start);
    }
    const next={...block};delete next.noteReferences;if(references.length)next.noteReferences=references;
    return commit(project,page,{...structure,blocks:structure.blocks.map(block=>block.id===sourceBlockId?next:block)});
}
