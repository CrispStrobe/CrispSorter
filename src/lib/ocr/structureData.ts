import type { OcrPage, OcrRegion } from './model';

export type StructureRole = 'paragraph'|'heading'|'list-item'|'figure'|'caption'|'table'|'formula'|'header'|'footer'|'footnote';
export interface TextRange { start:number; end:number; }
export interface InlineMarks { bold?:true; italic?:true; underline?:true; }
export interface InlineRun extends TextRange, InlineMarks {}
export interface NoteReference extends TextRange { targetBlockId:string; }
export interface StructureSection { id:string; columns:1|2; }
export interface StructureBlock {
    id:string; sourceRegionId:string; sectionId:string; column:1|2; role:StructureRole;
    headingLevel?:1|2|3|4|5|6;
    list?:{kind:'bullet'|'number';level:number;start?:number};
    alignment:'start'|'center'|'end'|'justify'; breakBefore:'none'|'page'; runs:InlineRun[];
    anchorBlockId?:string; noteReferences?:NoteReference[]; altText?:string; decorative?:boolean;
}
export interface PageStructure { version:1; sections:StructureSection[]; blocks:StructureBlock[]; }
export type StructureRegion = Pick<OcrRegion,'id'|'text'|'kind'>;
export type StructurePage = Pick<OcrPage,'id'> & {regions:readonly StructureRegion[];structure?:PageStructure};
export interface ReconcileResult { structure:PageStructure|undefined; notices:string[]; }
export interface StructureSegment extends TextRange, InlineMarks { text:string; noteTargetBlockId?:string; }
export const MAX_STRUCTURE_BYTES=8*1024*1024;
export const MAX_STRUCTURE_SECTIONS=10000;
export const MAX_STRUCTURE_BLOCKS=100000;
export const MAX_STRUCTURE_RUNS=100000;
export const MAX_STRUCTURE_REFERENCES=10000;
export const TEXT_STRUCTURE_ROLES=new Set<StructureRole>(['paragraph','heading','list-item','caption','header','footer','footnote']);
const roles=new Set<StructureRole>([...TEXT_STRUCTURE_ROLES,'figure','table','formula']);
const blockFields=new Set(['id','sourceRegionId','sectionId','column','role','headingLevel','list','alignment','breakBefore','runs','anchorBlockId','noteReferences','altText','decorative']);
const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const object=(value:unknown):value is Record<string,unknown>=>!!value && typeof value==='object' && !Array.isArray(value);
const id=(value:unknown):value is string=>typeof value==='string' && !!value.trim() && value.length<=200 && !value.includes('\0');
const fail=(message:string):never=>{throw new Error(`Invalid document structure: ${message}. Existing text and structure were preserved.`);};
const keys=(value:Record<string,unknown>,allowed:readonly string[]|Set<string>):boolean=>Object.keys(value).every(key=>allowed instanceof Set?allowed.has(key):allowed.includes(key));
const markKey=(marks:InlineMarks):string=>`${marks.bold?'b':''}${marks.italic?'i':''}${marks.underline?'u':''}`;
const segmenter=typeof Intl.Segmenter==='function'?new Intl.Segmenter(undefined,{granularity:'grapheme'}):null;

