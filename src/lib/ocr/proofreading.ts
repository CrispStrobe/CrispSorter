import { editRegion, type OcrProject, type OcrPage, type OcrRegion } from './model';
import { getWordEvidence } from './words';
import {
    readProofreadingData, normalizeProofreadingLanguage, isUtf16Boundary,
    type ProofreadingData, type VocabularyEntry, type ProofreadingSelection,
} from './proofreadingData';
export * from './proofreadingData';

export interface CorrectionSuggestion {
    id: string; text: string; source: 'engine'|'vocabulary'|'rule'; label: string; confidence: number|null;
}
export interface CorrectionRequest {
    from: string; to: string; language: string; caseSensitive: boolean; wholeWord: boolean;
    scope: 'passage'|'page'|'selected-pages'|'project'; pageId?: string; regionId?: string;
    pageIds?: string[]; includeApproved?: boolean; selection?: ProofreadingSelection;
}
export interface CorrectionMatch {
    id: string; pageId: string; pageName: string; regionId: string;
    start: number; end: number; before: string; after: string; context: string;
}
interface CorrectionTarget {
    pageId: string; regionId: string; imagePath: string; text: string;
    geometry: string; width: number; height: number; kind: string;
    approved: boolean; review: string;
}
export interface CorrectionPreview {
    projectId: string; request: CorrectionRequest; policy: string;
    targets: CorrectionTarget[]; matches: CorrectionMatch[];
    skippedProtected: number; skippedApproved: number; skippedExcluded: number;
}

const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
const editable = (region: OcrRegion): boolean => !region.excluded && !['picture','formula','table'].includes(region.kind);
const geometry = (region: OcrRegion): string => `${region.x},${region.y},${region.w},${region.h}`;
const identity = (pageId: string, regionId: string, start: number, end: number): string => JSON.stringify([pageId,regionId,start,end]);
const escapePattern = (text: string): string => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const wordCharacter = (character: string): boolean => /[\p{L}\p{M}\p{N}_]/u.test(character);
const graphemes = typeof Intl.Segmenter === 'function' ? new Intl.Segmenter(undefined, {granularity:'grapheme'}) : null;

interface NormalizedSource { text: string; boundaries: Map<number, number>|null; }
/** NFC comparison never supplies offsets directly; map back to source UTF-16. */
function normalizedSource(text: string): NormalizedSource {
    if(text.length>1000000) throw new Error('This passage exceeds the one-million-character proofreading limit. Split it into smaller passages first.');
    if(text.normalize('NFC')===text) return {text,boundaries:null};
    const segments = function* () {
        if(graphemes) for(const part of graphemes.segment(text)) yield {text:part.segment,index:part.index};
        else for(const match of text.matchAll(/\P{M}\p{M}*|\p{M}+/gu)) yield {text:match[0],index:match.index!};
    };
    let normalized = '';
    const boundaries = new Map<number,number>([[0,0]]);
    for (const part of segments()) {
        const next = part.text.normalize('NFC'), offset = normalized.length;
        if (next === part.text) {
            let current = 0;
            for (const character of part.text) {
                boundaries.set(offset + current, part.index + current);
                current += character.length;
            }
        } else boundaries.set(offset, part.index);
        normalized += next;
        boundaries.set(normalized.length, part.index + part.text.length);
    }
    return {text:normalized,boundaries};
}
function before(text: string, offset: number): string {
    if (!offset) return '';
    const last = text.charCodeAt(offset - 1);
    return text.slice(Math.max(0, offset - (last >= 0xdc00 && last <= 0xdfff ? 2 : 1)), offset);
}
function after(text: string, offset: number): string {
    const value = text.codePointAt(offset);
    return value === undefined ? '' : String.fromCodePoint(value);
}
function literalRanges(source: NormalizedSource, text: string, caseSensitive: boolean, wholeWord: boolean): {start:number;end:number}[] {
    if (!text) return [];
    const pattern = new RegExp(escapePattern(text.normalize('NFC')), caseSensitive ? 'gu' : 'giu');
    const ranges: {start:number;end:number}[] = [];
    for (const match of source.text.matchAll(pattern)) {
        const start = match.index!, end = start + match[0].length;
        if (wholeWord && (wordCharacter(before(source.text,start)) || wordCharacter(after(source.text,end)))) continue;
        const originalStart = source.boundaries ? source.boundaries.get(start) : isUtf16Boundary(source.text,start) ? start : undefined;
        const originalEnd = source.boundaries ? source.boundaries.get(end) : isUtf16Boundary(source.text,end) ? end : undefined;
        if (originalStart !== undefined && originalEnd !== undefined) ranges.push({start:originalStart,end:originalEnd});
        if (ranges.length > 5000) throw new Error('This preview has more than 5,000 occurrences. Choose a smaller page scope.');
    }
    return ranges;
}
function languageMatches(entry: VocabularyEntry, language: string): boolean {
    return entry.language === language || (entry.kind === 'protected' && entry.language === 'und');
}
function effectiveEntries(project: OcrProject, user: ProofreadingData, language: string): VocabularyEntry[] {
    return [...readProofreadingData(project.proofreading).entries,...readProofreadingData(user).entries]
        .filter(entry => languageMatches(entry, language));
}
function protectedRanges(source: NormalizedSource, entries: VocabularyEntry[]): {start:number;end:number}[] {
    const ranges:{start:number;end:number}[]=[],seen=new Set<string>();
    let work=0;
    for(const entry of entries.filter(entry=>entry.kind==='protected')) {
        const text=entry.text.normalize('NFC'),key=JSON.stringify([text,entry.caseSensitive]);
        if(seen.has(key) || text.length>source.text.length) continue;
        seen.add(key); work+=source.text.length;
        if(work>25000000) throw new Error('The protected-vocabulary scan is too large. Choose a smaller passage or reduce protected entries.');
        ranges.push(...literalRanges(source,text,entry.caseSensitive,true));
        if(ranges.length>5000) throw new Error('This preview contains more than 5,000 protected matches. Choose a smaller passage or page scope.');
    }
    return ranges;
}
const overlaps = (a:{start:number;end:number}, b:{start:number;end:number}) => a.start < b.end && a.end > b.start;
const policy = (project: OcrProject, user: ProofreadingData): string => JSON.stringify([readProofreadingData(project.proofreading), readProofreadingData(user)]);

