import { expect, it } from 'vitest';
import { availableDocumentTools, inWorkflow, processingOverrides } from './workflow';
it('keeps pending files and reviewed files in the appropriate work queue', () => {
    expect(inWorkflow({ status: 'queued' }, 'inbox')).toBe(true);
    expect(inWorkflow({ status: 'ready' }, 'inbox')).toBe(false);
    expect(inWorkflow({ status: 'ready' }, 'review')).toBe(true);
    expect(inWorkflow({ status: 'done' }, 'review')).toBe(false);
    expect(inWorkflow({ status: 'error' }, 'inbox')).toBe(true);
});
it('keeps a completed move with an unresolved index update in Inbox without returning it to sorting', () => {
    const item = { status: 'done' as const, errorMessage: 'INDEX_UPDATE_FAILED: index storage unavailable' };
    expect(inWorkflow(item, 'inbox')).toBe(true);
    expect(inWorkflow(item, 'review')).toBe(false);
    expect(inWorkflow({ status: 'done' }, 'inbox')).toBe(false);
});
it('preserves advanced settings and only offers tools compatible with a document', () => {
    expect(processingOverrides('advanced')).toBeUndefined();
    expect(processingOverrides('quality')!.maxChars).toBeGreaterThan(processingOverrides('fast')!.maxChars);
    expect(availableDocumentTools('report.pdf')).toEqual(['ocr', 'pdf']);
    expect(availableDocumentTools('book.docx')).toEqual(['translate']);
    expect(availableDocumentTools('audio.mp3')).toEqual([]);
    expect(availableDocumentTools('report.pdf', { ocr: false, pdf_render: false, ocr_tiers: [] })).toEqual(['pdf']);
    expect(availableDocumentTools('scan.png', { ocr: false, pdf_render: false, ocr_tiers: [] })).toEqual([]);
    expect(availableDocumentTools('report.pdf', { ocr: true, pdf_render: false, ocr_tiers: ['crispembed'] })).toEqual(['pdf']);
    for (const tier of ['ocrs', 'tesseract', 'paddle']) {
        expect(availableDocumentTools('scan.png', { ocr: true, pdf_render: true, ocr_tiers: [tier] })).toEqual(['ocr']);
        expect(availableDocumentTools('report.pdf', { ocr: true, pdf_render: false, ocr_tiers: [tier] })).toEqual(['pdf']);
    }
    expect(availableDocumentTools('scan.png', { ocr: false, pdf_render: true, ocr_tiers: ['ocrs'] })).toEqual([]);
});
