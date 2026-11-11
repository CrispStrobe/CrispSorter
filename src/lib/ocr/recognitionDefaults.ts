import type { OcrPolicy } from '../extractors/ocrSettings';
import { isOcrVisionEngine } from './engineCatalog';

const record = (value: unknown): Record<string, unknown> | undefined => value && typeof value === 'object' && !Array.isArray(value)
  ? value as Record<string, unknown> : undefined;
const text = (value: unknown): string | undefined => typeof value === 'string' && value.trim() ? value.trim() : undefined;
const LEGACY = ['system_tesseract', 'ocrs', 'paddle'];
const LEGACY_TIERS: Record<string, string> = { tier1: 'system_tesseract', tier2: 'ocrs', tier3: 'paddle', tier4: 'dbnet_trocr' };
const POSTPROCESSING_FIELDS = [
  { key: 'punct_model', label: 'Punctuation restoration' },
  { key: 'truecase_model', label: 'Truecase model' },
  { key: 'lid_model', label: 'Language identification model' },
];

function ownsModels(config: Record<string, unknown>): boolean {
  return (Array.isArray(config.stages) && config.stages.length > 0)
    || !!text(config.rec_model) || !!text(config.vlm_ocr_model)
    || (!!text(config.lid_model) && !!text(config.tess_model_dir));
}

function activeStageIndex(config: Record<string, unknown>, engine: string): number {
  if (!Array.isArray(config.stages) || LEGACY.includes(engine)) return -1;
  if (engine === 'auto') return config.stages.length && record(config.stages[0]) ? 0 : -1;
  return config.stages.findIndex(stage => record(stage)?.engine === engine);
}

export interface WorkspaceRecognition {
  engine: string;
  language: string;
  cpuPixelBudget: boolean;
  /** Native snapshot; unused flat models are not overwritten for presentation. */
  config: Record<string, unknown>;
  /** Values the current engine/stage actually consumes. */
  controls: Record<string, unknown>;
  modelFields: { key: string; label: string }[];
}