export function updateProjectProofreading(project: OcrProject, data: ProofreadingData): OcrProject {
    return {...project, proofreading:readProofreadingData(data), updatedAt:new Date().toISOString()};
}

function selectedSource(project: OcrProject, selection: ProofreadingSelection): {page:OcrPage;region:OcrRegion} {
    const page = project.pages.find(page => page.id === selection.pageId);
    const region = page?.regions.find(region => region.id === selection.regionId);
    if (!page || page.excluded || !region || !editable(region) || !Number.isInteger(selection.start) ||
        !Number.isInteger(selection.end) || selection.start < 0 || selection.end <= selection.start || selection.end > region.text.length ||
        !isUtf16Boundary(region.text,selection.start) || !isUtf16Boundary(region.text,selection.end) ||
        region.text.slice(selection.start,selection.end) !== selection.text) throw new Error('The selected word changed. Select its current text again.');
    return {page,region};
}

export function isSuggestionIgnored(project: OcrProject, selection: ProofreadingSelection): boolean {
    const page = project.pages.find(page => page.id === selection.pageId);
    const region = page?.regions.find(region => region.id === selection.regionId);
    return !!page && !!region && readProofreadingData(project.proofreading).ignored.some(entry =>
        entry.pageId === page.id && entry.regionId === region.id && entry.imagePath === page.imagePath &&
        entry.sourceText === region.text && entry.start === selection.start && entry.end === selection.end && entry.text === selection.text);
}

export function ignoreSuggestion(project: OcrProject, selection: ProofreadingSelection): OcrProject {
    const {page,region} = selectedSource(project,selection), data = readProofreadingData(project.proofreading);
    if (isSuggestionIgnored(project,selection)) return project;
    data.ignored = data.ignored.filter(entry => entry.regionId !== region.id ||
        (entry.sourceText === region.text && entry.imagePath === page.imagePath));
    data.ignored.push({...selection,imagePath:page.imagePath,sourceText:region.text});
    return updateProjectProofreading(project,data);
}

