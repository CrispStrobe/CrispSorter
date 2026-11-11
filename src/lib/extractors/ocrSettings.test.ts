import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getSetting, getStore } from '../store';
import { loadSavedOcrPipeline, loadSavedOcrPolicy, ocrPolicyLanguage, serializeOcrPipeline } from './ocrSettings';
import { nativeOcrOptions } from './nativeOptions';

vi.mock('../store', () => ({ getSetting: vi.fn(), getStore: vi.fn() }));
let saved: Record<string, unknown>;
beforeEach(() => {
    saved = {};
    vi.mocked(getSetting).mockReset().mockImplementation(async (key, fallback) => saved[key] ?? fallback);
    vi.mocked(getStore).mockReset().mockResolvedValue({ get: async (key: string) => saved[key] } as Awaited<ReturnType<typeof getStore>>);
});

describe('shared OCR settings serialization', () => {
    it('retains legal zero thresholds from the live Settings form', () => {
        const config = serializeOcrPipeline({ ocrPipelineEnabled: true, ocrPipelineMinChars: 0,
            ocrPipelineMinConf: 0, ocrPipelineLayoutThr: 0, ocrPipelineSrMaxPx: 0,
            ocrPipelineAdvanced: true, ocrStages: [{ det_prob_threshold: 0, det_box_threshold: 0,
                det_target_short: 0, min_chars: 0, min_confidence: 0 }] });
        expect(config).toMatchObject({ enabled: true, min_chars: 0, min_confidence: 0, layout_threshold: 0,
            sr_max_short_side: 0, stages: [{ det_prob_threshold: 0, det_box_threshold: 0, det_target_short: 0, min_chars: 0, min_confidence: 0 }] });
    });

    it('copies proxied Settings stage cleanup without structuredClone errors or mutable aliases', () => {
        const cleanup = new Proxy({ enabled: true, nested: new Proxy({ denoise: true }, {}) }, {});
        const stage = new Proxy({ engine: 'tesseract', cleanup }, {});
        const input = new Proxy({ ocrPipelineAdvanced: true, ocrStages: new Proxy([stage], {}) }, {});
        const config = serializeOcrPipeline(input);
        cleanup.nested.denoise = false;
        expect(config.stages).toMatchObject([{ engine: 'tesseract', cleanup: { enabled: true, nested: { denoise: true } } }]);
    });

    it('preserves configured model paths and disabled-pipeline selections', () => {
        const config = serializeOcrPipeline({ ocrEngine: 'qwen2vl', ocrLanguage: 'fra', ocrCpuPixelBudget: true,
            ocrPipelineDetModel: ' /models/det.gguf ', ocrPipelineRecModel: '/models/rec.gguf',
            ocrPipelineNafnetModel: 'nafnet-custom', ocrPipelineRestoreModel: 'restore-custom',
            ocrPipelineSrModel: 'sr-custom', ocrPipelineLayoutModel: 'layout-custom',
            ocrPipelineLidModel: 'lid-custom', ocrPipelineTessModelDir: '/models/tesseract' });
        expect(config).toMatchObject({ enabled: false, engine: 'qwen2vl', language: 'fra', cpu_pixel_budget: true,
            det_model: '/models/det.gguf', rec_model: '/models/rec.gguf', nafnet_model: 'nafnet-custom',
            restore_model: 'restore-custom', sr_model: 'sr-custom', layout_model: 'layout-custom',
            lid_model: 'lid-custom', tess_model_dir: '/models/tesseract' });
        expect(serializeOcrPipeline({ rec_model: 'legacy-native-path' }).rec_model).toBe('legacy-native-path');
    });

    it.each([{}, { stages: true }, [null]])('rejects malformed advanced stages: %j', stages => {
        expect(() => serializeOcrPipeline({ ocrPipelineAdvanced: true, ocrStages: stages })).toThrow(/stages? .*invalid/);
    });

    it('loads explicit selections even when optional Smart OCR preprocessing is disabled', async () => {
        expect(await loadSavedOcrPipeline()).toBeUndefined();
        saved = { ocrEngine: 'system_tesseract', ocrLanguage: 'deu', ocrPipelineRecModel: '/model.gguf' };
        expect(await loadSavedOcrPipeline()).toMatchObject({ enabled: false, engine: 'system_tesseract', language: 'deu', rec_model: '/model.gguf' });
    });
});

