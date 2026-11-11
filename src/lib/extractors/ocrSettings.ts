import { getSetting, getStore } from '../store';
import { resolveRecognitionLanguage } from '../ocr/recognitionLanguage';

export interface OcrSelection {
    engine?: string;
    language?: string;
    config?: Record<string, unknown>;
    cpuPixelBudget?: boolean;
    ocrTier?: string;
    ocrRecLang?: string;
}

/** A snapshot is captured once per extraction, including every PDF page. */
export interface OcrPolicy {
    enabled: boolean;
    engine: string;
    language: string;
    cpuPixelBudget: boolean;
    ocrTier: string;
    ocrRecLang: string;
    defaultLanguage: string;
    config: Record<string, unknown>;
    /** Distinguishes saved Automatic from defaults synthesized for a fresh store. */
    explicitSelections?: { engine: boolean; language: boolean; cpuPixelBudget: boolean };
}

const numberOr = (value: unknown, fallback: number): number => {
    const parsed = typeof value === 'number' || typeof value === 'string' ? Number(value) : NaN;
    return Number.isFinite(parsed) ? parsed : fallback;
};
const textOr = (value: unknown, fallback: string): string => typeof value === 'string' && value.trim() ? value.trim() : fallback;
const record = (value: unknown): Record<string, unknown> | null => value != null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown> : null;

/** Copy JSON-shaped state, including Svelte proxies, without retaining aliases. */
function copy<T>(value: T, active = new Set<object>()): T {
    if (!value || typeof value !== 'object') return value;
    if (active.has(value)) throw new Error('OCR settings cannot contain circular references.');
    active.add(value);
    const result = Array.isArray(value) ? value.map(item => copy(item, active))
        : Object.fromEntries(Object.entries(value).map(([key, item]) => [key, copy(item, active)]));
    active.delete(value);
    return result as T;
}

const MODEL_FIELDS = [
    ['ocrPipelineDetModel', 'det_model'], ['ocrPipelineRecModel', 'rec_model'],
    ['ocrPipelineNafnetModel', 'nafnet_model'], ['ocrPipelineRestoreModel', 'restore_model'],
    ['ocrPipelineSrModel', 'sr_model'], ['ocrPipelineLayoutModel', 'layout_model'],
    ['ocrPipelineLidModel', 'lid_model'], ['ocrPipelineTessModelDir', 'tess_model_dir'],
] as const;
export const OCR_MODEL_SETTING_KEYS = MODEL_FIELDS.map(([key]) => key);

const PIPELINE_FIELDS = [
    ['ocrPipelineRouter', 'router', true], ['ocrPipelineCleanup', 'cleanup_enabled', true],
    ['ocrPipelineDenoise', 'denoise', false], ['ocrPipelineMinChars', 'min_chars', 8],
    ['ocrPipelineMinConf', 'min_confidence', .5], ['ocrPipelinePunct', 'punct_model', ''],
    ['ocrPipelineSr', 'sr', false], ['ocrPipelineSrEngine', 'sr_engine', 'pan'],
    ['ocrPipelineSrMaxPx', 'sr_max_short_side', 1200], ['ocrPipelineRestore', 'restore', false],
    ['ocrPipelineRestoreEngine', 'restore_engine', 'restormer'], ['ocrPipelineRestoreTask', 'restore_task', 'denoise'],
    ['ocrPipelineDewarp', 'dewarp', false], ['ocrPipelineDewarpEngine', 'dewarp_engine', 'basic'],
    ['ocrPipelineVlmModel', 'vlm_ocr_model', ''], ['ocrPipelineVlmEngine', 'vlm_ocr_engine', 'qwen2vl'],
    ['ocrPipelineTruecase', 'truecase_model', ''], ['ocrPipelineLayout', 'layout', false],
    ['ocrPipelineLayoutEngine', 'layout_engine', 'rtdetr'], ['ocrPipelineLayoutThr', 'layout_threshold', .25],
    ['ocrPipelineDropHF', 'drop_headers_footers', false],
] as const;

/** Used by both the live Settings form and extraction's persisted snapshot. */
export function serializeOcrPipeline(input: Record<string, unknown>): Record<string, unknown> {
    const fields = PIPELINE_FIELDS.map(([key, field, fallback]) => {
        const value = input[key] ?? fallback;
        return [field, typeof fallback === 'boolean' ? value === true
            : typeof fallback === 'number' ? numberOr(value, fallback) : textOr(value, fallback) || null];
    });
    const savedStages = input.ocrPipelineAdvanced === true ? input.ocrStages ?? [] : [];
    if (!Array.isArray(savedStages)) throw new Error('Saved Smart OCR stages are invalid. Review the OCR pipeline in Settings.');
    const stages = savedStages.map((value, index) => {
        const stage = record(value);
        if (!stage) throw new Error(`Saved Smart OCR stage ${index + 1} is invalid. Review the OCR pipeline in Settings.`);
        return {
            source_type: textOr(stage.source_type, 'auto'), engine: textOr(stage.engine, 'dbnet_trocr'),
            det_model: textOr(stage.det_model, '') || null, rec_model: textOr(stage.rec_model, '') || null,
            cleanup: copy(record(stage.cleanup) ?? {}),
            det_prob_threshold: numberOr(stage.det_prob_threshold, .3), det_box_threshold: numberOr(stage.det_box_threshold, .5),
            det_target_short: numberOr(stage.det_target_short, 736), vlm_max_tokens: numberOr(stage.vlm_max_tokens, 0),
            vlm_prompt: textOr(stage.vlm_prompt, ''), min_chars: numberOr(stage.min_chars, 0),
            min_confidence: numberOr(stage.min_confidence, 0),
        };
    });
    return {
        enabled: input.ocrPipelineEnabled === true,
        engine: textOr(input.ocrEngine, 'auto'), language: textOr(input.ocrLanguage, 'auto'),
        cpu_pixel_budget: input.ocrCpuPixelBudget === true,
        ...Object.fromEntries(fields), stages,
        ...Object.fromEntries(MODEL_FIELDS.map(([key, field]) => [field, textOr(input[key] ?? input[field], '') || null])),
    };
}

