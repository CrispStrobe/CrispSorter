import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { getSetting, getStore } from '../store';
import { extractText, extractionTags, MULTIMODAL_EXTENSIONS, usesNativePathExtraction } from './index';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../log', () => ({ logInfo: vi.fn(), logWarn: vi.fn(), logDebug: vi.fn() }));
vi.mock('../store', () => ({ getSetting: vi.fn(), getStore: vi.fn() }));

const extracted = { text: 'Extracted source text.', headings: ['Source'], tags: ['extraction:native-eml', 'extraction:attachment-name-only'] };
const defaultPipeline = { enabled: false, engine: 'auto', language: 'eng', cpu_pixel_budget: false, stages: [] };
const input = (extension: string, path = `/documents/source.${extension}`) => ({
    name: `source.${extension}`, path, arrayBuffer: new ArrayBuffer(0),
});

beforeEach(() => {
    vi.mocked(invoke).mockReset().mockResolvedValue(extracted);
    vi.mocked(getSetting).mockReset().mockImplementation(async (_key, fallback) => fallback);
    vi.mocked(getStore).mockReset().mockResolvedValue({ get: async () => undefined } as unknown as Awaited<ReturnType<typeof getStore>>);
});

describe('direct native mail and presentation extraction', () => {
    it.each(['eml', 'mbox', 'PPTX'])('accepts %s and dispatches only its host path', async extension => {
        expect(MULTIMODAL_EXTENSIONS).toContain(extension.toLowerCase());
        expect(usesNativePathExtraction(`source.${extension}`)).toBe(true);
        const result = await extractText(input(extension));
        expect(invoke).toHaveBeenCalledExactlyOnceWith('native_extract_text', { path: `/documents/source.${extension}`,
            ...(extension.toLowerCase() === 'pptx' ? {} : { options: { tryOcr: false, ocrTier: 'auto', ocrRecLang: 'auto', ocrPipeline: defaultPipeline } }) });
        expect(result).toEqual({ text: extracted.text, headings: extracted.headings, metadata: { tags: extracted.tags } });
        expect(extractionTags(result)).toContain('extraction:attachment-name-only');
    });

    it('does not advertise additional mail, presentation, or archive families', () => {
        for (const extension of ['ppt', 'msg', 'pst', 'ost', 'odp', 'rar']) {
            expect(MULTIMODAL_EXTENSIONS).not.toContain(extension);
            expect(usesNativePathExtraction(`source.${extension}`)).toBe(false);
        }
        expect(usesNativePathExtraction('source.ZIP')).toBe(true);
    });

    it('requires a local host path without reading dropped in-memory files', async () => {
        for (const extension of ['eml', 'mbox', 'pptx']) {
            const file = new File(['source bytes'], `source.${extension}`);
            const read = vi.spyOn(file, 'arrayBuffer');
            await expect(extractText(file)).rejects.toThrow('local file-system path');
            expect(read).not.toHaveBeenCalled();
        }
        for (const path of ['', 'relative.eml', 'content://provider/mail.eml', 'file:///mail.eml', 'https://example.test/mail.eml', '/invalid\0.eml']) {
            await expect(extractText(input('eml', path))).rejects.toThrow('local file-system path');
        }
        expect(invoke).not.toHaveBeenCalled();
    });

    it('does not send an unsupported OCR option to text-only readers', async () => {
        await extractText(input('pptx'), { forceOCR: true });
        expect(invoke).toHaveBeenCalledExactlyOnceWith('native_extract_text', { path: '/documents/source.pptx' });
    });

    it('honors OCR choices for document attachments without changing the source message body', async () => {
        vi.mocked(getSetting).mockImplementation(async (key, fallback) => key === 'ocrTier' ? 'tier3' : fallback);
        await extractText(input('eml'), { forceOCR: true });
        expect(invoke).toHaveBeenCalledExactlyOnceWith('native_extract_text', { path: '/documents/source.eml',
            options: { tryOcr: true, ocrTier: 'tier3', ocrRecLang: 'auto', ocrPipeline: { ...defaultPipeline, language: 'auto' } } });
    });

    it('never starts an already cancelled extraction and discards late native results', async () => {
        const cancelled = new AbortController(); cancelled.abort();
        await expect(extractText(input('eml'), { signal: cancelled.signal })).rejects.toMatchObject({ name: 'AbortError' });
        expect(invoke).not.toHaveBeenCalled();
        let finish!: (value: typeof extracted) => void;
        vi.mocked(invoke).mockImplementation(() => new Promise(resolve => { finish = resolve as typeof finish; }));
        const controller = new AbortController();
        const pending = extractText(input('mbox'), { signal: controller.signal });
        const outcome = expect(pending).rejects.toThrow('Extraction stopped');
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledOnce());
        controller.abort(new Error('Extraction stopped')); finish(extracted);
        await outcome;
    });

    it('preserves native errors and sanitizes coverage arrays', async () => {
        vi.mocked(invoke).mockRejectedValueOnce(new Error('Presentation expanded-byte limit exceeded'));
        await expect(extractText(input('pptx'))).rejects.toThrow('expanded-byte limit');
        vi.mocked(invoke).mockResolvedValueOnce({ text: 'Body', headings: ['Title', null, 2], tags: ['extraction:text-only', null, '', 'extraction:text-only'] });
        expect(await extractText(input('pptx'))).toEqual({ text: 'Body', headings: ['Title'], metadata: { tags: ['extraction:text-only'] } });
        vi.mocked(invoke).mockResolvedValueOnce(null);
        await expect(extractText(input('pptx'))).rejects.toThrow('no usable response');
    });
});
