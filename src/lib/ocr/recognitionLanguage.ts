/** Automatic language only follows the interface for unconfigured embedded
 * Tesseract. Custom stages and the external system engine keep their defaults. */
export function resolveRecognitionLanguage(
    engine?: string | null,
    language?: string | null,
    config?: Record<string, unknown> | null,
    defaultLanguage = 'eng',
): string | null {
    const automatic = !language || language === 'auto';
    const tesseract = !engine || engine === 'auto' || engine === 'tesseract';
    // Without config, native state may contain an unseen custom recognizer.
    if (!automatic || !tesseract || !config) return language ?? null;
    const configured = (value: unknown) => typeof value === 'string' && value.trim().length > 0;
    if ((!engine || engine === 'auto') && config.enabled === false) return defaultLanguage;
    // The native LID model selects a Tesseract GGUF from this directory.
    if (config.enabled !== false && configured(config.lid_model) && configured(config.tess_model_dir)) return language ?? null;
    const stagesValue = config.stages;
    if (stagesValue !== undefined && !Array.isArray(stagesValue)) return language ?? null;
    const stages = Array.isArray(stagesValue) ? stagesValue : [];
    if (!engine || engine === 'auto') {
        if (stages.length || configured(config.rec_model) || configured(config.vlm_ocr_model)) return language ?? null;
    } else {
        const configured = stages?.find(stage => stage && typeof stage === 'object'
            && !Array.isArray(stage) && stage.engine === 'tesseract');
        if (configured && typeof configured.rec_model === 'string') return language ?? null;
    }
    return defaultLanguage;
}