/** Exact editing offsets; an old webview may safely format a whole Unicode string. */
export function structureTextBoundaries(text:string):Set<number> {
    const boundaries=new Set<number>([0,text.length]);
    if(segmenter) for(const part of segmenter.segment(text)) boundaries.add(part.index);
    else for(let index=1;index<text.length;index++) {
        const left=text.charCodeAt(index-1),right=text.charCodeAt(index);
        if(left<=127 && right<=127 && !(left===13 && right===10)) boundaries.add(index);
    }
    return boundaries;
}
export function isStructureRange(text:string,range:TextRange):boolean {
    if(!Number.isInteger(range.start)||!Number.isInteger(range.end)||range.start<0||range.end<=range.start||range.end>text.length)return false;
    const boundaries=structureTextBoundaries(text);return boundaries.has(range.start)&&boundaries.has(range.end);
}
export function roleMatchesRegion(role:StructureRole,region:Pick<OcrRegion,'kind'>):boolean {
    if(region.kind==='picture')return role==='figure';
    if(region.kind==='table')return role==='table';
    if(region.kind==='formula')return role==='formula';
    return TEXT_STRUCTURE_ROLES.has(role);
}
export function defaultStructureRole(region:Pick<OcrRegion,'kind'>):StructureRole {
    return region.kind==='picture'?'figure':region.kind==='table'?'table':region.kind==='formula'?'formula':
        region.kind==='title'?'heading':region.kind==='header'?'header':region.kind==='footer'?'footer':'paragraph';
}
function stableId(prefix:string,source:string):string {
    if(source.length<=170 && /^[A-Za-z0-9_-]+$/u.test(source))return `${prefix}-${source}`;
    let a=2166136261,b=2246822519;
    for(let index=0;index<source.length;index++){const value=source.charCodeAt(index);a=Math.imul(a^value,16777619);b=Math.imul(b^value,3266489917);}
    return `${prefix}-${(a>>>0).toString(36)}-${(b>>>0).toString(36)}`;
}
export function defaultStructureBlock(region:StructureRegion,sectionId:string):StructureBlock {
    const role=defaultStructureRole(region);
    return {id:stableId('block',region.id),sourceRegionId:region.id,sectionId,column:1,role,
        ...(role==='heading'?{headingLevel:1 as const}:{}),alignment:'start',breakBefore:'none',runs:[]};
}

