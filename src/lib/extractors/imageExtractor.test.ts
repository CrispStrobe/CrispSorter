import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createOcrSession } from './ocrDispatcher';
import { extractImage } from './imageExtractor';
import { extractText } from './index';

vi.mock('./ocrDispatcher', () => ({ createOcrSession: vi.fn() }));
vi.mock('../log', () => ({ logInfo: vi.fn(), logWarn: vi.fn(), logDebug: vi.fn() }));
const recognize = vi.fn();
beforeEach(() => {
    recognize.mockReset().mockResolvedValue({ text: 'Recognized text', engine: 'qwen2vl', warnings: ['Review uncertain words'] });
    vi.mocked(createOcrSession).mockReset().mockResolvedValue({ recognize } as unknown as Awaited<ReturnType<typeof createOcrSession>>);
});

describe('image extraction policy and provenance', () => {
    it('forwards bytes, image type and the full selected policy without object URLs', async () => {
        const options = { ocr: { engine: 'qwen2vl', language: 'deu', cpuPixelBudget: true, config: { stages: [{ rec_model: 'custom' }] } } };
        const result = await extractImage(new Uint8Array([1, 2, 3]).buffer, 'PHOTO.WEBP', options);
        expect(createOcrSession).toHaveBeenCalledExactlyOnceWith(options);
        const blob = recognize.mock.calls[0][0] as Blob;
        expect(blob.type).toBe('image/webp'); expect([...new Uint8Array(await blob.arrayBuffer())]).toEqual([1, 2, 3]);
        expect(recognize.mock.calls[0][1]).toBe('PHOTO.WEBP');
        expect(result).toEqual({ text: 'Recognized text', markdownText: 'Recognized text', headings: [],
            metadata: { ocr: { engine: 'qwen2vl', warnings: ['Review uncertain words'] } } });
    });

    it('retains the old explicit language argument', async () => {
        await extractImage(new ArrayBuffer(1), 'page.jpg', 'eng+deu');
        expect(createOcrSession).toHaveBeenCalledExactlyOnceWith({ ocr: { language: 'eng+deu' } });
        expect((recognize.mock.calls[0][0] as Blob).type).toBe('image/jpeg');
    });

    it('passes extraction options and actual engine metadata through extractText', async () => {
        const options = { signal: new AbortController().signal, ocr: { engine: 'surya', language: 'fra' } };
        const result = await extractText({ name: 'scan.png', arrayBuffer: new ArrayBuffer(1) }, options);
        expect(createOcrSession).toHaveBeenCalledExactlyOnceWith(options);
        expect(result.metadata?.ocr).toEqual({ engine: 'qwen2vl', warnings: ['Review uncertain words'] });
    });

    it('propagates selected-engine errors instead of returning empty text', async () => {
        const failure = new Error('Selected model unavailable'); recognize.mockRejectedValue(failure);
        await expect(extractImage(new ArrayBuffer(1), 'scan.png')).rejects.toBe(failure);
    });
});