async function pipelineFromSettings(selection: Record<string, unknown>, includeDisabled = false): Promise<Record<string, unknown> | undefined> {
    const inactive = selection.ocrPipelineEnabled !== true && textOr(selection.ocrEngine, 'auto') === 'auto'
        && textOr(selection.ocrLanguage, 'auto') === 'auto' && selection.ocrCpuPixelBudget !== true;
    if (inactive && !includeDisabled) return undefined;
    const store = await getStore();
    const keys = [...PIPELINE_FIELDS.map(([key]) => key), ...OCR_MODEL_SETTING_KEYS,
        ...MODEL_FIELDS.map(([, field]) => field), 'ocrPipelineAdvanced', 'ocrStages'];
    const values = await Promise.all(keys.map(async key => [key, await store.get<unknown>(key)]));
    if (inactive && values.every(([, value]) => value == null)) return undefined;
    return serializeOcrPipeline({ ...Object.fromEntries(values), ...selection });
}

async function savedSelection(): Promise<Record<string, unknown>> {
    const fields = [
        ['ocrPipelineEnabled', false], ['ocrEngine', null], ['ocrLanguage', null], ['ocrCpuPixelBudget', null],
    ] as const;
    return Object.fromEntries(await Promise.all(fields.map(async ([key, fallback]) => [key, await getSetting(key, fallback)])));
}

/** Same persisted Settings keys and snake_case payload as syncBackgroundOcr. */
export async function loadSavedOcrPipeline(): Promise<Record<string, unknown> | undefined> {
    return pipelineFromSettings(await savedSelection());
}

function freeze<T>(value: T): T {
    if (value && typeof value === 'object') {
        for (const child of Object.values(value)) freeze(child);
        Object.freeze(value);
    }
    return value;
}

/** Browser-only clients have no native settings store; explicit options still apply. */
export async function loadSavedOcrPolicy(overrides: OcrSelection = {}, readSettings = true): Promise<OcrPolicy> {
    const [selection, enabled, tier, recLang, uiLanguage] = readSettings ? await Promise.all([
        savedSelection(), getSetting('ocrEnabled', false), getSetting('ocrTier', 'auto'),
        getSetting('ocrRecLang', 'auto'), getSetting('language', 'en'),
    ]) : [{}, false, 'auto', 'auto', 'en'];
    const explicitConfig = overrides.config === undefined ? undefined : record(overrides.config);
    if (overrides.config !== undefined && !explicitConfig) throw new Error('OCR pipeline configuration must be an object.');
    const savedConfig = explicitConfig ?? (readSettings ? await pipelineFromSettings(selection, true) : undefined);
    const engine = textOr(overrides.engine ?? explicitConfig?.engine ?? selection.ocrEngine, 'auto');
    const language = textOr(overrides.language ?? explicitConfig?.language ?? selection.ocrLanguage, 'auto');
    const cpuPixelBudget = (overrides.cpuPixelBudget ?? explicitConfig?.cpu_pixel_budget ?? selection.ocrCpuPixelBudget) === true;
    const requestedTier = textOr(overrides.ocrTier, textOr(tier, 'auto'));
    const requestedRecLang = textOr(overrides.ocrRecLang, textOr(recLang, 'auto'));
    // Explicit engine/language choices activate native policy even when Smart
    // OCR's optional preprocessing pipeline is disabled. Do not enable it here.
    const config = { ...(savedConfig ? copy(savedConfig) : { enabled: false, stages: [] }),
        engine, language, cpu_pixel_budget: cpuPixelBudget };
    if (!['auto', 'tier1', 'tier2', 'tier3', 'tier4'].includes(requestedTier)) {
        throw new Error(`Unknown OCR tier '${requestedTier}'. Review the OCR engine in Settings.`);
    }
    if (!['auto', 'latin', 'cjk'].includes(requestedRecLang)) {
        throw new Error(`Unknown OCR script model '${requestedRecLang}'. Review the OCR language in Settings.`);
    }
    return freeze({ enabled: enabled === true, engine, language, cpuPixelBudget,
        ocrTier: requestedTier, ocrRecLang: requestedRecLang,
        defaultLanguage: uiLanguage === 'de' ? 'deu' : 'eng', config,
        explicitSelections: {
            engine: typeof (overrides.engine ?? explicitConfig?.engine ?? selection.ocrEngine) === 'string',
            language: typeof (overrides.language ?? explicitConfig?.language ?? selection.ocrLanguage) === 'string',
            cpuPixelBudget: typeof (overrides.cpuPixelBudget ?? explicitConfig?.cpu_pixel_budget ?? selection.ocrCpuPixelBudget) === 'boolean',
        },
    });
}

export function ocrPolicyLanguage(policy: OcrPolicy): string | null {
    const legacySelection = policy.ocrTier !== 'auto'
        || (policy.config.enabled !== true && policy.ocrRecLang !== 'auto');
    return legacySelection && policy.engine === 'auto' ? policy.language
        : resolveRecognitionLanguage(policy.engine, policy.language, policy.config, policy.defaultLanguage);
}
