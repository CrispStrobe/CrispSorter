import { editRegion, parseProject, updatePage, type OcrDocument, type OcrPage, type OcrProject, type OcrRegion } from './model';
import { collectSourceComponents, readSharedSourceGroups, validatePageSourceComponents } from './passageData';
import { joinRegionText, sourceOffsetForTextarea } from './text';
import { reconcilePageStructure } from './structureData';
import { archiveReplacedStructure } from './structureHistory';

/** One independent, recoverable page draft. Its source never contains a project/draft. */
export interface PageTextDraft {
    version:1; id:string; revision:number; projectId:string;
    page:OcrPage; document:OcrDocument; text:string; updatedAt:string;
}
export interface PageTextEditPreview {
    version:1; projectId:string; draftId:string; draftRevision:number;
    draftUpdatedAt:string; draftText:string; page:OcrPage;
    regionIds:string[]; replacementIds:string[]; regions:OcrRegion[];
    beforeText:string; afterText:string; affectedBeforeText:string; affectedAfterText:string;
    warnings:string[]; requiresCombinedCoverage:boolean;
}

const MAX_DRAFT_BYTES=16*1024*1024, MAX_TEXT_UNITS=1_000_000;
const editableKinds=new Set(['text','title','header','footer']);
const encoder=new TextEncoder();
const segmenter=typeof Intl.Segmenter==='function'?new Intl.Segmenter(undefined,{granularity:'grapheme'}):null;
const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const normalize=(value:string):string=>value.replace(/\r\n?/gu,'\n');
const size=(value:unknown):number=>encoder.encode(JSON.stringify(value)).byteLength;
const object=(value:unknown):value is Record<string,unknown>=>!!value&&typeof value==='object'&&!Array.isArray(value);
const identifier=(value:unknown):value is string=>typeof value==='string'&&!!value.trim()&&value.length<=200&&!value.includes('\0');
const fail=(message:string):never=>{throw new Error(message+' Existing text and the saved draft were preserved.');};
const stamp=():string=>new Date().toISOString();
const newId=():string=>'page-text-'+(globalThis.crypto?.randomUUID?.()??Date.now().toString(36)+'-'+Math.random().toString(36).slice(2));
type DraftProject=OcrProject&{pageTextDraft?:PageTextDraft};
const draftOf=(project:OcrProject):PageTextDraft|undefined=>(project as DraftProject).pageTextDraft;
const included=(region:OcrRegion):boolean=>!region.excluded&&region.kind!=='picture';
const editable=(region:OcrRegion):boolean=>!region.excluded&&editableKinds.has(region.kind);

/** Native JSON may reorder object keys. Array order and actual values stay exact. */
function same(left:unknown,right:unknown):boolean {
    if(left===right)return true;
    if(Array.isArray(left)||Array.isArray(right)) {
        if(!Array.isArray(left)||!Array.isArray(right)||left.length!==right.length)return false;
        for(let index=0;index<left.length;index++)if(!same(left[index],right[index]))return false;
        return true;
    }
    if(!object(left)||!object(right))return false;
    // Boundary readers may materialize optional fields as undefined; JSON omits them.
    const leftKeys=Object.keys(left).filter(key=>left[key]!==undefined);
    const rightKeys=Object.keys(right).filter(key=>right[key]!==undefined);
    return leftKeys.length===rightKeys.length&&leftKeys.every(key=>
        Object.prototype.hasOwnProperty.call(right,key)&&same(left[key],right[key]));
}

/** Tables/formulas are visible text, but remain protected editing boundaries. */
export function projectPageText(page:Pick<OcrPage,'regions'>):string {
    return joinRegionText(page.regions.filter(included));
}

function validateDraftSize(draft:PageTextDraft):void {
    if(typeof draft.text!=='string'||draft.text.length>MAX_TEXT_UNITS||draft.text.includes('\0')||normalize(draft.text)!==draft.text)
        fail('The page draft must contain at most 1,000,000 text units, with LF line endings and no NUL character.');
    if(size(draft)>MAX_DRAFT_BYTES)fail('The saved page draft exceeds the 16 MB limit. Copy the draft before reducing its content.');
}

