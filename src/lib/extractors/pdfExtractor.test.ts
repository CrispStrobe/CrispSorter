import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf';
import { createOcrSession } from './ocrDispatcher';
import { extractPdf } from './pdfExtractor';

vi.mock('pdfjs-dist/legacy/build/pdf', () => ({
    GlobalWorkerOptions: { workerSrc: '' }, getDocument: vi.fn(),
    OPS: { paintImageXObject: 85, paintInlineImageXObject: 86, fill: 22, showText: 44 },
}));
vi.mock('./ocrDispatcher', async importOriginal => ({
    ...await importOriginal<typeof import('./ocrDispatcher')>(), createOcrSession: vi.fn(),
}));
vi.mock('../log', () => ({ logInfo: vi.fn() }));

const digital = 'This page already contains useful digital text.';
const item = (str: string, x = 10, y = 100) => ({ str, transform: [1, 0, 0, 1, x, y] });
function page(text = '', operations: number[] = [85], width = 600, height = 800) {
    return {
        getTextContent: vi.fn().mockResolvedValue({ items: text ? [item(text)] : [] }),
        getOperatorList: vi.fn().mockResolvedValue({ fnArray: operations }),
        getViewport: vi.fn().mockImplementation(({ scale }: { scale: number }) => ({ width: width * scale, height: height * scale })),
        render: vi.fn().mockReturnValue({ promise: Promise.resolve(), cancel: vi.fn() }),
        cleanup: vi.fn(),
    };
}
let recognize: ReturnType<typeof vi.fn>;
let destroy: ReturnType<typeof vi.fn>;
let getPage: ReturnType<typeof vi.fn>;
let canvases: { width: number; height: number; getContext: ReturnType<typeof vi.fn> }[];
function documentWith(pages: ReturnType<typeof page>[]) {
    getPage = vi.fn().mockImplementation(async (index: number) => pages[index - 1]);
    vi.mocked(pdfjsLib.getDocument).mockReturnValue({
        promise: Promise.resolve({ numPages: pages.length, getPage }), destroy,
    } as unknown as ReturnType<typeof pdfjsLib.getDocument>);
}
function enableOcr(enabled: boolean) {
    vi.mocked(createOcrSession).mockResolvedValue({ policy: { enabled }, recognize } as unknown as Awaited<ReturnType<typeof createOcrSession>>);
}

beforeEach(() => {
    vi.mocked(pdfjsLib.getDocument).mockReset(); vi.mocked(createOcrSession).mockReset();
    recognize = vi.fn().mockResolvedValue({ text: 'OCR page', engine: 'selected-engine', warnings: [] });
    destroy = vi.fn().mockResolvedValue(undefined); canvases = [];
    enableOcr(true);
    vi.stubGlobal('document', { createElement: vi.fn().mockImplementation(() => {
        const canvas = { width: 0, height: 0, getContext: vi.fn().mockReturnValue({}) };
        canvases.push(canvas); return canvas;
    }) });
});
afterEach(() => vi.unstubAllGlobals());

