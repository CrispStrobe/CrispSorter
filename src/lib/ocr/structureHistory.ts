import type { OcrPage, OcrRegion } from './model';
import { readPageStructure, type PageStructure } from './structureData';

export interface StructureSnapshotRegion extends Pick<OcrRegion,'id'|'text'|'kind'|'x'|'y'|'w'|'h'|'excluded'> {
    separatorBefore?:OcrRegion['separatorBefore'];
}
export interface StructureSnapshot {
    capturedAt:string;pageId:string;imagePath:string;width:number;height:number;
    structure:PageStructure;regions:StructureSnapshotRegion[];
}
export interface StructureHistory { version:1;snapshots:StructureSnapshot[]; }
const MAX_BYTES=16*1024*1024,MAX_SNAPSHOTS=8;
const encoder=new TextEncoder();
const size=(value:unknown)=>encoder.encode(JSON.stringify(value)).byteLength;
const clone=<T>(value:T):T=>JSON.parse(JSON.stringify(value)) as T;
const object=(value:unknown):value is Record<string,unknown>=>!!value&&typeof value==='object'&&!Array.isArray(value);
const fail=():never=>{throw new Error('Invalid or oversized structure history. Existing text and structure were preserved.');};

/** Historical logical text has its own captured image; it is never current OCR evidence. */
export function readStructureHistory(raw:unknown):StructureHistory|undefined {
    if(raw===undefined)return undefined;
    if(!object(raw)||Object.keys(raw).some(key=>!['version','snapshots'].includes(key))||raw.version!==1||
        !Array.isArray(raw.snapshots)||raw.snapshots.length>MAX_SNAPSHOTS||size(raw)>MAX_BYTES)return fail();
    const snapshots=(raw.snapshots as unknown[]).map((entry):StructureSnapshot=>{
        if(!object(entry)||Object.keys(entry).some(key=>!['capturedAt','pageId','imagePath','width','height','structure','regions'].includes(key))||
            typeof entry.capturedAt!=='string'||entry.capturedAt.length>64||!Number.isFinite(Date.parse(entry.capturedAt))||
            typeof entry.pageId!=='string'||!entry.pageId||entry.pageId.length>200||entry.pageId.includes('\0')||
            typeof entry.imagePath!=='string'||!entry.imagePath||entry.imagePath.length>10000||entry.imagePath.includes('\0')||
            !Number.isInteger(entry.width)||!Number.isInteger(entry.height)||(entry.width as number)<=0||(entry.height as number)<=0||
            (entry.width as number)>100000||(entry.height as number)>100000||!Array.isArray(entry.regions)||entry.regions.length>100000)fail();
        const snapshot=entry as unknown as StructureSnapshot;
        const ids=new Set<string>();
        const regions=snapshot.regions.map(region=>{
            if(!object(region)||Object.keys(region).some(key=>!['id','text','kind','x','y','w','h','excluded','separatorBefore'].includes(key))||
                typeof region.id!=='string'||!region.id.trim()||region.id.length>200||region.id.includes('\0')||ids.has(region.id)||
                typeof region.text!=='string'||typeof region.excluded!=='boolean'||!['text','title','table','picture','formula','header','footer'].includes(region.kind)||
                ![region.x,region.y,region.w,region.h].every(value=>typeof value==='number'&&Number.isFinite(value))||
                region.x<0||region.y<0||region.w<=0||region.h<=0||region.x+region.w>snapshot.width+0.01||region.y+region.h>snapshot.height+0.01||
                (region.separatorBefore!==undefined&&!['',' ','\n','\n\n'].includes(region.separatorBefore)))fail();
            ids.add(region.id);
            return {...region};
        });
        const structure=readPageStructure(snapshot.structure,{id:snapshot.pageId,regions});
        if(!structure)return fail();
        return {...snapshot,regions,structure};
    });
    return {version:1,snapshots};
}

/** Keep eight recent structural replacements within a disclosed metadata budget. */
export function archiveReplacedStructure(before:OcrPage,next:OcrPage):{history:StructureHistory|undefined;notice?:string} {
    const nextIds=new Set(next.regions.map(region=>region.id));
    // Loaded histories are checked by parseProject. Ordinary keystrokes must
    // not repeatedly clone and revalidate up to sixteen MB of archived text.
    if(!before.structure||!before.regions.some(region=>!nextIds.has(region.id)))return {history:before.provenance.structureHistory as StructureHistory|undefined};
    const history=readStructureHistory(before.provenance.structureHistory);
    const snapshot:StructureSnapshot={capturedAt:new Date().toISOString(),pageId:before.id,imagePath:before.imagePath,width:before.width,height:before.height,
        structure:clone(before.structure),regions:before.regions.map(({id,text,kind,x,y,w,h,excluded,separatorBefore})=>({id,text,kind,x,y,w,h,excluded,
            ...(separatorBefore!==undefined?{separatorBefore}:{})}))};
    if(size({version:1,snapshots:[snapshot]})>MAX_BYTES)throw new Error('This structural snapshot exceeds the 16 MB history limit. Export the current project as a checkpoint before reducing its content; existing text was preserved.');
    const snapshots=[...(history?.snapshots??[]),snapshot];let pruned=0;
    while(snapshots.length>MAX_SNAPSHOTS||size({version:1,snapshots})>MAX_BYTES){snapshots.shift();pruned++;}
    return {history:readStructureHistory({version:1,snapshots}),...(pruned?{notice:`${pruned} oldest structure snapshot${pruned===1?' was':'s were'} removed from the bounded history (eight snapshots, 16 MB maximum). Export project checkpoints to retain a longer record.`}:{})};
}