/** Validate recovery independently of the current project/page; applying is stricter. */
export function readPageTextDraft(raw:unknown):PageTextDraft|undefined {
    if(raw===undefined)return undefined;
    const keys=['version','id','revision','projectId','page','document','text','updatedAt'];
    if(!object(raw)||Object.keys(raw).some(key=>!keys.includes(key))||raw.version!==1||
        !identifier(raw.id)||raw.id.length>180||!identifier(raw.projectId)||!Number.isSafeInteger(raw.revision)||(raw.revision as number)<1||
        typeof raw.updatedAt!=='string'||raw.updatedAt.length>64||!Number.isFinite(Date.parse(raw.updatedAt))||
        !object(raw.page)||!object(raw.document))return fail('The saved page draft has an invalid version or metadata.');
    const pageKeys=['id','documentId','sourcePage','name','imagePath','originalImagePath','width','height','widthPoints','heightPoints',
        'status','regions','excluded','approved','error','elapsedMs','provenance','profileId','structure'];
    if(Object.keys(raw.page).some(key=>!pageKeys.includes(key))||
        Object.keys(raw.document).some(key=>!['id','sourceUri','name'].includes(key)))
        return fail('The saved page draft contains unsupported source fields.');
    const draft=raw as unknown as PageTextDraft;
    validateDraftSize(draft);
    // No pageTextDraft property is supplied: the project reader cannot recurse
    // into this draft, and the captured page need not still exist in the project.
    parseProject({version:1,id:draft.projectId,name:'Captured page',createdAt:draft.updatedAt,updatedAt:draft.updatedAt,
        documents:[draft.document],pages:[draft.page],confidenceThreshold:0.85,profileId:draft.page.profileId});
    if(!identifier(draft.page.id)||!identifier(draft.document.id)||draft.page.status==='running')
        return fail('The saved page draft has an invalid or running source page.');
    // The reader is a validator, not a migration of captured source bytes.
    return clone(draft);
}

export function createPageTextDraft(project:OcrProject,pageId:string):OcrProject {
    const previous=draftOf(project);
    if(previous) {
        if(previous.projectId===project.id&&previous.page.id===pageId)return project;
        return fail('Resume or discard the existing page draft before starting another.');
    }
    const page=project.pages.find(candidate=>candidate.id===pageId);
    if(!page||page.excluded||page.status==='running')return fail('Choose an included page and wait for recognition before starting a draft.');
    if(!page.regions.some(editable))return fail('Recognize or add an included text passage before starting a page draft.');
    const document=project.documents.find(candidate=>candidate.id===page.documentId);
    if(!document)return fail('The source document is missing.');
    const updatedAt=stamp();
    const draft:PageTextDraft={version:1,id:newId(),revision:1,projectId:project.id,page:clone(page),document:clone(document),
        text:normalize(projectPageText(page)),updatedAt};
    readPageTextDraft(draft);
    return {...project,...{pageTextDraft:draft},updatedAt};
}

export function updatePageTextDraft(project:OcrProject,text:string):OcrProject {
    const draft=draftOf(project);
    if(!draft)return fail('Start or restore a page draft before editing.');
    if(typeof text!=='string')return fail('The page draft must be plain text.');
    const normalized=normalize(text);
    if(normalized===draft.text)return project;
    if(!Number.isSafeInteger(draft.revision)||draft.revision>=Number.MAX_SAFE_INTEGER)return fail('The page draft revision limit was reached.');
    const updatedAt=stamp(),next={...draft,text:normalized,revision:draft.revision+1,updatedAt};
    validateDraftSize(next);
    return {...project,...{pageTextDraft:next},updatedAt};
}

export function discardPageTextDraft(project:OcrProject):OcrProject {
    if(!draftOf(project))return project;
    const {pageTextDraft:_draft,...rest}=project as DraftProject;
    return {...rest,updatedAt:stamp()};
}

interface ProjectionRegion { region:OcrRegion; index:number; start:number; end:number; }
interface ProjectionSeparator { start:number; end:number; before:number; after:number; }
interface Projection { text:string; regions:ProjectionRegion[]; separators:ProjectionSeparator[]; }
function projectRanges(page:OcrPage):Projection {
    let text='';
    const regions:ProjectionRegion[]=[],separators:ProjectionSeparator[]=[];
    page.regions.forEach((region,index)=>{
        if(!included(region))return;
        if(regions.length) {
            const start=text.length;text+=region.separatorBefore??'\n';
            separators.push({start,end:text.length,before:regions.length-1,after:regions.length});
        }
        const start=text.length;text+=region.text;
        regions.push({region,index,start,end:text.length});
    });
    return {text,regions,separators};
}

