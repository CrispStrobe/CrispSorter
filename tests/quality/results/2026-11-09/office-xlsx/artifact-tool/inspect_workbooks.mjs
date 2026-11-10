import fs from 'node:fs/promises';
import path from 'node:path';
import { FileBlob, SpreadsheetFile } from '@oai/artifact-tool';

// Independent read/render only. The Rust production writer authors every XLSX.
const source = '/workspace/scratch/82243b18126c/ocr06b-qualified';
const output = '/workspace/scratch/82243b18126c/ocr06b-reader';
const fixtures = JSON.parse(await fs.readFile(path.join(source, 'workbooks.json'), 'utf8')).fixtures;
const report = { reader: '@oai/artifact-tool from the bundled primary runtime', mode: 'read-only import and render', files: [] };
for (const fixture of fixtures) {
  const workbook = await SpreadsheetFile.importXlsx(await FileBlob.load(path.join(source, fixture.file)));
  const file = { file: fixture.file, summary: (await workbook.inspect({ kind: 'sheet', include: 'id,name', maxChars: 4000 })).ndjson, sheets: [] };
  report.files.push(file);
  for (let index = 0; index < fixture.expected.sheets.length; index++) {
    const expected = fixture.expected.sheets[index];
    const sheet = workbook.worksheets.getItemAt(index);
    const result = { expectedName: expected.name, actualName: sheet.name, cellsChecked: 0, mismatches: [], formulas: [], renders: [] };
    file.sheets.push(result);
    if (expected.table.rows && expected.table.columns) {
      const range = sheet.getRangeByIndexes(0, 0, expected.table.rows, expected.table.columns);
      const values = range.values;
      const formulas = range.formulas;
      for (const cell of expected.table.cells) {
        const actual = values[cell.row]?.[cell.column];
        result.cellsChecked++;
        if (actual !== cell.text) result.mismatches.push({ row: cell.row + 1, column: cell.column + 1, expected: cell.text, actual: actual ?? null });
      }
      for (let row = 0; row < formulas.length; row++) for (let column = 0; column < (formulas[row]?.length ?? 0); column++) {
        if (formulas[row][column]) result.formulas.push({ row: row + 1, column: column + 1, formula: formulas[row][column] });
      }
    }
    const ranges = expected.table.columns === 64 ? ['A1:H3', 'BE1:BL3']
      : expected.table.rows === 8 ? ['A1:B8']
      : expected.table.columns === 3 ? ['A1:C5'] : ['A1:D6'];
    for (let view = 0; view < ranges.length; view++) {
      const name = fixture.file.replace('.xlsx', '') + '-sheet-' + (index + 1) + '-view-' + (view + 1) + '.png';
      try {
        const preview = await workbook.render({ sheetName: sheet.name, range: ranges[view], scale: 2, format: 'png' });
        await fs.writeFile(path.join(output, name), new Uint8Array(await preview.arrayBuffer()));
        result.renders.push({ range: ranges[view], file: name });
      } catch (error) {
        result.renders.push({ range: ranges[view], error: String(error) });
      }
    }
  }
}
await fs.writeFile(path.join(output, 'artifact-reader.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify(report.files.map(file => ({ file: file.file, sheets: file.sheets.map(sheet => ({ name: sheet.actualName, cells: sheet.cellsChecked, mismatches: sheet.mismatches.length, formulas: sheet.formulas.length, renders: sheet.renders })) })), null, 2));
