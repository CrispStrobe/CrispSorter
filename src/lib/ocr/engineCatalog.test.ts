import { describe, expect, it } from 'vitest';
import { engineRequirement, ocrEngineLabel, ocrEngineOptions, ocrLanguageOptions, ocrStageOptions, ocrVisionOptions, usesCpuBudget } from './engineCatalog';

describe('native OCR engine choices', () => {
  it('offers every advertised engine, including custom-model stages and future engines', () => {
    const values = ['auto', 'surya', 'unified', 'minicpm_ocr', 'ocrs', 'paddle', 'future_engine'];
    const options = ocrEngineOptions({ engines: [...values, 'surya'] });
    expect(options.map(option => option.value)).toEqual(values);
    expect(options.every(option => !option.disabled)).toBe(true);
    expect(options.at(-1)?.label).toBe('future_engine');
    expect(ocrEngineLabel('system_tesseract')).toContain('System Tesseract');
  });

  it('retains an unavailable saved choice without advertising it as usable', () => {
    expect(ocrEngineOptions({ engines: ['auto', 'tesseract'] }, 'system_tesseract')).toEqual([
      { value: 'auto', label: 'Automatic pipeline', disabled: false },
      { value: 'tesseract', label: 'Tesseract LSTM', disabled: false },
      { value: 'system_tesseract', label: 'System Tesseract · word evidence · unavailable', disabled: true },
    ]);
    expect(ocrEngineOptions({ engines: [] })).toEqual([]);
  });

  it('limits pipeline stages and vision escalation to their supported engine families', () => {
    const capabilities = { engines: ['auto', 'system_tesseract', 'ocrs', 'paddle', 'surya', 'ppocrv6', 'unified', 'olmocr', 'lfm2_vl'] };
    expect(ocrStageOptions(capabilities).map(option => option.value)).toEqual(['surya', 'ppocrv6', 'unified', 'olmocr', 'lfm2_vl']);
    expect(ocrVisionOptions(capabilities).map(option => option.value)).toEqual(['unified', 'olmocr', 'lfm2_vl']);
    expect(ocrVisionOptions(capabilities, 'unlimited_ocr2').at(-1)?.disabled).toBe(true);
  });

  it('shows installed System Tesseract languages and preserves a configured combination', () => {
    const options = ocrLanguageOptions('system_tesseract', {
      engines: ['system_tesseract'], system_tesseract: { languages: ['deu', 'deu_latf', 'deu'] },
    }, 'eng+deu');
    expect(options.map(option => option.value)).toEqual(['auto', 'deu', 'deu_latf', 'eng+deu']);
    expect(options[0].label).toBe('English (eng) default');
    expect(ocrLanguageOptions('tesseract', { engines: [] })[0].label).toBe('Model default');
  });

  it('keeps custom-model, experimental and CPU requirements explicit', () => {
    expect(engineRequirement('unified')).toContain('explicit GGUF');
    expect(engineRequirement('minicpm_ocr')).toContain('CRISPEMBED_QWEN35_OCR=1');
    // No published EasyOCR GGUF exists: say so instead of failing at run time.
    expect(engineRequirement('easyocr')).toContain('no published model download');
    expect(engineRequirement('ocrs')).toContain('downloaded after you confirm');
    expect(usesCpuBudget('qwen2vl')).toBe(true);
    expect(usesCpuBudget('tesseract')).toBe(false);
    expect(usesCpuBudget('unified')).toBe(false);
  });
});
