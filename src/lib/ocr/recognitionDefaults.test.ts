import { describe, expect, it } from 'vitest';
import { workspaceRecognitionDefaults, updateWorkspaceConfig } from './recognitionDefaults';
import type { OcrPolicy } from '../extractors/ocrSettings';

const saved: OcrPolicy = {
  enabled: true, engine: 'qwen2vl', language: 'deu', cpuPixelBudget: true,
  ocrTier: 'auto', ocrRecLang: 'auto', defaultLanguage: 'deu',
  config: { enabled: true, cleanup_enabled: false, stages: [{ engine: 'qwen2vl', det_model: '/models/custom.gguf', vlm_prompt: 'Read all columns' }] },
};

describe('workspace recognition defaults', () => {
  it('inherits saved models and selection while applying the chosen preparation profile', () => {
    const result = workspaceRecognitionDefaults({ enabled: true, cleanup_enabled: true }, saved, {}, { crispembed: true });
    expect(result).toMatchObject({ engine: 'qwen2vl', language: 'deu', cpuPixelBudget: true,
      config: { cleanup_enabled: true, stages: saved.config!.stages } });
    expect(saved.config!.cleanup_enabled).toBe(false);
  });

  it('keeps explicit project Automatic/false selections and stage overrides', () => {
    const stages = [{ engine: 'tesseract', rec_model: '/models/project.gguf' }];
    const result = workspaceRecognitionDefaults({ enabled: true }, saved,
      { engine: 'auto', language: 'auto', cpuPixelBudget: false, config: { stages } }, { crispembed: true });
    expect(result).toMatchObject({ engine: 'auto', language: 'auto', cpuPixelBudget: false, config: { stages } });
    expect(saved.config!.stages).not.toEqual(stages);
  });

  it('allows legacy builds to use current pixels without silently discarding explicit preparation requests', () => {
    expect(workspaceRecognitionDefaults({ enabled: true, cleanup_enabled: true }, null, {},
      { crispembed: false, default_engine: 'system_tesseract' })).toMatchObject({ engine: 'system_tesseract', config: { enabled: false } });
    expect(workspaceRecognitionDefaults({ enabled: true }, null, { config: { enabled: true, restore: true } },
      { crispembed: false }).config).toMatchObject({ enabled: true, restore: true });
  });

  it('keeps a project model authoritative when no top-level picker override was saved', () => {
    const stages = [{ engine: 'tesseract', rec_model: '/models/project-language.gguf' }];
    const result = workspaceRecognitionDefaults({ enabled: true }, saved, { config: { stages } }, { crispembed: true });
    expect(result).toMatchObject({ engine: 'auto', language: 'auto', cpuPixelBudget: false,
      config: { engine: 'auto', language: 'auto', cpu_pixel_budget: false, stages } });
  });

  it('uses a project configuration selection before global defaults', () => {
    const result = workspaceRecognitionDefaults({ enabled: true }, saved,
      { config: { engine: 'system_tesseract', language: 'eng+deu', cpu_pixel_budget: false } }, { crispembed: true });
    expect(result).toMatchObject({ engine: 'system_tesseract', language: 'eng+deu', cpuPixelBudget: false });
  });

  it('does not let inherited stages shadow a project flat model or its Automatic language', () => {
    const result = workspaceRecognitionDefaults({ enabled: true }, saved,
      { config: { rec_model: '/models/project-trocr.gguf' } }, { crispembed: true });
    expect(result).toMatchObject({ engine: 'auto', language: 'auto', cpuPixelBudget: false,
      config: { stages: [], rec_model: '/models/project-trocr.gguf' } });
  });

  it('keeps the mobile capability defaults for an untouched settings store', () => {
    const fresh: OcrPolicy = { enabled: false, engine: 'auto', language: 'auto', cpuPixelBudget: false,
      ocrTier: 'auto', ocrRecLang: 'auto', defaultLanguage: 'eng',
      explicitSelections: { engine: false, language: false, cpuPixelBudget: false },
      config: { enabled: false, engine: 'auto', language: 'auto', cpu_pixel_budget: false, stages: [] } };
    const mobile = { crispembed: true, default_engine: 'tesseract', default_language: 'eng' };
    expect(workspaceRecognitionDefaults({ enabled: true }, fresh, {}, mobile)).toMatchObject({ engine: 'tesseract', language: 'eng' });
    const explicit = { ...fresh, explicitSelections: { engine: true, language: true, cpuPixelBudget: true } };
    expect(workspaceRecognitionDefaults({ enabled: true }, explicit, {}, mobile)).toMatchObject({ engine: 'auto', language: 'auto' });
    expect(workspaceRecognitionDefaults({ enabled: true }, fresh,
      { engine: 'auto', language: 'auto', cpuPixelBudget: false }, mobile)).toMatchObject({ engine: 'auto', language: 'auto', cpuPixelBudget: false });
  });

  it('does not reactivate a disabled retained global recipe through the Balanced profile', () => {
    const inactive = { ...saved, engine: 'auto', language: 'auto', cpuPixelBudget: false,
      config: { ...saved.config, enabled: false, restore: true, rec_model: '/inactive/model.gguf' } };
    const result = workspaceRecognitionDefaults({ enabled: true, cleanup_enabled: true }, inactive, {}, { crispembed: true });
    expect(result).toMatchObject({ engine: 'auto', config: { enabled: true, cleanup_enabled: true } });
    expect(result.config.stages).toBeUndefined(); expect(result.config.rec_model).toBeUndefined(); expect(result.config.restore).toBeUndefined();
    const project = workspaceRecognitionDefaults({ enabled: true }, inactive,
      { config: { stages: [{ engine: 'tesseract', rec_model: 'project-model' }] } }, { crispembed: true });
    expect(project.config.stages).toEqual([{ engine: 'tesseract', rec_model: 'project-model' }]);
  });

  it('retains only the selected engine recipe when the saved advanced pipeline is off', () => {
    const inactive = { ...saved, config: { enabled: false, stages: [{ engine: 'surya', cleanup: { enabled: true } },
      { engine: 'qwen2vl', det_model: 'selected-vlm', cleanup: { enabled: false } }] } };
    const result = workspaceRecognitionDefaults({ enabled: true, cleanup_enabled: false }, inactive, {}, { crispembed: true });
    expect(result.config.stages).toEqual([{ engine: 'qwen2vl', det_model: 'selected-vlm', cleanup: { enabled: false, denoise: false } }]);
    expect(result.controls.cleanup_enabled).toBe(false);
  });

  it('does not resurrect disabled global preparation when reusing an explicit engine model', () => {
    const inactive = { ...saved, config: { enabled: false, restore: true, denoise: true, punct_model: 'hidden-punctuation',
      lid_model: 'hidden-lid', stages: [{ engine: 'qwen2vl', det_model: 'chosen-model', vlm_prompt: 'Exact text',
        cleanup: { enabled: false, denoise: true, sauvola_k: .9 } }] } };
    const result = workspaceRecognitionDefaults({ enabled: true, cleanup_enabled: true }, inactive, {}, { crispembed: true });
    expect(result.config.stages).toEqual([{ engine: 'qwen2vl', det_model: 'chosen-model', vlm_prompt: 'Exact text', cleanup: { enabled: true, denoise: false } }]);
    expect(result.config.restore).toBeUndefined(); expect(result.config.punct_model).toBeUndefined(); expect(result.config.lid_model).toBeUndefined();
    expect(result.controls.denoise).toBe(false);
  });

  it.each([['tier1', 'system_tesseract'], ['tier2', 'ocrs'], ['tier3', 'paddle'], ['tier4', 'dbnet_trocr']])(
    'maps a saved disabled-pipeline %s choice to %s while project models retain priority', (tier, engine) => {
      const legacy = { ...saved, engine: 'auto', language: 'auto', cpuPixelBudget: false, ocrTier: tier,
        config: { ...saved.config, enabled: false } };
      expect(workspaceRecognitionDefaults({ enabled: true }, legacy, {}, { crispembed: true }).engine).toBe(engine);
      const project = workspaceRecognitionDefaults({ enabled: true }, legacy,
        { config: { stages: [{ engine: 'tesseract', rec_model: 'project-model' }] } }, { crispembed: true });
      expect(project.engine).toBe('auto'); expect(project.language).toBe('auto');
    },
  );

  it('translates Paddle script defaults without replacing an explicit project language', () => {
    const legacy = { ...saved, engine: 'auto', language: 'auto', ocrTier: 'tier3', ocrRecLang: 'cjk',
      config: { ...saved.config, enabled: false } };
    expect(workspaceRecognitionDefaults({}, legacy, {}, { crispembed: true })).toMatchObject({ engine: 'paddle', language: 'chi_sim' });
    expect(workspaceRecognitionDefaults({}, { ...legacy, ocrRecLang: 'latin' }, {}, { crispembed: true }).language).toBe('eng');
    expect(workspaceRecognitionDefaults({}, legacy, { language: 'fra' }, { crispembed: true }).language).toBe('fra');
  });

  it('carries the legacy Auto script into a complete snapshot without changing fresh mobile defaults', () => {
    const legacy = { ...saved, engine: 'auto', language: 'auto', cpuPixelBudget: false, ocrTier: 'auto', ocrRecLang: 'cjk',
      explicitSelections: { engine: false, language: false, cpuPixelBudget: false }, config: { enabled: false } };
    expect(workspaceRecognitionDefaults({}, legacy, {}, { crispembed: false })).toMatchObject({
      engine: 'auto', language: 'chi_sim', config: { engine: 'auto', language: 'chi_sim' },
    });
    expect(workspaceRecognitionDefaults({}, legacy, { language: 'fra' }, { crispembed: false }).language).toBe('fra');
    const fresh = { ...legacy, ocrRecLang: 'auto' };
    expect(workspaceRecognitionDefaults({}, fresh, {}, { crispembed: true, default_engine: 'tesseract', default_language: 'eng' }))
      .toMatchObject({ engine: 'tesseract', language: 'eng' });
    expect(workspaceRecognitionDefaults({}, fresh, {}, { crispembed: false }).language).toBe('auto');
  });

  it('projects first-stage false cleanup/denoise ahead of profile and later-stage true values', () => {
    const config = { enabled: true, cleanup_enabled: true, denoise: true, stages: [
      { engine: 'tesseract', cleanup: { enabled: false, denoise: false } },
      { engine: 'qwen2vl', cleanup: { enabled: true, denoise: true } },
    ] };
    const result = workspaceRecognitionDefaults({ enabled: true, cleanup_enabled: true, denoise: true }, null,
      { engine: 'auto', config }, { crispembed: true });
    expect(result.controls).toMatchObject({ cleanup_enabled: false, denoise: false });
    expect(result.config.stages).toEqual(config.stages);
    expect(workspaceRecognitionDefaults({}, null, { engine: 'qwen2vl', config }, { crispembed: true }).controls)
      .toMatchObject({ cleanup_enabled: true, denoise: true });
  });

  it('uses native stage defaults when cleanup fields are absent', () => {
    const result = workspaceRecognitionDefaults({ cleanup_enabled: false, denoise: true }, null,
      { config: { stages: [{ engine: 'tesseract' }] } }, { crispembed: true });
    expect(result.controls).toMatchObject({ cleanup_enabled: true, denoise: false });
  });
});