interface ChangedRange { sourceStart:number;sourceEnd:number;draftStart:number;draftEnd:number; }
function changedRange(source:string,draft:string):ChangedRange|null {
    if(source===draft)return null;
    let start=0;
    while(start<source.length&&start<draft.length&&source[start]===draft[start])start++;
    let sourceEnd=source.length,draftEnd=draft.length;
    while(sourceEnd>start&&draftEnd>start&&source[sourceEnd-1]===draft[draftEnd-1]){sourceEnd--;draftEnd--;}
    // Only four endpoints need checking. Keeping a Set of every grapheme on a
    // million-character draft would otherwise consume far more than its text.
    const sourceSegments=segmenter?.segment(source),draftSegments=segmenter?.segment(draft);
    const boundary=(text:string,segments:Intl.Segments|undefined,offset:number):boolean=>{
        if(offset===0||offset===text.length)return true;
        if(segments)return segments.containing(offset)?.index===offset;
        return text.charCodeAt(offset-1)<=127&&text.charCodeAt(offset)<=127&&
            !(text.charCodeAt(offset-1)===13&&text.charCodeAt(offset)===10);
    };
    while(start>0&&(!boundary(source,sourceSegments,start)||!boundary(draft,draftSegments,start)))start--;
    while(!boundary(source,sourceSegments,sourceEnd)||!boundary(draft,draftSegments,draftEnd)){sourceEnd++;draftEnd++;}
    return {sourceStart:start,sourceEnd,draftStart:start,draftEnd};
}

/** Shared freshness guard for Review/Apply and draft-aware Undo/Redo. */
export function pageTextDraftSourceIsCurrent(project:OcrProject,draft:PageTextDraft):boolean {
    if(project.id!==draft.projectId)return false;
    const page=project.pages.find(candidate=>candidate.id===draft.page.id);
    const document=project.documents.find(candidate=>candidate.id===draft.document.id);
    return !!page&&!!document&&same(page,draft.page)&&same(document,draft.document);
}

function currentSource(project:OcrProject,draft:PageTextDraft):OcrPage {
    const page=project.pages.find(candidate=>candidate.id===draft.page.id);
    if(!page||!pageTextDraftSourceIsCurrent(project,draft))
        return fail('The captured source page or document changed. Copy or finish recovering this draft, then start a new draft from the current page.');
    if(page.excluded||page.status==='running')return fail('Include the source page and wait for recognition before applying the draft.');
    return page;
}

function changedPassages(projection:Projection,start:number,end:number):{entries:ProjectionRegion[];boundary:boolean} {
    const selected=new Set<number>();
    let boundary=false;
    projection.regions.forEach((entry,index)=>{
        if(start===end) {
            if(entry.start<start&&start<entry.end||entry.start===entry.end&&entry.start===start)selected.add(index);
        } else if(entry.start<end&&entry.end>start||entry.start===entry.end&&entry.start>=start&&entry.start<=end)selected.add(index);
    });
    for(const separator of projection.separators) {
        const hit=start===end?start>=separator.start&&start<=separator.end:
            separator.start===separator.end?separator.start>=start&&separator.start<=end:
                separator.start<end&&separator.end>start;
        if(hit){selected.add(separator.before);selected.add(separator.after);boundary=true;}
    }
    if(start===end&&!selected.size) {
        projection.regions.forEach((entry,index)=>{if(start===entry.start||start===entry.end)selected.add(index);});
    }
    if(!selected.size)return fail('The changed text cannot be mapped to an existing passage.');
    let first=projection.regions.length,last=-1;
    for(const index of selected){first=Math.min(first,index);last=Math.max(last,index);}
    return {entries:projection.regions.slice(first,last+1),boundary};
}

const overlaps=(a:Pick<OcrRegion,'x'|'y'|'w'|'h'>,b:Pick<OcrRegion,'x'|'y'|'w'|'h'>):boolean=>
    a.x<b.x+b.w&&a.x+a.w>b.x&&a.y<b.y+b.h&&a.y+a.h>b.y;