/** Strict, independently versioned semantics. Text and order remain in regions. */
export function readPageStructure(raw:unknown,page:StructurePage):PageStructure|undefined {
    if(raw===undefined)return undefined;
    if(!object(raw)||!keys(raw,['version','sections','blocks'])||raw.version!==1||!Array.isArray(raw.sections)||!Array.isArray(raw.blocks)||
        raw.sections.length>MAX_STRUCTURE_SECTIONS||raw.blocks.length>MAX_STRUCTURE_BLOCKS||raw.blocks.length!==page.regions.length)fail('unsupported version, limits, or source coverage');
    let bytes:number;try{bytes=new TextEncoder().encode(JSON.stringify(raw)).byteLength;}catch{return fail('metadata must be serializable');}
    if(bytes>MAX_STRUCTURE_BYTES)fail('metadata exceeds 8 MB');
    const value=raw as unknown as PageStructure,identities=new Set<string>();
    const identity=(candidate:unknown):boolean=>{if(!id(candidate)||identities.has(candidate))return false;identities.add(candidate);return true;};
    const sections=value.sections.map(section=>{
        if(!object(section)||!keys(section,['id','columns'])||!identity(section.id)||![1,2].includes(section.columns))fail('invalid or duplicate section');
        return {id:section.id,columns:section.columns};
    });
    const sectionMap=new Map(sections.map(section=>[section.id,section]));
    let runCount=0,referenceCount=0;
    const blocks=value.blocks.map((block,index):StructureBlock=>{
        const region=page.regions[index];
        if(!object(block)||!keys(block,blockFields)||!identity(block.id)||!id(block.sourceRegionId)||block.sourceRegionId!==region.id||
            !id(block.sectionId)||!sectionMap.has(block.sectionId)||![1,2].includes(block.column)||!roles.has(block.role)||!roleMatchesRegion(block.role,region)||
            !['start','center','end','justify'].includes(block.alignment)||!['none','page'].includes(block.breakBefore)||!Array.isArray(block.runs)||block.runs.length>10000)fail('invalid block or source order');
        if(block.role==='heading' ? !Number.isInteger(block.headingLevel)||block.headingLevel!<1||block.headingLevel!>6 : block.headingLevel!==undefined)fail('heading level belongs only to headings');
        if(block.role==='list-item') {
            const list=block.list;
            if(!object(list)||!keys(list,['kind','level','start'])||!['bullet','number'].includes(list.kind)||!Number.isInteger(list.level)||list.level<0||list.level>5||
                (list.start!==undefined && (list.kind!=='number'||!Number.isInteger(list.start)||list.start<1||list.start>1000000)))fail('invalid list settings');
        } else if(block.list!==undefined)fail('list settings belong only to list items');
        if(block.anchorBlockId!==undefined && (block.role!=='caption'||!id(block.anchorBlockId)))fail('only captions can anchor to a figure or table');
        if(block.altText!==undefined && (block.role!=='figure'||typeof block.altText!=='string'||block.altText.length>10000||block.altText.includes('\0')))fail('invalid figure alternative text');
        if(block.decorative!==undefined && (block.role!=='figure'||typeof block.decorative!=='boolean'))fail('invalid decorative figure flag');
        if(!TEXT_STRUCTURE_ROLES.has(block.role) && (block.runs.length||block.noteReferences!==undefined))fail('this block kind does not support inline marks or note references');
        const boundaries=block.runs.length||(block.noteReferences?.length??0)?structureTextBoundaries(region.text):new Set<number>();
        const range=(entry:TextRange):boolean=>Number.isInteger(entry.start)&&Number.isInteger(entry.end)&&entry.start>=0&&entry.end>entry.start&&entry.end<=region.text.length&&boundaries.has(entry.start)&&boundaries.has(entry.end);
        let priorEnd=0,prior:InlineRun|undefined;
        const runs=block.runs.map(run=>{
            if(!object(run)||!keys(run,['start','end','bold','italic','underline'])||!range(run)||run.start<priorEnd||
                [run.bold,run.italic,run.underline].some(mark=>mark!==undefined&&mark!==true)||!markKey(run)||
                (prior&&prior.end===run.start&&markKey(prior)===markKey(run)))fail('inline marks must be canonical, nonoverlapping, and on complete characters');
            priorEnd=run.end;prior=run;return {...run};
        });
        runCount+=runs.length;if(runCount>MAX_STRUCTURE_RUNS)fail('too many inline marks');
        let noteReferences:NoteReference[]|undefined;
        if(block.noteReferences!==undefined) {
            if(!Array.isArray(block.noteReferences)||!block.noteReferences.length||block.noteReferences.length>1000)fail('invalid note reference count');
            priorEnd=0;noteReferences=block.noteReferences.map(reference=>{
                if(!object(reference)||!keys(reference,['start','end','targetBlockId'])||!range(reference)||reference.start<priorEnd||
                    !region.text.slice(reference.start,reference.end).trim()||!id(reference.targetBlockId))fail('note references need exact, nonoverlapping text ranges');
                priorEnd=reference.end;return {...reference};
            });
            referenceCount+=noteReferences.length;if(referenceCount>MAX_STRUCTURE_REFERENCES)fail('too many note references');
        }
        return {...block,runs,...(block.list?{list:{...block.list}}:{}),...(noteReferences?{noteReferences}:{})};
    });
    const seenSections:string[]=[],closed=new Set<string>(),columnCounts=new Map<string,Set<number>>();let active:string|undefined,column=1;
    for(const block of blocks) {
        if(block.sectionId!==active) {
            if(active)closed.add(active);
            if(closed.has(block.sectionId))fail('sections must be contiguous in source reading order');
            seenSections.push(block.sectionId);active=block.sectionId;column=1;
        }
        const section=sectionMap.get(block.sectionId)!;
        if(block.column<column||block.column>section.columns)fail('columns must read first column then second');
        column=block.column;
        const present=columnCounts.get(block.sectionId)??new Set<number>();present.add(block.column);columnCounts.set(block.sectionId,present);
    }
    if(JSON.stringify(seenSections)!==JSON.stringify(sections.map(section=>section.id)))fail('section order or empty sections do not match the page');
    for(const section of sections)if(section.columns===2 && columnCounts.get(section.id)?.size!==2)fail('a two-column section needs both columns');
    const blockMap=new Map(blocks.map(block=>[block.id,block]));
    const edges=new Map<string,string[]>();
    for(const block of blocks) {
        const targets:string[]=[];
        if(block.anchorBlockId!==undefined) {
            const target=blockMap.get(block.anchorBlockId);if(!target||!['figure','table'].includes(target.role))fail('caption anchor is missing or has the wrong role');
            targets.push(block.anchorBlockId);
        }
        for(const reference of block.noteReferences??[]) {
            if(blockMap.get(reference.targetBlockId)?.role!=='footnote')fail('note target is missing or is not a footnote');
            targets.push(reference.targetBlockId);
        }
        edges.set(block.id,targets);
    }
    const state=new Map<string,number>();
    for(const block of blocks) {
        if(state.get(block.id)===2)continue;
        const stack:{id:string;exit:boolean}[]=[{id:block.id,exit:false}];
        while(stack.length) {
            const current=stack.pop()!;
            if(current.exit){state.set(current.id,2);continue;}
            if(state.get(current.id)===1)fail('caption or note links contain a cycle');
            if(state.get(current.id)===2)continue;
            state.set(current.id,1);stack.push({id:current.id,exit:true});
            for(const target of edges.get(current.id)??[])stack.push({id:target,exit:false});
        }
    }
    return {version:1,sections,blocks};
}

