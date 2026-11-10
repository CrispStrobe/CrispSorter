import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { getSetting, getStore } from '../store';
import { extractText, extractionTags, MULTIMODAL_EXTENSIONS, usesNativePathExtraction } from './index';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../store', () => ({ getSetting: vi.fn(), getStore: vi.fn() }));
vi.mock('../log', () => ({ logInfo: vi.fn(), logWarn: vi.fn(), logDebug: vi.fn() }));

const archive = { name: 'research.ZIP', path: '/research/source.zip', arrayBuffer: new ArrayBuffer(0) };
const extracted = { text: 'Member: notes.txt\nWarranty source.', headings: ['notes.txt'], tags: ['archive:zip', 'coverage:partial'] };

beforeEach(() => {
    vi.mocked(invoke).mockReset().mockResolvedValue(extracted);
    vi.mocked(getSetting).mockReset().mockImplementation(async (_key, fallback) => fallback);
    vi.mocked(getStore).mockReset();
});

describe('ZIP extraction bridge', () => {
    it.each(['tar', 'gz', 'gzip', 'tgz', '7z'])('routes %s through the same bounded native pipeline', async extension => {
        expect(MULTIMODAL_EXTENSIONS).toContain(extension);
        expect(usesNativePathExtraction(`source.${extension}`)).toBe(true);
        await extractText({ name: `source.${extension}`, path: `/records/source.${extension}`, arrayBuffer: new ArrayBuffer(0) });
        expect(invoke).toHaveBeenCalledExactlyOnceWith('archive_extract_text', { path: `/records/source.${extension}`,
            options: { tryOcr: false, ocrTier: 'auto', ocrRecLang: 'auto' } });
    });
    it('accepts ZIP and uses a bounded native path request while preserving provenance and coverage tags', async () => {
        expect(MULTIMODAL_EXTENSIONS).toContain('zip');
        const result = await extractText(archive);
        expect(invoke).toHaveBeenCalledExactlyOnceWith('archive_extract_text', {
            path: '/research/source.zip', options: { tryOcr: false, ocrTier: 'auto', ocrRecLang: 'auto' },
        });
        expect(result).toEqual({ text: extracted.text, headings: ['notes.txt'], metadata: { tags: extracted.tags } });
        expect(extractionTags(result)).toEqual(['archive:zip', 'coverage:partial']);
    });

    it('requires a real host path without loading an in-memory archive into the webview', async () => {
        const file = new File(['archive bytes'], 'notes.zip');
        const read = vi.spyOn(file, 'arrayBuffer');
        await expect(extractText(file)).rejects.toThrow('local file-system path');
        expect(read).not.toHaveBeenCalled();
        for (const path of [undefined, '', 'content://provider/archive.zip', 'file:///archive.zip', 'https://example.test/archive.zip']) {
            await expect(extractText({ ...archive, path })).rejects.toThrow('local file-system path');
        }
        expect(invoke).not.toHaveBeenCalled();
    });

    it('honors saved OCR preferences and an explicit OCR override', async () => {
        const settings: Record<string, unknown> = { ocrEnabled: true, ocrTier: 'tier3', ocrRecLang: 'cjk' };
        vi.mocked(getSetting).mockImplementation(async (key, fallback) => settings[key] ?? fallback);
        await extractText(archive);
        expect(invoke).toHaveBeenLastCalledWith('archive_extract_text', {
            path: archive.path, options: { tryOcr: true, ocrTier: 'tier3', ocrRecLang: 'cjk' },
        });
        settings.ocrEnabled = false;
        settings.ocrTier = 'invalid'; settings.ocrRecLang = 'invalid';
        await extractText(archive, { forceOCR: true });
        expect(invoke).toHaveBeenLastCalledWith('archive_extract_text', {
            path: archive.path, options: { tryOcr: true, ocrTier: 'auto', ocrRecLang: 'auto' },
        });
    });

    it('never starts a native extraction for an already cancelled request', async () => {
        const controller = new AbortController(); controller.abort();
        await expect(extractText(archive, { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
        expect(invoke).not.toHaveBeenCalled();
    });

    it('preserves enabled Smart OCR routing, preprocessing, models and configured stages', async () => {
        const settings: Record<string, unknown> = {
            ocrEnabled: true, ocrPipelineEnabled: true, ocrPipelineRouter: false,
            ocrPipelineCleanup: false, ocrPipelineDenoise: true, ocrPipelineMinChars: 14, ocrPipelineMinConf: .7,
            ocrPipelineLayout: true, ocrPipelineLayoutEngine: 'custom-layout', ocrPipelineLayoutThr: .4,
            ocrPipelineDropHF: true, ocrPipelineSr: true, ocrPipelineSrEngine: 'custom-sr', ocrPipelineSrMaxPx: 1800,
            ocrPipelineRestore: true, ocrPipelineRestoreEngine: 'custom-restore', ocrPipelineRestoreTask: 'deblur',
            ocrPipelineDewarp: true, ocrPipelineDewarpEngine: 'custom-dewarp',
            ocrPipelinePunct: '/models/punctuation.gguf', ocrPipelineVlmModel: '/models/vision.gguf',
            ocrPipelineVlmEngine: 'custom-vlm', ocrPipelineTruecase: '/models/truecase.gguf',
            ocrPipelineAdvanced: true, ocrStages: [{ source_type: 'scan', engine: 'tesseract',
                det_model: ' ', rec_model: '/models/recognizer.gguf', cleanup: { enabled: false, denoise: true },
                det_prob_threshold: .2, det_box_threshold: .6, det_target_short: 900,
                vlm_max_tokens: 256, vlm_prompt: 'Read every line', min_chars: 0, min_confidence: 0 }],
        };
        vi.mocked(getSetting).mockImplementation(async (key, fallback) => settings[key] ?? fallback);
        vi.mocked(getStore).mockResolvedValue({ get: async (key: string) => settings[key] } as Awaited<ReturnType<typeof getStore>>);
        await extractText(archive);
        expect(invoke).toHaveBeenLastCalledWith('archive_extract_text', {
            path: archive.path, options: { tryOcr: true, ocrTier: 'auto', ocrRecLang: 'auto', ocrPipeline: {
                enabled: true, router: false, cleanup_enabled: false, denoise: true, min_chars: 14, min_confidence: .7,
                layout: true, layout_engine: 'custom-layout', layout_threshold: .4, drop_headers_footers: true,
                sr: true, sr_engine: 'custom-sr', sr_max_short_side: 1800,
                restore: true, restore_engine: 'custom-restore', restore_task: 'deblur',
                dewarp: true, dewarp_engine: 'custom-dewarp', punct_model: '/models/punctuation.gguf',
                vlm_ocr_model: '/models/vision.gguf', vlm_ocr_engine: 'custom-vlm', truecase_model: '/models/truecase.gguf',
                det_model: null, rec_model: null, nafnet_model: null, restore_model: null, sr_model: null, layout_model: null,
                stages: [{ source_type: 'scan', engine: 'tesseract', det_model: null, rec_model: '/models/recognizer.gguf',
                    cleanup: { enabled: false, denoise: true }, det_prob_threshold: .2, det_box_threshold: .6,
                    det_target_short: 900, vlm_max_tokens: 256, vlm_prompt: 'Read every line', min_chars: 0, min_confidence: 0 }],
            } },
        });
    });

    it('does not quietly discard invalid saved Smart OCR stages', async () => {
        vi.mocked(getSetting).mockImplementation(async (key, fallback) => key === 'ocrPipelineEnabled' ? true : fallback);
        vi.mocked(getStore).mockResolvedValue({ get: async (key: string) => key === 'ocrPipelineAdvanced' ? true
            : key === 'ocrStages' ? 'invalid' : undefined } as Awaited<ReturnType<typeof getStore>>);
        await expect(extractText(archive)).rejects.toThrow('Saved Smart OCR stages are invalid');
        expect(invoke).not.toHaveBeenCalled();
    });

    it('discards a native result that finishes after cancellation', async () => {
        let finish!: (value: typeof extracted) => void;
        vi.mocked(invoke).mockImplementation(() => new Promise(resolve => { finish = resolve as typeof finish; }));
        const controller = new AbortController();
        const pending = extractText(archive, { signal: controller.signal });
        const outcome = expect(pending).rejects.toThrow('Extraction timed out');
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledOnce());
        controller.abort(new Error('Extraction timed out'));
        finish(extracted);
        await outcome;
    });

    it('keeps native archive errors visible and rejects an unusable response', async () => {
        vi.mocked(invoke).mockRejectedValueOnce(new Error('Archive expansion exceeds the byte limit'));
        await expect(extractText(archive)).rejects.toThrow('Archive expansion exceeds the byte limit');
        vi.mocked(invoke).mockResolvedValueOnce(null);
        await expect(extractText(archive)).rejects.toThrow('no usable response');
    });

    it('preserves only usable unique extraction tags for index writes', () => {
        expect(extractionTags({ text: '', metadata: { tags: ['archive:zip', '', null, 3, 'coverage:partial', 'archive:zip'] } }))
            .toEqual(['archive:zip', 'coverage:partial']);
        expect(extractionTags({ text: '', metadata: { tags: 'wrong shape' } })).toEqual([]);
    });
});
