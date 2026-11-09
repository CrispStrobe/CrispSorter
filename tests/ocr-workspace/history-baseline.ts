// Frozen comparison algorithm from CrispSorter 78edd8101bee3651b59cc50af183385527cd92bc.
// Kept only for the reproducible history benchmark; never imported by the application.
import type { History, OcrProject } from "../../src/lib/ocr/model";
const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value));

/** History is bounded by count and UTF-8 snapshot bytes, excluding the live project. */
export function createHistory(project: OcrProject, limit = 50, maxBytes = 24 * 1024 * 1024): History {
    return {past:[],present:clone(project),future:[],limit:Math.max(1,Math.min(200,Math.floor(limit)||50)),maxBytes:Math.max(0,Math.floor(maxBytes))};
}
function boundedHistory(history: History): History {
    const past = history.past.slice(-history.limit), future = history.future.slice(0,history.limit);
    const encoder = new TextEncoder();
    const size = (project:OcrProject) => encoder.encode(JSON.stringify(project)).byteLength;
    const pastSizes=past.map(size), futureSizes=future.map(size);
    let bytes=[...pastSizes,...futureSizes].reduce((sum,value)=>sum+value,0);
    while (bytes>history.maxBytes && (past.length || future.length)) {
        // Retain the nearest edit on each side for as long as possible.
        if (past.length >= future.length) { past.shift(); bytes-=pastSizes.shift()!; }
        else { future.pop(); bytes-=futureSizes.pop()!; }
    }
    return {...history,past,future};
}
export function commitHistory(history: History, project: OcrProject): History {
    if (project === history.present || JSON.stringify(project) === JSON.stringify(history.present)) return history;
    return boundedHistory({...history,past:[...history.past,clone(history.present)],present:clone(project),future:[]});
}
export function undoHistory(history: History): History {
    if (!history.past.length) return history;
    return boundedHistory({...history,past:history.past.slice(0,-1),present:clone(history.past[history.past.length-1]),future:[clone(history.present),...history.future]});
}
export function redoHistory(history: History): History {
    if (!history.future.length) return history;
    return boundedHistory({...history,past:[...history.past,clone(history.present)],present:clone(history.future[0]),future:history.future.slice(1)});
}

