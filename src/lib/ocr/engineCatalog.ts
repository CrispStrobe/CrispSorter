/** Labels are presentation only; the native capability list decides availability. */
const ENGINE_NAMES: Record<string, string> = {
  auto: 'Automatic pipeline',
  system_tesseract: 'System Tesseract · word evidence',
  ocrs: 'ocrs · Latin text',
  paddle: 'PaddleOCR v4 · Latin/CJK',
  dbnet_trocr: 'DBNet + TrOCR',
  surya: 'Surya multilingual',
  tesseract: 'Tesseract LSTM',
  tesseract_fraktur: 'Tesseract LSTM · Fraktur',
  parseq: 'PARSeq scene text',
  ppocrv6: 'PP-OCRv6 (CJK lines)',
  easyocr: 'EasyOCR',
  got: 'GOT OCR (non-commercial)',
  glm: 'GLM OCR',
  qwen2vl: 'Qwen2 VL',
  internvl2: 'InternVL2',
  deepseek_ocr2: 'DeepSeek OCR 2',
  pix2struct: 'Pix2Struct',
  granite_vision: 'Granite Vision',
  lightonocr: 'LightOnOCR',
  qwen3vl: 'Qwen3 VL',
  unlimited_ocr: 'Unlimited OCR',
  unlimited_ocr2: 'Unlimited-OCR-2 (experimental)',
  minicpm_ocr: 'MiniCPM-OCR-1.0 (experimental)',
  olmocr: 'olmOCR 2 (7B)',
  lfm2_vl: 'LFM2.5 VL',
  unified: 'Unified · custom GGUF',
};

export const OCR_LANGUAGE_NAMES: Record<string, string> = {
  eng: 'English', deu: 'German', fra: 'French', spa: 'Spanish',
  ita: 'Italian', por: 'Portuguese', nld: 'Dutch', pol: 'Polish',
  rus: 'Russian', ara: 'Arabic', chi_sim: 'Chinese, simplified',
  chi_tra: 'Chinese, traditional', jpn: 'Japanese', kor: 'Korean',
};

export interface OcrEngineCapabilities {
  engines: readonly string[];
  cpu_budget?: boolean;
  system_tesseract?: { languages?: readonly string[] };
}

export interface OcrEngineOption { value: string; label: string; disabled: boolean }

export function ocrEngineLabel(engine: string): string {
  return ENGINE_NAMES[engine] ?? engine;
}

export function ocrEngineOptions(capabilities: OcrEngineCapabilities, current?: string): OcrEngineOption[] {
  const engines = [...new Set(capabilities.engines)];
  const options = engines.map(value => ({ value, label: ocrEngineLabel(value), disabled: false }));
  if (current && !engines.includes(current)) {
    options.push({ value: current, label: `${ocrEngineLabel(current)} · unavailable`, disabled: true });
  }
  return options;
}

const LEGACY_ENGINES = ['auto', 'system_tesseract', 'ocrs', 'paddle'];
const DET_REC_ENGINES = ['dbnet_trocr', 'surya', 'tesseract', 'parseq', 'tesseract_fraktur', 'ppocrv6', 'easyocr'];

export function isOcrVisionEngine(engine: string): boolean {
  return !LEGACY_ENGINES.includes(engine) && !DET_REC_ENGINES.includes(engine);
}

export function ocrStageOptions(capabilities: OcrEngineCapabilities, current?: string): OcrEngineOption[] {
  return ocrEngineOptions({ ...capabilities, engines: capabilities.engines.filter(engine => !LEGACY_ENGINES.includes(engine)) }, current);
}

export function ocrVisionOptions(capabilities: OcrEngineCapabilities, current?: string): OcrEngineOption[] {
  return ocrEngineOptions({ ...capabilities, engines: capabilities.engines.filter(isOcrVisionEngine) }, current);
}

export function ocrLanguageOptions(engine: string, capabilities: OcrEngineCapabilities, current = 'auto') {
  const languages = engine === 'system_tesseract'
    ? capabilities.system_tesseract?.languages ?? [] : Object.keys(OCR_LANGUAGE_NAMES);
  const values = [...new Set(languages.filter(language => language !== 'auto'))];
  const options = [{ value: 'auto', label: engine === 'system_tesseract' ? 'English (eng) default' : 'Model default' },
    ...values.map(value => ({ value, label: `${OCR_LANGUAGE_NAMES[value] ?? value} (${value})` }))];
  if (current && current !== 'auto' && !values.includes(current)) {
    options.push({ value: current, label: `${current} (configured)` });
  }
  return options;
}

export function usesCpuBudget(engine: string): boolean { return engine === 'qwen2vl'; }

export function engineRequirement(engine: string): string | null {
  if (engine === 'unified') return 'Choose a matching advanced stage with an explicit GGUF model. Its metadata selects the recognizer.';
  if (engine === 'surya') return 'Configure suitable detection and recognition models in a matching advanced stage.';
  if (engine === 'unlimited_ocr2') return 'Requires matching model files and CRISPEMBED_UNLIMITED_OCR2=1 in the native runtime.';
  if (engine === 'minicpm_ocr') return 'Requires matching model files, CRISPEMBED_MINICPM_OCR=1 and CRISPEMBED_QWEN35_OCR=1.';
  if (engine === 'got') return 'The model is labelled non-commercial. Its licence consent and model availability are checked before recognition.';
  if (engine === 'system_tesseract') return 'Uses the installed desktop executable and language packs. Model default means English; language detection is not automatic.';
  if (engine === 'ocrs') return 'Latin-script text. Its two models (about 12 MB) are downloaded after you confirm, the first time it runs.';
  if (engine === 'paddle') return 'The script selection chooses the Latin or CJK recognizer. Its models (about 12-16 MB) are downloaded after you confirm, the first time they are needed.';
  if (engine === 'easyocr') return 'EasyOCR has no published model download. Place easyocr-english-g2-f16.gguf in the model folder, or choose PP-OCRv6 or Tesseract. Automatic never selects it.';
  return null;
}
