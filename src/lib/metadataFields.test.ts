import { expect, it } from 'vitest';
import { applyFieldTemplate, fieldsCsv, approvedValues, type MetadataField } from './metadataFields';
const field: MetadataField = { id: 'a', label: 'vendor', original_value: '=cmd()', normalized_value: '../Acme', model: 'test', confidence: 0.8, start_char: 2, end_char: 8, page: null, anchor_method: 'first_exact_match', review_status: 'suggested', revision: 0 };
it('uses only reviewed fields and prevents field values from creating path components', () => {
    expect(approvedValues([field])).toEqual({});
    expect(applyFieldTemplate('{Field:vendor}/{Title}', approvedValues([{ ...field, review_status: 'accepted' }]))).toBe('.._Acme/{Title}');
    expect(() => applyFieldTemplate('{Field:vendor}', {})).toThrow(/approve/);
    expect(applyFieldTemplate('{Field:vendor}', { vendor: 'CON' })).toBe('_CON');
});
it('exports original evidence while neutralizing spreadsheet formulas', () => {
    expect(fieldsCsv([field])).toContain("\"'=cmd()\"");
    expect(fieldsCsv([field])).toContain('first_exact_match');
});