function distance(a: string, b: string): number {
    const left=Array.from(a),right=Array.from(b);
    if (Math.abs(left.length-right.length)>2 || left.length>64 || right.length>64) return 3;
    let row=right.map((_,i)=>i+1); row.unshift(0);
    for(let i=0;i<left.length;i++) {
        const next=[i+1];
        for(let j=0;j<right.length;j++) next.push(Math.min(next[j]+1,row[j+1]+1,row[j]+(left[i]===right[j]?0:1)));
        if(Math.min(...next)>2) return 3;
        row=next;
    }
    return row[right.length];
}
const confusionKey = (text: string) => text.replace(/rn/g,'m').replace(/cl/g,'d').replace(/0/g,'o').replace(/1/g,'l');
const identifierLike = (text: string) => /[_@:/\\]/u.test(text) || /\d.*\d/u.test(text) ||
    /^[\p{Sc}\d+−-]/u.test(text) || (/\d/u.test(text) && !/\p{Ll}/u.test(text)) || /\p{L}[-.]\d/u.test(text);

export function suggestCorrections(project: OcrProject, selection: ProofreadingSelection, user: ProofreadingData, requestedLanguage: string): CorrectionSuggestion[] {
    let source: {page:OcrPage;region:OcrRegion};
    try { source=selectedSource(project,selection); } catch { return []; }
    if(isSuggestionIgnored(project,selection)) return [];
    const language=normalizeProofreadingLanguage(requestedLanguage), entries=effectiveEntries(project,user,language);
    if(protectedRanges(normalizedSource(source.region.text),entries).some(range=>overlaps(range,selection))) return [];
    const suggestions: CorrectionSuggestion[]=[];
    const add=(text:string,kind:CorrectionSuggestion['source'],label:string,confidence:number|null=null)=>{
        if(text !== selection.text && text.trim() && !suggestions.some(item=>item.text===text)) suggestions.push({id:`${kind}:${text}`,text,source:kind,label,confidence});
    };
    if (!identifierLike(selection.text)) {
        const original = getWordEvidence(source.region).find(word=>word.start===selection.start && word.end===selection.end && word.text===selection.text);
        for(const alternative of original?.alternatives ?? []) add(alternative.text,'engine','Engine alternative',alternative.confidence);
        const spelling=selection.text.normalize('NFC');
        const folded=(text:string)=>text.toLocaleLowerCase(language==='und'?undefined:language);
        const known=entries.some(entry=>entry.kind==='word' && (entry.caseSensitive
            ? entry.text.normalize('NFC')===spelling : folded(entry.text.normalize('NFC'))===folded(spelling)));
        const candidates=(known ? [] : entries.filter(entry=>entry.kind==='word')).flatMap(entry=>{
            const key=entry.caseSensitive ? spelling : folded(spelling);
            const candidate=entry.caseSensitive ? entry.text.normalize('NFC') : folded(entry.text.normalize('NFC'));
            const difference=distance(key,candidate);
            if(!difference || (difference>1 && confusionKey(key)!==confusionKey(candidate))) return [];
            return [{entry,difference}];
        }).sort((a,b)=>a.difference-b.difference || a.entry.text.localeCompare(b.entry.text));
        for(const {entry} of candidates.slice(0,8)) add(entry.text,'vocabulary','Vocabulary suggestion');
    }
    for(const rule of [...readProofreadingData(project.proofreading).rules,...readProofreadingData(user).rules]) {
        if(!rule.enabled || rule.language!==language) continue;
        if(literalRanges(normalizedSource(selection.text),rule.from,rule.caseSensitive,rule.wholeWord)
            .some(range=>range.start===0 && range.end===selection.text.length)) add(rule.to,'rule',`Saved rule: ${rule.name}`);
    }
    return suggestions.slice(0,12);
}

