import { getSetting, getStore } from '../store';

const numberOr = (value: unknown, fallback: number): number => {
    const parsed = typeof value === 'number' || typeof value === 'string' ? Number(value) : NaN;
    return Number.isFinite(parsed) ? parsed : fallback;
};
const textOr = (value: unknown, fallback: string): string => typeof value === 'string' && value.trim() ? value.trim() : fallback;
const record = (value: unknown): Record<string, unknown> | null => value != null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown> : null;

/** Same persisted Settings keys and snake_case payload as syncBackgroundOcr. */
export async function loadSavedOcrPipeline(): Promise<Record<string, unknown> | undefined> {
    if (await getSetting('ocrPipelineEnabled', false) !== true) return undefined;
    const store = await getStore();
    const mapping = [
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
    const fields = await Promise.all(mapping.map(async ([key, field, fallback]) => {
        const stored = await store.get<unknown>(key);
        const value = stored ?? fallback;
        return [field, typeof fallback === 'boolean' ? value === true
            : typeof fallback === 'number' ? numberOr(value, fallback) : textOr(value, fallback) || null] as const;
    }));
    const advanced = await store.get<boolean>('ocrPipelineAdvanced') === true;
    const savedStages = advanced ? await store.get<unknown>('ocrStages') ?? [] : [];
    if (!Array.isArray(savedStages)) throw new Error('Saved Smart OCR stages are invalid. Review the OCR pipeline in Settings.');
    const stages = savedStages.map((value, index) => {
        const stage = record(value);
        if (!stage) throw new Error(`Saved Smart OCR stage ${index + 1} is invalid. Review the OCR pipeline in Settings.`);
        return {
            source_type: textOr(stage.source_type, 'auto'), engine: textOr(stage.engine, 'dbnet_trocr'),
            det_model: textOr(stage.det_model, '') || null, rec_model: textOr(stage.rec_model, '') || null,
            cleanup: { ...(record(stage.cleanup) ?? {}) },
            det_prob_threshold: numberOr(stage.det_prob_threshold, .3), det_box_threshold: numberOr(stage.det_box_threshold, .5),
            det_target_short: numberOr(stage.det_target_short, 736), vlm_max_tokens: numberOr(stage.vlm_max_tokens, 0),
            vlm_prompt: textOr(stage.vlm_prompt, ''), min_chars: numberOr(stage.min_chars, 0),
            min_confidence: numberOr(stage.min_confidence, 0),
        };
    });
    return {
        enabled: true, ...Object.fromEntries(fields), stages,
        det_model: null, rec_model: null, nafnet_model: null, restore_model: null, sr_model: null, layout_model: null,
    };
}
