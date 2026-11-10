export type PassageSeparator = ''|' '|'\n'|'\n\n';
export interface TextPassage { text: string; separatorBefore?: PassageSeparator; }

/** Explicit boundaries preserve a split's exact text; older regions use newline. */
export function joinRegionText(regions: readonly TextPassage[], defaultSeparator = '\n'): string {
    return regions.map((region,index)=>(index ? region.separatorBefore ?? defaultSeparator : '')+region.text).join('');
}

/** Map a browser textarea caret back into unchanged source text without rewriting CRLF. */
export function sourceOffsetForTextarea(sourceText:string,textareaValue:string,textareaOffset:number):number|null {
    if(!Number.isInteger(textareaOffset) || textareaOffset<0 || textareaOffset>textareaValue.length ||
        sourceText.replace(/\r\n?/gu,'\n')!==textareaValue) return null;
    let sourceOffset=0,normalizedOffset=0;
    while(normalizedOffset<textareaOffset) {
        sourceOffset+=sourceText[sourceOffset]==='\r' && sourceText[sourceOffset+1]==='\n' ? 2 : 1;
        normalizedOffset++;
    }
    return sourceOffset;
}

interface SpatialPassage {
    x:number;y:number;w:number;h:number;sharedSourceGroups?:string[];
}
/** Shared coverage is unresolved only while two exported members still overlap. */
export function hasUnresolvedSharedGeometry(regions: readonly SpatialPassage[]): boolean {
    const groups = new Map<string,SpatialPassage[]>();
    for(const region of regions) for(const id of region.sharedSourceGroups ?? []) {
        const members=groups.get(id) ?? [];
        if(members.some(other=>Math.min(other.x+other.w,region.x+region.w)>Math.max(other.x,region.x) &&
            Math.min(other.y+other.h,region.y+region.h)>Math.max(other.y,region.y))) return true;
        members.push(region);groups.set(id,members);
    }
    return false;
}
