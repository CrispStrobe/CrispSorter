import { expect, it } from 'vitest';
import { availableDocumentTools, inWorkflow, processingOverrides } from './workflow';
it('keeps pending files and reviewed files in the appropriate work queue', () => {
    expect(inWorkflow({ status: 'queued' }, 'inbox')).toBe(true);
    expect(inWorkflow({ status: 'ready' }, 'inbox')).toBe(false);
    expect(inWorkflow({ status: 'ready' }, 'review')).toBe(true);
    expect(inWorkflow({ status: 'done' }, 'review')).toBe(false);
    expect(inWorkflow({ status: 'error' }, 'inbox')).toBe(true);
});
it('preserves advanced settings and only offers tools compatible with a document', () => {
    expect(processingOverrides('advanced')).toBeUndefined();
    expect(processingOverrides('quality')!.maxChars).toBeGreaterThan(processingOverrides('fast')!.maxChars);
    expect(availableDocumentTools('report.pdf')).toEqual(['ocr', 'pdf']);
    expect(availableDocumentTools('book.docx')).toEqual(['translate']);
    expect(availableDocumentTools('audio.mp3')).toEqual([]);
});