/** Saved app models are defaults; a project's explicit choices stay local. */
export function workspaceRecognitionDefaults(
  profile: Record<string, unknown>,
  saved: OcrPolicy | null,
  settings: Record<string, unknown>,
  capabilities: { crispembed: boolean; default_engine?: string; default_language?: string },
): WorkspaceRecognition {
  const configured = record(settings.config) ?? {};
  const ownsModel = ownsModels(configured);
  const savedEngine = saved?.engine ?? 'auto';
  const savedActive = !!saved && (saved.config.enabled === true || savedEngine !== 'auto');
  // A disabled global pipeline retains its recipe for later editing. It must
  // not become active merely because the selected workspace profile is enabled.
  let inherited: Record<string, unknown> = savedActive ? saved!.config : {};
  if (savedActive && saved!.config.enabled !== true) {
    const matching = Array.isArray(inherited.stages)
      ? record(inherited.stages.find(stage => record(stage)?.engine === savedEngine)) : undefined;
    const modelKeys = ['engine', 'language', 'cpu_pixel_budget', 'det_model', 'rec_model',
      'vlm_ocr_model', 'vlm_ocr_engine', 'min_chars', 'min_confidence'];
    inherited = Object.fromEntries(modelKeys.filter(key => key in inherited).map(key => [key, inherited[key]]));
    inherited.stages = matching && !LEGACY.includes(savedEngine) ? [{ ...matching,
      // Only the selected profile can enable preparation from an inactive
      // global recipe; its hidden stage cleanup and postprocessors stay off.
      cleanup: { enabled: profile.cleanup_enabled ?? true, denoise: profile.denoise ?? false },
    }] : [];
  }
  const legacyEngine = saved && !savedActive ? LEGACY_TIERS[saved.ocrTier] : undefined;
  const savedEngineChosen = saved?.explicitSelections?.engine ?? (savedEngine !== 'auto');
  const defaultEngine = legacyEngine ?? (savedActive || savedEngineChosen ? savedEngine : capabilities.default_engine ?? 'auto');
  const engine = text(settings.engine) ?? text(configured.engine) ?? (ownsModel ? 'auto' : defaultEngine);
  const savedLanguageChosen = saved?.explicitSelections?.language ?? (saved?.language !== undefined && saved.language !== 'auto');
  const usesLegacyScript = defaultEngine === 'paddle'
    || (!capabilities.crispembed && defaultEngine === 'auto' && !savedActive);
  const scriptLanguage = usesLegacyScript && saved?.language === 'auto'
    ? saved.ocrRecLang === 'cjk' ? 'chi_sim' : saved.ocrRecLang === 'latin' ? 'eng' : undefined : undefined;
  const defaultLanguage = scriptLanguage ?? (saved && (savedActive || savedLanguageChosen || legacyEngine)
    ? saved.language : capabilities.default_language ?? 'auto');
  const language = text(settings.language) ?? text(configured.language) ?? (ownsModel ? 'auto' : defaultLanguage);
  const cpuPixelBudget = typeof settings.cpuPixelBudget === 'boolean' ? settings.cpuPixelBudget
    : typeof configured.cpu_pixel_budget === 'boolean' ? configured.cpu_pixel_budget
    : ownsModel ? false : saved?.cpuPixelBudget ?? false;
  const projectFlatModel = !Array.isArray(configured.stages) && ownsModel;
  const config: Record<string, unknown> = {
    ...inherited,
    ...profile,
    ...(!capabilities.crispembed ? { enabled: false } : {}),
    // A project's flat recognizer must not be shadowed by inherited stages.
    ...(projectFlatModel ? { stages: [] } : {}),
    ...configured,
    engine, language, cpu_pixel_budget: cpuPixelBudget,
  };
  const index = activeStageIndex(config, engine);
  const stage = index >= 0 ? record((config.stages as unknown[])[index]) : undefined;
  const activeEngine = text(stage?.engine) ?? engine;
  const controls = { ...config };
  if (stage) {
    const cleanup = record(stage.cleanup);
    controls.cleanup_enabled = typeof cleanup?.enabled === 'boolean' ? cleanup.enabled : true;
    controls.denoise = typeof cleanup?.denoise === 'boolean' ? cleanup.denoise : false;
    controls.det_model = stage.det_model ?? null;
    controls.rec_model = stage.rec_model ?? null;
  } else if (engine !== 'auto' && !LEGACY.includes(engine)) {
    // Mirror native creation of a new stage; DBNet's flat recognition model
    // must not appear to be a Tesseract or single-model vision engine's model.
    controls.det_model = isOcrVisionEngine(engine)
      ? config.vlm_ocr_engine === engine ? config.vlm_ocr_model ?? null : null
      : ['dbnet_trocr', 'tesseract', 'tesseract_fraktur', 'parseq', 'easyocr'].includes(engine) ? config.det_model ?? null : null;
    controls.rec_model = engine === 'dbnet_trocr' ? config.rec_model ?? null : null;
  }
  const modelFields = LEGACY.includes(activeEngine) ? [] : isOcrVisionEngine(activeEngine)
    ? [{ key: 'det_model', label: 'Recognition model' }]
    : [{ key: 'det_model', label: 'Detection model' }, { key: 'rec_model', label: 'Recognition model' }];
  if (engine === 'auto' && !stage) modelFields.push({ key: 'vlm_ocr_model', label: 'Escalation model' });
  modelFields.push(...POSTPROCESSING_FIELDS);
  return { engine, language, cpuPixelBudget, config, controls, modelFields };
}

/** Editing inherited controls creates a project snapshot and changes the
 * active recipe, leaving other stages and saved global settings intact. */
export function updateWorkspaceConfig(effective: WorkspaceRecognition, key: string, value: unknown): Record<string, unknown> {
  const config = JSON.parse(JSON.stringify(effective.config)) as Record<string, unknown>;
  const stages = Array.isArray(config.stages) ? config.stages : [];
  let index = activeStageIndex(config, effective.engine);
  const modelField = key === 'det_model' || key === 'rec_model';
  if (modelField && index < 0 && effective.engine !== 'auto' && !LEGACY.includes(effective.engine)) {
    // Some engines consume these models only from explicit stages. Create the
    // selected recipe instead of writing an ignored simple-mode field.
    stages.push({ source_type: 'auto', engine: effective.engine,
      det_model: effective.controls.det_model ?? null, rec_model: effective.controls.rec_model ?? null,
      cleanup: { enabled: config.cleanup_enabled ?? true, denoise: config.denoise ?? false },
      min_chars: config.min_chars ?? 8, min_confidence: config.min_confidence ?? .5 });
    config.stages = stages;
    index = stages.length - 1;
  }
  if (index >= 0 && (modelField || key === 'cleanup_enabled' || key === 'denoise')) {
    const stage = record(stages[index])!;
    if (modelField) stage[key] = value;
    else stage.cleanup = { ...record(stage.cleanup), [key === 'cleanup_enabled' ? 'enabled' : 'denoise']: value };
  } else config[key] = value;
  return config;
}