describe('Workbench controls update the active recipe', () => {
  it('edits inherited cleanup in the matching stage while preserving other recipes and selection', () => {
    const policy = { ...saved, config: { ...saved.config, stages: [
      { engine: 'tesseract', cleanup: { enabled: true }, rec_model: 'tesseract-custom' },
      { engine: 'qwen2vl', cleanup: { enabled: false, denoise: false, deskew: false }, det_model: 'vision-custom' },
    ] } };
    const effective = workspaceRecognitionDefaults({ cleanup_enabled: true }, policy, {}, { crispembed: true });
    const changed = updateWorkspaceConfig(effective, 'cleanup_enabled', true);
    expect(changed).toMatchObject({ engine: 'qwen2vl', language: 'deu', cpu_pixel_budget: true, stages: [
      { engine: 'tesseract', cleanup: { enabled: true }, rec_model: 'tesseract-custom' },
      { engine: 'qwen2vl', cleanup: { enabled: true, denoise: false, deskew: false }, det_model: 'vision-custom' },
    ] });
    expect((policy.config.stages[1].cleanup as Record<string, unknown>).enabled).toBe(false);
    expect(workspaceRecognitionDefaults({}, saved, { config: changed }, { crispembed: true })).toMatchObject({
      engine: 'qwen2vl', language: 'deu', cpuPixelBudget: true, controls: { cleanup_enabled: true },
    });
  });

  it('updates active stage models without overwriting unrelated simple-mode model defaults', () => {
    const config = { rec_model: 'flat-trocr', stages: [
      { engine: 'tesseract', rec_model: 'first-language' }, { engine: 'tesseract', rec_model: 'second-language' },
    ] };
    const effective = workspaceRecognitionDefaults({}, null, { engine: 'tesseract', config }, { crispembed: true });
    expect(effective.controls.rec_model).toBe('first-language');
    const changed = updateWorkspaceConfig(effective, 'rec_model', 'edited-language');
    expect(changed.rec_model).toBe('flat-trocr');
    expect(changed.stages).toEqual([{ engine: 'tesseract', rec_model: 'edited-language' }, { engine: 'tesseract', rec_model: 'second-language' }]);
    expect(config.stages[0].rec_model).toBe('first-language');
  });

  it('creates a matching Tesseract model stage instead of editing an ignored flat TrOCR model', () => {
    const effective = workspaceRecognitionDefaults({ cleanup_enabled: false, denoise: true }, null,
      { engine: 'tesseract', config: { rec_model: 'flat-trocr' } }, { crispembed: true });
    expect(effective.controls.rec_model).toBeNull();
    const changed = updateWorkspaceConfig(effective, 'rec_model', 'custom-tesseract');
    expect(changed).toMatchObject({ rec_model: 'flat-trocr', stages: [{ engine: 'tesseract', rec_model: 'custom-tesseract',
      cleanup: { enabled: false, denoise: true } }] });
  });

  it('shows and updates the single model field consumed by a selected vision engine', () => {
    const effective = workspaceRecognitionDefaults({}, null, { engine: 'unified' }, { crispembed: true });
    expect(effective.modelFields).toContainEqual({ key: 'det_model', label: 'Recognition model' });
    expect(effective.modelFields.map(field => field.key)).not.toContain('rec_model');
    expect(effective.modelFields.map(field => field.key)).not.toContain('vlm_ocr_model');
    expect(updateWorkspaceConfig(effective, 'det_model', '/models/custom.gguf').stages)
      .toMatchObject([{ engine: 'unified', det_model: '/models/custom.gguf' }]);
  });

  it('retains the flat escalation field only for an unstaged Automatic pipeline', () => {
    const flat = workspaceRecognitionDefaults({}, null, {}, { crispembed: true });
    expect(flat.modelFields.map(field => field.key)).toContain('vlm_ocr_model');
    expect(updateWorkspaceConfig(flat, 'vlm_ocr_model', 'escalation-custom').vlm_ocr_model).toBe('escalation-custom');
    const staged = workspaceRecognitionDefaults({}, null, { config: { stages: [{ engine: 'tesseract' }] } }, { crispembed: true });
    expect(staged.modelFields.map(field => field.key)).not.toContain('vlm_ocr_model');
  });

  it('keeps global restoration controls and unrelated stage recipes intact', () => {
    const effective = workspaceRecognitionDefaults({}, saved, {}, { crispembed: true });
    const changed = updateWorkspaceConfig(effective, 'restore', true);
    expect(changed.restore).toBe(true); expect(changed.stages).toEqual(saved.config.stages);
    expect(saved.config.restore).toBeUndefined();
  });
});
