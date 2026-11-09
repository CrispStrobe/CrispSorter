import { editRegion, updatePage, type OcrPage, type OcrProject, type OcrRegion } from './model';
import { joinRegionText } from './text';
import { collectSourceComponents, readSharedSourceGroups, readSourceComponents, validatePageSourceComponents } from './passageData';
import type { WordBounds } from './wordEvidence';
export * from './passageData';

export type PassageSeparator = '' | ' ' | '\n' | '\n\n';
export type SplitCoverage = {mode:'shared'} | {mode:'divide';axis:'x'|'y';position:number;firstSide:'low'|'high'};
export type PassageRequest =
    | {kind:'split';regionId:string;offset:number;coverage:SplitCoverage}
    | {kind:'merge';regionIds:string[];separator:PassageSeparator};
export interface PassagePreview {
    projectId:string; page:OcrPage; operationId:string; request:PassageRequest;
    regions:OcrRegion[]; regionIds:string[]; replacementIds:string[];
    warnings:string[]; beforeText:string; afterText:string;
}
export interface PassageEligibility { splitAllowed:boolean; nextMergeId:string|null; reasons:string[]; }
export type PassageRepairKind = 'normalize-line-endings' | 'trim-trailing-spaces' | 'collapse-inline-spaces' | 'join-lines' | 'dehyphenate';
export interface RepairChange {
    id:string; regionId:string; start:number; end:number; before:string; after:string; context:string; risky:boolean;
}
export interface RepairPreview {
    projectId:string; page:OcrPage; regionIds:string[]; kind:PassageRepairKind;
    changes:RepairChange[]; warnings:string[]; beforeText:string; afterText:string;
}