export function deriveStructure(page:StructurePage):PageStructure {
    if(page.structure!==undefined)return readPageStructure(page.structure,page)!;
    if(!page.regions.length)return {version:1,sections:[],blocks:[]};
    const sectionId=stableId('section',page.id);
    return readPageStructure({version:1,sections:[{id:sectionId,columns:1}],blocks:page.regions.map(region=>defaultStructureBlock(region,sectionId))},page)!;
}

export function canonicalInlineRuns(runs:readonly InlineRun[]):InlineRun[] {
    const result:InlineRun[]=[];
    for(const run of [...runs].sort((a,b)=>a.start-b.start)) {
        const marks:InlineMarks={...(run.bold?{bold:true}:{}),...(run.italic?{italic:true}:{}),...(run.underline?{underline:true}:{})};
        if(run.end<=run.start||!markKey(marks))continue;
        const previous=result.at(-1);
        if(previous&&previous.end===run.start&&markKey(previous)===markKey(marks))previous.end=run.end;
        else result.push({start:run.start,end:run.end,...marks});
    }
    return result;
}

export function structureSegments(text:string,block:StructureBlock):StructureSegment[] {
    if(!text)return [];
    const points=[...new Set([0,text.length,...block.runs.flatMap(run=>[run.start,run.end]),...(block.noteReferences??[]).flatMap(reference=>[reference.start,reference.end])])].sort((a,b)=>a-b);
    let runIndex=0,noteIndex=0;const notes=block.noteReferences??[];
    return points.slice(0,-1).map((start,index)=>{
        const end=points[index+1];
        while(runIndex<block.runs.length&&block.runs[runIndex].end<=start)runIndex++;
        while(noteIndex<notes.length&&notes[noteIndex].end<=start)noteIndex++;
        const candidate=block.runs[runIndex],reference=notes[noteIndex];
        const run=candidate&&candidate.start<=start&&candidate.end>=end?candidate:undefined;
        const note=reference&&reference.start<=start&&reference.end>=end?reference:undefined;
        return {start,end,text:text.slice(start,end),...(run?.bold?{bold:true as const}:{}),...(run?.italic?{italic:true as const}:{}),...(run?.underline?{underline:true as const}:{}),...(note?{noteTargetBlockId:note.targetBlockId}:{})};
    });
}