function manualReplacement(project:OcrProject,page:OcrPage,draft:PageTextDraft,sources:OcrRegion[],text:string):OcrRegion {
    const id=draft.id+'-combined';
    const ids=new Set([project.id,...project.documents.map(document=>document.id),...project.pages.flatMap(source=>[source.id,...source.regions.map(region=>region.id)])]);
    if(ids.has(id))return fail('The draft replacement identity is already in use. Start a fresh draft.');
    const x=Math.min(...sources.map(region=>region.x)),y=Math.min(...sources.map(region=>region.y));
    const bounds={x,y,w:Math.max(...sources.map(region=>region.x+region.w))-x,h:Math.max(...sources.map(region=>region.y+region.h))-y};
    const selected=new Set(sources.map(region=>region.id));
    if(page.regions.some(region=>!selected.has(region.id)&&!region.excluded&&overlaps(bounds,region)))
        return fail('Combined source coverage would overlap another included region. Edit the passages separately or correct their source rectangles first.');
    const groups=[...new Set(sources.flatMap(region=>region.sharedSourceGroups??[]))]
        .filter(group=>page.regions.some(region=>!selected.has(region.id)&&region.sharedSourceGroups?.includes(group)));
    const sourceComponents=collectSourceComponents(page,sources);
    return {id,text,originalText:text,...bounds,kind:'text',confidence:null,confidenceBasis:'manual',
        char_conf:[],charConfidenceUnit:'unknown',charConfidenceAligned:false,review:'corrected',excluded:false,sourceComponents,
        ...(sources[0].separatorBefore!==undefined?{separatorBefore:sources[0].separatorBefore}:{}),
        ...(groups.length?{sharedSourceGroups:readSharedSourceGroups(groups)}:{})};
}

function replacePassages(page:OcrPage,regionIds:readonly string[],regions:readonly OcrRegion[]):OcrRegion[] {
    const first=page.regions.findIndex(region=>region.id===regionIds[0]);
    return [...page.regions.slice(0,first),...regions,...page.regions.slice(first+regionIds.length)];
}

function structureWarnings(page:OcrPage,next:OcrPage,removedIds:readonly string[]):string[] {
    const warnings=reconcilePageStructure(page,next).notices;
    if(page.structure&&removedIds.length) {
        const removed=new Set(removedIds),blocks=page.structure.blocks.filter(block=>removed.has(block.sourceRegionId));
        const roles=[...new Set(blocks.map(block=>block.role))];
        const marks=blocks.reduce((sum,block)=>sum+block.runs.length,0);
        const links=blocks.reduce((sum,block)=>sum+(block.noteReferences?.length??0)+(block.anchorBlockId?1:0),0);
        const sections=new Set(blocks.map(block=>block.sectionId)).size;
        warnings.push('The replacement is one plain paragraph. Removed active structure: '+roles.join(', ')+'; '+
            marks+' styled range(s), '+links+' outgoing link(s), and placement in '+sections+
            ' section(s)/their reading columns. Previous roles, marks, links and layout remain in structure history.');
    }
    const history=archiveReplacedStructure(page,next);
    if(history.notice)warnings.push(history.notice);
    return warnings;
}