const clone = <T>(value:T):T => JSON.parse(JSON.stringify(value)) as T;
const textKinds = new Set(['text','title','header','footer']);
const separators = new Set(['',' ','\n','\n\n']);
const repairKinds = new Set<PassageRepairKind>(['normalize-line-endings','trim-trailing-spaces','collapse-inline-spaces','join-lines','dehyphenate']);
const segmenter = typeof Intl.Segmenter === 'function' ? new Intl.Segmenter(undefined,{granularity:'grapheme'}) : null;
const fail = (message:string):never => { throw new Error(`${message} Existing text was preserved.`); };
const operationId = ():string => `passage-${globalThis.crypto?.randomUUID?.() ?? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`}`;
const box = (region:WordBounds):WordBounds => ({x:region.x,y:region.y,w:region.w,h:region.h});
const overlaps = (a:WordBounds,b:WordBounds):boolean => a.x < b.x+b.w && a.x+a.w > b.x && a.y < b.y+b.h && a.y+a.h > b.y;
const editable = (region:OcrRegion):boolean => !region.excluded && textKinds.has(region.kind) && !!region.text.trim();

/** An interior caret that does not divide an emoji, combining sequence, or CRLF. */
export function isPassageBoundary(text:string,offset:number):boolean {
    if (!Number.isInteger(offset) || offset <= 0 || offset >= text.length) return false;
    if (segmenter) {
        for (const part of segmenter.segment(text)) { if (part.index === offset) return true; if (part.index > offset) return false; }
        return false;
    }
    // Without a Unicode segmenter, only ASCII-to-ASCII boundaries are certain.
    // Guessing from marks/ZWJ alone misses Indic conjuncts and emoji tag flags.
    const left=text.charCodeAt(offset-1),right=text.charCodeAt(offset);
    return left<=0x7f && right<=0x7f && !(left===13 && right===10);
}

function splittable(text:string):boolean {
    const first=text.length-text.trimStart().length,last=text.trimEnd().length;
    if (segmenter) {
        for (const part of segmenter.segment(text)) if (part.index>first && part.index<last) return true;
        return false;
    }
    for (let offset=first+1;offset<last;offset++) if(isPassageBoundary(text,offset)) return true;
    return false;
}

export function passageEligibility(page:OcrPage,regionId:string):PassageEligibility {
    const index=page.regions.findIndex(region=>region.id===regionId),region=page.regions[index],reasons:string[]=[];
    if(page.excluded) reasons.push('Include the page before changing its passages.');
    if(page.status==='running') reasons.push('Wait for the current recognition to finish.');
    if(!region || !editable(region)) reasons.push('Choose an included, nonempty text passage. Tables, pictures, and formulas use separate tools.');
    if(reasons.length) return {splitAllowed:false,nextMergeId:null,reasons};
    const next=page.regions[index+1];
    const splitAllowed=splittable(region.text);
    if(!splitAllowed) reasons.push('A split needs nonempty text on both sides of a complete character.');
    const nextMergeId=next && editable(next) && next.kind===region.kind ? next.id : null;
    if(!nextMergeId) reasons.push('Merge needs the immediately following included passage of the same kind.');
    return {splitAllowed,nextMergeId,reasons};
}

function pageFor(project:OcrProject,pageId:string):OcrPage {
    const page=project.pages.find(page=>page.id===pageId);
    if(!page) return fail('The source page is no longer in this project.');
    if(page.excluded || page.status==='running') return fail('Include the page and wait for recognition before changing passages.');
    return page;
}
function requireRegions(page:OcrPage,ids:readonly string[],adjacent=false):OcrRegion[] {
    if(!ids.length || ids.length>128 || new Set(ids).size!==ids.length) return fail('Choose 1–128 distinct passages.');
    const indices=ids.map(id=>page.regions.findIndex(region=>region.id===id));
    if(indices.some(index=>index<0)) return fail('A selected passage no longer exists.');
    const regions=indices.map(index=>page.regions[index]);
    if(regions.some(region=>!editable(region))) return fail('Only included, nonempty text, title, header, or footer passages can be changed here.');
    if(adjacent && (regions.length<2 || indices.some((index,i)=>i>0 && index!==indices[i-1]+1))) return fail('Merge passages in their existing reading order with no intervening region.');
    if(adjacent && regions.some(region=>region.kind!==regions[0].kind)) return fail('Merge passages of the same kind.');
    return regions;
}
function unchangedPage(project:OcrProject,preview:{projectId:string;page:OcrPage}):OcrPage {
    const page=pageFor(project,preview.page.id);
    if(project.id!==preview.projectId || JSON.stringify(page)!==JSON.stringify(preview.page)) return fail('The source page changed. Preview the passage edit again.');
    return page;
}
function replaceRegions(page:OcrPage,ids:readonly string[],regions:readonly OcrRegion[]):OcrRegion[] {
    const first=page.regions.findIndex(region=>region.id===ids[0]),selected=new Set(ids);
    return [...page.regions.slice(0,first),...regions,...page.regions.slice(first).filter(region=>!selected.has(region.id))];
}
function manualRegion(id:string,text:string,bounds:WordBounds,source:OcrRegion,components:ReturnType<typeof collectSourceComponents>,groups:string[],separatorBefore:OcrRegion['separatorBefore']):OcrRegion {
    return {id,text,originalText:text,...bounds,kind:source.kind,review:'corrected',excluded:false,
        confidence:null,confidenceBasis:'manual',char_conf:[],charConfidenceUnit:'unknown',charConfidenceAligned:false,
        sourceComponents:readSourceComponents(components),
        ...(separatorBefore!==undefined ? {separatorBefore} : {}),
        ...(groups.length ? {sharedSourceGroups:readSharedSourceGroups(groups)} : {}),
    };
}
function ensureFreshIds(project:OcrProject,ids:string[]):void {
    const occupied=new Set([project.id,...project.documents.map(doc=>doc.id),...project.pages.flatMap(page=>[page.id,...page.regions.map(region=>region.id)])]);
    if(ids.some(id=>occupied.has(id))) fail('The preview has an existing passage identity. Preview again.');
}

function buildPassagePreview(project:OcrProject,pageId:string,request:PassageRequest,seed:string):PassagePreview {
    const page=pageFor(project,pageId),warnings:string[]=[];
    if(typeof seed!=='string' || !/^passage-[A-Za-z0-9-]{1,100}$/u.test(seed)) return fail('Invalid passage preview identity.');
    let regionIds:string[],regions:OcrRegion[];
    if(request.kind==='split') {
        regionIds=[request.regionId];
        const [source]=requireRegions(page,regionIds),offset=request.offset;
        if(!isPassageBoundary(source.text,offset) || !source.text.slice(0,offset).trim() || !source.text.slice(offset).trim()) return fail('Place the caret between complete characters with nonempty text on both sides.');
        const components=collectSourceComponents(page,[source]);
        let groups=readSharedSourceGroups(source.sharedSourceGroups) ?? [],bounds:[WordBounds,WordBounds];
        const coverage=request.coverage;
        if(coverage?.mode==='shared') {
            bounds=[box(source),box(source)]; groups=[...groups,`${seed}-shared`]; readSharedSourceGroups(groups);
            warnings.push('Both passages share the same source area. Spatial exports require distinct coverage or merging these passages again.');
        } else if(coverage?.mode==='divide') {
            const {axis,position,firstSide}=coverage;
            if(!['x','y'].includes(axis) || !['low','high'].includes(firstSide) || !Number.isFinite(position) ||
                position<=source[axis] || position>=source[axis]+(axis==='x'?source.w:source.h)) return fail('Choose a cut strictly inside the source rectangle and which side contains the first passage.');
            const low=box(source),high=box(source);
            if(axis==='x') { low.w=position-source.x; high.x=position; high.w=source.x+source.w-position; }
            else { low.h=position-source.y; high.y=position; high.h=source.y+source.h-position; }
            bounds=firstSide==='low'?[low,high]:[high,low];
            warnings.push('The source cut is your manual coverage choice; it does not create native word boxes or confidence.');
        } else return fail('Choose shared source coverage or an explicit source cut.');
        regions=[
            manualRegion(`${seed}-1`,source.text.slice(0,offset),bounds[0],source,components,groups,source.separatorBefore),
            manualRegion(`${seed}-2`,source.text.slice(offset),bounds[1],source,components,groups,''),
        ];
    } else if(request.kind==='merge') {
        regionIds=[...request.regionIds];
        const sources=requireRegions(page,regionIds,true);
        if(!separators.has(request.separator)) return fail('Choose no separator, a space, a newline, or a blank line.');
        const components=collectSourceComponents(page,sources),selected=new Set(regionIds);
        const allGroups=[...new Set(sources.flatMap(region=>readSharedSourceGroups(region.sharedSourceGroups) ?? []))];
        const groups=allGroups.filter(group=>page.regions.some(region=>!selected.has(region.id) && region.sharedSourceGroups?.includes(group)));
        readSharedSourceGroups(groups);
        const x=Math.min(...sources.map(region=>region.x)),y=Math.min(...sources.map(region=>region.y));
        const bounds={x,y,w:Math.max(...sources.map(region=>region.x+region.w))-x,h:Math.max(...sources.map(region=>region.y+region.h))-y};
        const text=sources.map(region=>region.text).join(request.separator);
        regions=[manualRegion(`${seed}-1`,text,bounds,sources[0],components,groups,sources[0].separatorBefore)];
        const nearby=page.regions.filter(region=>!selected.has(region.id) && !region.excluded && overlaps(bounds,region));
        if(nearby.length) warnings.push(`The enclosing source rectangle overlaps ${nearby.length} other region${nearby.length===1?'':'s'}. Review the source coverage before accepting.`);
        if(groups.some(group=>page.regions.some(region=>!selected.has(region.id) && region.sharedSourceGroups?.includes(group) && overlaps(bounds,region)))) warnings.push('Source coverage still overlaps another member of a shared source family. Spatial exports remain unavailable until coverage is resolved.');
    } else return fail('Unsupported passage operation.');
    ensureFreshIds(project,regions.map(region=>region.id));
    validatePageSourceComponents({...page,regions:replaceRegions(page,regionIds,regions)});
    const sources=regionIds.map(id=>page.regions.find(region=>region.id===id)!);
    return {projectId:project.id,page:clone(page),operationId:seed,request:clone(request),regions,regionIds,
        replacementIds:regions.map(region=>region.id),warnings,beforeText:joinRegionText(sources),afterText:joinRegionText(regions)};
}

export function capturePassageSplit(project:OcrProject,pageId:string,regionId:string,offset:number,coverage:SplitCoverage):PassagePreview {
    return buildPassagePreview(project,pageId,{kind:'split',regionId,offset,coverage},operationId());
}
export function capturePassageMerge(project:OcrProject,pageId:string,regionIds:string[],separator:PassageSeparator):PassagePreview {
    return buildPassagePreview(project,pageId,{kind:'merge',regionIds,separator},operationId());
}
export function applyPassagePreview(project:OcrProject,preview:PassagePreview):OcrProject {
    const page=unchangedPage(project,preview),fresh=buildPassagePreview(project,page.id,preview.request,preview.operationId);
    for(const key of ['regions','regionIds','replacementIds','beforeText','afterText'] as const) {
        if(JSON.stringify(fresh[key])!==JSON.stringify(preview[key])) return fail('The passage preview changed. Preview again.');
    }
    const {extraction:_stale,...provenance}=page.provenance;
    return updatePage(project,page.id,{regions:replaceRegions(page,fresh.regionIds,fresh.regions),approved:false,status:'recognized',error:null,
        provenance:{...provenance,lastPassageEdit:{kind:fresh.request.kind,sourceRegionIds:fresh.regionIds,replacementIds:fresh.replacementIds}}});
}

function repairRanges(text:string,kind:PassageRepairKind):{start:number;end:number;after:string;risky:boolean}[] {
    const ranges:{start:number;end:number;after:string;risky:boolean}[]=[];
    const add=(start:number,end:number,after:string,risky=false)=>{
        if(text.slice(start,end)===after) return;
        ranges.push({start,end,after,risky});
        if(ranges.length>5000) fail('This repair has more than 5,000 changes. Choose fewer passages.');
    };
    const plainContinuation=(start:number,end:number):boolean=>{
        const lineStart=Math.max(text.lastIndexOf('\n',start-1),text.lastIndexOf('\r',start-1))+1;
        const following=text.slice(end),lineEnd=following.search(/[\r\n]/u);
        const left=text.slice(lineStart,start),right=lineEnd<0?following:following.slice(0,lineEnd);
        const list=/^[ \t]*(?:[-*•‣▪]|\d+[.)]|[A-Za-z][.)])(?:[ \t]+|$)/u;
        const number=/^[ \t]*[+−-]?\d/u;
        if(list.test(left) || list.test(right) || number.test(left) || number.test(right) || /^[ \t]/u.test(left) || /^[ \t]/u.test(right)) return false;
        const previousWord=left.match(/\S+$/u)?.[0] ?? '',nextWord=right.match(/^\S+/u)?.[0] ?? '';
        return !/[-\d_@:/\\−–—]/u.test(previousWord+nextWord);
    };
    if(kind==='normalize-line-endings') {
        for(const match of text.matchAll(/\r\n?/gu)) add(match.index!,match.index!+match[0].length,'\n');
    } else if(kind==='trim-trailing-spaces') {
        for(const match of text.matchAll(/[ \t]+(?=\r?\n|\r|$)/gu)) add(match.index!,match.index!+match[0].length,'');
    } else if(kind==='collapse-inline-spaces') {
        // Do not touch indentation, tabs, NBSP, or alignment around numbers.
        for(const match of text.matchAll(/(\p{L}\p{M}*) {2,}(?=\p{L})/gu)) {
            const start=match.index!+match[1].length;
            add(start,match.index!+match[0].length,' ',true);
        }
    } else if(kind==='dehyphenate') {
        // Only lowercase continuations are candidates; each remains a question
        // for the reviewer (well-\nknown is not safe to join automatically).
        for(const match of text.matchAll(/(\p{Ll}\p{M}*)-\r?\n(?=\p{Ll})/gu)) {
            const start=match.index!+match[1].length;
            const end=match.index!+match[0].length;
            if(plainContinuation(start,end)) add(start,end,'',true);
        }
    } else if(kind==='join-lines') {
        // A single unindented continuation only: never blank lines, lists,
        // numbers, identifiers, punctuation/hyphens, or a page boundary.
        for(const match of text.matchAll(/(\p{Ll}\p{M}*)\r?\n(?=\p{Ll})/gu)) {
            const start=match.index!+match[1].length;
            const end=match.index!+match[0].length;
            if(plainContinuation(start,end)) add(start,end,' ',true);
        }
    }
    return ranges;
}

export function capturePassageRepair(project:OcrProject,pageId:string,regionIds:string[],kind:PassageRepairKind):RepairPreview {
    const page=pageFor(project,pageId);
    if(!repairKinds.has(kind)) return fail('Choose a supported text repair.');
    const selected=requireRegions(page,regionIds),ids=new Set(regionIds),ordered=page.regions.filter(region=>ids.has(region.id));
    if(selected.reduce((total,region)=>total+region.text.length,0)>10000000) return fail('This repair exceeds ten million characters. Choose fewer passages.');
    const changes:RepairChange[]=[];
    for(const region of ordered) for(const range of repairRanges(region.text,kind)) {
        changes.push({id:JSON.stringify([region.id,kind,range.start,range.end]),regionId:region.id,...range,
            before:region.text.slice(range.start,range.end),context:region.text.slice(Math.max(0,range.start-40),Math.min(region.text.length,range.end+40))});
        if(changes.length>5000) return fail('This repair has more than 5,000 changes. Choose fewer passages.');
    }
    const warnings=changes.some(change=>change.risky)?['Review each proposed join or spacing change. Intentional hyphens, line breaks, and alignment can carry meaning.']:[];
    const preview:RepairPreview={projectId:project.id,page:clone(page),regionIds:ordered.map(region=>region.id),kind,changes,warnings,beforeText:joinRegionText(ordered),afterText:''};
    preview.afterText=previewRepairText(preview,changes.map(change=>change.id));
    return preview;
}

function selectedRepairRegions(preview:RepairPreview,selectedIds:readonly string[]):OcrRegion[] {
    const ids=new Set(selectedIds),known=new Set(preview.changes.map(change=>change.id));
    if(ids.size!==selectedIds.length || [...ids].some(id=>!known.has(id))) return fail('The selected repair changes are invalid. Preview again.');
    return preview.regionIds.map(id=>{
        const region=preview.page.regions.find(region=>region.id===id);
        if(!region) return fail('A repair source passage is missing.');
        let text=region.text,lastStart=text.length;
        const changes=preview.changes.filter(change=>change.regionId===id && ids.has(change.id)).sort((a,b)=>b.start-a.start);
        for(const change of changes) {
            if(!Number.isInteger(change.start) || !Number.isInteger(change.end) || change.start<0 || change.end<=change.start ||
                change.end>lastStart || region.text.slice(change.start,change.end)!==change.before) return fail('Repair ranges changed or overlap. Preview again.');
            text=text.slice(0,change.start)+change.after+text.slice(change.end); lastStart=change.start;
        }
        return {...region,text};
    });
}

/** Render exactly the currently selected proposals, including none. */
export function previewRepairText(preview:RepairPreview,selectedChangeIds:readonly string[]):string {
    return joinRegionText(selectedRepairRegions(preview,selectedChangeIds));
}

export function applyPassageRepair(project:OcrProject,preview:RepairPreview,selectedChangeIds:string[]):OcrProject {
    const page=unchangedPage(project,preview),fresh=capturePassageRepair(project,page.id,preview.regionIds,preview.kind);
    if(JSON.stringify(fresh.changes)!==JSON.stringify(preview.changes) || fresh.beforeText!==preview.beforeText || fresh.afterText!==preview.afterText) return fail('The repair preview changed. Preview again.');
    const regions=selectedRepairRegions(fresh,selectedChangeIds);
    if(!selectedChangeIds.length) return project;
    let next=project;
    for(const region of regions) {
        if(region.text!==page.regions.find(source=>source.id===region.id)!.text) next=editRegion(next,page.id,region.id,region.text);
    }
    const {extraction:_stale,...provenance}=page.provenance;
    return updatePage(next,page.id,{approved:false,provenance:{...provenance,lastPassageRepair:{kind:preview.kind,changes:selectedChangeIds.length}}});
}