function rebaseText(block:StructureBlock,oldText:string,newText:string):{block:StructureBlock;lostMarks:boolean;lostNotes:number} {
    if(oldText===newText)return {block:clone(block),lostMarks:false,lostNotes:0};
    const oldBoundaries=structureTextBoundaries(oldText),newBoundaries=structureTextBoundaries(newText);
    let prefix=0;
    while(prefix<oldText.length&&prefix<newText.length&&oldText[prefix]===newText[prefix])prefix++;
    while(prefix>0&&(!oldBoundaries.has(prefix)||!newBoundaries.has(prefix)))prefix--;
    let suffix=0;
    while(suffix<oldText.length-prefix&&suffix<newText.length-prefix&&oldText[oldText.length-1-suffix]===newText[newText.length-1-suffix])suffix++;
    while(suffix>0&&(!oldBoundaries.has(oldText.length-suffix)||!newBoundaries.has(newText.length-suffix)))suffix--;
    const oldSuffix=oldText.length-suffix,shift=newText.length-oldText.length,runs:InlineRun[]=[];
    let lostMarks=false;
    for(const run of block.runs) {
        const leftEnd=Math.min(run.end,prefix),rightStart=Math.max(run.start,oldSuffix);
        if(leftEnd>run.start)runs.push({...run,end:leftEnd});
        if(run.end>rightStart)runs.push({...run,start:rightStart+shift,end:run.end+shift});
        if(run.start<oldSuffix&&run.end>prefix)lostMarks=true;
    }
    const notes=(block.noteReferences??[]).flatMap(reference=>{
        if(reference.end<=prefix)return [{...reference}];
        if(reference.start>=oldSuffix)return [{...reference,start:reference.start+shift,end:reference.end+shift}];
        return [];
    });
    const next={...block,runs:canonicalInlineRuns(runs)};
    delete next.noteReferences;if(notes.length)next.noteReferences=notes;
    return {block:next,lostMarks,lostNotes:(block.noteReferences?.length??0)-notes.length};
}

/** Keep source order. Repeated section visits/column reversals form new sections. */
function contiguousLayout(blocks:StructureBlock[]):{sections:StructureSection[];blocks:StructureBlock[];changed:boolean} {
    if(!blocks.length)return {sections:[],blocks:[],changed:false};
    const groups:StructureBlock[][]=[];
    for(const block of blocks) {
        const group=groups.at(-1),last=group?.at(-1);
        if(!last||last.sectionId!==block.sectionId||block.column<last.column)groups.push([block]);
        else group!.push(block);
    }
    const used=new Set<string>(blocks.map(block=>block.id)),sections:StructureSection[]=[],result:StructureBlock[]=[];
    let changed=false;
    for(const group of groups) {
        let sectionId=group[0].sectionId;
        if(used.has(sectionId)) {
            const seed=`${sectionId}|${group[0].sourceRegionId}`;sectionId=stableId('section',seed);
            for(let attempt=1;used.has(sectionId);attempt++)sectionId=stableId('section',`${seed}|${attempt}`);
        }
        used.add(sectionId);
        const both=group.some(block=>block.column===1)&&group.some(block=>block.column===2);
        sections.push({id:sectionId,columns:both?2:1});
        for(const block of group) {
            const column:1|2=both?block.column:1;
            if(sectionId!==block.sectionId||column!==block.column)changed=true;
            result.push({...block,sectionId,column});
        }
    }
    return {sections,blocks:result,changed};
}
function pruneLinks(blocks:StructureBlock[]):{blocks:StructureBlock[];removed:number} {
    const targets=new Map(blocks.map(block=>[block.id,block.role]));let removed=0;
    return {blocks:blocks.map(block=>{
        const next=clone(block);
        if(next.anchorBlockId!==undefined&&!['figure','table'].includes(targets.get(next.anchorBlockId)??'')){delete next.anchorBlockId;removed++;}
        const references=(next.noteReferences??[]).filter(reference=>targets.get(reference.targetBlockId)==='footnote');
        removed+=(next.noteReferences?.length??0)-references.length;
        delete next.noteReferences;if(references.length)next.noteReferences=references;
        return next;
    }),removed};
}