/** Build an immutable, checked replacement; this does not modify saved OCR text. */
export function preparePageTextEdit(project:OcrProject):PageTextEditPreview|null {
    const raw=draftOf(project);
    if(!raw)return fail('Start or restore a page draft before reviewing.');
    const draft=readPageTextDraft(raw)!,page=currentSource(project,draft);
    const projection=projectRanges(page),normalized=normalize(projection.text),change=changedRange(normalized,draft.text);
    if(!change)return null;
    const start=sourceOffsetForTextarea(projection.text,normalized,change.sourceStart);
    const end=sourceOffsetForTextarea(projection.text,normalized,change.sourceEnd);
    if(start===null||end===null)return fail('The changed text does not align with the captured source.');
    const selected=changedPassages(projection,start,end),entries=selected.entries;
    if(entries.length>128)return fail('A combined draft edit can cover at most 128 passages. Apply a smaller edit.');
    if(entries.some(entry=>!editable(entry.region))||
        entries.some((entry,index)=>index>0&&entry.index!==entries[index-1].index+1))
        return fail('The edit crosses a table, formula, picture, or excluded passage. Keep each edit within consecutive included text passages.');
    const first=entries[0],last=entries[entries.length-1];
    if(start<first.start||end>last.end)return fail('The changed boundary cannot be assigned safely to these passages.');
    const replacement=draft.text.slice(change.draftStart,change.draftEnd);
    const afterText=projection.text.slice(0,start)+replacement+projection.text.slice(end);
    const affectedBeforeText=projection.text.slice(first.start,last.end);
    const affectedAfterText=projection.text.slice(first.start,start)+replacement+projection.text.slice(end,last.end);
    const sources=entries.map(entry=>entry.region),regionIds=sources.map(region=>region.id);
    const combined=sources.length>1,manual=combined||afterText.length===0;
    const regions=manual?[manualReplacement(project,page,draft,sources,affectedAfterText)]:
        [{...sources[0],text:affectedAfterText,review:affectedAfterText===sources[0].originalText&&sources[0].confidenceBasis!=='manual'?'unreviewed' as const:'corrected' as const}];
    const candidate={...page,regions:replacePassages(page,regionIds,regions)};
    validatePageSourceComponents(candidate);
    if(projectPageText(candidate)!==afterText||normalize(afterText)!==draft.text)
        return fail('The replacement would change text outside the reviewed draft.');
    const warnings:string[]=[];
    if(combined)warnings.push('This edit replaces '+sources.length+' passages with one manual text paragraph. Its rectangle encloses their captured source coverage; it does not create new word boxes or OCR confidence.');
    if(selected.boundary&&combined)warnings.push('The edit touches a passage separator or an ambiguous boundary. Both neighboring passages belong to this replacement; line breaks remain inside the new paragraph.');
    if(afterText.length===0)warnings.push('The empty result remains an empty manual passage with its original source history.');
    if(projection.text.slice(start,end).includes('\r'))warnings.push('Line endings inside the changed range use LF. Unchanged text outside that range keeps its original line endings.');
    warnings.push(...structureWarnings(page,candidate,manual?regionIds:[]));
    return {version:1,projectId:project.id,draftId:draft.id,draftRevision:draft.revision,draftUpdatedAt:draft.updatedAt,draftText:draft.text,
        page:clone(page),regionIds,replacementIds:regions.map(region=>region.id),regions:clone(regions),
        beforeText:projection.text,afterText,affectedBeforeText,affectedAfterText,warnings,requiresCombinedCoverage:combined};
}

/** One atomic OCR edit. A stale, changed or unaccepted preview leaves the draft intact. */
export function applyPageTextEdit(project:OcrProject,preview:PageTextEditPreview,acceptCombinedCoverage=false):OcrProject {
    const fresh=preparePageTextEdit(project);
    if(!fresh||!same(fresh,preview))return fail('The page draft or its preview changed. Review the current draft again.');
    if(fresh.requiresCombinedCoverage&&acceptCombinedCoverage!==true)
        return fail('Accept the combined source coverage before applying this multi-passage edit.');
    const page=project.pages.find(candidate=>candidate.id===fresh.page.id)!;
    const {extraction:_staleExtraction,...provenance}=page.provenance;
    let next:OcrProject;
    if(fresh.regionIds.length===1&&fresh.replacementIds[0]===fresh.regionIds[0]) {
        next=editRegion(project,page.id,fresh.regionIds[0],fresh.regions[0].text);
    } else {
        next=updatePage(project,page.id,{regions:replacePassages(page,fresh.regionIds,fresh.regions),approved:false,status:'recognized',error:null,
            provenance:{...provenance}});
    }
    const changed=next.pages.find(candidate=>candidate.id===page.id)!;
    const {extraction:_oldExtraction,...changedProvenance}=changed.provenance;
    // An explicitly accepted manual repair is usable even when an earlier OCR
    // retry failed or remains pending; apply the same rule to one or many passages.
    next=updatePage(next,page.id,{status:'recognized',error:null,provenance:{...changedProvenance,lastPageTextEdit:{
        draftId:fresh.draftId,draftRevision:fresh.draftRevision,combined:fresh.requiresCombinedCoverage,
        sourceRegionIds:fresh.regionIds,replacementIds:fresh.replacementIds,
    }}});
    return discardPageTextDraft(next);
}