describe('PDF text and OCR decisions', () => {
    it('preserves digital text without rendering or inspecting large text-only pages', async () => {
        const pages = [page(digital), page('A second usable digital text layer.')]; documentWith(pages);
        expect(await extractPdf(new ArrayBuffer(1))).toBe(`${digital}\nA second usable digital text layer.`);
        expect(createOcrSession).toHaveBeenCalledOnce(); expect(recognize).not.toHaveBeenCalled();
        for (const current of pages) { expect(current.getOperatorList).not.toHaveBeenCalled(); expect(current.cleanup).toHaveBeenCalledOnce(); }
        expect(destroy).toHaveBeenCalledOnce();
    });

    it('does not OCR short digital titles or blank pages without visible drawing operations', async () => {
        const pages = [page('Title', [44]), page('', [])]; documentWith(pages);
        expect(await extractPdf(new ArrayBuffer(1))).toBe('Title');
        expect(recognize).not.toHaveBeenCalled(); expect(canvases).toHaveLength(0);
    });

    it('OCRs sparse scanned pages and outlined text with one saved policy', async () => {
        const pages = [page('Header', [85]), page('', [22])]; documentWith(pages);
        const options = { ocr: { engine: 'surya', language: 'fra' } };
        expect(await extractPdf(new ArrayBuffer(1), options)).toBe('OCR page\nOCR page');
        expect(createOcrSession).toHaveBeenCalledExactlyOnceWith(options);
        expect(recognize).toHaveBeenCalledTimes(2);
        expect(recognize.mock.calls.map(call => call[1])).toEqual(['page-1.png', 'page-2.png']);
        for (const canvas of canvases) expect([canvas.width, canvas.height]).toEqual([0, 0]);
        for (const current of pages) expect(current.cleanup).toHaveBeenCalledOnce();
    });

    it('force OCR bypasses digital text and drawing heuristics even when automatic OCR is disabled', async () => {
        enableOcr(false); const current = page(digital, []); documentWith([current]);
        expect(await extractPdf(new ArrayBuffer(1), { forceOCR: true })).toBe('OCR page');
        expect(current.getTextContent).not.toHaveBeenCalled(); expect(current.getOperatorList).not.toHaveBeenCalled();
        expect(recognize).toHaveBeenCalledOnce();
    });

    it('respects disabled OCR for scanned pages', async () => {
        enableOcr(false); const current = page('', [85]); documentWith([current]);
        expect(await extractPdf(new ArrayBuffer(1))).toBe('');
        expect(recognize).not.toHaveBeenCalled(); expect(current.getOperatorList).not.toHaveBeenCalled();
        expect(destroy).toHaveBeenCalledOnce();
    });

    it('retains sparse digital text if optional OCR returns no text', async () => {
        recognize.mockResolvedValue({ text: '', engine: 'selected-engine', warnings: [] });
        documentWith([page('Invoice', [85])]);
        expect(await extractPdf(new ArrayBuffer(1))).toBe('Invoice');
    });

    it('filters marked-content/control items and sorts usable text safely', async () => {
        enableOcr(false); const current = page();
        current.getTextContent.mockResolvedValue({ items: [{ type: 'beginMarkedContent', id: 'one' },
            item('second', 10, 20), { str: 'invalid', transform: [] }, item('first', 10, 80), item('line', 30, 80)] });
        documentWith([current]);
        expect(await extractPdf(new ArrayBuffer(1))).toBe('first line\nsecond');
    });

    it('tries OCR when text extraction fails, and never silently ignores its OCR failure', async () => {
        const current = page(); current.getTextContent.mockRejectedValue(new Error('Broken text layer')); documentWith([current]);
        expect(await extractPdf(new ArrayBuffer(1))).toBe('OCR page');
        expect(current.getOperatorList).not.toHaveBeenCalled();
    });

    it('honors hard character/page limits without loading later pages', async () => {
        const pages = [page(digital), page(digital)]; documentWith(pages);
        const progress = vi.fn();
        expect(await extractPdf(new ArrayBuffer(1), { maxPages: 1, maxChars: 7, onProgress: progress })).toBe('This pa');
        expect(getPage).toHaveBeenCalledExactlyOnceWith(1);
        expect(progress).toHaveBeenCalledExactlyOnceWith(1, 2);
        expect(pages[0].cleanup).toHaveBeenCalledOnce(); expect(pages[1].cleanup).not.toHaveBeenCalled();
        expect(destroy).toHaveBeenCalledOnce();
    });

    it('does not load a PDF when an explicit extraction budget is zero', async () => {
        expect(await extractPdf(new ArrayBuffer(1), { maxPages: 0 })).toBe('');
        expect(await extractPdf(new ArrayBuffer(1), { maxChars: 0 })).toBe('');
        expect(pdfjsLib.getDocument).not.toHaveBeenCalled();
    });

    it('bounds large page raster dimensions while releasing the canvas after recognition', async () => {
        documentWith([page('', [85], 20_000, 30_000)]);
        let rendered: { width: number; height: number } | undefined;
        recognize.mockImplementation(async (canvas: { width: number; height: number }) => {
            rendered = { width: canvas.width, height: canvas.height };
            return { text: 'large page', engine: 'selected-engine', warnings: [] };
        });
        expect(await extractPdf(new ArrayBuffer(1))).toBe('large page');
        expect(rendered!.width * rendered!.height).toBeLessThanOrEqual(12_000_000);
        expect(Math.max(rendered!.width, rendered!.height)).toBeLessThanOrEqual(8192);
        expect([canvases[0].width, canvases[0].height]).toEqual([0, 0]);
    });
});

