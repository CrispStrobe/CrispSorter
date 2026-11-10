import { readExtractionObjects, serializeExtractionObject, type ExtractionObject } from './extractionObjects';
import { tableDataToCsv } from './tableData';

/** The selected file's extension and content always come from one format decision. */
export function prepareExtractionExport(object: ExtractionObject): {extension:'json'|'csv'|'tex';text:string} {
    const checked=readExtractionObjects({version:1,objects:[object]})!.objects[0];
    if(checked.kind==='table') return {extension:'csv',text:tableDataToCsv(checked.currentData)};
    if(checked.kind==='formula') return {extension:'tex',text:checked.currentData.latex};
    return {extension:'json',text:serializeExtractionObject(checked)};
}