export function reconcilePageStructure(beforePage:StructurePage,nextPage:StructurePage):ReconcileResult {
    if(beforePage.structure===undefined)return {structure:undefined,notices:[]};
    const old=readPageStructure(beforePage.structure,beforePage)!;
    const previous=new Map(old.blocks.map(block=>[block.sourceRegionId,block]));
    const oldRegions=new Map(beforePage.regions.map(region=>[region.id,region]));
    const retained=new Set(nextPage.regions.map(region=>region.id)),usedIds=new Set(old.blocks.filter(block=>retained.has(block.sourceRegionId)).map(block=>block.id));
    const notices:string[]=[];let changedMarks=0,lostNotes=0,changedRoles=0,newBlocks=0;
    const blocks=nextPage.regions.map(region=>{
        const existing=previous.get(region.id);
        if(existing) {
            if(!roleMatchesRegion(existing.role,region)) {
                const reset=defaultStructureBlock(region,existing.sectionId);changedRoles++;
                return {...reset,id:existing.id,column:existing.column,alignment:existing.alignment,breakBefore:existing.breakBefore};
            }
            const rebased=rebaseText(existing,oldRegions.get(region.id)!.text,region.text);
            if(rebased.lostMarks)changedMarks++;lostNotes+=rebased.lostNotes;return rebased.block;
        }
        newBlocks++;
        const block=defaultStructureBlock(region,stableId('section',nextPage.id));
        for(let attempt=0;usedIds.has(block.id);attempt++)block.id=stableId('block',`${region.id}|new|${attempt}`);
        usedIds.add(block.id);return block;
    });
    // New regions inherit a nearby section/column only as layout placement;
    // their roles, marks and links remain fresh defaults.
    for(let index=0;index<blocks.length;index++)if(!previous.has(blocks[index].sourceRegionId)) {
        const neighbor=index>0?blocks[index-1]:blocks.slice(index+1).find(block=>previous.has(block.sourceRegionId));
        if(neighbor){blocks[index].sectionId=neighbor.sectionId;blocks[index].column=neighbor.column;}
    }
    const removed=old.blocks.filter(block=>!retained.has(block.sourceRegionId)).length;
    if(removed)notices.push(`${removed} replaced or removed block${removed===1?'':'s'} lost active structure. Review the replacement blocks and their default formatting.`);
    if(newBlocks)notices.push(`${newBlocks} new block${newBlocks===1?' uses':'s use'} default roles and plain text.`);
    if(changedRoles)notices.push(`${changedRoles} block role${changedRoles===1?' was':'s were'} reset to match changed source kinds.`);
    if(changedMarks)notices.push(`Formatting intersecting changed text was reset in ${changedMarks} block${changedMarks===1?'':'s'}; inserted or replaced text is plain.`);
    if(lostNotes)notices.push(`${lostNotes} note reference${lostNotes===1?' was':'s were'} removed because its marked text changed.`);
    const layout=contiguousLayout(blocks),links=pruneLinks(layout.blocks);
    if(layout.changed)notices.push('Reading-order changes regrouped sections or columns without reordering text. Review the layout.');
    if(links.removed)notices.push(`${links.removed} caption or note link${links.removed===1?' was':'s were'} removed because the target disappeared or changed role.`);
    const structure=readPageStructure({version:1,sections:layout.sections,blocks:links.blocks},nextPage)!;
    return {structure,notices};
}

/** Projection of an already validated structure to an included source subset. */
export function projectStructureForRegions(structure:PageStructure,regions:readonly StructureRegion[],pageId='export'):ReconcileResult {
    const bySource=new Map(structure.blocks.map(block=>[block.sourceRegionId,block]));
    const blocks=regions.map(region=>{const block=bySource.get(region.id);if(!block)return fail('export contains a source without a structure block');return clone(block);});
    const layout=contiguousLayout(blocks),links=pruneLinks(layout.blocks),notices:string[]=[];
    if(links.removed)notices.push(`${links.removed} link${links.removed===1?' was':'s were'} omitted because its target is outside the export.`);
    if(layout.changed)notices.push('The exported subset uses contiguous sections and columns.');
    return {structure:readPageStructure({version:1,sections:layout.sections,blocks:links.blocks},{id:pageId,regions:[...regions]}),notices};
}