describe('immutable OCR extraction policy', () => {
    it('captures a known default config and the actual saved UI language', async () => {
        saved.language = 'de';
        const policy = await loadSavedOcrPolicy();
        expect(policy.config).toEqual({ enabled: false, stages: [], engine: 'auto', language: 'auto', cpu_pixel_budget: false });
        expect(ocrPolicyLanguage(policy)).toBe('deu');
        expect(policy.explicitSelections).toEqual({ engine: false, language: false, cpuPixelBudget: false });
        expect(Object.isFrozen(policy)).toBe(true);
        expect(Object.isFrozen(policy.config.stages)).toBe(true);
        saved.language = 'en';
        expect(ocrPolicyLanguage(policy)).toBe('deu');
    });

    it('distinguishes intentionally saved Automatic/false from a fresh store', async () => {
        saved = { ocrEngine: 'auto', ocrLanguage: 'auto', ocrCpuPixelBudget: false };
        expect((await loadSavedOcrPolicy()).explicitSelections).toEqual({ engine: true, language: true, cpuPixelBudget: true });
        expect((await loadSavedOcrPolicy({ engine: 'auto', language: 'auto', cpuPixelBudget: false }, false)).explicitSelections)
            .toEqual({ engine: true, language: true, cpuPixelBudget: true });
    });

    it('retains saved model defaults for a disabled pipeline before a project overrides the engine', async () => {
        saved = { ocrPipelineEnabled: false, ocrPipelineRecModel: '/models/custom.gguf', ocrPipelineLidModel: 'custom-lid' };
        const policy = await loadSavedOcrPolicy();
        expect(policy.config).toMatchObject({ enabled: false, rec_model: '/models/custom.gguf', lid_model: 'custom-lid' });
        expect(await loadSavedOcrPipeline()).toBeUndefined();
    });

    it('preserves custom stage models and explicit language overrides without mutating saved input', async () => {
        const stages = [{ engine: 'tesseract', rec_model: '/models/custom.gguf', cleanup: { denoise: true } }];
        saved = { ocrPipelineEnabled: true, ocrPipelineAdvanced: true, ocrStages: stages, ocrEngine: 'tesseract', language: 'de' };
        const policy = await loadSavedOcrPolicy();
        stages[0].rec_model = '/other.gguf';
        expect(ocrPolicyLanguage(policy)).toBe('auto');
        expect(policy.config.stages).toMatchObject([{ rec_model: '/models/custom.gguf' }]);
        const explicit = await loadSavedOcrPolicy({ language: 'fra' });
        expect(ocrPolicyLanguage(explicit)).toBe('fra');
        expect(explicit.config.language).toBe('fra');
    });

    it('lets an explicit config override malformed saved stages and retains its model choices', async () => {
        saved = { ocrPipelineEnabled: true, ocrPipelineAdvanced: true, ocrStages: [null], ocrEngine: 'got' };
        const policy = await loadSavedOcrPolicy({ config: { engine: 'qwen2vl', language: 'deu', cpu_pixel_budget: true, stages: [] } });
        expect(policy).toMatchObject({ engine: 'qwen2vl', language: 'deu', cpuPixelBudget: true });
        expect(getStore).not.toHaveBeenCalled();
    });

    it('keeps native legacy tier and System Tesseract automatic-language defaults', async () => {
        saved = { language: 'de', ocrTier: 'tier1' };
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy())).toBe('auto');
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ engine: 'system_tesseract' }))).toBe('auto');
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ engine: 'tesseract' }))).toBe('deu');
    });

    it('preserves the legacy Auto script choice ahead of an inferred interface language', async () => {
        saved = { language: 'de', ocrEngine: 'auto', ocrTier: 'auto', ocrLanguage: 'auto', ocrRecLang: 'cjk' };
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy())).toBe('auto');
        expect(await nativeOcrOptions({ forceOCR: true })).toMatchObject({ ocrTier: 'auto', ocrRecLang: 'cjk', ocrPipeline: { language: 'auto' } });
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ language: 'fra' }))).toBe('fra');
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ engine: 'tesseract' }))).toBe('deu');
        saved.ocrRecLang = 'auto';
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy())).toBe('deu');
    });

    it.each([{ rec_model: '/models/custom.gguf' }, { vlm_ocr_model: 'custom-vlm' }])(
        'preserves Automatic for configured flat recognition models: %j', async config => {
            const policy = await loadSavedOcrPolicy({ config: { ...config, stages: [] } });
            expect(ocrPolicyLanguage(policy)).toBe('auto');
            const explicit = await loadSavedOcrPolicy({ language: 'fra', config });
            expect(ocrPolicyLanguage(explicit)).toBe('fra');
        },
    );

    it('protects configured LID-based Tesseract selection without reusing a flat DBNet model', async () => {
        saved.language = 'de';
        const config = { lid_model: 'language-detector', tess_model_dir: '/models/tesseract', stages: [] };
        for (const engine of ['auto', 'tesseract']) {
            expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ engine, config }))).toBe('auto');
        }
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ engine: 'tesseract', config: { rec_model: 'trocr-custom' } }))).toBe('deu');
    });

    it('ignores inactive Automatic model fields but preserves an explicit engine\'s matching custom model', async () => {
        saved.language = 'de';
        const config = { enabled: false, rec_model: 'inactive-trocr', lid_model: 'inactive-lid', tess_model_dir: '/inactive',
            stages: [{ engine: 'tesseract', rec_model: 'custom-language' }] };
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ config }))).toBe('deu');
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ engine: 'tesseract', config }))).toBe('auto');
        expect(ocrPolicyLanguage(await loadSavedOcrPolicy({ engine: 'tesseract', config: { ...config, stages: [] } }))).toBe('deu');
    });

    it.each([['ocrTier', 'Unknown OCR tier'], ['ocrRecLang', 'Unknown OCR script model']])('rejects an unknown saved %s instead of selecting another engine', async (key, message) => {
        saved[key] = 'typo';
        await expect(loadSavedOcrPolicy()).rejects.toThrow(message);
    });

    it('does not read native settings in a browser-only session', async () => {
        const policy = await loadSavedOcrPolicy({ language: 'eng+deu' }, false);
        expect(policy.language).toBe('eng+deu');
        expect(getSetting).not.toHaveBeenCalled(); expect(getStore).not.toHaveBeenCalled();
    });

    it('shares explicit engine, language, model and CPU choices with archive/mail requests', async () => {
        const options = await nativeOcrOptions({ forceOCR: true, ocr: { engine: 'qwen2vl', language: 'fra', cpuPixelBudget: true,
            config: { enabled: false, stages: [{ engine: 'qwen2vl', rec_model: 'custom-vlm' }], restore_model: 'custom-restore' } } });
        expect(options).toMatchObject({ tryOcr: true, ocrPipeline: { enabled: false, engine: 'qwen2vl', language: 'fra',
            cpu_pixel_budget: true, stages: [{ rec_model: 'custom-vlm' }], restore_model: 'custom-restore' } });
    });
});