describe('PDF failures and resource ownership', () => {
    it('destroys the loading task after a parser failure', async () => {
        const failure = new Error('Malformed PDF');
        vi.mocked(pdfjsLib.getDocument).mockReturnValue({ promise: Promise.reject(failure), destroy } as unknown as ReturnType<typeof pdfjsLib.getDocument>);
        await expect(extractPdf(new ArrayBuffer(1))).rejects.toBe(failure);
        expect(destroy).toHaveBeenCalledOnce();
    });

    it('destroys the document after a page loading failure', async () => {
        documentWith([page()]); getPage.mockRejectedValue(new Error('Page unreadable'));
        await expect(extractPdf(new ArrayBuffer(1))).rejects.toThrow('Page unreadable');
        expect(destroy).toHaveBeenCalledOnce();
    });

    it('preserves digital-text errors when OCR is disabled', async () => {
        enableOcr(false); const current = page(); current.getTextContent.mockRejectedValue(new Error('Invalid text layer')); documentWith([current]);
        await expect(extractPdf(new ArrayBuffer(1))).rejects.toThrow('Invalid text layer');
        expect(current.cleanup).toHaveBeenCalledOnce(); expect(destroy).toHaveBeenCalledOnce();
    });

    it('cleans canvas/page/document after rasterization fails', async () => {
        const current = page(); current.render.mockReturnValue({ promise: Promise.reject(new Error('Render failed')), cancel: vi.fn() });
        documentWith([current]);
        await expect(extractPdf(new ArrayBuffer(1))).rejects.toThrow('Render failed');
        expect(recognize).not.toHaveBeenCalled(); expect(current.cleanup).toHaveBeenCalledOnce(); expect(destroy).toHaveBeenCalledOnce();
        expect([canvases[0].width, canvases[0].height]).toEqual([0, 0]);
    });

    it('propagates selected-engine failure instead of returning partial or empty success', async () => {
        const current = page(); documentWith([page(digital), current]);
        const failure = new Error('Selected model is unavailable'); recognize.mockRejectedValue(failure);
        await expect(extractPdf(new ArrayBuffer(1))).rejects.toBe(failure);
        expect(current.cleanup).toHaveBeenCalledOnce(); expect(destroy).toHaveBeenCalledOnce();
        expect([canvases[0].width, canvases[0].height]).toEqual([0, 0]);
    });

    it('cancels a pending document load and destroys the loading task', async () => {
        vi.mocked(pdfjsLib.getDocument).mockReturnValue({ promise: new Promise(() => {}), destroy } as unknown as ReturnType<typeof pdfjsLib.getDocument>);
        const controller = new AbortController();
        const result = extractPdf(new ArrayBuffer(1), { signal: controller.signal });
        const stopped = expect(result).rejects.toThrow('Stop PDF');
        await vi.waitFor(() => expect(pdfjsLib.getDocument).toHaveBeenCalledOnce());
        controller.abort(new Error('Stop PDF')); await stopped;
        expect(destroy).toHaveBeenCalledOnce();
    });

    it('cancels an active render and releases its canvas, page and document', async () => {
        const current = page(); const cancel = vi.fn();
        current.render.mockReturnValue({ promise: new Promise(() => {}), cancel }); documentWith([current]);
        const controller = new AbortController();
        const stopped = expect(extractPdf(new ArrayBuffer(1), { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
        await vi.waitFor(() => expect(current.render).toHaveBeenCalledOnce());
        controller.abort(); await stopped;
        expect(cancel).toHaveBeenCalledOnce(); expect(recognize).not.toHaveBeenCalled();
        expect(current.cleanup).toHaveBeenCalledOnce(); expect(destroy).toHaveBeenCalledOnce();
        expect([canvases[0].width, canvases[0].height]).toEqual([0, 0]);
    });

    it('does not begin parsing an already cancelled extraction', async () => {
        const controller = new AbortController(); controller.abort();
        await expect(extractPdf(new ArrayBuffer(1), { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
        expect(createOcrSession).not.toHaveBeenCalled(); expect(pdfjsLib.getDocument).not.toHaveBeenCalled();
    });
});