export function captureCorrectionPreview(project: OcrProject, input: CorrectionRequest, user: ProofreadingData): CorrectionPreview {
    const request=clone(input);
    request.language=normalizeProofreadingLanguage(request.language);
    if(typeof request.from!=='string' || !request.from || request.from.length>1000 || typeof request.to!=='string' || request.to.length>10000 ||
        request.from===request.to || /[\u0000-\u0008\u000b\u000c\u000e-\u001f]/u.test(request.from+request.to) ||
        typeof request.caseSensitive!=='boolean' || typeof request.wholeWord!=='boolean' ||
        !['passage','page','selected-pages','project'].includes(request.scope)) throw new Error('Choose a valid literal correction and scope.');
    if((request.scope==='passage' && (!request.pageId || !request.regionId)) || (request.scope==='page' && !request.pageId) ||
        (request.scope==='selected-pages' && !request.pageIds?.length)) throw new Error('Select the passage or pages to preview.');
    if(request.selection) selectedSource(project,request.selection);
    const entries=effectiveEntries(project,user,request.language);
    const preview: CorrectionPreview={projectId:project.id,request,policy:policy(project,user),targets:[],matches:[],skippedProtected:0,skippedApproved:0,skippedExcluded:0};
    let scanned=0;
    for(const page of project.pages) {
        if((['page','passage'].includes(request.scope) && page.id!==request.pageId) ||
            (request.scope==='selected-pages' && !request.pageIds!.includes(page.id))) continue;
        for(const region of page.regions) {
            if(request.scope==='passage' && region.id!==request.regionId) continue;
            if(request.selection && (page.id!==request.selection.pageId || region.id!==request.selection.regionId)) continue;
            if(page.excluded || !editable(region)) { preview.skippedExcluded++; continue; }
            scanned+=region.text.length;
            if(scanned>1000000) throw new Error('This preview contains more than one million characters. Choose a smaller page scope.');
            const normalized=normalizedSource(region.text);
            const ranges=literalRanges(normalized,request.from,request.caseSensitive,request.wholeWord)
                .filter(range=>!request.selection || (range.start===request.selection.start && range.end===request.selection.end));
            if(!request.includeApproved && (page.approved || region.review==='approved')) { preview.skippedApproved+=ranges.length; continue; }
            if(!ranges.length) continue;
            const protectedSpans=protectedRanges(normalized,entries);
            let hasMatch=false;
            for(const range of ranges) {
                if(protectedSpans.some(span=>overlaps(span,range))) { preview.skippedProtected++; continue; }
                preview.matches.push({id:identity(page.id,region.id,range.start,range.end),pageId:page.id,pageName:page.name,regionId:region.id,
                    ...range,before:region.text.slice(range.start,range.end),after:request.to,
                    context:region.text.slice(Math.max(0,range.start-45),Math.min(region.text.length,range.end+45))});
                hasMatch=true;
                if(preview.matches.length>5000) throw new Error('This preview has more than 5,000 occurrences. Choose a smaller page scope.');
            }
            if(hasMatch) preview.targets.push({pageId:page.id,regionId:region.id,imagePath:page.imagePath,text:region.text,
                geometry:geometry(region),width:page.width,height:page.height,kind:region.kind,approved:page.approved,review:region.review});
        }
    }
    return preview;
}

export function applyCorrectionPreview(project: OcrProject, preview: CorrectionPreview, selectedIds: string[], user: ProofreadingData): OcrProject {
    if(project.id!==preview.projectId || policy(project,user)!==preview.policy) throw new Error('Vocabulary, rules, or project changed. Preview the correction again.');
    const ids=new Set(selectedIds);
    if(ids.size!==selectedIds.length || [...ids].some(id=>!preview.matches.some(match=>match.id===id))) throw new Error('The selected correction occurrences are invalid. Preview again.');
    if(!ids.size) return project;
    const fresh=captureCorrectionPreview(project,preview.request,user);
    const freshMatches=new Map(fresh.matches.map(match=>[match.id,match]));
    const groups=new Map<string,CorrectionMatch[]>();
    for(const match of preview.matches.filter(match=>ids.has(match.id))) {
        const target=preview.targets.find(target=>target.pageId===match.pageId && target.regionId===match.regionId);
        const page=project.pages.find(page=>page.id===match.pageId), region=page?.regions.find(region=>region.id===match.regionId);
        const current=freshMatches.get(match.id);
        if(!target || !page || !region || page.imagePath!==target.imagePath || page.width!==target.width || page.height!==target.height ||
            region.text!==target.text || geometry(region)!==target.geometry || region.kind!==target.kind ||
            page.approved!==target.approved || region.review!==target.review || !current ||
            current.before!==match.before || current.after!==match.after || current.start!==match.start || current.end!==match.end) {
            throw new Error('A previewed passage or its source changed. Preview the correction again; no text was replaced.');
        }
        const key=JSON.stringify([page.id,region.id]); groups.set(key,[...(groups.get(key)??[]),match]);
    }
    let next=project;
    for(const matches of groups.values()) {
        const target=preview.targets.find(target=>target.pageId===matches[0].pageId && target.regionId===matches[0].regionId)!;
        let text=target.text,lastStart=text.length;
        for(const match of matches.sort((a,b)=>b.start-a.start)) {
            if(match.end>lastStart) throw new Error('Correction ranges overlap. Preview again.');
            text=text.slice(0,match.start)+match.after+text.slice(match.end); lastStart=match.start;
        }
        next=editRegion(next,target.pageId,target.regionId,text);
    }
    return next;
}
