<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import { DEFAULT_PROVIDERS, type LLMProvider, llmClient } from '../llm/client';
    import { isDesktop } from '../platform';
    import {
        resolveSecret,
        bulkSetSecrets,
        makeSentinel,
        sentinelAccount,
        llmProviderAccount,
    } from '../secrets';

    /**
     * Walk a providers list, find any plain-text `apiKey` values (i.e.
     * not already `@keyring/…` sentinels), move them into the OS
     * keychain in a single bulk call, and return a new list with those
     * fields rewritten to sentinels.
     *
     * Empty / sentinel / non-string apiKeys pass through unchanged.
     * Returns `{ providers, migrated }` so the caller can decide
     * whether to persist the rewritten list (the migration is only
     * useful if we write the sentinels back to settings.json).
     */
    async function migrateProviderKeysToKeychain(
        list: LLMProvider[]
    ): Promise<{ providers: LLMProvider[]; migrated: number }> {
        const toMigrate: Array<[string, string]> = [];
        for (const p of list) {
            const k = p.apiKey;
            if (!k || sentinelAccount(k)) continue;
            // Skip the well-known local-server sentinel strings that
            // aren't real secrets: 'ollama', 'no-key', '', 'local'.
            if (k === 'ollama' || k === 'no-key' || k === 'local') continue;
            toMigrate.push([llmProviderAccount(p.id), k]);
        }
        if (toMigrate.length === 0) return { providers: list, migrated: 0 };
        const stored = await bulkSetSecrets(toMigrate);
        const storedSet = new Set(stored);
        const next = list.map((p) => {
            const account = llmProviderAccount(p.id);
            if (storedSet.has(account)) {
                return { ...p, apiKey: makeSentinel(account) };
            }
            return p;
        });
        return { providers: next, migrated: stored.length };
    }

    /**
     * Resolve every provider's apiKey to its plain-text form, returning
     * a map suitable for `llmClient.setKeys`. Sentinels go through the
     * keychain; plain values pass through unchanged.
     */
    async function resolveAllApiKeys(
        list: LLMProvider[]
    ): Promise<Record<string, string>> {
        const out: Record<string, string> = {};
        await Promise.all(
            list.map(async (p) => {
                out[p.id] = await resolveSecret(p.apiKey);
            })
        );
        return out;
    }
    import { getSetting, saveSetting } from '../store';
    import { i18n, type Language } from '../i18n.svelte';
    import { getDefaultPrompt, batchManager } from '../batch/store.svelte';
    import {
        RefreshCw, CheckCircle, XCircle, Key, Globe, Cpu,
        Loader2, FolderOpen, Save, Languages, MessageSquare,
        Scan, Edit, Zap, Trash2, Download, Plus, HardDrive, Code,
        Rocket, FileText, Brain, Square, ChevronUp, ChevronDown, Info,
        RotateCcw, Search, CheckCircle2, AlertCircle, Beaker, Play, Check,
        Server
    } from 'lucide-svelte';
    import { open as openDialog, save, ask } from '@tauri-apps/plugin-dialog';
    import * as opener from '@tauri-apps/plugin-opener';
    import { stat, remove } from '@tauri-apps/plugin-fs';
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';
    import { fetch } from '@tauri-apps/plugin-http';
    import { loadWebLLM, unloadWebLLM, getWebLLMLoadedModel, WEBLLM_MODELS } from '../llm/webllm';
    import type { InitProgressReport } from '@mlc-ai/web-llm';
    import { loadORT, unloadORT, getORTLoadedModel, getORTDevice, ORT_MODELS } from '../llm/ort';

    interface LocalModel {
        id: string;
        name: string;
        path: string;
        size?: string;
        isDownloaded: boolean;
        isActive: boolean;
        downloadUrl?: string;
        progress?: number;
    }

    interface MlxModel {
        id: string;
        repoId: string;
        name: string;
        params: string;
        vision?: boolean;
    }

    interface OllamaModel {
        tag: string;
        name: string;
        size?: string;
        isDownloaded: boolean;
        progress?: number;
    }

    const DEFAULT_MLX_MODELS: MlxModel[] = [
        { id: 'llama32-3b',         repoId: 'mlx-community/Llama-3.2-3B-Instruct-4bit',               name: 'Llama 3.2 3B',           params: '3B' },
        { id: 'ministral-3b',       repoId: 'mlx-community/Ministral-3-3B-Instruct-2512-4bit',         name: 'Ministral 3.3B',         params: '3.3B' },
        { id: 'phi35-mini',         repoId: 'mlx-community/Phi-3.5-mini-instruct-4bit',                name: 'Phi-3.5 Mini',           params: '3.8B' },
        { id: 'gemma3-4b',          repoId: 'mlx-community/gemma-3-4b-it-4bit',                        name: 'Gemma 3 4B',             params: '4B' },
        { id: 'mistral-7b',         repoId: 'mlx-community/Mistral-7B-Instruct-v0.3-4bit',             name: 'Mistral 7B v0.3',        params: '7B' },
        { id: 'ministral-8b',       repoId: 'mlx-community/Ministral-3-8B-Instruct-2512-4bit',         name: 'Ministral 8B',           params: '8B' },
        { id: 'phi4',               repoId: 'mlx-community/Phi-4-4bit',                                name: 'Phi-4 14B',              params: '14B' },
        { id: 'qwen35-0.8b-4bit',   repoId: 'mlx-community/Qwen3.5-0.8B-4bit',                        name: 'Qwen 3.5 0.8B ⚠',        params: '0.8B' },
        { id: 'qwen35-0.8b-optiq',  repoId: 'mlx-community/Qwen3.5-0.8B-OptiQ-4bit',                  name: 'Qwen 3.5 0.8B OptiQ ⚠',  params: '0.8B' },
        { id: 'qwen35-2b-optiq',    repoId: 'mlx-community/Qwen3.5-2B-OptiQ-4bit',                    name: 'Qwen 3.5 2B OptiQ ⚠',    params: '2B' },
        { id: 'qwen35-4b-4bit',     repoId: 'mlx-community/Qwen3.5-4B-4bit',                          name: 'Qwen 3.5 4B ⚠',          params: '4B' },
        { id: 'qwen35-4b-optiq',    repoId: 'mlx-community/Qwen3.5-4B-OptiQ-4bit',                    name: 'Qwen 3.5 4B OptiQ ⚠',    params: '4B' },
        { id: 'qwen35-9b-optiq',    repoId: 'mlx-community/Qwen3.5-9B-OptiQ-4bit',                    name: 'Qwen 3.5 9B OptiQ ⚠',    params: '9B' },
    ];

    const DEFAULT_OLLAMA_MODELS: string[] = [
        'qwen3.5:0.8b', 'qwen3.5:2b', 'qwen3.5:4b', 'qwen3.5:9b',
        'ministral-3:3b', 'ministral-3:8b',
        'granite4:350m', 'granite4:1b', 'granite4:3b', 'granite4:350m-h-q8_0',
        'llama3.2:1b-instruct-q4_K_M', 'llama3.2:3b-instruct-q4_K_M',
        'gemma3:1b-it-q4_K_M', 'gemma3:4b'
    ];

    // MLX is Apple Silicon only — hide on Windows/Linux
    const isMacOS = typeof navigator !== 'undefined' && navigator.platform.startsWith('Mac');
    // Sidecar controls (Start/Stop Ollama, llama.cpp, MLX) only shown on desktop
    const showSidecarControls = isDesktop();

    let providers = $state<LLMProvider[]>(JSON.parse(JSON.stringify(DEFAULT_PROVIDERS)));
    let selectedProviderId = $state('global');
    let selectedProvider = $derived(providers.find(p => p.id === selectedProviderId) || providers[0]);

    // Global App Settings
    let activeProviderId = $state('ollama');
    let exportPath = $state('');
    let exportPathMode = $state<'absolute' | 'relative'>('absolute');
    let pathTemplate = $state('{Author}/{Year}/{Title}');
    let saveTxt = $state(true);

    const PATH_TEMPLATE_PRESETS = [
        { label: 'Author/Year/Title', value: '{Author}/{Year}/{Title}' },
        { label: 'Author/Year - Title', value: '{Author}/{Year} - {Title}' },
        { label: 'Author/(Year) Title', value: '{Author}/({Year}) {Title}' },
        { label: 'Year/Author/Title', value: '{Year}/{Author}/{Title}' },
        { label: 'Title (flat)', value: '{Title}' },
    ];

    function pathTemplatePreview(template: string): string {
        return (template || '{Author}/{Year}/{Title}')
            .replace(/\{Author\}/gi, 'Doe, Jane')
            .replace(/\{Year\}/gi, '2024')
            .replace(/\{Title\}/gi, 'My Document')
            .replace(/\{Ext\}/gi, 'pdf')
            .replace(/\{Filename\}/gi, 'original.pdf')
            + (/\{Ext\}/i.test(template) ? '' : '.pdf');
    }
    let currentLanguage = $state<Language>('en');
    
    // LLM & OCR Settings
    let llmMaxChars = $state(5000);
    let llmContextLimit = $state(4096);
    let llmPrompt = $state(''); 
    let ocrEnabled = $state(false);
    let ocrTier    = $state<'auto' | 'tier1' | 'tier2' | 'tier3'>('auto');
    let ocrRecLang = $state<'auto' | 'latin' | 'cjk'>('auto');
    // Smart OCR pipeline (C++ orchestrator: source-type routing + per-stage
    // image cleanup [classical + NAFNet denoise] + accept-gate escalation).
    // Off by default → legacy tier ladder. GGUF-only via CrispEmbed.
    let ocrPipelineEnabled  = $state(false);
    let ocrPipelineRouter   = $state(true);
    let ocrPipelineCleanup  = $state(true);
    let ocrPipelineDenoise  = $state(false);
    let ocrPipelineMinChars = $state(8);
    let ocrPipelineMinConf  = $state(0.5);
    // Optional post-OCR punctuation/spacing/truecasing restorer (FireRedPunc /
    // PCS / fullstop-punc) — model name or GGUF path; empty = off.
    let ocrPipelinePunct    = $state('');
    // Advanced per-stage builder: when on, the user composes explicit stages
    // (full tweakability) instead of the simple toggles above.
    let ocrPipelineAdvanced = $state(false);
    // P20 slice 3 — layout-aware reading-order pass (RT-DETRv2 region detect →
    // column-aware reading order → per-region OCR; formulas → math OCR).
    let ocrPipelineLayout     = $state(false);
    let ocrPipelineLayoutThr  = $state(0.25);
    let ocrPipelineLayoutEngine = $state('rtdetr');
    let ocrPipelineDropHF     = $state(false);
    // P20 #2 — pre-OCR super-resolution for low-res pages (PAN 4×).
    let ocrPipelineSr         = $state(false);
    let ocrPipelineSrMaxPx    = $state(1200);
    let ocrPipelineSrEngine   = $state('pan');
    let ocrPipelineRestore    = $state(false);
    let ocrPipelineRestoreEngine = $state('restormer');
    let ocrPipelineRestoreTask = $state('denoise');
    let ocrPipelineDewarp     = $state(false);
    let ocrPipelineDewarpEngine  = $state('basic');
    type OcrStage = {
        source_type: string; engine: string; det_model: string; rec_model: string;
        cleanup: { enabled: boolean; deskew: boolean; crop_borders: boolean; whiten_background: boolean;
                   binarize: boolean; binarize_method: number; sauvola_k: number; sauvola_window: number;
                   morph_kernel: number; border_threshold: number; deskew_max_angle: number; denoise: boolean };
        det_prob_threshold: number; det_box_threshold: number; det_target_short: number;
        vlm_max_tokens: number; vlm_prompt: string; min_chars: number; min_confidence: number;
    };
    let ocrStages = $state<OcrStage[]>([]);
    function newOcrStage(): OcrStage {
        return {
            source_type: 'auto', engine: 'dbnet_trocr', det_model: '', rec_model: '',
            cleanup: { enabled: true, deskew: true, crop_borders: true, whiten_background: true,
                       binarize: false, binarize_method: 0, sauvola_k: 0.2, sauvola_window: 25,
                       morph_kernel: 51, border_threshold: 0.15, deskew_max_angle: 15.0, denoise: false },
            det_prob_threshold: 0.3, det_box_threshold: 0.5, det_target_short: 736,
            vlm_max_tokens: 0, vlm_prompt: '', min_chars: 8, min_confidence: 0.5,
        };
    }
    function addOcrStage()         { ocrStages = [...ocrStages, newOcrStage()]; }
    function removeOcrStage(i: number) { ocrStages = ocrStages.filter((_, j) => j !== i); }
    function moveOcrStage(i: number, dir: -1 | 1) {
        const j = i + dir; if (j < 0 || j >= ocrStages.length) return;
        const a = [...ocrStages]; [a[i], a[j]] = [a[j], a[i]]; ocrStages = a;
    }
    function isVlmEngine(e: string) { return e !== 'dbnet_trocr' && e !== 'surya' && e !== 'tesseract'; }
    /** Build the OcrPipelineConfig payload (snake_case = Rust serde field names). */
    function ocrPipelineConfig() {
        const stages = (ocrPipelineAdvanced ? ocrStages : []).map((s) => ({
            source_type: s.source_type, engine: s.engine,
            det_model: s.det_model.trim() || null, rec_model: s.rec_model.trim() || null,
            cleanup: { ...s.cleanup },
            det_prob_threshold: Number(s.det_prob_threshold) || 0.3,
            det_box_threshold:  Number(s.det_box_threshold) || 0.5,
            det_target_short:   Number(s.det_target_short) || 736,
            vlm_max_tokens:     Number(s.vlm_max_tokens) || 0,
            vlm_prompt:         s.vlm_prompt,
            min_chars:          Number(s.min_chars) || 0,
            min_confidence:     Number(s.min_confidence) || 0,
        }));
        return {
            enabled:         ocrPipelineEnabled,
            router:          ocrPipelineRouter,
            cleanup_enabled: ocrPipelineCleanup,
            denoise:         ocrPipelineDenoise,
            min_chars:       Number(ocrPipelineMinChars) || 8,
            min_confidence:  Number(ocrPipelineMinConf) || 0.5,
            det_model:       null,
            rec_model:       null,
            nafnet_model:    null,
            punct_model:     ocrPipelinePunct.trim() || null,
            stages,
            sr:                  ocrPipelineSr,
            sr_engine:           ocrPipelineSrEngine,
            restore:             ocrPipelineRestore,
            restore_engine:      ocrPipelineRestoreEngine,
            restore_task:        ocrPipelineRestoreTask,
            restore_model:       null,
            dewarp:              ocrPipelineDewarp,
            dewarp_engine:       ocrPipelineDewarpEngine,
            sr_model:            null,
            sr_max_short_side:   Number(ocrPipelineSrMaxPx) || 1200,
            layout:              ocrPipelineLayout,
            layout_engine:       ocrPipelineLayoutEngine,
            layout_model:        null,
            layout_threshold:    Number(ocrPipelineLayoutThr) || 0.25,
            drop_headers_footers: ocrPipelineDropHF,
        };
    }
    let authorSortEnabled = $state(false);
    let noThinking = $state(true);
    let pdfBackend = $state<'js' | 'rust'>('js');
    let parsingFormat = $state<'xml' | 'json'>('xml');
    // Auto-speak chat replies via the platform's native TTS synth (macOS
    // `say` / Windows SAPI / Linux espeak). Off by default — voice mode
    // is opt-in.
    let autoSpeakReplies = $state(false);
    // Pre-fill suggestedTitle/Author/Year from the PDF /Info dict before
    // (or in lieu of) running the LLM. Default on — most academic PDFs
    // have decent embedded metadata, and the LLM still wins when it runs.
    let pdfMetadataPrefill = $state(true);
    // PLAN P8.1 — per-file conversion timeout. Default 120s; 0 = no
    // timeout (the page watchdog still catches frozen extractors).
    // Distinct from extractionMaxPages, which limits how MUCH text we
    // pull; this limits how LONG we wait for a single file.
    let conversionTimeoutSeconds = $state(120);
    // PLAN P10 step c -- worker concurrency for the Stapel pipeline.
    let extractionWorkers = $state(1);
    let llmWorkers = $state(1);
    // Frontend log verbosity (silent / error / info / debug). Mutated
    // via Settings -> Allgemein; the in-process global lives in
    // `src/lib/log.ts` so non-component code paths see the same value.
    let logVerbosity = $state<'silent' | 'error' | 'info' | 'debug'>('info');
    // Folder watcher: list of folders for v0.1.34+. Each entry is
    // implicitly active (presence == watching). The Rust side
    // canonicalizes paths before installing watchers and emits
    // 'folder-watch:added' Tauri events; +page.svelte owns the global
    // listener that calls batchManager.addItem.
    let watchFolders = $state<string[]>([]);
    let watchStatusMsg = $state('');

    // Local Model Management
    let localModels = $state<LocalModel[]>([]);
    let loadingModels = $state(false);
    let testingConnection = $state(false);
    let testResult = $state<{ success: boolean; message: string } | null>(null);
    let mlxPort = $state(8000);
    let llamacppPort = $state(8080);
    let mlxRunning = $state(false);
    let llamacppRunning = $state(false);
    let llamacppReady = $state(false);

    // ORT state
    let ortStatus = $state<'idle' | 'downloading' | 'loading' | 'ready' | 'error'>('idle');
    let ortProgress = $state(0);
    let ortProgressText = $state('');
    let ortSelectedModel = $state(ORT_MODELS[0].id);

    // WebLLM state
    let webllmStatus = $state<'idle' | 'downloading' | 'loading' | 'ready' | 'error'>('idle');
    let webllmProgress = $state(0);
    let webllmProgressText = $state('');
    let webllmSelectedModel = $state(WEBLLM_MODELS[0].id);

    // Restore engine status when navigating back to these providers
    $effect(() => {
        if (selectedProviderId === 'webllm' && webllmStatus === 'idle') {
            const loaded = getWebLLMLoadedModel();
            if (loaded) { webllmStatus = 'ready'; webllmSelectedModel = loaded; }
        }
        if (selectedProviderId === 'ort' && ortStatus === 'idle') {
            const loaded = getORTLoadedModel();
            if (loaded) { ortStatus = 'ready'; ortSelectedModel = loaded; }
        }
    });

    // Rate-limit round-robin fallback providers (ordered list of provider IDs)
    let roundRobinProviders = $state<string[]>([]);

    let sidecarStatus = $state(''); // '', 'starting', 'ready', 'error'
    let sidecarLogs = $state<string[]>([]);
    let sidecarLogsVisible = $state(false);
    let sidecarLogEl: HTMLTextAreaElement | null = $state(null);
    let mlxReady = $state(false);
    let saveIndicator = $state(false);
    let customModelInput = $state('');

    // MLX Model Management
    let mlxModels = $state<MlxModel[]>([]);
    let mlxCustomInput = $state('');
    let mlxModelCached = $state<Record<string, boolean>>({});
    let mlxCacheDir = $state('...');
    let mlxLogs = $state<string[]>([]);
    let mlxLogsVisible = $state(false);
    let mlxLogEl: HTMLTextAreaElement | null = $state(null);

    // Ollama Management
    let ollamaCustomInput = $state('');
    let ollamaPulling = $state<Record<string, number>>({});
    let ollamaStatus = $state(''); // '', 'starting', 'ready', 'error'
    let ollamaRunning = $state(false);
    let ollamaLogs = $state<string[]>([]);
    let ollamaLogsVisible = $state(false);
    let ollamaLogEl: HTMLTextAreaElement | null = $state(null);

    // ── Search Index Settings ────────────────────────────────────────────────
    let indexEnabled        = $state(false);
    let indexSearchMode     = $state<'text' | 'vector' | 'hybrid'>('hybrid');
    let indexBackendType    = $state<'local' | 'remote' | 'hybrid'>('local');
    let indexRemoteUrl      = $state('');
    let indexRemoteApiKey   = $state('');
    let indexEmbedderModel  = $state<string>('bge_m3');
    let indexEmbedderBackend = $state<'onnx' | 'gguf'>('onnx');
    let indexDevice         = $state<'auto' | 'cpu' | 'metal' | 'cuda'>('auto');
    /** Master switch for vector capabilities. When false, no embedder is
     *  ever loaded — saves multi-GB downloads + minutes of init time when
     *  the user only wants offline file cataloguing (L1 + L2) and
     *  full-text search via Tantivy. Mirrored into IndexConfig. */
    let indexUseVector          = $state(true);
    let indexEmbedderLocation   = $state<'client' | 'server'>('client');
    // Reranker: empty string = disabled (maps to null on the Rust side).
    // Other values are UI keys; mapped to Rust kebab-case via rerankerToRust.
    let indexRerankerModel  = $state<string>('');
    let indexRerankerTopN   = $state<number>(50);
    // Bi-encoder fallback (P13.5 follow-up).  Activates only when
    // `indexRerankerModel` is empty / 'none' AND this is true —
    // reuses the loaded dense embedder for cosine-similarity
    // reranking with zero extra disk / RAM.  Less accurate per
    // pair than the cross-encoder path, but a real lift over
    // no-rerank for users who haven't installed a separate
    // reranker model.
    let indexUseEmbedderAsReranker = $state<boolean>(false);
    /** Stage Z — alternate reranker for CJK/Arabic/Cyrillic queries. */
    let indexRerankerModelMultilingual = $state<string>('');
    // P19 — GLiNER named-entity recognition (opt-in, GGUF-only via CrispEmbed).
    // `indexNerModel` holds the serde kebab-case string directly (sent verbatim
    // to the Rust `NerModel` enum). Labels are a newline/comma-separated textarea.
    let indexNerEnabled     = $state<boolean>(false);
    let indexNerModel       = $state<string>('sauerkraut-gliner-lfm');
    let indexNerLabels      = $state<string>('');
    let indexNerThreshold   = $state<number>(0.5);
    let indexNerMaxEntities = $state<number>(30);
    let indexNerMaxChars    = $state<number>(8000);
    // Empty = use default ({data_dir}/models). Override is shared by
    // ONNX (fastembed/OrtPath) AND GGUF (CrispEmbed embedder + reranker)
    // downloads, so one setting controls every model weight on disk.
    let indexModelCacheDir  = $state<string>('');
    // Matryoshka truncation dim. 0 = use model default (no truncation).
    // Honored only on GGUF backend; ignored otherwise. Quality only holds
    // for MRL-trained models.
    let indexMatryoshkaDim  = $state<number>(0);
    // P13.5 follow-up — index-time translation target ISO 639-1 code.
    // Empty / 'none' = translation skipped; 'en' / 'de' / 'fr' etc. =
    // every extracted doc gets translated at ingest via m2m100, with
    // CLD3 text-LID auto-resolving as the source-language detector.
    // Result lands in the LanceDB text_translated + text_translated_lang
    // columns, then search-side `SearchFilters.prefer_translated_lang`
    // surfaces them.
    let indexTranslateTo    = $state<string>('none');

    // ── P13.6 Multimodal Settings (audio + image processing) ───────────────
    /** Audio + video extraction master switch.  When false, bg_ingest
     *  skips audio/video extensions entirely and L1 metadata-only is
     *  written.  Default true on feature-enabled builds; users with
     *  feature-disabled builds can leave it on (the runtime
     *  `is_audio_extraction_available()` gate fires first). */
    let indexAudioExtractionEnabled = $state<boolean>(true);
    /** ASR backend name from the crispasr registry.  `whisper` is the
     *  default (multilingual, 99 langs, base ~150 MB).  Other choices:
     *  `whisper-large-v3` (3 GB, higher accuracy),
     *  `parakeet`, `qwen3-omni`, …  Surfaced as a dropdown with a
     *  small curated set; advanced users can edit the
     *  IndexConfig directly to use any registry entry. */
    let indexAudioAsrBackend = $state<string>('whisper');
    /** Audio LID method.  `whisper` (default) reuses the loaded ASR
     *  model's LID head and auto-resolves a whisper-base ggml when
     *  the ASR backend is non-whisper-family (per `2b80345`).
     *  `silero`/`ecapa`/`firered` placeholders for future upstream
     *  registry entries — currently require an explicit lid-model
     *  path that the GUI doesn't surface. */
    let indexAudioLidMethod = $state<string>('whisper');
    /** P13.6 Step 7c — how deeply bg_ingest processes audio/video.
     *  'l1' = filesystem metadata only (fast, no probe, no ASR).
     *  'l2' = also runs the cheap symphonia probe to populate the
     *  audio_* columns (duration / codec / bitrate); no transcript.
     *  'l3' (default) = full pipeline including ASR transcription.
     *  Promote from L1/L2 to L3 via the "Transcribe" search-result
     *  action (Step 8 follow-up). */
    let indexIngestAudioLevel = $state<string>('l3');
    /** P13.7 Step 1+3 — image-side master switch + ingest level.
     *  Parallel to indexAudioExtractionEnabled / indexIngestAudioLevel.
     *  L1 = filesystem only, L2 = EXIF probe (no OCR), L3 (default)
     *  = EXIF + OCR.  Promote via the "Re-OCR" search-result
     *  action (Step 2). */
    let indexImageExtractionEnabled = $state<boolean>(true);
    let indexIngestImageLevel = $state<string>('l3');
    /** P13.7 Step 4 — when on, each indexed image is also pushed
     *  to the configured CrispLens server (POST /api/ingest/upload-local).
     *  Default off because pushing every image upstream is a
     *  privacy-sensitive action; users opt in explicitly. */
    let indexCrispLensImageEnrichment = $state<boolean>(false);

    /** P13.7 Step 5 — cloud-backup HTTP sync settings.
     *  URL persists in IndexConfig (JSON-safe); the API key is
     *  write-only — the input field captures it once and we hand it
     *  to `sync_cb_set_token` for OS-keychain storage.  Empty + save
     *  clears the stored token. */
    let indexCloudBackupUrl                = $state<string>('');
    let indexCloudBackupApiKeyInput        = $state<string>('');
    let indexCloudBackupPushManifests      = $state<boolean>(false);
    let indexCloudBackupPushEmbeddings     = $state<boolean>(false);
    let indexCloudBackupPullManifests      = $state<boolean>(false);
    /** P13.7 Stage I — tiered-cache opt-in.  When true, pulls
     *  include the body text; when false (default), metadata-only. */
    let indexCloudBackupPullFullText       = $state<boolean>(false);
    /** P13.7 Stage P — local DB size cap in GB (0 = unlimited). */
    let indexLocalMaxSizeGb        = $state<number>(0);
    /** P13.7 Stage N — partition controls.  These don't persist
     *  per-run (the partition map itself is the persistent
     *  artifact); the form just remembers the user's last picks. */
    let cloudBackupPartitionRoot   = $state<string>('');
    let cloudBackupPartitionMax    = $state<number>(64);
    let cloudBackupPartitionDepth  = $state<number>(1);
    let cloudBackupPartitionStatus = $state<string>('');
    /** Last status payload (refreshed when the panel opens or after
     *  a save).  Drives the inline "connected to cloud-backup
     *  0.1.0" hint + the watermark display. */
    let indexCloudBackupStatus: any        = $state(null);
    let indexCloudBackupTokenSavedMsg      = $state<string>('');
    /** Stage U — thin client.  When false, bg_ingest skips extraction
     *  and only writes L1 metadata; the VPS extraction worker handles
     *  full_text from the uploaded bytes. */
    let indexLocalExtractionEnabled        = $state<boolean>(true);
    let indexSkeletonOnly                  = $state<boolean>(false);

    // ── Stage T — admin key management ────────────────────────────────────
    let cbAdminToken      = $state<string>('');
    let cbAdminMintName   = $state<string>('');
    let cbAdminMintOwner  = $state<string>('');
    let cbAdminMintedKey  = $state<string>('');
    let cbAdminRevokeName = $state<string>('');
    let cbAdminKeys: any[]= $state([]);
    let cbAdminBusy       = $state<boolean>(false);
    let cbAdminMsg        = $state<string>('');

    // ── Catalogs (named bundles of the above settings) ────────────────────
    interface Catalog {
        id:       string;            // uuid-ish, stable
        name:     string;            // user-visible
        dataDir:  string;            // override of indexDataDir for this catalog
        mode:     'text' | 'vector' | 'hybrid';
        backend:  'local' | 'remote' | 'hybrid';
        remoteUrl: string;
        embedderModel: string;
        embedderBackend: 'onnx' | 'gguf';
        device:   'auto' | 'cpu' | 'metal' | 'cuda';
        createdAt: number;
    }
    let catalogs       = $state<Catalog[]>([]);
    let activeCatalogId = $state<string | null>(null);
    let renamingCatalogId = $state<string | null>(null);
    let renameDraft    = $state('');

    // Which UI model values have a GGUF counterpart in CrispEmbed. Kept in
    // sync with `EmbedderModel::gguf_registry_name()` on the Rust side.
    // Models with GGUF counterpart in CrispEmbed (post-v0.2.3 sync, 22 entries).
    const GGUF_CAPABLE_MODELS = new Set([
        // Only models that exist in the EmbedderModel enum AND have a GGUF
        // equivalent in CrispEmbed.  Additional GGUF-only models can be added
        // here once corresponding ONNX enum variants are created.
        'pixie', 'pixie_q', 'pixie_int4', 'pixie_int4_full',
        'snowflake_l', 'snowflake_l_fp16', 'snowflake_l_int8',
        'snowflake_l_q4', 'snowflake_l_q4f16', 'snowflake_l_o4', 'snowflake_l_fp32',
        'octen', 'jina_nano', 'jina_small',
        'qwen3_embed', 'qwen3_embed_int8', 'qwen3_embed_uint8',
        // Sync of CrispEmbed model registry (May 2026):
        'multilingual_e5_small', 'multilingual_e5_base', 'multilingual_e5_large',
        'bge_small_en_v15', 'bge_base_en_v15', 'bge_large_en_v15',
        'nomic_embed_v15', 'mxbai_large_v1', 'minilm_l6_v2',
        'embedding_gemma_300m', 'gte_base_en_v15', 'gte_large_en_v15',
        // Older HEAD-side aliases kept for backwards compat with persisted prefs:
        'mxbai_embed_large_v1', 'nomic_embed_text_v15', 'all_mini_lm_l6_v2',
    ]);

    /** GUI model keys whose license requires explicit consent before
     *  download/use — non-commercial (CC-BY-NC: the Jina retrieval models)
     *  or use-restricted (Gemma Terms: EmbeddingGemma). Selecting one prompts
     *  a confirmation dialog; declining persists the key in
     *  `nonCommercialDeclined` and disables that <option> until cleared.
     *  Confirming records consent with the backend (so the Rust download gate
     *  in `index::license_consent` lets it through). */
    const NON_COMMERCIAL_MODELS = new Set(['jina_nano', 'jina_small', 'embedding_gemma_300m']);
    let nonCommercialDeclined = $state<Set<string>>(new Set());
    /** Backend consent keys (= `indexEmbedderToRust(...)`) the user has
     *  accepted; persisted and replayed to the backend on startup so search /
     *  ingest don't hit the consent gate after a one-time confirmation. */
    let acceptedModelLicenses = $state<Set<string>>(new Set());

    /** Record consent for a backend model key: tell the Rust gate + persist. */
    async function recordModelLicenseConsent(key: string) {
        if (!key || acceptedModelLicenses.has(key)) return;
        acceptedModelLicenses = new Set([...acceptedModelLicenses, key]);
        await saveSetting('acceptedModelLicenses', Array.from(acceptedModelLicenses));
        try { await invoke('embedder_accept_model_license', { keys: [key] }); } catch { /* backend not wired yet */ }
    }

    /** Reranker dropdowns: the Jina v2 reranker is CC-BY-NC. Confirm + record
     *  consent on selection; revert to "off" if declined (the Rust gate would
     *  otherwise hard-error at score time). */
    async function handleRerankerChange(e: Event, which: 'main' | 'multi') {
        const sel = e.target as HTMLSelectElement;
        const key = 'jina-reranker-v2-base-multilingual';
        if (sel.value === 'jina_v2_multi' && !acceptedModelLicenses.has(key)) {
            const ok = await ask(i18n.t.settings.index.non_commercial_confirm, {
                title: 'License confirmation — CC-BY-NC-4.0', kind: 'warning'
            });
            if (!ok) {
                if (which === 'main') indexRerankerModel = ''; else indexRerankerModelMultilingual = '';
                return;
            }
            await recordModelLicenseConsent(key);
        }
    }
    /** P19 — NER model dropdown: the German-tuned Sauerkraut-GLiNER model
     *  ships under the LFM Open License v1.0 (restricted). Confirm + record
     *  consent on selection; revert to the permissive DeBERTa model if
     *  declined (the Rust gate would otherwise hard-error at load time). */
    async function handleNerModelChange(e: Event) {
        const sel = e.target as HTMLSelectElement;
        const key = 'sauerkraut-gliner-lfm';
        if (sel.value === 'sauerkraut-gliner-lfm' && !acceptedModelLicenses.has(key)) {
            const ok = await ask(i18n.t.settings.index.ner_license_confirm, {
                title: 'License confirmation — LFM Open License v1.0', kind: 'warning'
            });
            if (!ok) {
                indexNerModel = 'gliner-deberta';
                return;
            }
            await recordModelLicenseConsent(key);
        }
    }
    function supportsGguf(uiModel: string): boolean {
        return GGUF_CAPABLE_MODELS.has(uiModel);
    }
    /** Reactive boolean — true when the currently-selected embedder has a
     *  verified GGUF equivalent (the *model* supports it). */
    let ggufModelSupported = $derived(supportsGguf(indexEmbedderModel));

    /** True iff the running binary was compiled with a CrispEmbed feature.
     *  Filled from the `index_capabilities` Tauri command at mount time.
     *  Default: `false` — the dev `npm run tauri dev` build uses
     *  `--no-default-features` so CrispEmbed is NOT linked in. To enable it,
     *  rebuild with e.g. `cargo run --features crispembed-vulkan`. */
    let crispEmbedCompiledIn = $state(false);

    /** Which GPU backend was linked into CrispEmbed at compile time —
     *  `'vulkan'` / `'cuda'` / `'metal'` / `'cpu'` or `null` if the
     *  `crispembed` feature itself is off. Drives the device dropdown
     *  filter when the GGUF engine is selected. */
    let crispEmbedGpu = $state<string | null>(null);

    /** Final gate: GGUF can be selected iff the model has a spec AND the
     *  binary actually contains the CrispEmbed backend code. */
    let ggufAvailable = $derived(ggufModelSupported && crispEmbedCompiledIn);

    /** Upstream CrispEmbed model registry (43 entries as of crispembed 0.3.2).
     *  Empty Vec when the `crispembed` feature is off.  Surfaced as an
     *  informational expandable panel beneath the engine toggle so users
     *  can see what GGUF models the linked CrispEmbed ships with — the
     *  existing embedder dropdown still keys off `EmbedderModel` enum
     *  variants today, so non-enum entries are read-only.  Wiring full
     *  registry-driven selection is tracked separately. */
    type EmbedderRegistryEntry = { name: string; desc: string; filename: string; size: string; cached: boolean };
    let embedderRegistry = $state<EmbedderRegistryEntry[]>([]);
    /** Name of registry model selected via Stage X UI ("" = use enum dropdown). */
    let indexEmbedderModelName = $state<string>('');
    let registryDownloadBusy = $state<string>('');  // name of model currently downloading
    let registryDownloadMsg  = $state<string>('');

    /** Approximate download size (MB) for the selected embedder, returned by
     *  `index_model_download_mb`. 0 means unknown. Drives the "first run
     *  downloads ~X MB" hint. */
    let modelDownloadMb = $state(0);

    /** Whether `m` can be currently selected. Engine-only filter — NC
     *  models are NOT disabled here; instead, picking a previously-declined
     *  one re-shows the confirmation dialog so the user can opt back in. */
    function isModelAvailable(m: string): boolean {
        if (indexEmbedderBackend === 'gguf' && !GGUF_CAPABLE_MODELS.has(m)) return false;
        return true;
    }

    /** Optional suffix shown next to a model name in the dropdown to signal
     *  its license / state. */
    function ncLabelSuffix(m: string): string {
        if (nonCommercialDeclined.has(m)) return ' (NC — confirmation needed)';
        if (NON_COMMERCIAL_MODELS.has(m)) return ' (NC license)';
        return '';
    }

    async function persistNonCommercialDeclines() {
        await saveSetting('nonCommercialDeclined', Array.from(nonCommercialDeclined));
    }

    function resetNonCommercialDeclines() {
        nonCommercialDeclined = new Set();
        persistNonCommercialDeclines();
    }

    async function downloadRegistryModel(name: string) {
        // License-consent gate: ask the backend whether this registry model
        // needs consent; if so, confirm before downloading and pass acceptance.
        let accepted = acceptedModelLicenses.has(name);
        if (!accepted) {
            let lic: string | null = null;
            try { lic = await invoke<string | null>('embedder_model_license', { name }); } catch { lic = null; }
            if (lic) {
                const ok = await ask(i18n.t.settings.index.non_commercial_confirm, {
                    title: `License confirmation — ${lic}`,
                    kind: 'warning'
                });
                if (!ok) { registryDownloadMsg = 'Download cancelled (license not accepted).'; return; }
                await recordModelLicenseConsent(name);
                accepted = true;
            }
        }
        registryDownloadBusy = name;
        registryDownloadMsg  = '';
        try {
            await invoke('embedder_download_registry_model', { name, acceptLicense: accepted });
            // Refresh the registry list so `cached` flags update.
            embedderRegistry = await invoke<EmbedderRegistryEntry[]>('embedder_registry_list');
            registryDownloadMsg = 'Downloaded.';
        } catch (e: any) {
            registryDownloadMsg = String(e);
        } finally {
            registryDownloadBusy = '';
        }
    }

    function selectRegistryModel(name: string) {
        indexEmbedderModelName = name;
        // When a registry model is active, force GGUF backend (registry models are GGUF only).
        indexEmbedderBackend = 'gguf';
    }

    /** Switch the Inference Engine. If the currently-selected model isn't
     *  available on the new engine, auto-pick the first one that is so the
     *  dropdown isn't left in a phantom state. */
    function onSelectEngine(engine: 'onnx' | 'gguf') {
        indexEmbedderBackend = engine;
        if (!isModelAvailable(indexEmbedderModel)) {
            // Find the first available option in the dropdown order.
            const candidates = ['bge_m3', 'all_mini_lm_l6_v2', 'bge_small_en_v15',
                'multilingual_e5_small', 'bge_base_en_v15', 'multilingual_e5_base',
                'nomic_embed_text_v15', 'bge_large_en_v15', 'multilingual_e5_large',
                'mxbai_embed_large_v1', 'octen', 'pixie_q', 'snowflake_l_int8',
                'jina_nano', 'multilingual_mini_lm'];
            const next = candidates.find(c => isModelAvailable(c));
            if (next) {
                indexEmbedderModel = next;
                refreshModelDownloadSize();
            }
        }
    }

    async function handleEmbedderChange(e: Event) {
        const sel = e.target as HTMLSelectElement;
        const val = sel.value;
        // For any NC model — first time, or after a previous decline —
        // re-show the dialog. Confirming opts in (and clears the decline);
        // declining records the decline and reverts the dropdown.
        if (NON_COMMERCIAL_MODELS.has(val) && val !== indexEmbedderModel) {
            const confirmed = await ask(i18n.t.settings.index.non_commercial_confirm, {
                title: 'Non-Commercial License Confirmation',
                kind: 'warning'
            });
            if (!confirmed) {
                if (!nonCommercialDeclined.has(val)) {
                    nonCommercialDeclined = new Set([...nonCommercialDeclined, val]);
                    await persistNonCommercialDeclines();
                }
                sel.value = indexEmbedderModel;
                return;
            }
            // Confirmed — record consent with the backend (so the Rust gate
            // lets search/ingest download+use it) and clear any prior decline.
            await recordModelLicenseConsent(indexEmbedderToRust(val));
            if (nonCommercialDeclined.has(val)) {
                const next = new Set(nonCommercialDeclined);
                next.delete(val);
                nonCommercialDeclined = next;
                await persistNonCommercialDeclines();
            }
        }
        indexEmbedderModel = val;
        await refreshModelDownloadSize();
    }

    // ── Catalog helpers ───────────────────────────────────────────────────

    /** Snapshot the live Search-Index settings into a Catalog payload. */
    function liveSettingsAsCatalog(name: string, id?: string): Catalog {
        return {
            id: id ?? (crypto.randomUUID?.() ?? `cat-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`),
            name,
            dataDir: indexDataDir,
            mode: indexSearchMode,
            backend: indexBackendType,
            remoteUrl: indexRemoteUrl,
            embedderModel: indexEmbedderModel,
            embedderBackend: indexEmbedderBackend,
            device: indexDevice,
            createdAt: Date.now(),
        };
    }

    /** Apply a saved catalog to the live Search-Index inputs. Does NOT
     *  immediately re-init the Rust backend — that happens when the user
     *  presses Apply (or via `applyCatalog`). */
    function loadCatalogIntoInputs(c: Catalog) {
        indexDataDir         = c.dataDir;
        indexSearchMode      = c.mode;
        indexBackendType     = c.backend;
        indexRemoteUrl       = c.remoteUrl ?? '';
        indexEmbedderModel   = c.embedderModel;
        indexEmbedderBackend = c.embedderBackend;
        indexDevice          = c.device;
    }

    async function persistCatalogs() {
        await saveSetting('catalogs', $state.snapshot(catalogs));
        await saveSetting('activeCatalogId', activeCatalogId);
    }

    async function createCatalogFromCurrent() {
        // Default name derived from data-dir basename or a sequence number.
        const base = (indexDataDir || '').replace(/[\\/]$/, '').split(/[\\/]/).pop() || 'catalog';
        let name = base;
        let n = 2;
        while (catalogs.some(c => c.name === name)) {
            name = `${base}-${n++}`;
        }
        const cat = liveSettingsAsCatalog(name);
        catalogs = [...catalogs, cat];
        activeCatalogId = cat.id;
        await persistCatalogs();
    }

    async function deleteCatalog(id: string) {
        if (!confirm(`Catalog "${catalogs.find(c => c.id === id)?.name ?? id}" entfernen? (Index-Daten bleiben auf der Festplatte.)`)) return;
        catalogs = catalogs.filter(c => c.id !== id);
        if (activeCatalogId === id) activeCatalogId = catalogs[0]?.id ?? null;
        await persistCatalogs();
    }

    function startRename(id: string) {
        renamingCatalogId = id;
        renameDraft = catalogs.find(c => c.id === id)?.name ?? '';
    }

    async function commitRename() {
        if (!renamingCatalogId || !renameDraft.trim()) {
            renamingCatalogId = null;
            return;
        }
        catalogs = catalogs.map(c =>
            c.id === renamingCatalogId ? { ...c, name: renameDraft.trim() } : c
        );
        renamingCatalogId = null;
        await persistCatalogs();
    }

    async function selectCatalog(id: string) {
        const c = catalogs.find(c => c.id === id);
        if (!c) return;
        activeCatalogId = id;
        loadCatalogIntoInputs(c);
        await persistCatalogs();
    }

    /** Persist current inputs back to the active catalog (silent). */
    async function syncActiveCatalogFromInputs() {
        if (!activeCatalogId) return;
        catalogs = catalogs.map(c =>
            c.id === activeCatalogId
                ? { ...liveSettingsAsCatalog(c.name, c.id), createdAt: c.createdAt }
                : c
        );
        await saveSetting('catalogs', $state.snapshot(catalogs));
    }

    /** Ask the backend for the approximate first-run download size of the
     *  currently-selected embedder + engine combination, so the UI shows
     *  the real GGUF or ONNX size instead of a generic "~500 MB". */
    async function refreshModelDownloadSize() {
        try {
            modelDownloadMb = await invoke<number>('index_model_download_mb', {
                model: indexEmbedderToRust(indexEmbedderModel),
                backend: indexEmbedderBackend,
            });
        } catch {
            modelDownloadMb = 0;
        }
    }
    // Recompute size whenever model OR engine changes.
    $effect(() => {
        // touch reactive deps so $effect runs on either change
        const _ = `${indexEmbedderModel}|${indexEmbedderBackend}`;
        refreshModelDownloadSize();
    });
    let indexDataDir        = $state('');
    let indexStatus         = $state<'idle' | 'loading' | 'ok' | 'error'>('idle');
    let indexStatusMsg      = $state('');
    let indexInitProgress   = $state('');
    /** Live bytes-of-total during embedder download — populated by the
     *  `index://download-progress` Tauri event, drives a second progress
     *  bar inside the init message so the user sees real download
     *  movement instead of a stuck-at-5% bar. */
    let indexDownloadProgress = $state<{ repo: string; file: string; bytes_done: number; bytes_total: number; pct: number } | null>(null);
    let indexInitPct        = $state(0);
    let indexIvfRunning     = $state(false);
    let indexScalarRunning  = $state(false);

    // Benchmarking
    let benchProviders = $state<string[]>([]);

    // Embedding benchmark state (FastEmbed vs CrispEmbed for the same model).
    interface EmbedderBenchResult {
        backend: 'onnx' | 'gguf';
        model_id: string;
        load_time_ms: number;
        embed_time_ms: number;
        texts_per_second: number;
        dim: number;
        vectors_count: number;
        self_cosine: number;
        error: string | null;
    }
    let embedderBenchModel    = $state('bge_small_en_v15');
    let embedderBenchTexts    = $state('');
    let embedderBenchRunning  = $state(false);
    let embedderBenchResults  = $state<EmbedderBenchResult[]>([]);

    /** Default 8-text corpus across English + German. Mirrors the eval set
     *  CrispEmbed publishes its compatibility numbers on. */
    const DEFAULT_BENCH_TEXTS = [
        'A small step for a man, a giant leap for mankind.',
        'Climate models suggest accelerating polar ice loss.',
        'The composition of black holes remains poorly understood.',
        'A neural network learns from gradients, not rules.',
        'Die Theorie der Relativität wurde 1905 veröffentlicht.',
        'Diese Bibliothek umfasst dreitausend mittelalterliche Manuskripte.',
        'Schubert komponierte die Winterreise im Jahr 1827.',
        'Die Quantenmechanik beschreibt subatomare Phänomene.',
    ];

    async function runEmbedderBenchmark() {
        if (embedderBenchRunning) return;
        embedderBenchRunning = true;
        embedderBenchResults = [];
        const texts = embedderBenchTexts.trim()
            ? embedderBenchTexts.split('\n').map(s => s.trim()).filter(Boolean)
            : DEFAULT_BENCH_TEXTS;
        const modelId = indexEmbedderToRust(embedderBenchModel);
        const engines: Array<'onnx' | 'gguf'> = crispEmbedCompiledIn ? ['onnx', 'gguf'] : ['onnx'];
        try {
            for (const engine of engines) {
                try {
                    const result = await invoke<EmbedderBenchResult>('index_benchmark_embedder', {
                        model: modelId,
                        backend: engine,
                        texts,
                    });
                    embedderBenchResults = [...embedderBenchResults, result];
                } catch (e: any) {
                    embedderBenchResults = [...embedderBenchResults, {
                        backend: engine,
                        model_id: modelId,
                        load_time_ms: 0,
                        embed_time_ms: 0,
                        texts_per_second: 0,
                        dim: 0,
                        vectors_count: 0,
                        self_cosine: 0,
                        error: String(e?.message ?? e),
                    }];
                }
            }
        } finally {
            embedderBenchRunning = false;
        }
    }
    let benchModels = $state<Record<string, string>>({});
    let benchDocuments = $state<string[]>([]);
    let benchRuns = $state(1);
    let benchPromptMode = $state<'batch' | 'custom'>('batch');
    let benchCustomPrompt = $state('List the first 10 prime numbers. Be concise.');
    let benchResults = $state<any[]>([]);
    let benchRunning = $state(false);
    let benchModal = $state<any>(null);

    // Automated Licenses. `scripts/generate-licenses.js` writes either the
    // legacy bare-array shape or the newer `{generatedAt, counts, licenses}`
    // wrapper — we accept both so an older static/licenses.json on disk
    // doesn't break a freshly-built app.
    let automatedLicenses = $state<any[]>([]);
    let licensesGeneratedAt = $state<string | null>(null);
    let licensesError = $state<string | null>(null);
    let licensesLoading = $state(false);
    let licenseSearch = $state('');
    let filteredLicenses = $derived(automatedLicenses.filter(l =>
        l.name.toLowerCase().includes(licenseSearch.toLowerCase()) ||
        l.author?.toLowerCase().includes(licenseSearch.toLowerCase()) ||
        (l.license ?? '').toLowerCase().includes(licenseSearch.toLowerCase())
    ));

    // App version (Vite-injected from package.json — see vite.config.js).
    const appVersion = typeof __APP_VERSION__ !== 'undefined' ? __APP_VERSION__ : '?';

    onMount(() => {
        let cleanup = () => {};
        (async () => {
        const savedProviders = await getSetting('providers');
        if (savedProviders) {
            const merged = DEFAULT_PROVIDERS.map(def => {
                const saved = (savedProviders as LLMProvider[]).find(p => p.id === def.id);
                return saved ? { ...def, ...saved } : def;
            });
            // One-time migration: move any plain-text apiKey out of
            // settings.json into the OS keychain, replace with a sentinel.
            // Idempotent — entries already on sentinels are no-op.
            const { providers: migrated, migrated: n } = await migrateProviderKeysToKeychain(merged);
            providers = migrated;
            if (n > 0) {
                // Persist the rewritten list so the plain keys never
                // touch disk again from this point on.
                await saveSetting('providers', $state.snapshot(providers));
                console.log(`secrets: migrated ${n} provider API key(s) to OS keychain`);
            }
        }

        activeProviderId = await getSetting('activeProviderId', 'ollama');
        exportPath = await getSetting('exportPath', '');
        exportPathMode = await getSetting('exportPathMode', 'absolute') as any;
        pathTemplate = await getSetting('pathTemplate', '{Author}/{Year}/{Title}') as string;
        saveTxt = await getSetting('saveTxt', true);
        currentLanguage = await getSetting('language', 'en') as Language;
        i18n.setLanguage(currentLanguage);

        llmMaxChars = await getSetting('llmMaxChars', 5000);
        llmContextLimit = await getSetting('llmContextLimit', 4096);
        ocrEnabled = await getSetting('ocrEnabled', false);
        ocrTier    = await getSetting('ocrTier', 'auto') as 'auto' | 'tier1' | 'tier2' | 'tier3';
        ocrRecLang = await getSetting('ocrRecLang', 'auto') as 'auto' | 'latin' | 'cjk';
        invoke('bg_ingest_set_ocr', { enabled: ocrEnabled, tier: ocrTier, recLang: ocrRecLang }).catch(() => {});
        ocrPipelineEnabled  = await getSetting('ocrPipelineEnabled', false) as boolean;
        ocrPipelineRouter   = await getSetting('ocrPipelineRouter', true) as boolean;
        ocrPipelineCleanup  = await getSetting('ocrPipelineCleanup', true) as boolean;
        ocrPipelineDenoise  = await getSetting('ocrPipelineDenoise', false) as boolean;
        ocrPipelineMinChars = await getSetting('ocrPipelineMinChars', 8) as number;
        ocrPipelineMinConf  = await getSetting('ocrPipelineMinConf', 0.5) as number;
        ocrPipelineAdvanced = await getSetting('ocrPipelineAdvanced', false) as boolean;
        ocrStages           = await getSetting('ocrStages', []) as OcrStage[];
        ocrPipelinePunct    = await getSetting('ocrPipelinePunct', '') as string;
        ocrPipelineLayout    = await getSetting('ocrPipelineLayout', false) as boolean;
        ocrPipelineLayoutThr = await getSetting('ocrPipelineLayoutThr', 0.25) as number;
        ocrPipelineLayoutEngine = await getSetting('ocrPipelineLayoutEngine', 'rtdetr') as string;
        ocrPipelineDropHF    = await getSetting('ocrPipelineDropHF', false) as boolean;
        ocrPipelineSr        = await getSetting('ocrPipelineSr', false) as boolean;
        ocrPipelineSrMaxPx   = await getSetting('ocrPipelineSrMaxPx', 1200) as number;
        ocrPipelineSrEngine  = await getSetting('ocrPipelineSrEngine', 'pan') as string;
        ocrPipelineRestore   = await getSetting('ocrPipelineRestore', false) as boolean;
        ocrPipelineRestoreEngine = await getSetting('ocrPipelineRestoreEngine', 'restormer') as string;
        ocrPipelineRestoreTask = await getSetting('ocrPipelineRestoreTask', 'denoise') as string;
        ocrPipelineDewarp    = await getSetting('ocrPipelineDewarp', false) as boolean;
        ocrPipelineDewarpEngine  = await getSetting('ocrPipelineDewarpEngine', 'basic') as string;
        invoke('bg_ingest_set_ocr_pipeline', { config: ocrPipelineConfig() }).catch(() => {});
        authorSortEnabled = await getSetting('authorSortEnabled', false);
        noThinking = await getSetting('noThinking', true);
        autoSpeakReplies = await getSetting('autoSpeakReplies', false);
        pdfMetadataPrefill = await getSetting('pdfMetadataPrefill', true);
        conversionTimeoutSeconds = (await getSetting('conversionTimeoutSeconds', 120)) as number;
        extractionWorkers = (await getSetting('extractionWorkers', 1)) as number;
        llmWorkers = (await getSetting('llmWorkers', 1)) as number;
        logVerbosity = (await getSetting('logVerbosity', 'info')) as any;
        // Migrate v0.1.32 single-folder shape on first read.
        const stored = (await getSetting('watchFolders', null)) as string[] | null;
        if (stored != null) {
            watchFolders = stored;
        } else {
            const legacyEnabled = (await getSetting('watchEnabled', false)) as boolean;
            const legacyFolder = (await getSetting('watchFolder', '')) as string;
            watchFolders = legacyEnabled && legacyFolder ? [legacyFolder] : [];
        }
        try {
            const active = await invoke<string[]>('watch_list');
            // Resync UI list from backend in case +page.svelte already
            // started watchers from saved state — keeps the two views
            // aligned even after migration.
            for (const p of active) {
                if (!watchFolders.includes(p)) watchFolders.push(p);
            }
            watchStatusMsg = active.length > 0
                ? `${active.length} folder(s) watched`
                : '';
        } catch { /* command not yet wired */ }
        roundRobinProviders = (await getSetting('roundRobinProviders', [])) as string[];
        pdfBackend = await getSetting('pdfBackend', 'js') as any;
        parsingFormat = await getSetting('parsingFormat', 'xml') as any;
        
        console.log(`[Settings] Loading prompt... lang=${currentLanguage}, format=${parsingFormat}`);
        const savedPrompt = await getSetting('llmPrompt', '');
        if (savedPrompt) {
            console.log(`[Settings] Using saved prompt.`);
            llmPrompt = savedPrompt;
        } else {
            console.log(`[Settings] No saved prompt found, generating default.`);
            llmPrompt = getDefaultPrompt(parsingFormat, currentLanguage);
        }

        localModels = await getSetting('localModels', [
            { id: 'qwen3-0.6b', name: 'Qwen 3 0.6B (Q4_K_M)', path: '', isDownloaded: false, isActive: true, downloadUrl: 'https://huggingface.co/Mungert/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-q4_k_m.gguf' },
            { id: 'ministral-3b', name: 'Ministral 3B (Q4_K_M)', path: '', isDownloaded: false, isActive: false, downloadUrl: 'https://huggingface.co/bartowski/Ministral-3b-instruct-GGUF/resolve/main/Ministral-3b-instruct-Q4_K_M.gguf' }
        ]);

        mlxModels = await getSetting('mlxModels', [...DEFAULT_MLX_MODELS]);
        mlxPort = await getSetting('mlxPort', 8000);
        llamacppPort = await getSetting('llamacppPort', 8080);

        // Search Index settings
        indexEnabled       = await getSetting('indexEnabled', false);
        indexSearchMode    = await getSetting('indexSearchMode', 'hybrid') as any;
        indexBackendType   = await getSetting('indexBackendType', 'local') as any;
        indexRemoteUrl     = await getSetting('indexRemoteUrl', '');
        indexRemoteApiKey  = await getSetting('indexRemoteApiKey', '');
        indexEmbedderModel = await getSetting('indexEmbedderModel', 'bge_m3') as any;
        indexEmbedderBackend = await getSetting('indexEmbedderBackend', 'onnx') as any;
        indexDevice        = await getSetting('indexDevice', 'auto') as any;
        indexUseVector          = await getSetting('indexUseVector', true) as boolean;
        indexEmbedderLocation   = await getSetting('indexEmbedderLocation', 'client') as any;
        indexRerankerModel = await getSetting('indexRerankerModel', '') as any;
        indexRerankerModelMultilingual = await getSetting('indexRerankerModelMultilingual', '') as any;
        indexRerankerTopN  = await getSetting('indexRerankerTopN', 50) as number;
        indexUseEmbedderAsReranker = await getSetting('indexUseEmbedderAsReranker', false) as boolean;
        indexNerEnabled     = await getSetting('indexNerEnabled', false) as boolean;
        indexNerModel       = await getSetting('indexNerModel', 'sauerkraut-gliner-lfm') as string;
        indexNerLabels      = await getSetting('indexNerLabels', '') as string;
        indexNerThreshold   = await getSetting('indexNerThreshold', 0.5) as number;
        indexNerMaxEntities = await getSetting('indexNerMaxEntities', 30) as number;
        indexNerMaxChars    = await getSetting('indexNerMaxChars', 8000) as number;
        indexModelCacheDir = await getSetting('indexModelCacheDir', '');
        indexMatryoshkaDim = await getSetting('indexMatryoshkaDim', 0) as number;
        indexTranslateTo   = await getSetting('indexTranslateTo', 'none') as string;
        indexAudioExtractionEnabled = await getSetting('indexAudioExtractionEnabled', true) as boolean;
        indexAudioAsrBackend = await getSetting('indexAudioAsrBackend', 'whisper') as string;
        indexAudioLidMethod  = await getSetting('indexAudioLidMethod', 'whisper') as string;
        indexIngestAudioLevel = await getSetting('indexIngestAudioLevel', 'l3') as string;
        indexImageExtractionEnabled = await getSetting('indexImageExtractionEnabled', true) as boolean;
        indexIngestImageLevel = await getSetting('indexIngestImageLevel', 'l3') as string;
        indexCrispLensImageEnrichment = await getSetting('indexCrispLensImageEnrichment', false) as boolean;
        // P13.7 Step 5 — cloud-backup sync settings.
        indexCloudBackupUrl            = await getSetting('indexCloudBackupUrl', '');
        indexCloudBackupPushManifests  = await getSetting('indexCloudBackupPushManifests', false) as boolean;
        indexCloudBackupPushEmbeddings = await getSetting('indexCloudBackupPushEmbeddings', false) as boolean;
        indexCloudBackupPullManifests  = await getSetting('indexCloudBackupPullManifests', false) as boolean;
        indexCloudBackupPullFullText   = await getSetting('indexCloudBackupPullFullText', false) as boolean;
        indexLocalExtractionEnabled    = await getSetting('indexLocalExtractionEnabled', true) as boolean;
        indexSkeletonOnly              = await getSetting('indexSkeletonOnly', false) as boolean;
        indexEmbedderModelName         = await getSetting('indexEmbedderModelName', '') as string;
        indexLocalMaxSizeGb            = await getSetting('indexLocalMaxSizeGb', 0) as number;
        backupDriveId                  = await getSetting('backupDriveId', '') as string;
        backupKeepDaily                = await getSetting('backupKeepDaily', 7) as number;
        cloudBackupPartitionRoot       = await getSetting('cloudBackupPartitionRoot', '');
        cloudBackupPartitionMax        = await getSetting('cloudBackupPartitionMax', 64) as number;
        cloudBackupPartitionDepth      = await getSetting('cloudBackupPartitionDepth', 1) as number;
        indexDataDir       = await getSetting('indexDataDir', '');
        catalogs           = (await getSetting('catalogs', [])) as Catalog[];
        activeCatalogId    = await getSetting('activeCatalogId', null);
        const declinedArr  = (await getSetting('nonCommercialDeclined', [])) as string[];
        nonCommercialDeclined = new Set(declinedArr ?? []);
        const acceptedArr  = (await getSetting('acceptedModelLicenses', [])) as string[];
        acceptedModelLicenses = new Set(acceptedArr ?? []);
        // Replay accepted licenses to the backend gate so previously-confirmed
        // restrictive models keep working without re-prompting this session.
        if (acceptedModelLicenses.size > 0) {
            try { await invoke('embedder_accept_model_license', { keys: Array.from(acceptedModelLicenses) }); } catch { /* backend not wired yet */ }
        }
        // Sync saved config into the backend
        try {
            await invoke('index_get_config').then(() => {}).catch(() => {});
        } catch { /* index not yet wired */ }
        // Discover what backends were compiled in (CrispEmbed is feature-gated).
        try {
            const caps = await invoke<{ crispembed: boolean; crispembed_gpu: string | null }>('index_capabilities');
            crispEmbedCompiledIn = !!caps.crispembed;
            crispEmbedGpu = caps.crispembed_gpu ?? null;
        } catch { /* command not available */ }
        // CrispEmbed bundled-registry browse (empty when feature is off).
        try {
            embedderRegistry = await invoke<EmbedderRegistryEntry[]>('embedder_registry_list');
        } catch { /* command not available */ }
        await refreshModelDownloadSize();
        // Check if index is already initialized (e.g. after navigating back to Settings)
        try {
            const ready = await invoke<boolean>('index_is_ready');
            if (ready) indexStatus = 'ok';
        } catch { /* command not available */ }

        licensesLoading = true;
        licensesError = null;
        try {
            const resp = await fetch('/licenses.json');
            if (!resp.ok) {
                throw new Error(`HTTP ${resp.status} ${resp.statusText}`);
            }
            const raw = await resp.json();
            if (Array.isArray(raw)) {
                // Legacy shape: bare array of license entries.
                automatedLicenses = raw;
                licensesGeneratedAt = null;
            } else if (raw && Array.isArray(raw.licenses)) {
                automatedLicenses = raw.licenses;
                licensesGeneratedAt = raw.generatedAt ?? null;
            } else {
                throw new Error('Unexpected licenses.json shape');
            }
        } catch(e: any) {
            console.error('Failed to load automated licenses', e);
            licensesError = String(e?.message ?? e);
        } finally {
            licensesLoading = false;
        }

        checkMlxModelsCache();
        refreshTesseractModels().catch(() => {});
        try { mlxCacheDir = await invoke('get_mlx_cache_dir'); } catch(e) {}

        const unlistenMlx = await listen('mlx-log', (event: any) => {
            mlxLogs = [...mlxLogs, event.payload].slice(-500);
            if (event.payload.includes('Uvicorn running on')) mlxReady = true;
            setTimeout(() => { if (mlxLogEl) mlxLogEl.scrollTop = mlxLogEl.scrollHeight; }, 10);
        });

        const unlistenSidecar = await listen('sidecar-ready', () => {
            console.log(`[Settings] Sidecar ready event received!`);
            llamacppReady = true;
            sidecarStatus = 'ready';
        });

        const unlistenSidecarFailed = await listen('sidecar-failed', (event: any) => {
            console.error(`[Settings] Sidecar failed:`, event.payload);
            sidecarStatus = 'error';
        });

        const unlistenSidecarLog = await listen('sidecar-log', (event: any) => {
            sidecarLogs = [...sidecarLogs, event.payload].slice(-500);
            setTimeout(() => { if (sidecarLogEl) sidecarLogEl.scrollTop = sidecarLogEl.scrollHeight; }, 10);
        });

        const unlistenOllamaReady = await listen('ollama-ready', () => {
            ollamaStatus = 'ready';
            ollamaRunning = true;
        });
        const unlistenOllamaFailed = await listen('ollama-failed', () => {
            ollamaStatus = 'error';
            ollamaRunning = false;
        });
        const unlistenOllamaLog = await listen('ollama-log', (event: any) => {
            ollamaLogs = [...ollamaLogs, event.payload].slice(-500);
            setTimeout(() => { if (ollamaLogEl) ollamaLogEl.scrollTop = ollamaLogEl.scrollHeight; }, 10);
        });

        if (activeProviderId === 'ollama') {
            handleRefreshModels();
        }

        // Check if Ollama is already running
        try {
            const r = await fetch('http://localhost:11434/api/tags', { connectTimeout: 1500 });
            if (r.ok) { ollamaStatus = 'ready'; ollamaRunning = true; }
        } catch { /* not running */ }

        const unlistenIndexProgress = await listen<{ step: string; label: string; pct: number }>(
            'index://init-progress',
            (event) => {
                indexInitProgress = event.payload.label;
                indexInitPct      = event.payload.pct;
            }
        );

        // Per-MB embedder download progress (drives the bar inside the
        // init-progress note instead of leaving it stuck at 5%).
        const unlistenDownload = await listen<{ repo: string; file: string; bytes_done: number; bytes_total: number; pct: number }>(
            'index://download-progress',
            (event) => {
                indexDownloadProgress = event.payload;
                if (event.payload.pct >= 100) {
                    setTimeout(() => { if (indexDownloadProgress?.pct === 100) indexDownloadProgress = null; }, 1500);
                }
            }
        );

            cleanup = () => {
                unlistenMlx();
                unlistenSidecar();
                unlistenSidecarFailed();
                unlistenSidecarLog();
                unlistenOllamaReady();
                unlistenOllamaFailed();
                unlistenOllamaLog();
                unlistenIndexProgress();
                unlistenDownload();
            };
        })();
        return () => cleanup();
    });

    // Tesseract Management
    let tesseractModels = $state<{ id: string; name: string; isDownloaded: boolean }[]>([
        { id: 'eng', name: 'English', isDownloaded: false },
        { id: 'deu', name: 'German', isDownloaded: false },
        { id: 'fra', name: 'French', isDownloaded: false },
        { id: 'spa', name: 'Spanish', isDownloaded: false },
        { id: 'ita', name: 'Italian', isDownloaded: false },
    ]);
    let tesseractRefreshing = $state(false);

    /** Probe IndexedDB / OPFS / Cache Storage for cached Tesseract `.traineddata` files. */
    async function detectInstalledTesseractLangs(): Promise<Set<string>> {
        const found = new Set<string>();
        // Tesseract.js v7 caches language packs as `<lang>.traineddata.gz` in either
        // IndexedDB (`keyval-store` → `keyval`), OPFS, or the Cache API depending on
        // platform. We probe each cheaply.
        try {
            // 1. IndexedDB key-value store used by tesseract.js (idb-keyval).
            const dbs = (await indexedDB.databases?.()) ?? [];
            for (const info of dbs) {
                if (!info.name) continue;
                if (!/keyval-store|tesseract/i.test(info.name)) continue;
                await new Promise<void>(resolve => {
                    const req = indexedDB.open(info.name!);
                    req.onsuccess = () => {
                        const db = req.result;
                        const stores = Array.from(db.objectStoreNames);
                        if (stores.length === 0) { db.close(); resolve(); return; }
                        let pending = stores.length;
                        for (const storeName of stores) {
                            try {
                                const tx = db.transaction(storeName, 'readonly');
                                const store = tx.objectStore(storeName);
                                const keyReq = store.getAllKeys();
                                keyReq.onsuccess = () => {
                                    for (const k of keyReq.result as IDBValidKey[]) {
                                        const s = String(k);
                                        const m = s.match(/([a-z]{3})\.traineddata/);
                                        if (m) found.add(m[1]);
                                    }
                                    if (--pending === 0) { db.close(); resolve(); }
                                };
                                keyReq.onerror = () => { if (--pending === 0) { db.close(); resolve(); } };
                            } catch {
                                if (--pending === 0) { db.close(); resolve(); }
                            }
                        }
                    };
                    req.onerror = () => resolve();
                });
            }
            // 2. Cache Storage (some tesseract.js versions use it for the wasm core).
            if ('caches' in self) {
                const names = await caches.keys();
                for (const n of names) {
                    if (!/tesseract/i.test(n)) continue;
                    const cache = await caches.open(n);
                    const reqs = await cache.keys();
                    for (const r of reqs) {
                        const m = r.url.match(/([a-z]{3})\.traineddata/);
                        if (m) found.add(m[1]);
                    }
                }
            }
        } catch (e) {
            console.warn('[Tesseract] cache probe failed:', e);
        }
        return found;
    }

    async function refreshTesseractModels() {
        tesseractRefreshing = true;
        try {
            const installed = await detectInstalledTesseractLangs();
            tesseractModels = tesseractModels.map(m => ({
                ...m,
                isDownloaded: installed.has(m.id),
            }));
        } finally {
            tesseractRefreshing = false;
        }
    }

    // Save all settings without showing the "Gespeichert!" badge
    async function saveSettingsSilent() {
        // If the user typed a fresh API key into a provider field, the
        // bound state holds it in plain text. Migrate to keychain before
        // it lands in settings.json.
        const { providers: migrated, migrated: n } = await migrateProviderKeysToKeychain(
            $state.snapshot(providers)
        );
        if (n > 0) {
            providers = migrated;
        }
        await saveSetting('providers', $state.snapshot(providers));
        await saveSetting('activeProviderId', activeProviderId);
        await saveSetting('exportPath', exportPath);
        await saveSetting('exportPathMode', exportPathMode);
        await saveSetting('pathTemplate', pathTemplate);
        await saveSetting('saveTxt', saveTxt);
        await saveSetting('language', currentLanguage);
        await saveSetting('llmMaxChars', llmMaxChars);
        await saveSetting('llmContextLimit', llmContextLimit);
        await saveSetting('llmPrompt', llmPrompt);
        await saveSetting('ocrEnabled', ocrEnabled);
        await saveSetting('ocrTier', ocrTier);
        await saveSetting('ocrRecLang', ocrRecLang);
        await saveSetting('ocrPipelineEnabled',  ocrPipelineEnabled);
        await saveSetting('ocrPipelineRouter',   ocrPipelineRouter);
        await saveSetting('ocrPipelineCleanup',  ocrPipelineCleanup);
        await saveSetting('ocrPipelineDenoise',  ocrPipelineDenoise);
        await saveSetting('ocrPipelineMinChars', ocrPipelineMinChars);
        await saveSetting('ocrPipelineMinConf',  ocrPipelineMinConf);
        await saveSetting('ocrPipelineAdvanced', ocrPipelineAdvanced);
        await saveSetting('ocrStages',           $state.snapshot(ocrStages));
        await saveSetting('ocrPipelinePunct',    ocrPipelinePunct);
        await saveSetting('ocrPipelineLayout',    ocrPipelineLayout);
        await saveSetting('ocrPipelineLayoutThr', ocrPipelineLayoutThr);
        await saveSetting('ocrPipelineLayoutEngine', ocrPipelineLayoutEngine);
        await saveSetting('ocrPipelineDropHF',    ocrPipelineDropHF);
        await saveSetting('ocrPipelineSr',        ocrPipelineSr);
        await saveSetting('ocrPipelineSrMaxPx',   ocrPipelineSrMaxPx);
        await saveSetting('ocrPipelineSrEngine',  ocrPipelineSrEngine);
        await saveSetting('ocrPipelineRestore',   ocrPipelineRestore);
        await saveSetting('ocrPipelineRestoreEngine', ocrPipelineRestoreEngine);
        await saveSetting('ocrPipelineRestoreTask', ocrPipelineRestoreTask);
        await saveSetting('ocrPipelineDewarp',    ocrPipelineDewarp);
        await saveSetting('ocrPipelineDewarpEngine',  ocrPipelineDewarpEngine);
        invoke('bg_ingest_set_ocr_pipeline', { config: ocrPipelineConfig() }).catch(() => {});
        // Sync OCR options to the background ingest worker.
        invoke('bg_ingest_set_ocr', { enabled: ocrEnabled, tier: ocrTier, recLang: ocrRecLang }).catch(() => {});
        await saveSetting('authorSortEnabled', authorSortEnabled);
        await saveSetting('noThinking', noThinking);
        await saveSetting('autoSpeakReplies', autoSpeakReplies);
        await saveSetting('pdfMetadataPrefill', pdfMetadataPrefill);
        await saveSetting('conversionTimeoutSeconds', conversionTimeoutSeconds);
        await saveSetting('extractionWorkers', extractionWorkers);
        await saveSetting('llmWorkers', llmWorkers);
        batchManager.extractionTargetWorkers = extractionWorkers;
        batchManager.llmTargetWorkers = llmWorkers;
        await saveSetting('logVerbosity', logVerbosity);
        // Push the verbosity into the in-process flog filter so other
        // (non-Settings) code paths see the change immediately.
        const { setLogVerbosity } = await import('../log');
        setLogVerbosity(logVerbosity);
        await saveSetting('watchFolders', watchFolders);
        await saveSetting('roundRobinProviders', $state.snapshot(roundRobinProviders));
        await saveSetting('pdfBackend', pdfBackend);
        await saveSetting('parsingFormat', parsingFormat);
        await saveSetting('localModels', $state.snapshot(localModels));
        await saveSetting('mlxModels', $state.snapshot(mlxModels));
        await saveSetting('mlxPort', mlxPort);
        await saveSetting('llamacppPort', llamacppPort);
        // Search Index
        await saveSetting('indexEnabled',       indexEnabled);
        await saveSetting('indexSearchMode',    indexSearchMode);
        await saveSetting('indexBackendType',   indexBackendType);
        await saveSetting('indexRemoteUrl',     indexRemoteUrl);
        await saveSetting('indexRemoteApiKey',  indexRemoteApiKey);
        await saveSetting('indexEmbedderModel', indexEmbedderModel);
        await saveSetting('indexEmbedderBackend', indexEmbedderBackend);
        await saveSetting('indexDevice',        indexDevice);
        await saveSetting('indexUseVector',         indexUseVector);
        await saveSetting('indexEmbedderLocation',  indexEmbedderLocation);
        await saveSetting('indexRerankerModel', indexRerankerModel);
        await saveSetting('indexRerankerModelMultilingual', indexRerankerModelMultilingual);
        await saveSetting('indexRerankerTopN',  indexRerankerTopN);
        await saveSetting('indexUseEmbedderAsReranker', indexUseEmbedderAsReranker);
        await saveSetting('indexNerEnabled',     indexNerEnabled);
        await saveSetting('indexNerModel',       indexNerModel);
        await saveSetting('indexNerLabels',      indexNerLabels);
        await saveSetting('indexNerThreshold',   indexNerThreshold);
        await saveSetting('indexNerMaxEntities', indexNerMaxEntities);
        await saveSetting('indexNerMaxChars',    indexNerMaxChars);
        await saveSetting('indexModelCacheDir', indexModelCacheDir);
        await saveSetting('indexMatryoshkaDim', indexMatryoshkaDim);
        await saveSetting('indexTranslateTo',   indexTranslateTo);
        await saveSetting('indexAudioExtractionEnabled', indexAudioExtractionEnabled);
        await saveSetting('indexAudioAsrBackend',        indexAudioAsrBackend);
        await saveSetting('indexAudioLidMethod',         indexAudioLidMethod);
        await saveSetting('indexIngestAudioLevel',       indexIngestAudioLevel);
        await saveSetting('indexImageExtractionEnabled', indexImageExtractionEnabled);
        await saveSetting('indexIngestImageLevel',       indexIngestImageLevel);
        await saveSetting('indexCrispLensImageEnrichment', indexCrispLensImageEnrichment);
        // P13.7 Step 5 — cloud-backup sync settings (URL + 3 toggles).
        // The API key is NEVER persisted here — it goes straight to
        // the OS keychain via sync_cb_set_token in saveCloudBackupToken.
        await saveSetting('indexCloudBackupUrl',            indexCloudBackupUrl);
        await saveSetting('indexCloudBackupPushManifests',  indexCloudBackupPushManifests);
        await saveSetting('indexCloudBackupPushEmbeddings', indexCloudBackupPushEmbeddings);
        await saveSetting('indexCloudBackupPullManifests',  indexCloudBackupPullManifests);
        await saveSetting('indexCloudBackupPullFullText',   indexCloudBackupPullFullText);
        await saveSetting('indexLocalExtractionEnabled',   indexLocalExtractionEnabled);
        await saveSetting('indexSkeletonOnly',             indexSkeletonOnly);
        await saveSetting('indexEmbedderModelName',        indexEmbedderModelName);
        await saveSetting('indexLocalMaxSizeGb',            indexLocalMaxSizeGb);
        await saveSetting('backupDriveId',                  backupDriveId);
        await saveSetting('backupKeepDaily',                backupKeepDaily);
        await saveSetting('cloudBackupPartitionRoot',       cloudBackupPartitionRoot);
        await saveSetting('cloudBackupPartitionMax',        cloudBackupPartitionMax);
        await saveSetting('cloudBackupPartitionDepth',      cloudBackupPartitionDepth);
        await saveSetting('indexDataDir',       indexDataDir);
        llmClient.setKeys(await resolveAllApiKeys(providers));
        llmClient.noThinking = noThinking;
        llmClient.llamacppPort = llamacppPort;
        llmClient.mlxPort = mlxPort;
        i18n.setLanguage(currentLanguage);
    }

    // ── Round-robin helpers ──────────────────────────────────────────────────
    // Remote providers only — local servers don't rate-limit
    const REMOTE_PROVIDER_IDS = ['groq','openrouter','mistral','openai','nebius','scaleway','anthropic','google','poe'];
    let rrCandidates = $derived(providers.filter(p => REMOTE_PROVIDER_IDS.includes(p.id) && (p.apiKey || p.isConfigured)));

    function rrToggle(id: string) {
        if (roundRobinProviders.includes(id)) {
            roundRobinProviders = roundRobinProviders.filter(x => x !== id);
        } else {
            roundRobinProviders = [...roundRobinProviders, id];
        }
    }
    function rrMoveUp(idx: number) {
        if (idx === 0) return;
        const arr = [...roundRobinProviders];
        [arr[idx - 1], arr[idx]] = [arr[idx], arr[idx - 1]];
        roundRobinProviders = arr;
    }
    function rrMoveDown(idx: number) {
        if (idx >= roundRobinProviders.length - 1) return;
        const arr = [...roundRobinProviders];
        [arr[idx], arr[idx + 1]] = [arr[idx + 1], arr[idx]];
        roundRobinProviders = arr;
    }

    // ── Search Index helpers ─────────────────────────────────────────────────

    function indexModeToRust(m: string): string {
        return { text: 'text_only', vector: 'vector_only', hybrid: 'hybrid' }[m] ?? 'hybrid';
    }
    function indexBackendToRust(b: string): string {
        if (b === 'remote') return 'remote';
        if (b === 'hybrid') return 'hybrid';
        return 'local';
    }
    /// UI key → serde kebab string for `RerankerModel`. Empty input
    /// (= disabled) maps to null on the Rust side. Pinned by the
    /// `reranker_model_serde_strings` test in `index/reranker.rs`.
    function rerankerToRust(m: string): string | null {
        if (!m) return null;
        const map: Record<string, string> = {
            bge_v2_m3:       'bge-reranker-v2-m3',
            bge_base:        'bge-reranker-base',
            jina_v2_multi:   'jina-reranker-v2-base-multilingual',
        };
        return map[m] ?? null;
    }

    function indexEmbedderToRust(m: string): string {
        return {
            bge_m3:                       'bge-m3',
            pixie:                        'pixie-rune-v1',
            pixie_q:                      'pixie-rune-v1-q',
            pixie_int4:                   'pixie-rune-v1-int4',
            pixie_int4_full:              'pixie-rune-v1-int4-full',
            octen:                        'octen-06b-int8-local',
            snowflake_l:                  'snowflake-arctic-lv2',
            snowflake_l_fp16:             'snowflake-arctic-lv2-fp16',
            snowflake_l_int8:             'snowflake-arctic-lv2-int8',
            snowflake_l_q4:               'snowflake-arctic-lv2-q4',
            snowflake_l_q4f16:            'snowflake-arctic-lv2-q4-f16',
            snowflake_l_o4:               'snowflake-arctic-lv2-o4',
            snowflake_l_fp32:             'snowflake-arctic-lv2-fp32',
            jina_nano:                    'jina-v5-nano',
            multilingual_mini_lm:         'multilingual-mini-lm',
            // fastembed-rs/CrispEmbed registry sync (May 2026)
            multilingual_e5_small:        'multilingual-e5-small',
            multilingual_e5_base:         'multilingual-e5-base',
            multilingual_e5_large:        'multilingual-e5-large',
            bge_small_en_v15:             'bge-small-en-v15',
            bge_base_en_v15:              'bge-base-en-v15',
            bge_large_en_v15:             'bge-large-en-v15',
            nomic_embed_v15:              'nomic-embed-text-v15',
            mxbai_large_v1:               'mxbai-embed-large-v1',
            minilm_l6_v2:                 'all-mini-lm-l6-v2',
            embedding_gemma_300m:         'embedding-gemma300-m',
            gte_base_en_v15:              'gte-base-en-v15',
            gte_large_en_v15:             'gte-large-en-v15',
            // Older HEAD-side aliases kept for backwards compat with persisted prefs
            mxbai_embed_large_v1:         'mxbai-embed-large-v1',
            nomic_embed_text_v15:         'nomic-embed-text-v15',
            all_mini_lm_l6_v2:            'all-mini-lm-l6-v2',
        }[m] ?? 'bge-m3';
    }
    function indexDeviceToRust(d: string): string {
        return { auto: 'auto', cpu: 'cpu', metal: 'metal', cuda: 'cuda' }[d] ?? 'auto';
    }

    async function applyIndexConfig() {
        await saveSettingsSilent();
        await syncActiveCatalogFromInputs();
        indexStatus       = 'loading';
        indexStatusMsg    = '';
        indexInitProgress = 'Konfiguration wird gespeichert …';
        indexInitPct      = 0;
        try {
            // Push config to backend.
            await invoke('index_set_config', {
                config: {
                    enabled:          indexEnabled,
                    mode:             indexModeToRust(indexSearchMode),
                    backend_type:     indexBackendToRust(indexBackendType),
                    remote_url:       indexRemoteUrl || null,
                    remote_api_key:   indexRemoteApiKey || null,
                    embedder_model:   indexEmbedderToRust(indexEmbedderModel),
                    embedder_device:  indexDeviceToRust(indexDevice),
                    embedder_backend: supportsGguf(indexEmbedderModel) ? indexEmbedderBackend : 'onnx',
                    use_vector:           indexUseVector,
                    embedder_location:    indexEmbedderLocation,
                    reranker_model:             rerankerToRust(indexRerankerModel),
                    reranker_model_multilingual: rerankerToRust(indexRerankerModelMultilingual),
                    rerank_top_n:     Number(indexRerankerTopN) || 50,
                    use_embedder_as_reranker: indexUseEmbedderAsReranker,
                    // P19 — GLiNER NER. Labels textarea is split on commas /
                    // newlines into the Rust `Vec<String>` (empty = backend
                    // default label set). `ner_model` is the serde kebab-case
                    // string for the `NerModel` enum.
                    ner_enabled:      indexNerEnabled,
                    ner_model:        indexNerModel,
                    ner_labels:       indexNerLabels
                        .split(/[\n,]+/).map((s) => s.trim()).filter(Boolean),
                    ner_threshold:    Number(indexNerThreshold) || 0.5,
                    ner_max_entities: Number(indexNerMaxEntities) || 0,
                    ner_max_chars:    Number(indexNerMaxChars) || 0,
                    model_cache_dir:  indexModelCacheDir.trim() || null,
                    matryoshka_dim:   (indexEmbedderBackend === 'gguf' && Number(indexMatryoshkaDim) > 0)
                        ? Number(indexMatryoshkaDim)
                        : null,
                    // P13.5 follow-up — index-time translation target.
                    // Empty / 'none' string = no translation; 'en' /
                    // 'de' / etc. = translate every extracted doc to
                    // that ISO 639-1 language at ingest time.  CLD3
                    // text-LID auto-resolves on first use to provide
                    // the source language.
                    translate_to:     (indexTranslateTo && indexTranslateTo.trim() && indexTranslateTo !== 'none')
                        ? indexTranslateTo.trim()
                        : null,
                    // P13.6 Step 5 — multimodal processing.
                    audio_extraction_enabled: indexAudioExtractionEnabled,
                    audio_asr_backend:        indexAudioAsrBackend || 'whisper',
                    audio_lid_method:         indexAudioLidMethod || 'whisper',
                    ingest_audio_level:       indexIngestAudioLevel || 'l3',
                    image_extraction_enabled: indexImageExtractionEnabled,
                    ingest_image_level:       indexIngestImageLevel || 'l3',
                    crisplens_image_enrichment_enabled: indexCrispLensImageEnrichment,
                    // P13.7 Step 5 — cloud-backup sync.
                    cloud_backup_url:                        indexCloudBackupUrl.trim() || null,
                    cloud_backup_push_manifests_enabled:     indexCloudBackupPushManifests,
                    cloud_backup_push_embeddings_enabled:    indexCloudBackupPushEmbeddings,
                    cloud_backup_pull_manifests_enabled:     indexCloudBackupPullManifests,
                    cloud_backup_pull_full_text_enabled:     indexCloudBackupPullFullText,
                    // P13.7 Stage P — 0 means unlimited (null in Rust).
                    local_max_size_bytes: indexLocalMaxSizeGb > 0
                        ? Math.round(indexLocalMaxSizeGb * 1_000_000_000)
                        : null,
                    // Stage U — thin client: when false, bg_ingest skips extraction.
                    local_extraction_enabled: indexLocalExtractionEnabled,
                    local_skeleton_only: indexSkeletonOnly,
                    // Stage X — registry-driven model override (empty = use enum dropdown).
                    embedder_model_name: indexEmbedderModelName.trim() || null,
                }
            });
            if (indexEnabled) {
                indexInitProgress = 'Starte Index-Initialisierung …';
                indexInitPct      = 2;
                const dataDir = indexDataDir || await invoke<string>('get_app_data_dir').catch(() => '');
                await invoke('index_init', { dataDir });
            }
            indexStatus       = 'ok';
            indexInitProgress = '';
            indexInitPct      = 100;
        } catch(e: any) {
            indexStatus       = 'error';
            indexStatusMsg    = String(e);
            indexInitProgress = '';
        }
    }

    async function pickIndexDataDir() {
        const selected = await openDialog({ directory: true, multiple: false });
        if (selected) { indexDataDir = selected as string; }
    }

    /** P13.7 Step 5 — write the typed API key value to the OS keychain
     *  via the sync_cb_set_token Tauri command.  Clears the input field
     *  on success so the value doesn't linger in DOM memory.  An empty
     *  value clears the stored token (matches the command's idempotent
     *  delete semantics). */
    async function saveCloudBackupToken() {
        if (!indexCloudBackupUrl.trim()) {
            indexCloudBackupTokenSavedMsg = 'Set the URL first, then save the key.';
            return;
        }
        // The cloud_backup_url is read off IndexConfig by the command;
        // make sure the in-memory config has the URL applied before we
        // ask the backend to write the keychain entry against it.
        try {
            await invoke('index_set_config', {
                config: { cloud_backup_url: indexCloudBackupUrl.trim() }
            });
        } catch {
            /* index_set_config tolerates partial configs only when
               nothing else is in flight; if it fails here we still
               try to save the token — the URL might already be applied. */
        }
        try {
            await invoke('sync_cb_set_token', { token: indexCloudBackupApiKeyInput });
            indexCloudBackupApiKeyInput = '';
            indexCloudBackupTokenSavedMsg = indexCloudBackupApiKeyInput === ''
                ? 'Token cleared.'
                : 'Saved to keychain.';
            await refreshCloudBackupStatus();
        } catch (e: any) {
            indexCloudBackupTokenSavedMsg = `Error: ${e}`;
        }
    }

    /** P13.7 Stage N — trigger sync_cb_partition with the form's
     *  current root / max-shards / group-depth values.  Reports
     *  the resulting num_files + num_shards inline so the operator
     *  sees the partition map size at a glance. */
    async function runCloudBackupPartition() {
        const root = cloudBackupPartitionRoot.trim();
        if (!root) {
            cloudBackupPartitionStatus = 'Set a watched root path first.';
            return;
        }
        cloudBackupPartitionStatus = 'Partitioning…';
        try {
            const r = await invoke<any>('sync_cb_partition', {
                rootPath:   root,
                maxShards:  Number(cloudBackupPartitionMax) || 64,
                groupDepth: Number(cloudBackupPartitionDepth) || 1,
            });
            cloudBackupPartitionStatus =
                `partitioned ${r.num_files} file(s) into ${r.num_shards} shard(s) ` +
                `(max ${r.max_shards}, depth ${r.group_depth}). ` +
                `sample: ${(r.sample_collections ?? []).slice(0, 3).join(', ')}…`;
        } catch (e: any) {
            cloudBackupPartitionStatus = `Error: ${e}`;
        }
    }

    /** Refresh the inline status line (version + watermarks + error).
     *  Tolerates the cold-start case where data_dir isn't seeded yet. */
    async function refreshCloudBackupStatus() {
        try {
            indexCloudBackupStatus = await invoke('sync_cb_status');
        } catch {
            indexCloudBackupStatus = null;
        }
    }

    // ── Stage T — admin key management handlers ──────────────────────────
    async function cbAdminMint() {
        if (!cbAdminToken.trim() || !cbAdminMintName.trim()) {
            cbAdminMsg = 'Admin token and key name are required.';
            return;
        }
        cbAdminBusy = true;
        cbAdminMsg  = '';
        cbAdminMintedKey = '';
        try {
            const r = await invoke<any>('sync_cb_admin_mint', {
                adminToken: cbAdminToken.trim(),
                name:       cbAdminMintName.trim(),
                ownerId:    cbAdminMintOwner.trim() || null,
            });
            cbAdminMintedKey = r.raw_key ?? '';
            cbAdminMsg = `Key "${r.name}" minted. Copy it now — it won't be shown again.`;
            cbAdminMintName  = '';
            cbAdminMintOwner = '';
            await cbAdminRefreshList();
        } catch (e: any) {
            cbAdminMsg = `Mint error: ${e}`;
        } finally {
            cbAdminBusy = false;
        }
    }

    async function cbAdminRevoke() {
        if (!cbAdminToken.trim() || !cbAdminRevokeName.trim()) {
            cbAdminMsg = 'Admin token and key name are required.';
            return;
        }
        cbAdminBusy = true;
        cbAdminMsg  = '';
        try {
            const r = await invoke<any>('sync_cb_admin_revoke', {
                adminToken: cbAdminToken.trim(),
                name:       cbAdminRevokeName.trim(),
            });
            cbAdminMsg = r.revoked
                ? `Key "${r.name}" revoked.`
                : `No active key named "${r.name}".`;
            cbAdminRevokeName = '';
            await cbAdminRefreshList();
        } catch (e: any) {
            cbAdminMsg = `Revoke error: ${e}`;
        } finally {
            cbAdminBusy = false;
        }
    }

    async function cbAdminRefreshList() {
        if (!cbAdminToken.trim()) return;
        try {
            const r = await invoke<any>('sync_cb_admin_list_keys', {
                adminToken: cbAdminToken.trim(),
            });
            cbAdminKeys = r.keys ?? [];
        } catch (e: any) {
            cbAdminMsg = `List error: ${e}`;
        }
    }

    // Stage O — "Sync now" button state.
    let syncNowBusy = $state(false);
    let syncNowMsg  = $state('');

    /** Drain the outbox (push), then pull manifest deltas — mirrors
     *  the auto-drain timer but triggered on demand from the UI. */
    async function syncNow() {
        syncNowBusy = true;
        syncNowMsg  = 'Pushing…';
        try {
            const drain = await invoke<any>('sync_cb_drain', { batchSize: 64 });
            syncNowMsg = `Pushed ${drain.drained ?? 0} row(s). Pulling…`;
            const pull  = await invoke<any>('sync_cb_manifest_pull');
            const pulled = pull.pulled ?? pull.applied ?? 0;
            syncNowMsg = `Done — pushed ${drain.drained ?? 0}, pulled ${pulled} row(s).`;
            await refreshCloudBackupStatus();
        } catch (e: any) {
            syncNowMsg = `Error: ${e}`;
        } finally {
            syncNowBusy = false;
        }
    }

    // Stage Q — shard backup controls.
    let backupDriveId     = $state<string>('');
    let backupKeepDaily   = $state<number>(7);
    let backupNowBusy     = $state(false);
    let backupNowMsg      = $state('');

    // Stage R — controller.py manifest import.
    let importManifestPath  = $state<string>('');
    let importManifestOwner = $state<string>('');
    let importManifestBusy  = $state(false);
    let importManifestMsg   = $state('');

    async function importFromManifestDb() {
        if (!importManifestPath.trim()) { importManifestMsg = 'Enter a path first.'; return; }
        importManifestBusy = true;
        importManifestMsg  = 'Importing…';
        try {
            const result = await invoke<any>('sync_cb_import_from_manifest_db', {
                path:      importManifestPath.trim(),
                ownerId:   importManifestOwner.trim(),
                batchSize: 200,
                dryRun:    false,
            });
            importManifestMsg = `Imported ${result.imported ?? 0} rows (watermark → ${result.watermark ?? 0})`;
        } catch (e: any) {
            importManifestMsg = `Error: ${e}`;
        } finally {
            importManifestBusy = false;
        }
    }

    async function backupShardsNow() {
        if (!backupDriveId.trim()) { backupNowMsg = 'Select a drive first.'; return; }
        backupNowBusy = true;
        backupNowMsg  = 'Starting backup…';
        try {
            const result = await invoke<any>('sync_cb_backup_shards', {
                driveId:    backupDriveId,
                shard:      null,
                force:      false,
                keepDaily:  backupKeepDaily,
            });
            const n = result.backed_up ?? 0;
            const errs = (result.errors ?? []) as string[];
            backupNowMsg = errs.length > 0
                ? `Backed up ${n} shard(s). Errors: ${errs.join('; ')}`
                : `Backed up ${n} shard(s) → cb-backups/${result.date_dir ?? ''}`;
        } catch (e: any) {
            backupNowMsg = `Error: ${e}`;
        } finally {
            backupNowBusy = false;
        }
    }

    async function pickIndexModelCacheDir() {
        const selected = await openDialog({ directory: true, multiple: false });
        if (selected) { indexModelCacheDir = selected as string; }
    }

    // ── Folder watcher controls ──────────────────────────────────────────────
    async function addWatchFolder() {
        const selected = await openDialog({ directory: true, multiple: false });
        if (!selected) return;
        const folder = selected as string;
        if (watchFolders.includes(folder)) {
            watchStatusMsg = 'Already watching that folder.';
            return;
        }
        try {
            await invoke('watch_start', { folder });
            watchFolders = [...watchFolders, folder];
            await saveSetting('watchFolders', watchFolders);
            watchStatusMsg = `Watching: ${folder}`;
        } catch (e: any) {
            watchStatusMsg = `Watcher error: ${e?.message ?? e}`;
        }
    }

    async function removeWatchFolder(folder: string) {
        try {
            await invoke('watch_stop_one', { folder });
        } catch (e) {
            // Stop is idempotent on the backend; failures here are usually
            // "path no longer canonicalizes" — we still want to drop it
            // from the UI list so the user can recover.
            console.warn('[watch] stop_one failed (still removing from list)', e);
        }
        watchFolders = watchFolders.filter(f => f !== folder);
        await saveSetting('watchFolders', watchFolders);
        watchStatusMsg = watchFolders.length > 0
            ? `${watchFolders.length} folder(s) watched`
            : 'No folders being watched.';
    }

    async function buildIvfPq() {
        indexIvfRunning = true;
        try {
            await invoke('index_build_ivf_pq');
            alert('IVF-PQ index built successfully.');
        } catch(e: any) {
            alert('IVF-PQ build failed: ' + e);
        } finally {
            indexIvfRunning = false;
        }
    }

    async function buildScalarIndex() {
        indexScalarRunning = true;
        try {
            await invoke('index_build_scalar_index');
            alert('Scalar index built successfully.');
        } catch(e: any) {
            alert('Scalar index build failed: ' + e);
        } finally {
            indexScalarRunning = false;
        }
    }

    async function handleSave() {
        console.log(`[Settings] Saving global settings...`);
        await saveSettingsSilent();
        console.log(`[Settings] All settings saved.`);
        saveIndicator = true;
        setTimeout(() => saveIndicator = false, 2000);
    }

    async function handleSaveProvider() {
        console.log(`[Settings] Saving provider settings for: ${selectedProvider.name}`);
        await saveSetting('providers', $state.snapshot(providers));
        await saveSetting('activeProviderId', activeProviderId);
        llmClient.setKeys(await resolveAllApiKeys(providers));
        saveIndicator = true;
        setTimeout(() => saveIndicator = false, 2000);
    }

    async function resetToDefaults() {
        if (!confirm(i18n.t.settings.reset_defaults + '?')) return;
        providers = JSON.parse(JSON.stringify(DEFAULT_PROVIDERS));
        activeProviderId = 'ollama';
        llmMaxChars = 5000;
        noThinking = true;
        pdfBackend = 'js';
        parsingFormat = 'xml';
        llmPrompt = getDefaultPrompt(parsingFormat, currentLanguage);
        await handleSave();
    }

    async function updatePrompt() {
        console.log(`[Settings] updatePrompt triggered. format=${parsingFormat}, lang=${currentLanguage}`);
        llmPrompt = getDefaultPrompt(parsingFormat, currentLanguage);
        console.log(`[Settings] Prompt updated to: ${llmPrompt.substring(0, 50)}...`);
    }

    async function setActiveProvider(id: string) {
        activeProviderId = id;
        await handleSave();
    }

    async function pickExportPath() {
        const selected = await openDialog({ directory: true, multiple: false });
        if (selected) exportPath = selected as string;
    }

    async function handleRefreshModels() {
        loadingModels = true;
        try {
            const resolvedKey = await resolveSecret(selectedProvider.apiKey);
            const models = await llmClient.fetchModels(selectedProvider.id, resolvedKey, selectedProvider.baseUrl);
            selectedProvider.models = models;
            if (models.length > 0 && !selectedProvider.selectedModel) {
                selectedProvider.selectedModel = models[0];
            }
            await handleSave();
        } catch (e: any) {
            console.error(`Failed to fetch models for ${selectedProvider.id}:`, e);
        } finally {
            loadingModels = false;
        }
    }

    /**
     * Per-provider testing state.  Reading `testingProviders[id]` is the
     * source of truth for the Test button's disabled state.  The legacy
     * single `testingConnection` boolean still tracks the active panel
     * so the spinner icon stays correct; it just no longer locks the
     * Test buttons across other providers' panels.
     *
     * Reported: "testen of providers is blocked on all as long as one
     * provider test runs" — the global flag was the cause.
     */
    let testingProviders = $state<Record<string, boolean>>({});

    async function handleTestConnection() {
        const pid = selectedProvider.id;
        testingProviders[pid] = true;
        testingConnection = true;
        testResult = null;
        try {
            const prompt = "Reply with 'OK' and nothing else.";
            const resolvedKey = await resolveSecret(selectedProvider.apiKey);
            const response = await llmClient.query(selectedProvider.id, selectedProvider.selectedModel, prompt, resolvedKey);
            testResult = { success: true, message: `Connected! Response: ${response}` };
        } catch (e: any) {
            testResult = { success: false, message: e.message };
        } finally {
            delete testingProviders[pid];
            testingConnection = false;
        }
    }

    // Local Model Helpers
    async function addLocalModel() {
        const selected = await openDialog({ multiple: false, filters: [{ name: 'GGUF Models', extensions: ['gguf'] }] });
        if (selected && typeof selected === 'string') {
            const name = selected.split(/[\\/]/).pop() || 'Unknown Model';
            const metadata = await stat(selected).catch(() => null);
            const size = metadata ? (metadata.size / (1024 * 1024 * 1024)).toFixed(2) + ' GB' : 'Unknown';
            
            localModels.push({
                id: crypto.randomUUID(),
                name,
                path: selected,
                size,
                isDownloaded: true,
                isActive: false
            });
            await handleSave();
        }
    }

    async function addCustomModel() {
        if (!customModelInput) return;
        const name = customModelInput.split('/').pop() || customModelInput;
        localModels.push({
            id: crypto.randomUUID(),
            name: name,
            path: '',
            isDownloaded: false,
            isActive: false,
            downloadUrl: customModelInput.startsWith('http') ? customModelInput : `https://huggingface.co/${customModelInput}`
        });
        customModelInput = '';
        await handleSave();
    }

    async function downloadLocalModel(index: number) {
        const model = localModels[index];
        if (!model.downloadUrl) return;
        
        console.log(`[Settings] Starting download for ${model.name} (ID: ${model.id}) from ${model.downloadUrl}`);
        try {
            const fileName = model.downloadUrl.split('/').pop() || 'model.gguf';
            const path = await save({ defaultPath: fileName, filters: [{ name: 'GGUF', extensions: ['gguf'] }] });
            if (!path) {
                console.log(`[Settings] Download cancelled by user.`);
                return;
            }

            console.log(`[Settings] Target path for download: ${path}`);
            model.progress = 0;
            await invoke('download_file', { id: model.id, url: model.downloadUrl, path });
            
            model.path = path;
            model.isDownloaded = true;
            model.progress = undefined;
            const metadata = await stat(path);
            model.size = (metadata.size / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
            console.log(`[Settings] Download complete: ${model.name}, size=${model.size}`);
            await handleSave();
        } catch (e) {
            console.error(`[Settings] Download failed for ${model.name}:`, e);
            alert('Download failed: ' + e);
            model.progress = undefined;
        }
    }

    async function removeLocalModel(index: number) {
        const model = localModels[index];
        if (model.isDownloaded && model.path && confirm(`Delete ${model.name} from disk?`)) {
            await remove(model.path).catch(() => {});
        }
        localModels.splice(index, 1);
        await handleSave();
    }

    async function setLocalModelActive(path: string) {
        if (!path) return alert('Select a valid model file first.');
        console.log(`[Settings] Setting active model: ${path} (Provider: ${selectedProviderId})`);

        if (selectedProviderId === 'llamacpp') {
            try {
                sidecarStatus = 'starting';
                llamacppReady = false;
                sidecarLogs = [];
                selectedProvider.selectedModel = path;
                selectedProvider.baseUrl = `http://localhost:${llamacppPort}/v1`;
                // Save settings silently — no "Gespeichert!" badge for a server start action
                await saveSettingsSilent();
                console.log(`[Settings] Starting llama.cpp sidecar on port ${llamacppPort}...`);
                await invoke('stop_llamacpp_sidecar');
                const res = await invoke('start_llamacpp_sidecar', { modelPath: path, port: llamacppPort });
                console.log(`[Settings] Sidecar invoke result: ${res}`);
            } catch(e) {
                sidecarStatus = 'error';
                console.error(`[Settings] Failed to start sidecar:`, e);
                alert('Failed to start sidecar: ' + e);
            }
        } else {
            selectedProvider.selectedModel = path;
            await handleSave();
        }
    }

    // MLX Helpers
    async function addMlxModel() {
        if (!mlxCustomInput) return;
        const name = mlxCustomInput.split('/').pop()?.replace(/-4bit|-8bit/gi, '') || mlxCustomInput;
        mlxModels.push({
            id: crypto.randomUUID(),
            repoId: mlxCustomInput,
            name,
            params: 'Custom'
        });
        mlxCustomInput = '';
        await checkMlxModelsCache();
        await handleSave();
    }

    async function removeMlxModel(id: string) {
        mlxModels = mlxModels.filter(m => m.id !== id);
        await handleSave();
    }

    async function checkMlxModelsCache() {
        try {
            const repos = mlxModels.map(m => m.repoId);
            const cached: Record<string, boolean> = await invoke('check_mlx_models_cached', { repoIds: repos });
            const result: Record<string, boolean> = {};
            mlxModels.forEach(m => { result[m.id] = cached[m.repoId] || false; });
            mlxModelCached = result;
        } catch(e) { console.error('Failed to check MLX cache', e); }
    }

    async function setMlxModelActive(repoId: string) {
        selectedProvider.selectedModel = repoId;
        if (mlxRunning) {
            await stopMlxServer();
            await startMlxServer();
        }
        await handleSave();
    }

    async function startMlxServer() {
        if (!selectedProvider.selectedModel) return alert('Select an MLX model first.');
        mlxRunning = true;
        mlxReady = false;
        mlxLogs = ["Starting MLX server..."];
        try {
            await invoke('start_mlx_server', { modelPath: selectedProvider.selectedModel, port: mlxPort });
        } catch(e) {
            mlxRunning = false;
            alert('MLX Start Failed: ' + e);
        }
    }

    async function stopMlxServer() {
        try {
            await invoke('stop_mlx_server');
            mlxRunning = false;
            mlxReady = false;
        } catch(e) { console.error(e); }
    }

    // WebLLM Helpers
    async function handleLoadWebLLM(modelId: string) {
        webllmStatus = 'downloading';
        webllmProgress = 0;
        webllmProgressText = '';
        try {
            await loadWebLLM(modelId, (report: InitProgressReport) => {
                webllmProgress = Math.round(report.progress * 100);
                webllmProgressText = report.text;
                // Switch to 'loading' once download phase is done
                if (report.progress >= 1 && webllmStatus === 'downloading') webllmStatus = 'loading';
            });
            webllmStatus = 'ready';
            webllmSelectedModel = modelId;
            selectedProvider.selectedModel = modelId;
            saveSettingsSilent();
        } catch(e) {
            webllmStatus = 'error';
            console.error('[WebLLM] Load failed:', e);
            alert('WebLLM load failed: ' + e);
        }
    }

    function handleUnloadWebLLM() {
        unloadWebLLM();
        webllmStatus = 'idle';
        webllmProgress = 0;
        webllmProgressText = '';
    }

    function handleUseWebLLM(modelId: string) {
        webllmSelectedModel = modelId;
        selectedProvider.selectedModel = modelId;
        saveSettingsSilent();
    }

    // ORT Helpers
    async function handleLoadORT(modelId: string) {
        ortStatus = 'downloading';
        ortProgress = 0;
        ortProgressText = '';
        try {
            await loadORT(modelId, (p: any) => {
                if (p.progress != null) ortProgress = Math.round(p.progress);
                if (p.name) ortProgressText = p.name + (p.progress != null ? ` ${Math.round(p.progress)}%` : '');
                if (p.status === 'ready') ortStatus = 'loading';
            });
            ortStatus = 'ready';
            ortSelectedModel = modelId;
            selectedProvider.selectedModel = modelId;
            saveSettingsSilent();
        } catch(e) {
            ortStatus = 'error';
            console.error('[ORT] Load failed:', e);
            alert('ORT load failed: ' + e);
        }
    }

    function handleUnloadORT() {
        unloadORT();
        ortStatus = 'idle';
        ortProgress = 0;
        ortProgressText = '';
    }

    function handleUseORT(modelId: string) {
        ortSelectedModel = modelId;
        selectedProvider.selectedModel = modelId;
        saveSettingsSilent();
    }

    async function deleteMlxModelFromDisk(model: MlxModel) {
        if (!confirm(`Delete MLX model "${model.name}" (${model.repoId}) from disk? This will remove the directory from HuggingFace cache.`)) return;
        try {
            await invoke('delete_mlx_model', { repoId: model.repoId });
            await checkMlxModelsCache();
        } catch(e) { alert('Delete failed: ' + e); }
    }

    // Ollama Helpers
    async function pullOllamaModel(tag: string) {
        console.log(`[Settings] Pulling Ollama model: ${tag}`);
        ollamaPulling[tag] = 0;
        try {
            const stream = await llmClient.pullModel('ollama', tag, selectedProvider.baseUrl);
            const reader = stream.getReader();
            const decoder = new TextDecoder();
            while (true) {
                const { done, value } = await reader.read();
                if (done) break;
                const lines = decoder.decode(value).split('\n');
                for (const line of lines) {
                    if (!line.trim()) continue;
                    try {
                        const status = JSON.parse(line);
                        if (status.total && status.completed) {
                            ollamaPulling[tag] = Math.round((status.completed / status.total) * 100);
                        }
                    } catch(e) {}
                }
            }
            console.log(`[Settings] Ollama pull complete: ${tag}`);
            delete ollamaPulling[tag];
            await handleRefreshModels();
        } catch(e) {
            console.error('[Settings] Ollama pull failed:', e);
            alert('Pull failed: ' + e);
            delete ollamaPulling[tag];
        }
    }

    async function addCustomOllamaModel() {
        if (!ollamaCustomInput.trim()) return;
        await pullOllamaModel(ollamaCustomInput.trim());
        ollamaCustomInput = '';
    }

    async function startOllamaService() {
        ollamaStatus = 'starting';
        ollamaLogs = [];
        try {
            await invoke('start_ollama');
        } catch(e) {
            ollamaStatus = 'error';
            alert('Failed to start Ollama: ' + e);
        }
    }

    async function stopOllamaService() {
        try {
            await invoke('stop_ollama');
            ollamaStatus = '';
            ollamaRunning = false;
        } catch(e) { console.error(e); }
    }

    // Benchmarking
    async function runBenchmark() {
        if (benchProviders.length === 0) return alert(i18n.t.settings.benchmark.alert_select_provider);
        if (benchDocuments.length === 0 && benchPromptMode === 'batch') return alert(i18n.t.settings.benchmark.alert_add_documents);
        
        benchRunning = true;
        benchResults = [];
        
        try {
            for (const pid of benchProviders) {
                const prov = providers.find(p => p.id === pid);
                const model = benchModels[pid];
                if (!prov || !model) continue;

                let prompt = benchCustomPrompt;
                if (benchPromptMode === 'batch') {
                    prompt = "Extract metadata from the following text...";
                }

                const runs = [];
                const resolvedKey = await resolveSecret(prov.apiKey);
                for (let i = 0; i < benchRuns; i++) {
                    const start = Date.now();
                    try {
                        const response = await llmClient.query(pid, model, prompt, resolvedKey);
                        const latency = Date.now() - start;
                        runs.push({ latencyMs: latency, response, tokensPerSec: Math.round((response.length / 4) / (latency / 1000)) });
                    } catch(e: any) {
                        runs.push({ error: e.message, latencyMs: Date.now() - start });
                    }
                }
                
                const avgLatency = runs.filter(r => !r.error).reduce((a, b) => a + b.latencyMs, 0) / runs.filter(r => !r.error).length || 0;
                benchResults.push({
                    providerId: pid,
                    providerName: prov.name,
                    model,
                    runs,
                    avgLatency
                });
            }
        } finally {
            benchRunning = false;
        }
    }

    let availableModels = $derived.by(() => {
        if (selectedProvider.id === 'mistralrs' || selectedProvider.id === 'llamacpp') {
            return localModels.filter(m => m.isDownloaded).map(m => m.path).filter(p => p !== '');
        }
        if (selectedProvider.id === 'mlx') {
            return mlxModels.map(m => m.repoId);
        }
        const fetched = selectedProvider.models || [];
        const saved = selectedProvider.selectedModel;
        if (saved && !fetched.includes(saved)) return [saved, ...fetched];
        return fetched;
    });

    let ollamaModelsList = $derived.by(() => {
        const installed = selectedProvider.models || [];
        const combined = Array.from(new Set([...installed, ...DEFAULT_OLLAMA_MODELS]));
        return combined.map(tag => ({
            tag,
            isInstalled: installed.includes(tag)
        }));
    });
</script>

<div class="settings-container">
    <aside class="sidebar">
        <div class="sidebar-scrollable">
            <h2>{i18n.t.settings.app_settings}</h2>
            <button class="provider-btn" class:active={selectedProviderId === 'global'} onclick={() => selectedProviderId = 'global'}>
                <span class="prov-label"><Globe size={16} /> {i18n.t.settings.general}</span>
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'extraction'} onclick={() => selectedProviderId = 'extraction'}>
                <span class="prov-label"><Scan size={16} /> {i18n.t.settings.extraction}</span>
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'llm'} onclick={() => selectedProviderId = 'llm'}>
                <span class="prov-label"><Zap size={16} /> {i18n.t.settings.llm_options}</span>
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'bench'} onclick={() => selectedProviderId = 'bench'}>
                <span class="prov-label"><Beaker size={16} /> {i18n.t.settings.benchmark.title}</span>
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'index'} onclick={() => selectedProviderId = 'index'}>
                <span class="prov-label"><Search size={16} /> {i18n.t.settings.index.title}</span>
                {#if indexStatus === 'ok'}<CheckCircle2 size={12} style="color:#22c55e;" />{/if}
                {#if indexStatus === 'error'}<AlertCircle size={12} style="color:#ef4444;" />{/if}
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'about'} onclick={() => selectedProviderId = 'about'}>
                <span class="prov-label"><Info size={16} /> {i18n.t.settings.about}</span>
            </button>

            <div class="sidebar-divider"></div>
            <!-- Long provider list (14 entries) is collapsed by default —
                 only opens when the user explicitly clicks it.  The
                 active provider's chip is still visible above as a chevron
                 hint via the {Zap} icon on whichever provider is the
                 currently-selected one. -->
            <details class="provider-details">
                <summary><h2 style="display:inline; margin:0;">{i18n.t.settings.providers}</h2></summary>
                <div class="provider-list">
                    {#each providers.filter(p => isMacOS || p.id !== 'mlx') as p}
                        <button class="provider-btn" class:active={selectedProviderId === p.id} onclick={() => selectedProviderId = p.id}>
                            <span style="display:flex; align-items:center; gap:8px;">
                                {#if p.id === 'ollama' || p.id === 'llamacpp' || p.id === 'mlx' || p.id === 'mistralrs' || p.id === 'webllm' || p.id === 'ort'}<Cpu size={14} />{:else}<Globe size={14} />{/if}
                                {p.name}
                            </span>
                            {#if activeProviderId === p.id}<Zap size={12} style="color: #eab308;" />{/if}
                        </button>
                    {/each}
                </div>
            </details>
        </div>

        <div class="sidebar-footer">
            <button class="action-btn secondary reset-nav-btn" style="width:100%; justify-content:center; margin-bottom:8px;" onclick={resetToDefaults}>
                <RotateCcw size={14} /> {i18n.t.settings.reset_defaults}
            </button>
            <button class="save-btn" style="width:100%; padding: 10px;" onclick={handleSave}>
                <Save size={18} /> {i18n.t.settings.save_all}
            </button>
        </div>
    </aside>

    <div class="content">
        {#if selectedProviderId === 'global'}
            <div class="header">
                <h1>{i18n.t.settings.general}</h1>
                <div class="save-area">
                    {#if saveIndicator}<span class="save-badge"><CheckCircle size={14} /> {i18n.t.settings.saved}</span>{/if}
                    <button class="save-btn" onclick={handleSave}>{i18n.t.settings.save_all}</button>
                </div>
            </div>

            <div class="section-card">
                <label for="lang-select"><Languages size={16} /> {i18n.t.settings.language}</label>
                <select id="lang-select" bind:value={currentLanguage} class="styled-select" onchange={updatePrompt}>
                    <option value="en">English</option>
                    <option value="de">Deutsch</option>
                </select>
            </div>

            <div class="section-card">
                <label for="export-path-input"><FolderOpen size={16} /> {i18n.t.settings.export_dir}</label>
                <div class="input-with-action">
                    <input id="export-path-input" type="text" bind:value={exportPath} placeholder={i18n.t.settings.path_placeholder} />
                    <button class="action-btn small" onclick={pickExportPath}>{i18n.t.settings.browse}</button>
                </div>
                <div style="margin-top: 12px;">
                    <label for="export-mode-select" style="margin-bottom: 6px;">{i18n.t.settings.export_mode}</label>
                    <select id="export-mode-select" bind:value={exportPathMode} class="styled-select">
                        <option value="absolute">{i18n.t.settings.export_mode_absolute}</option>
                        <option value="relative">{i18n.t.settings.export_mode_relative}</option>
                    </select>
                </div>
                <p class="hint">{i18n.t.settings.dir_hint}</p>
            </div>

            <div class="section-card">
                <label for="path-template-input">{i18n.t.settings.path_template}</label>
                <input
                    id="path-template-input"
                    type="text"
                    bind:value={pathTemplate}
                    placeholder={'{Author}/{Year}/{Title}'}
                    style="font-family: monospace;"
                />
                <div class="preset-chips">
                    {#each PATH_TEMPLATE_PRESETS as preset}
                        <button
                            class="chip"
                            class:active={pathTemplate === preset.value}
                            onclick={() => pathTemplate = preset.value}
                        >{preset.label}</button>
                    {/each}
                </div>
                <p class="hint template-preview">
                    <span style="opacity:.6">{i18n.t.settings.path_template_preview}:</span>
                    &nbsp;{pathTemplatePreview(pathTemplate)}
                </p>
                <p class="hint">{i18n.t.settings.path_template_hint}</p>
            </div>

            <div class="section-card">
                <label for="log-verbosity">{i18n.t.settings.log_verbosity_label}</label>
                <select id="log-verbosity" bind:value={logVerbosity} class="styled-select" style="max-width: 320px;">
                    <option value="silent">{i18n.t.settings.log_verbosity_silent}</option>
                    <option value="error">{i18n.t.settings.log_verbosity_error}</option>
                    <option value="info">{i18n.t.settings.log_verbosity_info}</option>
                    <option value="debug">{i18n.t.settings.log_verbosity_debug}</option>
                </select>
                <p class="hint">{i18n.t.settings.log_verbosity_hint}</p>
            </div>

        {:else if selectedProviderId === 'extraction'}
            <div class="header">
                <h1>{i18n.t.settings.extraction}</h1>
                <div class="save-area">
                    {#if saveIndicator}<span class="save-badge"><CheckCircle size={14} /> {i18n.t.settings.saved}</span>{/if}
                    <button class="save-btn" onclick={handleSave}>{i18n.t.settings.save_all}</button>
                </div>
            </div>

            <!-- Extractor backend choice -->
            <div class="section-card">
                <label for="pdf-engine-select">{i18n.t.settings.pdf_engine}</label>
                <div class="toggle-group" id="pdf-engine-select">
                    <button class:active={pdfBackend === 'js'} class="toggle-btn" onclick={() => pdfBackend = 'js'}>{i18n.t.settings.pdf_engine_js}</button>
                    <button class:active={pdfBackend === 'rust'} class="toggle-btn" onclick={() => pdfBackend = 'rust'}>{i18n.t.settings.pdf_engine_rust}</button>
                </div>
                <p class="hint">{i18n.t.settings.pdf_engine_hint}</p>
            </div>

            <!-- Smart OCR Pipeline (C++ orchestrator: cleanup + NAFNet + routing) -->
            <div class="section-card">
                <label for="ocr-pipeline-enabled" style="display:flex; align-items:center; gap:8px; cursor:pointer;">
                    <input id="ocr-pipeline-enabled" type="checkbox"
                        bind:checked={ocrPipelineEnabled}
                        disabled={!crispEmbedCompiledIn} />
                    <Scan size={16} /> <span><strong>{i18n.t.settings.ocr_pipeline_enabled}</strong></span>
                </label>
                {#if !crispEmbedCompiledIn}
                    <p class="hint">{i18n.t.settings.ocr_pipeline_requires_crispembed}</p>
                {:else}
                    <p class="hint">{i18n.t.settings.ocr_pipeline_hint}</p>
                {/if}
                {#if ocrPipelineEnabled && crispEmbedCompiledIn}
                    <div class="checkbox-group" style="margin-top:8px;">
                        <input id="ocr-pipeline-router" type="checkbox" bind:checked={ocrPipelineRouter} />
                        <label for="ocr-pipeline-router">{i18n.t.settings.ocr_pipeline_router}</label>
                    </div>
                    <div class="checkbox-group" style="margin-top:4px;">
                        <input id="ocr-pipeline-cleanup" type="checkbox" bind:checked={ocrPipelineCleanup} />
                        <label for="ocr-pipeline-cleanup">{i18n.t.settings.ocr_pipeline_cleanup}</label>
                    </div>
                    <div class="checkbox-group" style="margin-top:4px;">
                        <input id="ocr-pipeline-denoise" type="checkbox" bind:checked={ocrPipelineDenoise} />
                        <label for="ocr-pipeline-denoise">{i18n.t.settings.ocr_pipeline_denoise}</label>
                    </div>
                    <p class="hint">{i18n.t.settings.ocr_pipeline_denoise_hint}</p>

                    <div class="checkbox-group" style="margin-top:4px;">
                        <input id="ocr-pipeline-dewarp" type="checkbox" bind:checked={ocrPipelineDewarp} />
                        <label for="ocr-pipeline-dewarp">{i18n.t.settings.ocr_pipeline_dewarp}</label>
                    </div>
                    {#if ocrPipelineDewarp}
                        <div class="field-row" style="margin-top:4px;">
                            <label for="ocr-pipeline-dewarp-engine" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                                {i18n.t.settings.ocr_pipeline_dewarp_engine}
                                <select id="ocr-pipeline-dewarp-engine" bind:value={ocrPipelineDewarpEngine} style="margin-left:4px;">
                                    <option value="basic">Basic (deskew, fast)</option>
                                    <option value="tps">TPS (curved pages)</option>
                                </select>
                            </label>
                        </div>
                    {/if}
                    <div class="checkbox-group" style="margin-top:4px;">
                        <input id="ocr-pipeline-restore" type="checkbox" bind:checked={ocrPipelineRestore} />
                        <label for="ocr-pipeline-restore">{i18n.t.settings.ocr_pipeline_restore}</label>
                    </div>
                    <p class="hint">{i18n.t.settings.ocr_pipeline_restore_hint}</p>
                    {#if ocrPipelineRestore}
                        <div class="field-row" style="margin-top:4px;">
                            <label for="ocr-pipeline-restore-engine" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                                {i18n.t.settings.ocr_pipeline_restore_engine}
                                <select id="ocr-pipeline-restore-engine" bind:value={ocrPipelineRestoreEngine} style="margin-left:4px;">
                                    <option value="restormer">Restormer (deblur)</option>
                                    <option value="scunet">SCUNet (denoise)</option>
                                    <option value="instructir">InstructIR (all-in-one)</option>
                                </select>
                            </label>
                        </div>
                        {#if ocrPipelineRestoreEngine === 'instructir'}
                            <div class="field-row" style="margin-top:4px;">
                                <label for="ocr-pipeline-restore-task" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                                    {i18n.t.settings.ocr_pipeline_restore_task}
                                    <select id="ocr-pipeline-restore-task" bind:value={ocrPipelineRestoreTask} style="margin-left:4px;">
                                        <option value="denoise">Denoise</option>
                                        <option value="deblur">Deblur</option>
                                        <option value="dehaze">Dehaze</option>
                                        <option value="derain">Derain</option>
                                        <option value="super_resolution">Super-resolution</option>
                                        <option value="low_light">Low-light</option>
                                        <option value="enhance">Enhance</option>
                                    </select>
                                </label>
                            </div>
                        {/if}
                    {/if}

                    <div class="checkbox-group" style="margin-top:4px;">
                        <input id="ocr-pipeline-sr" type="checkbox" bind:checked={ocrPipelineSr} />
                        <label for="ocr-pipeline-sr">{i18n.t.settings.ocr_pipeline_sr}</label>
                    </div>
                    <p class="hint">{i18n.t.settings.ocr_pipeline_sr_hint}</p>
                    {#if ocrPipelineSr}
                        <div class="field-row" style="margin-top:4px;">
                            <label for="ocr-pipeline-sr-engine" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                                {i18n.t.settings.ocr_pipeline_sr_engine}
                                <select id="ocr-pipeline-sr-engine" bind:value={ocrPipelineSrEngine} style="margin-left:4px;">
                                    <option value="pan">PAN (4×, fast)</option>
                                    <option value="esrgan">Real-ESRGAN (blur/noise)</option>
                                    <option value="safmn">SAFMN (lightweight)</option>
                                    <option value="hat">HAT (4×, SOTA quality)</option>
                                    <option value="tbsrn">TBSRN (text-line, tiny)</option>
                                    <option value="dat">DAT (2×, transformer)</option>
                                    <option value="swinir">SwinIR (4×, tiny)</option>
                                </select>
                            </label>
                        </div>
                        <div class="field-row" style="margin-top:4px;">
                            <label for="ocr-pipeline-sr-maxpx" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                                {i18n.t.settings.ocr_pipeline_sr_max_px}
                            </label>
                            <input id="ocr-pipeline-sr-maxpx" type="number" min="200" max="4000" step="100"
                                bind:value={ocrPipelineSrMaxPx} style="max-width:100px;" />
                        </div>
                    {/if}

                    <div class="field-row" style="margin-top:8px;">
                        <label for="ocr-pipeline-minchars" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                            {i18n.t.settings.ocr_pipeline_min_chars}
                        </label>
                        <input id="ocr-pipeline-minchars" type="number" min="0" max="200" step="1"
                            bind:value={ocrPipelineMinChars} style="max-width:90px;" />
                    </div>
                    <div class="field-row" style="margin-top:6px;">
                        <label for="ocr-pipeline-minconf" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                            {i18n.t.settings.ocr_pipeline_min_conf} ({Number(ocrPipelineMinConf).toFixed(2)})
                        </label>
                        <input id="ocr-pipeline-minconf" type="range" min="0" max="1" step="0.05"
                            bind:value={ocrPipelineMinConf} style="flex:1; max-width:200px;" />
                    </div>
                    <p class="hint">{i18n.t.settings.ocr_pipeline_gate_hint}</p>

                    <label for="ocr-pipeline-punct" style="margin-top:10px;">{i18n.t.settings.ocr_pipeline_punct}</label>
                    <input id="ocr-pipeline-punct" type="text" bind:value={ocrPipelinePunct}
                        placeholder={i18n.t.settings.ocr_pipeline_punct_ph} />
                    <p class="hint">{i18n.t.settings.ocr_pipeline_punct_hint}</p>

                    <!-- P20 slice 3 — layout-aware reading order -->
                    <div style="margin-top:14px; border-top:1px solid #27272a; padding-top:10px;">
                        <div class="checkbox-group">
                            <input id="ocr-pipeline-layout" type="checkbox" bind:checked={ocrPipelineLayout} />
                            <label for="ocr-pipeline-layout"><strong>{i18n.t.settings.ocr_pipeline_layout}</strong></label>
                        </div>
                        <p class="hint">{i18n.t.settings.ocr_pipeline_layout_hint}</p>
                        {#if ocrPipelineLayout}
                            <div class="field-row" style="margin-top:6px;">
                                <label for="ocr-pipeline-layout-engine" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                                    {i18n.t.settings.ocr_pipeline_layout_engine}
                                    <select id="ocr-pipeline-layout-engine" bind:value={ocrPipelineLayoutEngine} style="margin-left:4px;">
                                        <option value="rtdetr">RT-DETRv2 (semantic)</option>
                                        <option value="cc">Connected-components (model-free)</option>
                                    </select>
                                </label>
                            </div>
                            <div class="field-row" style="margin-top:6px;">
                                <label for="ocr-pipeline-layout-thr" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">
                                    {i18n.t.settings.ocr_pipeline_layout_threshold} ({Number(ocrPipelineLayoutThr).toFixed(2)})
                                </label>
                                <input id="ocr-pipeline-layout-thr" type="range" min="0.05" max="0.9" step="0.05"
                                    bind:value={ocrPipelineLayoutThr} style="flex:1; max-width:200px;" />
                            </div>
                            <div class="checkbox-group" style="margin-top:4px;">
                                <input id="ocr-pipeline-drophf" type="checkbox" bind:checked={ocrPipelineDropHF} />
                                <label for="ocr-pipeline-drophf">{i18n.t.settings.ocr_pipeline_drop_hf}</label>
                            </div>
                        {/if}
                    </div>

                    <!-- Advanced per-stage builder -->
                    <div style="margin-top:14px; border-top:1px solid #27272a; padding-top:10px;">
                        <label style="display:flex; align-items:center; gap:8px; cursor:pointer;">
                            <input type="checkbox" bind:checked={ocrPipelineAdvanced} />
                            <span><strong>{i18n.t.settings.ocr_pipeline_advanced}</strong></span>
                        </label>
                        <p class="hint">{i18n.t.settings.ocr_pipeline_advanced_hint}</p>

                        {#if ocrPipelineAdvanced}
                            {#each ocrStages as stage, i (i)}
                                <div class="section-card" style="background:#1c1c1f; margin-top:8px; padding:10px;">
                                    <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:6px;">
                                        <strong style="font-size:0.85rem;">{i18n.t.settings.ocr_pipeline_stage} {i + 1}</strong>
                                        <div style="display:flex; gap:4px;">
                                            <button class="action-btn small" onclick={() => moveOcrStage(i, -1)} disabled={i === 0} title="↑">↑</button>
                                            <button class="action-btn small" onclick={() => moveOcrStage(i, 1)} disabled={i === ocrStages.length - 1} title="↓">↓</button>
                                            <button class="action-btn small" onclick={() => removeOcrStage(i)} title="✕">✕</button>
                                        </div>
                                    </div>
                                    <div class="field-row" style="gap:8px; flex-wrap:wrap;">
                                        <label style="font-size:0.78rem; color:#a1a1aa;">{i18n.t.settings.ocr_pipeline_source}
                                            <select bind:value={stage.source_type} style="margin-left:4px;">
                                                <option value="auto">auto</option>
                                                <option value="screenshot">screenshot</option>
                                                <option value="scanned_doc">scanned doc</option>
                                                <option value="photo">photo</option>
                                            </select>
                                        </label>
                                        <label style="font-size:0.78rem; color:#a1a1aa;">{i18n.t.settings.ocr_pipeline_engine}
                                            <select bind:value={stage.engine} style="margin-left:4px;">
                                                <option value="dbnet_trocr">DBNet + TrOCR</option>
                                                <option value="surya">Surya</option>
                                                <option value="tesseract">Tesseract LSTM (DBNet + Tesseract)</option>
                                                <option value="got">GOT-OCR2</option>
                                                <option value="glm">GLM-OCR</option>
                                                <option value="qwen2vl">Qwen2.5-VL</option>
                                                <option value="internvl2">InternVL2</option>
                                            </select>
                                        </label>
                                    </div>
                                    <div class="field-row" style="gap:8px; flex-wrap:wrap; margin-top:6px;">
                                        <input type="text" bind:value={stage.det_model} style="flex:1; min-width:140px;"
                                            placeholder={isVlmEngine(stage.engine) ? i18n.t.settings.ocr_pipeline_model_ph : i18n.t.settings.ocr_pipeline_det_ph} />
                                        {#if !isVlmEngine(stage.engine)}
                                            <input type="text" bind:value={stage.rec_model} style="flex:1; min-width:140px;"
                                                placeholder={i18n.t.settings.ocr_pipeline_rec_ph} />
                                        {/if}
                                    </div>
                                    <div style="display:flex; gap:12px; flex-wrap:wrap; margin-top:6px; font-size:0.78rem; color:#d4d4d8;">
                                        <label><input type="checkbox" bind:checked={stage.cleanup.enabled} /> {i18n.t.settings.ocr_pipeline_clean_short}</label>
                                        <label><input type="checkbox" bind:checked={stage.cleanup.deskew} /> deskew</label>
                                        <label><input type="checkbox" bind:checked={stage.cleanup.crop_borders} /> crop</label>
                                        <label><input type="checkbox" bind:checked={stage.cleanup.whiten_background} /> whiten</label>
                                        <label><input type="checkbox" bind:checked={stage.cleanup.binarize} /> binarize</label>
                                        <label><input type="checkbox" bind:checked={stage.cleanup.denoise} /> NAFNet</label>
                                    </div>
                                    {#if isVlmEngine(stage.engine)}
                                        <div class="field-row" style="gap:8px; flex-wrap:wrap; margin-top:6px;">
                                            <label style="font-size:0.78rem; color:#a1a1aa;">max tokens
                                                <input type="number" min="0" step="64" bind:value={stage.vlm_max_tokens} style="width:80px; margin-left:4px;" /></label>
                                            <input type="text" bind:value={stage.vlm_prompt} style="flex:1; min-width:160px;" placeholder={i18n.t.settings.ocr_pipeline_prompt_ph} />
                                        </div>
                                    {:else}
                                        <div class="field-row" style="gap:8px; flex-wrap:wrap; margin-top:6px; font-size:0.78rem; color:#a1a1aa;">
                                            <label>prob<input type="number" min="0" max="1" step="0.05" bind:value={stage.det_prob_threshold} style="width:70px; margin-left:4px;" /></label>
                                            <label>box<input type="number" min="0" max="1" step="0.05" bind:value={stage.det_box_threshold} style="width:70px; margin-left:4px;" /></label>
                                            <label>short side<input type="number" min="64" step="32" bind:value={stage.det_target_short} style="width:80px; margin-left:4px;" /></label>
                                        </div>
                                    {/if}
                                    <div class="field-row" style="gap:8px; flex-wrap:wrap; margin-top:6px; font-size:0.78rem; color:#a1a1aa;">
                                        <label>{i18n.t.settings.ocr_pipeline_min_chars}<input type="number" min="0" bind:value={stage.min_chars} style="width:70px; margin-left:4px;" /></label>
                                        <label>min conf<input type="number" min="0" max="1" step="0.05" bind:value={stage.min_confidence} style="width:70px; margin-left:4px;" /></label>
                                    </div>
                                </div>
                            {/each}
                            <button class="action-btn small" style="margin-top:8px;" onclick={addOcrStage}>+ {i18n.t.settings.ocr_pipeline_add_stage}</button>
                            <p class="hint">{i18n.t.settings.ocr_pipeline_stage_hint}</p>
                        {/if}
                    </div>
                {/if}
            </div>

            <!-- OCR (Tesseract) -->
            <div class="section-card">
                <div class="header" style="margin-bottom: 12px; display:flex; align-items:center; justify-content:space-between;">
                    <h2 style="font-size: 1rem; color: #a1a1aa;"><Scan size={16} /> {i18n.t.settings.ocr_tesseract_title}</h2>
                    <button class="action-btn small" onclick={refreshTesseractModels} disabled={tesseractRefreshing} title={i18n.t.settings.refresh_models}>
                        {#if tesseractRefreshing}
                            <Loader2 size={14} class="loader-spin" />
                        {:else}
                            <RefreshCw size={14} />
                        {/if}
                        {i18n.t.settings.refresh_models}
                    </button>
                </div>
                <div class="checkbox-group">
                    <input id="ocr-enabled-check" type="checkbox" bind:checked={ocrEnabled} />
                    <label for="ocr-enabled-check">{i18n.t.settings.ocr_enabled}</label>
                </div>
                {#if ocrEnabled}
                    <div class="field-row" style="margin-top: 8px;">
                        <label for="ocr-tier-select" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">OCR-Engine:</label>
                        <select id="ocr-tier-select" bind:value={ocrTier} style="flex:1; max-width:260px;">
                            <option value="auto">Automatisch (beste verfügbare)</option>
                            <option value="tier3">PaddleOCR — multilingual, schnell (empfohlen)</option>
                            <option value="tier2">ocrs — Rust, nur lateinische Schrift</option>
                            <option value="tier1">Tesseract — System-Installation erforderlich</option>
                        </select>
                    </div>
                    {#if ocrTier === 'tier3' || ocrTier === 'auto'}
                        <div class="field-row" style="margin-top: 6px;">
                            <label for="ocr-rec-lang-select" style="font-size:0.8125rem; color:#a1a1aa; white-space:nowrap;">Schrift:</label>
                            <select id="ocr-rec-lang-select" bind:value={ocrRecLang} style="flex:1; max-width:260px;">
                                <option value="auto">Automatisch (Pfad-Heuristik)</option>
                                <option value="latin">Lateinisch (EN, DE, FR, …)</option>
                                <option value="cjk">CJK (Chinesisch, Japanisch, Koreanisch)</option>
                            </select>
                        </div>
                    {/if}
                    <p class="hint" style="margin-bottom:8px;">
                        PaddleOCR benötigt <code>--features paddle-ocr</code> beim Build.
                        ocrs ist in der Standard-Binary enthalten und braucht keine Installation.
                        Tesseract muss separat installiert werden (brew / apt / winget).
                    </p>
                {/if}
                <p class="hint" style="margin-bottom: 16px;">{i18n.t.settings.ocr_tesseract_hint}</p>

                <div class="models-grid">
                    {#each tesseractModels as model}
                        <div class="local-model-row">
                            <div class="model-info">
                                <div class="model-title-line">
                                    <strong>{model.name}</strong>
                                    <span class="size-badge" style="font-size: 0.6rem;">{model.id}</span>
                                </div>
                                <span class="model-path">{model.isDownloaded ? i18n.t.settings.tesseract_ready : i18n.t.settings.not_installed}</span>
                            </div>
                            <div class="model-status">
                                {#if model.isDownloaded}
                                    <span class="save-badge" style="color: #10b981;"><Check size={14} /> {i18n.t.settings.installed}</span>
                                {:else}
                                    <button class="action-btn small primary">
                                        <Download size={14} /> {i18n.t.settings.download}
                                    </button>
                                {/if}
                            </div>
                        </div>
                    {/each}
                </div>
            </div>

            <!-- PDF /Info dict prefill -->
            <div class="section-card">
                <div class="checkbox-group">
                    <input id="pdf-metadata-check" type="checkbox" bind:checked={pdfMetadataPrefill} />
                    <label for="pdf-metadata-check">{i18n.t.settings.pdf_metadata_prefill}</label>
                </div>
                <p class="hint">{i18n.t.settings.pdf_metadata_prefill_hint}</p>
            </div>

            <!-- Save extracted text alongside the sorted file -->
            <div class="section-card">
                <div class="checkbox-group">
                    <input id="save-txt-check" type="checkbox" bind:checked={saveTxt} />
                    <label for="save-txt-check"><FileText size={16} /> {i18n.t.settings.save_txt}</label>
                </div>
                <p class="hint">{i18n.t.settings.save_txt_hint}</p>
            </div>

            <!-- LLM input cap (lives here because it caps the *extracted* text -->
            <div class="section-card">
                <label for="max-chars-input">{i18n.t.settings.llm_max_chars}</label>
                <input id="max-chars-input" type="number" bind:value={llmMaxChars} step="500" min="500" />
                <p class="hint">{i18n.t.settings.llm_max_chars_hint}</p>
            </div>

            <!-- PLAN P8.1 — per-file conversion timeout -->
            <div class="section-card">
                <label for="conv-timeout">{i18n.t.settings.conv_timeout_label}</label>
                <input
                    id="conv-timeout"
                    type="number"
                    min="0"
                    step="10"
                    bind:value={conversionTimeoutSeconds}
                    style="width: 120px;"
                />
                <p class="hint">
                    {i18n.t.settings.conv_timeout_hint_prefix}<strong>{i18n.t.settings.conv_timeout_hint_zero}</strong>{i18n.t.settings.conv_timeout_hint_suffix}
                </p>
            </div>

            <!-- PLAN P10 step c — extraction worker count -->
            <div class="section-card">
                <label for="extraction-workers">{i18n.t.settings.extraction_workers_label}</label>
                <input
                    id="extraction-workers"
                    type="number"
                    min="1"
                    max="16"
                    step="1"
                    bind:value={extractionWorkers}
                    style="width: 120px;"
                />
                <p class="hint">{i18n.t.settings.extraction_workers_hint}</p>
            </div>

        {:else if selectedProviderId === 'llm'}
            <div class="header">
                <h1>{i18n.t.settings.llm_options}</h1>
                <div class="save-area">
                    {#if saveIndicator}<span class="save-badge"><CheckCircle size={14} /> {i18n.t.settings.saved}</span>{/if}
                    <button class="save-btn" onclick={handleSave}>{i18n.t.settings.save_all}</button>
                </div>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <input id="no-think-check" type="checkbox" bind:checked={noThinking} />
                    <label for="no-think-check">{i18n.t.settings.no_think}</label>
                </div>
                <p class="hint">{i18n.t.settings.no_think_hint}</p>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <input id="auto-speak-check" type="checkbox" bind:checked={autoSpeakReplies} />
                    <label for="auto-speak-check">{i18n.t.settings.auto_speak}</label>
                </div>
                <p class="hint">{i18n.t.settings.auto_speak_hint}</p>
            </div>

            <div class="section-card">
                <label><FolderOpen size={16} /> {i18n.t.settings.watch_folders}</label>
                {#if watchFolders.length === 0}
                    <p class="hint" style="margin-top:6px;">{i18n.t.settings.watch_none}</p>
                {:else}
                    <ul style="list-style:none; padding:0; margin:8px 0; display:flex; flex-direction:column; gap:4px;">
                        {#each watchFolders as folder (folder)}
                            <li style="display:flex; align-items:center; gap:8px;">
                                <code style="flex:1; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; font-size:0.75rem;">{folder}</code>
                                <button class="action-btn small danger" onclick={() => removeWatchFolder(folder)}
                                        title={i18n.t.settings.watch_remove}>
                                    ×
                                </button>
                            </li>
                        {/each}
                    </ul>
                {/if}
                <button class="action-btn small" style="margin-top:8px;" onclick={addWatchFolder}>
                    + {i18n.t.settings.watch_add}
                </button>
                {#if watchStatusMsg}
                    <p class="hint" style="margin-top:6px;">{watchStatusMsg}</p>
                {/if}
                <p class="hint">{i18n.t.settings.watch_hint}</p>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <input id="author-sort-check" type="checkbox" bind:checked={authorSortEnabled} />
                    <label for="author-sort-check">{i18n.t.settings.author_sort}</label>
                </div>
                <p class="hint">{i18n.t.settings.author_sort_hint}</p>
            </div>

            <div class="section-card">
                <label for="parsing-format-select">{i18n.t.settings.parsing_format}</label>
                <div class="toggle-group" id="parsing-format-select">
                    <button class:active={parsingFormat === 'xml'} class="toggle-btn" onclick={() => { parsingFormat = 'xml'; updatePrompt(); }}>{i18n.t.settings.parsing_xml}</button>
                    <button class:active={parsingFormat === 'json'} class="toggle-btn" onclick={() => { parsingFormat = 'json'; updatePrompt(); }}>{i18n.t.settings.parsing_json}</button>
                </div>
            </div>

            <div class="section-card">
                <div class="header" style="margin-bottom: 12px;">
                    <h2 style="font-size: 1rem; color: #a1a1aa;"><RefreshCw size={16} /> {i18n.t.settings.roundrobin_title}</h2>
                </div>
                <p class="hint" style="margin-top: 0; margin-bottom: 12px;">{i18n.t.settings.roundrobin_hint}</p>

                {#if rrCandidates.length === 0}
                    <p class="hint">{i18n.t.settings.roundrobin_no_providers}</p>
                {:else}
                    <div class="rr-list">
                        {#each rrCandidates as p}
                            {@const rrPos = roundRobinProviders.indexOf(p.id)}
                            {@const isEnabled = rrPos !== -1}
                            <div class="rr-row" class:rr-enabled={isEnabled}>
                                <label class="rr-label" for="rr-{p.id}">
                                    <input type="checkbox" id="rr-{p.id}" checked={isEnabled} onchange={() => rrToggle(p.id)} />
                                    <span>{p.name}</span>
                                </label>
                                {#if isEnabled}
                                    <div class="rr-order">
                                        <span class="rr-idx">#{rrPos + 1}</span>
                                        <button class="icon-btn-tiny" onclick={() => rrMoveUp(rrPos)} disabled={rrPos === 0} title={i18n.t.settings.move_up}>▲</button>
                                        <button class="icon-btn-tiny" onclick={() => rrMoveDown(rrPos)} disabled={rrPos === roundRobinProviders.length - 1} title={i18n.t.settings.move_down}>▼</button>
                                    </div>
                                {/if}
                            </div>
                        {/each}
                    </div>
                {/if}
            </div>

            <div class="section-card">
                <label for="llm-workers">{i18n.t.settings.llm_workers_label}</label>
                <input
                    id="llm-workers"
                    type="number"
                    min="1"
                    max="16"
                    step="1"
                    bind:value={llmWorkers}
                    style="width: 120px;"
                />
                <p class="hint">{i18n.t.settings.llm_workers_hint}</p>
            </div>

            <div class="section-card">
                <label for="llm-prompt-input">{i18n.t.settings.llm_prompt}</label>
                <textarea id="llm-prompt-input" class="styled-textarea" bind:value={llmPrompt} rows="10"></textarea>
                <p class="hint">{i18n.t.settings.llm_prompt_hint}</p>
            </div>

        {:else if selectedProviderId === 'bench'}
            <div class="header">
                <h1>{i18n.t.settings.benchmark.title}</h1>
            </div>
            <div class="section-card benchmark-ui">
                <p class="hint" style="margin-top:0; margin-bottom:20px;">{i18n.t.settings.benchmark.hint}</p>
                
                <div class="bench-config-row">
                    <span class="bench-config-label">{i18n.t.settings.benchmark.providers}</span>
                    <div class="bench-providers-grid">
                        {#each providers.filter(p => isMacOS || p.id !== 'mlx') as p}
                            <div class="bench-provider-card" class:selected={benchProviders.includes(p.id)}>
                                <label class="bench-check-label" for="bench-p-{p.id}">
                                    <input type="checkbox" id="bench-p-{p.id}" bind:group={benchProviders} value={p.id} />
                                    <span class="p-name">{p.name}</span>
                                </label>
                                <select id="bench-model-select-{p.id}" class="bench-model-select" bind:value={benchModels[p.id]} disabled={!benchProviders.includes(p.id)} aria-label={i18n.t.settings.action_select_bench_model}>
                                    <option value="">{i18n.t.settings.benchmark.select_model}</option>
                                    {#each p.models || [] as m}<option value={m}>{m}</option>{/each}
                                </select>
                            </div>
                        {/each}
                    </div>
                </div>

                <div class="bench-config-row">
                    <span class="bench-config-label">{i18n.t.settings.benchmark.docs}</span>
                    <div style="flex:1;">
                        <div class="bench-file-list">
                            {#each batchManager.items as item}
                                <label class="bench-check-label file-item" for="bench-doc-{item.id}">
                                    <input type="checkbox" id="bench-doc-{item.id}" bind:group={benchDocuments} value={item.id} />
                                    <span class="file-name">{item.originalName}</span>
                                    <span class="char-count">{(item.extractedText?.length || 0)} chars</span>
                                </label>
                            {:else}
                                <div class="empty-docs">{i18n.t.settings.benchmark.no_documents}</div>
                            {/each}
                        </div>
                    </div>
                </div>

                <div class="bench-config-row">
                    <span class="bench-config-label">{i18n.t.settings.benchmark.prompt_label}</span>
                    <div style="flex:1;">
                        <div class="toggle-group small" style="margin-bottom:12px;">
                            <button class:active={benchPromptMode === 'batch'} class="toggle-btn" onclick={() => benchPromptMode = 'batch'}>{i18n.t.settings.benchmark.prompt_batch}</button>
                            <button class:active={benchPromptMode === 'custom'} class="toggle-btn" onclick={() => benchPromptMode = 'custom'}>{i18n.t.settings.benchmark.prompt_custom}</button>
                        </div>
                        {#if benchPromptMode === 'custom'}
                            <textarea id="bench-custom-prompt" class="bench-prompt-input" bind:value={benchCustomPrompt} rows="3" aria-label={i18n.t.settings.action_custom_bench_prompt}></textarea>
                        {/if}
                    </div>
                </div>

                <div class="bench-config-row">
                    <span class="bench-config-label">{i18n.t.settings.benchmark.runs}</span>
                    <div class="runs-select-grid">
                        <button class:active={benchRuns === 1} onclick={() => benchRuns = 1}>
                            <span class="r-num">1</span> <span class="r-text">{i18n.t.settings.benchmark.run_cold}</span>
                        </button>
                        <button class:active={benchRuns === 2} onclick={() => benchRuns = 2}>
                            <span class="r-num">2</span> <span class="r-text">{i18n.t.settings.benchmark.run_cold_warm}</span>
                        </button>
                        <button class:active={benchRuns === 3} onclick={() => benchRuns = 3}>
                            <span class="r-num">3</span> <span class="r-text">{i18n.t.settings.benchmark.run_cold_2warm}</span>
                        </button>
                    </div>
                </div>

                <button class="action-btn primary large-bench-btn" onclick={runBenchmark} disabled={benchRunning}>
                    {#if benchRunning}<Loader2 size={20} class="loader-spin" />{:else}<Play size={20} />{/if}
                    <span>{i18n.t.settings.benchmark.run_btn}</span>
                </button>
            </div>

            {#if benchResults.length > 0}
                <div class="section-card">
                    <table class="bench-table">
                        <thead>
                            <tr>
                                <th>{i18n.t.settings.benchmark.col_provider}</th>
                                <th class="bench-num">{i18n.t.settings.benchmark.col_avg_latency}</th>
                                <th class="bench-num">{i18n.t.settings.benchmark.col_runs}</th>
                                <th>{i18n.t.settings.benchmark.col_details}</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each benchResults as res}
                                <tr>
                                    <td>
                                        <div style="font-weight:600;">{res.providerName}</div>
                                        <div class="bench-model">{res.model}</div>
                                    </td>
                                    <td class="bench-num">{res.avgLatency.toLocaleString()} ms</td>
                                    <td class="bench-num">{res.runs.length}</td>
                                    <td><button class="bench-view-btn" onclick={() => benchModal = { title: `${res.providerName} — ${res.model}`, runs: res.runs }}>{i18n.t.settings.benchmark.view}</button></td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </div>
            {/if}

            <!-- Embedding Benchmark: ONNX vs GGUF for the same model -->
            <div class="section-card">
                <div class="header" style="margin-bottom:12px; display:flex; align-items:center; justify-content:space-between;">
                    <h2 style="font-size:1rem; color:#a1a1aa; margin:0;"><Cpu size={16} /> {i18n.t.settings.benchmark.embed_bench_title}</h2>
                </div>
                <p class="hint" style="margin-bottom:12px;">
                    {i18n.t.settings.benchmark.embed_bench_hint}
                </p>

                <div class="bench-config-row">
                    <span class="bench-config-label">{i18n.t.settings.benchmark.embed_bench_model}</span>
                    <select bind:value={embedderBenchModel} class="styled-select" style="flex:1;">
                        <option value="bge_small_en_v15">BGE Small EN v1.5 (384d, fast)</option>
                        <option value="all_mini_lm_l6_v2">all-MiniLM-L6-v2 (384d, fastest)</option>
                        <option value="bge_base_en_v15">BGE Base EN v1.5 (768d)</option>
                        <option value="bge_large_en_v15">BGE Large EN v1.5 (1024d)</option>
                        <option value="multilingual_e5_small">Multilingual E5 Small (384d)</option>
                        <option value="multilingual_e5_base">Multilingual E5 Base (768d)</option>
                        <option value="nomic_embed_text_v15">Nomic Embed Text v1.5 (768d, 8k ctx)</option>
                    </select>
                </div>

                <div class="bench-config-row">
                    <span class="bench-config-label">{i18n.t.settings.benchmark.embed_bench_texts}</span>
                    <textarea class="bench-prompt-input" rows="3"
                        bind:value={embedderBenchTexts}
                        placeholder={i18n.t.settings.benchmark.embed_bench_texts_placeholder}></textarea>
                </div>

                <button class="action-btn primary large-bench-btn" onclick={runEmbedderBenchmark}
                    disabled={embedderBenchRunning}>
                    {#if embedderBenchRunning}<Loader2 size={20} class="loader-spin" />{:else}<Play size={20} />{/if}
                    <span>{i18n.t.settings.benchmark.embed_bench_run_btn}</span>
                </button>

                {#if embedderBenchResults.length > 0}
                    <table class="bench-table" style="margin-top:14px;">
                        <thead>
                            <tr>
                                <th>{i18n.t.settings.benchmark.embed_bench_col_engine}</th>
                                <th>{i18n.t.settings.benchmark.embed_bench_model}</th>
                                <th class="bench-num">{i18n.t.settings.benchmark.embed_bench_col_load_ms}</th>
                                <th class="bench-num">{i18n.t.settings.benchmark.embed_bench_col_embed_ms}</th>
                                <th class="bench-num">{i18n.t.settings.benchmark.embed_bench_col_throughput}</th>
                                <th class="bench-num">{i18n.t.settings.benchmark.embed_bench_col_dim}</th>
                                <th>{i18n.t.settings.benchmark.embed_bench_col_status}</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each embedderBenchResults as r}
                                <tr>
                                    <td>{r.backend === 'onnx' ? i18n.t.settings.benchmark.embed_bench_engine_onnx : i18n.t.settings.benchmark.embed_bench_engine_gguf}</td>
                                    <td><div class="bench-model">{r.model_id}</div></td>
                                    <td class="bench-num">{r.load_time_ms.toLocaleString()}</td>
                                    <td class="bench-num">{r.error ? '—' : r.embed_time_ms.toLocaleString()}</td>
                                    <td class="bench-num">{r.error ? '—' : r.texts_per_second.toFixed(1)}</td>
                                    <td class="bench-num">{r.dim || '—'}</td>
                                    <td>
                                        {#if r.error}
                                            <span style="color:#f87171;" title={r.error}>error</span>
                                        {:else}
                                            <span style="color:#22c55e;">{i18n.t.settings.benchmark.embed_bench_status_ok} ({r.vectors_count})</span>
                                        {/if}
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                {/if}
            </div>

        {:else if selectedProviderId === 'index'}
            <div class="header">
                <h1>{i18n.t.settings.index.title}</h1>
                <div class="save-area">
                    {#if saveIndicator}<span class="save-badge"><CheckCircle size={14} /> {i18n.t.settings.saved}</span>{/if}
                </div>
            </div>

            <p style="color:#a1a1aa; font-size:0.85rem; margin-bottom:16px;">{i18n.t.settings.index.hint}</p>

            <!-- Enable toggle -->
            <div class="section-card">
                <label style="display:flex; align-items:center; gap:10px; cursor:pointer;">
                    <input type="checkbox" bind:checked={indexEnabled} />
                    <span><strong>{i18n.t.settings.index.enabled}</strong></span>
                </label>
                <p class="hint">{i18n.t.settings.index.enabled_hint}</p>
            </div>

            <!-- Vector capabilities master switch -->
            <div class="section-card">
                <label style="display:flex; align-items:center; gap:10px; cursor:pointer;">
                    <input type="checkbox" bind:checked={indexUseVector} />
                    <span><strong>{i18n.t.settings.index.use_vector}</strong></span>
                </label>
                <p class="hint">{i18n.t.settings.index.use_vector_hint}</p>
            </div>

            <!-- Hide every embedder-related option when vectors are off,
                 since none of those settings has any effect in that mode. -->
            {#if !indexUseVector}
                <div class="section-card" style="border-color:#3b82f6; background:#1e3a8a14;">
                    <p class="hint" style="margin:0;">
                        {i18n.t.settings.index.no_vector_active}
                    </p>
                </div>
            {/if}

            <!-- Catalogs (named bundles of the settings below) -->
            <div class="section-card">
                <div style="display:flex; align-items:center; justify-content:space-between; margin-bottom:8px;">
                    <label style="margin:0;"><HardDrive size={16} /> Kataloge</label>
                    <button class="action-btn small" onclick={createCatalogFromCurrent}>
                        <Plus size={13} /> Aktuelle Einstellungen als Katalog speichern
                    </button>
                </div>
                <p class="hint" style="margin-bottom:12px;">
                    Ein Katalog bündelt Daten-Verzeichnis, Embedder-Modell, Modus und Backend unter einem Namen — so können verschiedene Bibliotheken (z. B. „Theologie", „Musik-PDFs") parallel verwaltet und schnell umgeschaltet werden.
                </p>
                {#if catalogs.length === 0}
                    <p class="hint" style="font-style:italic; color:#52525b;">Noch kein Katalog gespeichert. Konfiguriere die Felder unten und drücke „Aktuelle Einstellungen als Katalog speichern".</p>
                {:else}
                    <div class="catalog-list">
                        {#each catalogs as cat (cat.id)}
                            <div class="catalog-row" class:active={cat.id === activeCatalogId}>
                                <input type="radio" name="active-catalog" value={cat.id}
                                    checked={cat.id === activeCatalogId}
                                    onchange={() => selectCatalog(cat.id)} />
                                {#if renamingCatalogId === cat.id}
                                    <input class="catalog-rename-input" bind:value={renameDraft}
                                        onkeydown={e => { if (e.key === 'Enter') commitRename(); if (e.key === 'Escape') renamingCatalogId = null; }}
                                        onblur={commitRename} />
                                {:else}
                                    <button class="catalog-name" onclick={() => selectCatalog(cat.id)}>
                                        <strong>{cat.name}</strong>
                                    </button>
                                {/if}
                                <span class="catalog-meta">
                                    {cat.embedderModel} · {cat.mode} · {cat.backend}
                                    {#if cat.dataDir}· {cat.dataDir}{/if}
                                </span>
                                <div class="catalog-actions">
                                    <button class="icon-btn" onclick={() => startRename(cat.id)} title="Umbenennen">
                                        <Edit size={13} />
                                    </button>
                                    <button class="icon-btn danger" onclick={() => deleteCatalog(cat.id)} title="Entfernen">
                                        <Trash2 size={13} />
                                    </button>
                                </div>
                            </div>
                        {/each}
                    </div>
                {/if}
            </div>

            <!-- Search mode -->
            <div class="section-card">
                <label for="index-mode-select"><Brain size={16} /> {i18n.t.settings.index.search_mode}</label>
                <select id="index-mode-select" bind:value={indexSearchMode} class="styled-select">
                    <option value="hybrid">{i18n.t.settings.index.mode_hybrid}</option>
                    <option value="text">{i18n.t.settings.index.mode_text}</option>
                    <option value="vector">{i18n.t.settings.index.mode_vector}</option>
                </select>
            </div>

            <!-- Backend -->
            <div class="section-card">
                <label for="index-backend-select"><HardDrive size={16} /> {i18n.t.settings.index.backend}</label>
                <select id="index-backend-select" bind:value={indexBackendType} class="styled-select">
                    <option value="local">{i18n.t.settings.index.backend_local}</option>
                    <option value="remote">{i18n.t.settings.index.backend_remote}</option>
                    <option value="hybrid">{i18n.t.settings.index.backend_hybrid}</option>
                </select>
            </div>

            {#if indexBackendType === 'remote' || indexBackendType === 'hybrid'}
            <!-- Remote URL + Key -->
            <div class="section-card">
                <label for="index-remote-url">{i18n.t.settings.index.remote_url}</label>
                <input id="index-remote-url" type="text" bind:value={indexRemoteUrl}
                    placeholder={i18n.t.settings.index.remote_url_placeholder} />
                <label for="index-remote-key" style="margin-top:10px;"><Key size={14} /> {i18n.t.settings.index.remote_api_key}</label>
                <input id="index-remote-key" type="password" bind:value={indexRemoteApiKey} placeholder="••••••••" />
            </div>

            {#if indexUseVector}
            <!-- Embedder location (only relevant for remote backend + vectors on) -->
            <div class="section-card">
                <label for="index-embedder-location">{i18n.t.settings.index.embedder_location}</label>
                <select id="index-embedder-location" bind:value={indexEmbedderLocation} class="styled-select">
                    <option value="client">{i18n.t.settings.index.embedder_location_client}</option>
                    <option value="server">{i18n.t.settings.index.embedder_location_server}</option>
                </select>
                <p class="hint">{i18n.t.settings.index.embedder_location_hint}</p>
            </div>
            {/if}
            {/if}

            <!-- Inference engine FIRST so the model dropdown below can filter
                 to what's actually runnable on the chosen engine. -->
            <div class="section-card">
                <label for="index-backend-select">
                    <Cpu size={14} /> {i18n.t.settings.index.backend_engine}
                </label>
                <div class="toggle-group" id="index-backend-select" style="margin-top:4px;">
                    <button
                        class="toggle-btn"
                        class:active={indexEmbedderBackend === 'onnx'}
                        onclick={() => onSelectEngine('onnx')}>
                        FastEmbed (ONNX)
                    </button>
                    <button
                        class="toggle-btn"
                        class:active={indexEmbedderBackend === 'gguf'}
                        disabled={!crispEmbedCompiledIn}
                        title={crispEmbedCompiledIn ? '' : 'CrispEmbed is not linked into this build. Re-run via .\\enable-crispembed.ps1.'}
                        onclick={() => { if (crispEmbedCompiledIn) onSelectEngine('gguf'); }}>
                        CrispEmbed (GGUF)
                    </button>
                </div>
                <div style="font-size: 12px; color: #71717a; margin-top: 6px; line-height:1.45;">
                    {#if !crispEmbedCompiledIn}
                        {i18n.t.settings.index.engine_hint_no_crispembed}
                    {:else if indexEmbedderBackend === 'onnx'}
                        {i18n.t.settings.index.engine_hint_onnx}
                    {:else}
                        {i18n.t.settings.index.engine_hint_gguf}
                    {/if}
                </div>
                {#if crispEmbedCompiledIn && embedderRegistry.length > 0}
                    <details style="margin-top: 10px; font-size: 12px;">
                        <summary style="cursor: pointer; color: #71717a;">
                            {i18n.t.settings.index.crispembed_registry_summary.replace('{count}', String(embedderRegistry.length))}
                        </summary>
                        <div style="font-size: 11px; color: #71717a; margin: 6px 0 4px; line-height:1.45;">
                            {i18n.t.settings.index.crispembed_registry_hint}
                        </div>
                        {#if indexEmbedderModelName}
                            <div style="font-size: 11px; background: #ede9fe; color: #6d28d9; padding: 4px 8px; border-radius: 4px; margin-bottom: 6px; display:flex; align-items:center; gap:6px;">
                                <span>Active override: <strong>{indexEmbedderModelName}</strong></span>
                                <button onclick={() => { indexEmbedderModelName = ''; }} style="margin-left:auto; font-size:10px; color:#6d28d9; background:none; border:1px solid #c4b5fd; border-radius:3px; padding:1px 6px; cursor:pointer;">Clear</button>
                            </div>
                        {/if}
                        {#if registryDownloadMsg}
                            <div style="font-size:11px; color: #6b7280; margin-bottom:4px;">{registryDownloadMsg}</div>
                        {/if}
                        <ul style="margin: 0; padding: 0; list-style: none; max-height: 260px; overflow-y: auto; border: 1px solid #e5e7eb; border-radius: 6px;">
                            {#each embedderRegistry as entry}
                                <li style="padding: 6px 10px; border-bottom: 1px solid #f1f5f9; display:flex; gap:8px; align-items:flex-start;">
                                    <div style="flex:1; min-width:0;">
                                        <div style="font-weight: 600; font-family: ui-monospace, monospace; color: #1f2937;">{entry.name}</div>
                                        {#if entry.desc}
                                            <div style="color: #4b5563; margin-top: 2px; font-size:11px;">{entry.desc}</div>
                                        {/if}
                                        <div style="color: #9ca3af; margin-top: 2px; font-family: ui-monospace, monospace; font-size: 10px;">
                                            {entry.filename}{entry.size ? ` — ${entry.size}` : ''}
                                        </div>
                                    </div>
                                    <div style="display:flex; flex-direction:column; gap:3px; flex-shrink:0;">
                                        {#if entry.cached || entry.name === indexEmbedderModelName}
                                            <button
                                                onclick={() => selectRegistryModel(entry.name)}
                                                style="font-size:10px; padding:2px 8px; border-radius:3px; border:1px solid {entry.name === indexEmbedderModelName ? '#6d28d9' : '#a78bfa'}; background:{entry.name === indexEmbedderModelName ? '#ede9fe' : 'white'}; color:{entry.name === indexEmbedderModelName ? '#6d28d9' : '#7c3aed'}; cursor:pointer;">
                                                {entry.name === indexEmbedderModelName ? 'Active' : 'Select'}
                                            </button>
                                        {:else}
                                            <button
                                                disabled={registryDownloadBusy === entry.name}
                                                onclick={() => downloadRegistryModel(entry.name)}
                                                style="font-size:10px; padding:2px 8px; border-radius:3px; border:1px solid #d1d5db; background:white; color:#374151; cursor:pointer; display:flex; align-items:center; gap:3px;">
                                                {#if registryDownloadBusy === entry.name}
                                                    <Loader2 size={10} style="animation: spin 1s linear infinite;" />
                                                {:else}
                                                    <Download size={10} />
                                                {/if}
                                                Download
                                            </button>
                                        {/if}
                                    </div>
                                </li>
                            {/each}
                        </ul>
                    </details>
                {/if}
            </div>

            <!-- Embedder model: filtered by chosen engine + NC-license-aware -->
            <div class="section-card">
                <label for="index-model-select"><Cpu size={16} /> {i18n.t.settings.index.embedder_model}</label>
                <select id="index-model-select" value={indexEmbedderModel} onchange={handleEmbedderChange} class="styled-select">
                    <option value="bge_m3" disabled={!isModelAvailable('bge_m3')}>
                        {i18n.t.settings.index.model_bge_m3}{ncLabelSuffix('bge_m3')}
                    </option>
                    <optgroup label="PIXIE-Rune-v1.0 (cstr/PIXIE-Rune-v1.0-ONNX)">
                        <option value="pixie_q" disabled={!isModelAvailable('pixie_q')}>{i18n.t.settings.index.model_pixie_q}{ncLabelSuffix('pixie_q')}</option>
                        <option value="pixie_int4" disabled={!isModelAvailable('pixie_int4')}>{i18n.t.settings.index.model_pixie_int4}{ncLabelSuffix('pixie_int4')}</option>
                        <option value="pixie_int4_full" disabled={!isModelAvailable('pixie_int4_full')}>{i18n.t.settings.index.model_pixie_int4_full}{ncLabelSuffix('pixie_int4_full')}</option>
                        <option value="pixie" disabled={!isModelAvailable('pixie')}>{i18n.t.settings.index.model_pixie}{ncLabelSuffix('pixie')}</option>
                    </optgroup>
                    <optgroup label="Snowflake Arctic Embed L v2.0">
                        <option value="snowflake_l" disabled={!isModelAvailable('snowflake_l')}>{i18n.t.settings.index.model_snowflake_l}{ncLabelSuffix('snowflake_l')}</option>
                        <option value="snowflake_l_int8" disabled={!isModelAvailable('snowflake_l_int8')}>{i18n.t.settings.index.model_snowflake_l_int8}{ncLabelSuffix('snowflake_l_int8')}</option>
                        <option value="snowflake_l_fp16" disabled={!isModelAvailable('snowflake_l_fp16')}>{i18n.t.settings.index.model_snowflake_l_fp16}{ncLabelSuffix('snowflake_l_fp16')}</option>
                        <option value="snowflake_l_q4" disabled={!isModelAvailable('snowflake_l_q4')}>{i18n.t.settings.index.model_snowflake_l_q4}{ncLabelSuffix('snowflake_l_q4')}</option>
                        <option value="snowflake_l_q4f16" disabled={!isModelAvailable('snowflake_l_q4f16')}>{i18n.t.settings.index.model_snowflake_l_q4f16}{ncLabelSuffix('snowflake_l_q4f16')}</option>
                        <option value="snowflake_l_o4" disabled={!isModelAvailable('snowflake_l_o4')}>{i18n.t.settings.index.model_snowflake_l_o4}{ncLabelSuffix('snowflake_l_o4')}</option>
                        <option value="snowflake_l_fp32" disabled={!isModelAvailable('snowflake_l_fp32')}>{i18n.t.settings.index.model_snowflake_l_fp32}{ncLabelSuffix('snowflake_l_fp32')}</option>
                    </optgroup>
                    <option value="octen" disabled={!isModelAvailable('octen')}>{i18n.t.settings.index.model_octen}{ncLabelSuffix('octen')}</option>
                    <option value="jina_nano" disabled={!isModelAvailable('jina_nano')}>{i18n.t.settings.index.model_jina_nano}{ncLabelSuffix('jina_nano')}</option>
                    <option value="multilingual_mini_lm" disabled={!isModelAvailable('multilingual_mini_lm')}>{i18n.t.settings.index.model_mini_lm}{ncLabelSuffix('multilingual_mini_lm')}</option>
                    <optgroup label="Small / fast (recommended for fast first run)">
                        <option value="minilm_l6_v2" disabled={!isModelAvailable('minilm_l6_v2')}>{i18n.t.settings.index.model_minilm_l6_v2}{ncLabelSuffix('minilm_l6_v2')}</option>
                        <option value="bge_small_en_v15" disabled={!isModelAvailable('bge_small_en_v15')}>{i18n.t.settings.index.model_bge_small_en_v15}{ncLabelSuffix('bge_small_en_v15')}</option>
                        <option value="multilingual_e5_small" disabled={!isModelAvailable('multilingual_e5_small')}>{i18n.t.settings.index.model_multilingual_e5_small}{ncLabelSuffix('multilingual_e5_small')}</option>
                    </optgroup>
                    <optgroup label="Mid-size (768d, balanced)">
                        <option value="bge_base_en_v15" disabled={!isModelAvailable('bge_base_en_v15')}>{i18n.t.settings.index.model_bge_base_en_v15}{ncLabelSuffix('bge_base_en_v15')}</option>
                        <option value="multilingual_e5_base" disabled={!isModelAvailable('multilingual_e5_base')}>{i18n.t.settings.index.model_multilingual_e5_base}{ncLabelSuffix('multilingual_e5_base')}</option>
                        <option value="nomic_embed_v15" disabled={!isModelAvailable('nomic_embed_v15')}>{i18n.t.settings.index.model_nomic_embed_v15}{ncLabelSuffix('nomic_embed_v15')}</option>
                        <option value="gte_base_en_v15" disabled={!isModelAvailable('gte_base_en_v15')}>{i18n.t.settings.index.model_gte_base_en_v15}{ncLabelSuffix('gte_base_en_v15')}</option>
                        <option value="embedding_gemma_300m" disabled={!isModelAvailable('embedding_gemma_300m')}>{i18n.t.settings.index.model_embedding_gemma_300m}{ncLabelSuffix('embedding_gemma_300m')}</option>
                    </optgroup>
                    <optgroup label="Large (1024d, top quality)">
                        <option value="bge_large_en_v15" disabled={!isModelAvailable('bge_large_en_v15')}>{i18n.t.settings.index.model_bge_large_en_v15}{ncLabelSuffix('bge_large_en_v15')}</option>
                        <option value="multilingual_e5_large" disabled={!isModelAvailable('multilingual_e5_large')}>{i18n.t.settings.index.model_multilingual_e5_large}{ncLabelSuffix('multilingual_e5_large')}</option>
                        <option value="mxbai_large_v1" disabled={!isModelAvailable('mxbai_large_v1')}>{i18n.t.settings.index.model_mxbai_large_v1}{ncLabelSuffix('mxbai_large_v1')}</option>
                        <option value="gte_large_en_v15" disabled={!isModelAvailable('gte_large_en_v15')}>{i18n.t.settings.index.model_gte_large_en_v15}{ncLabelSuffix('gte_large_en_v15')}</option>
                    </optgroup>
                </select>
                {#if nonCommercialDeclined.size > 0}
                    <button class="action-btn small" style="margin-top:6px;" onclick={resetNonCommercialDeclines}>
                        Re-allow declined non-commercial models ({nonCommercialDeclined.size})
                    </button>
                {/if}

                {#if supportsGguf(indexEmbedderModel) && indexEmbedderBackend === 'gguf'}
                    <label for="index-matryoshka-dim" style="margin-top:10px;">
                        {i18n.t.settings.index.matryoshka_dim}
                    </label>
                    <select id="index-matryoshka-dim" bind:value={indexMatryoshkaDim} class="styled-select">
                        <option value={0}>{i18n.t.settings.index.matryoshka_default}</option>
                        <option value={128}>128</option>
                        <option value={256}>256</option>
                        <option value={384}>384</option>
                        <option value={512}>512</option>
                        <option value={768}>768</option>
                    </select>
                    <p class="hint">{i18n.t.settings.index.matryoshka_hint}</p>
                {/if}

                <!-- P13.5 follow-up: index-time translation target.
                     When set, every extracted document goes through
                     CLD3 text-LID + m2m100 MT at ingest, with the
                     result stored in the LanceDB text_translated
                     column.  Search-side query rewrite (set
                     SearchFilters.prefer_translated_lang) surfaces
                     these translations as the snippet for matching
                     hits.  Disabled by default — translation is
                     CPU-intensive and the LanceDB column stays NULL
                     for users not opting in. -->
                <label for="index-translate-to" style="margin-top:10px;">
                    {i18n.t.settings.index.translate_to ?? 'Index-time translation'}
                </label>
                <select id="index-translate-to" bind:value={indexTranslateTo} class="styled-select">
                    <option value="none">{i18n.t.settings.index.translate_to_none ?? 'Off (no translation at index time)'}</option>
                    <option value="en">English (en) — m2m100</option>
                    <option value="de">Deutsch (de) — m2m100</option>
                    <option value="fr">Français (fr) — m2m100</option>
                    <option value="es">Español (es) — m2m100</option>
                    <option value="it">Italiano (it) — m2m100</option>
                    <option value="ja">日本語 (ja) — m2m100</option>
                    <option value="zh">中文 (zh) — m2m100</option>
                </select>
                <p class="hint">
                    {i18n.t.settings.index.translate_to_hint ??
                     'Each extracted document is translated to the chosen language at ingest time via m2m100 (100-language any-to-any model). Source language is auto-detected via CLD3. The translated text lands in the text_translated LanceDB column; search hits surface it when the prefer_translated_lang filter is set.'}
                </p>
            </div>

            <!-- P13.6 Steps 5+6: Multimodal processing sub-panel.  Audio
                 + video extraction toggle + ASR backend selection + LID
                 method.  Image OCR settings already live in their own
                 panel (settings.ocr.*) — we cross-link from here so the
                 user has a single mental model of "multimodal
                 processing" even though the two paths sit in different
                 IndexConfig fields. -->
            <div class="section-card">
                <div style="display:flex; align-items:center; gap:6px; font-weight: 500;">
                    {i18n.t.settings.index.multimodal_section ?? 'Multimodal processing'}
                </div>
                <p class="hint" style="margin-bottom:10px;">
                    {i18n.t.settings.index.multimodal_section_hint ??
                     'Audio + video files in the index are transcribed via CrispASR (whisper / parakeet / qwen3-omni). Image OCR is configured in the OCR panel above; image semantic search via CrispEmbed is planned.'}
                </p>

                <label class="cb-row" style="display:flex; align-items:center; gap:8px; margin-top:4px;">
                    <input type="checkbox" bind:checked={indexAudioExtractionEnabled} />
                    <span>{i18n.t.settings.index.audio_extraction ?? 'Audio + video extraction'}</span>
                </label>
                <p class="hint">
                    {i18n.t.settings.index.audio_extraction_hint ??
                     'When off, audio/video files (mp3/wav/mp4/…) are indexed with filesystem metadata only — no transcript. Symphonia decode + CrispASR transcription is the default index-time path otherwise.'}
                </p>

                <label for="index-audio-asr-backend" style="margin-top:10px;">
                    {i18n.t.settings.index.audio_asr_backend ?? 'ASR backend'}
                </label>
                <select id="index-audio-asr-backend" bind:value={indexAudioAsrBackend} class="styled-select" disabled={!indexAudioExtractionEnabled}>
                    <option value="whisper">whisper — multilingual, 99 langs (base, ~150 MB)</option>
                    <option value="whisper-large-v3">whisper-large-v3 — multilingual, higher accuracy (~3 GB)</option>
                    <option value="whisper-small">whisper-small — multilingual, balanced (~500 MB)</option>
                    <option value="whisper-medium">whisper-medium — multilingual (~1.5 GB)</option>
                    <option value="parakeet">parakeet — NVIDIA Parakeet TDT (English-only, fast)</option>
                    <option value="qwen3-omni">qwen3-omni — Qwen3 omnimodal ASR</option>
                </select>
                <p class="hint">
                    {i18n.t.settings.index.audio_asr_backend_hint ??
                     'Pick which CrispASR backend transcribes audio/video files at index time. whisper (default) is the multilingual baseline; larger variants trade disk + RAM for accuracy. The model auto-downloads on first use through the CrispASR registry. Changing the backend requires an app restart to take effect.'}
                </p>

                <label for="index-audio-lid-method" style="margin-top:10px;">
                    {i18n.t.settings.index.audio_lid_method ?? 'LID method'}
                </label>
                <select id="index-audio-lid-method" bind:value={indexAudioLidMethod} class="styled-select" disabled={!indexAudioExtractionEnabled}>
                    <option value="whisper">whisper — reuses ASR model (auto-resolves a whisper-base for non-whisper backends)</option>
                    <option value="silero" disabled>silero — needs explicit --lid-model (not surfaced yet)</option>
                    <option value="ecapa" disabled>ecapa — needs explicit --lid-model (not surfaced yet)</option>
                    <option value="firered" disabled>firered — needs explicit --lid-model (not surfaced yet)</option>
                </select>
                <p class="hint">
                    {i18n.t.settings.index.audio_lid_method_hint ??
                     'Which method detects the source language of an audio file. whisper reuses the loaded ASR model and auto-resolves a whisper-base ggml when the ASR backend is non-whisper-family. Silero / Ecapa / Firered are placeholders for future CrispASR registry entries.'}
                </p>

                <label for="index-audio-level" style="margin-top:10px;">
                    {i18n.t.settings.index.ingest_audio_level ?? 'Ingest level'}
                </label>
                <select id="index-audio-level" bind:value={indexIngestAudioLevel} class="styled-select" disabled={!indexAudioExtractionEnabled}>
                    <option value="l3">L3 — full pipeline (probe + transcribe, default)</option>
                    <option value="l2">L2 — probe only (duration + codec, no transcript)</option>
                    <option value="l1">L1 — filesystem only (path + size, no probe, no transcript)</option>
                </select>
                <p class="hint">
                    {i18n.t.settings.index.ingest_audio_level_hint ??
                     'How deeply bg_ingest processes audio/video files. L1 is fastest (just the filename in the index); L2 adds a sub-millisecond symphonia probe for duration / codec / bitrate; L3 (default) runs the full transcription too. Promote individual L1/L2 rows to L3 later via the "Transcribe" action in search results.'}
                </p>

                <!-- P13.7 Step 1+3 — image-side master switch + ingest level. -->
                <label class="cb-row" style="display:flex; align-items:center; gap:8px; margin-top:14px;">
                    <input type="checkbox" bind:checked={indexImageExtractionEnabled} />
                    <span>{i18n.t.settings.index.image_extraction ?? 'Image extraction (OCR + EXIF)'}</span>
                </label>
                <p class="hint">
                    {i18n.t.settings.index.image_extraction_hint ??
                     'When off, image files (jpg/png/heic/…) are indexed with filesystem metadata only — no OCR, no EXIF. OCR is still gated by the OCR-tier toggle above; this is the master switch that overrides everything.'}
                </p>

                <label for="index-image-level" style="margin-top:10px;">
                    {i18n.t.settings.index.ingest_image_level ?? 'Image ingest level'}
                </label>
                <select id="index-image-level" bind:value={indexIngestImageLevel} class="styled-select" disabled={!indexImageExtractionEnabled}>
                    <option value="l3">L3 — full pipeline (EXIF + OCR, default)</option>
                    <option value="l2">L2 — EXIF probe only (camera / lens / iso / taken_at, no OCR)</option>
                    <option value="l1">L1 — filesystem only (path + size, no EXIF, no OCR)</option>
                </select>
                <p class="hint">
                    {i18n.t.settings.index.ingest_image_level_hint ??
                     'How deeply bg_ingest processes images.  L1 is fastest; L2 adds the kamadak-exif probe (camera make / model / lens / ISO / taken_at_unix); L3 (default) runs OCR too.  Promote L1/L2 rows to L3 via the "Re-OCR" action in search results.'}
                </p>

                <!-- P13.7 Step 4 — CrispLens image push.  Default off
                     because pushing every image upstream is a
                     privacy-sensitive action; users opt in. -->
                <label class="cb-row" style="display:flex; align-items:center; gap:8px; margin-top:14px;">
                    <input type="checkbox" bind:checked={indexCrispLensImageEnrichment} />
                    <span>{i18n.t.settings.index.crisplens_image_enrichment ?? 'CrispLens image push (Tier 2 server)'}</span>
                </label>
                <p class="hint">
                    {i18n.t.settings.index.crisplens_image_enrichment_hint ??
                     'When on, each indexed image is also pushed to the configured CrispLens server for face detection + clustering. Requires Tier 2 to be configured (URL + login). Default off — pushing every image upstream is a privacy-sensitive action that you should turn on intentionally.'}
                </p>
            </div>

            <!-- P13.7 Step 5 — cloud-backup HTTP sync.  Talks to a
                 FastAPI module on a VPS that owns the same
                 catalog DB vps_worker.py manages.  URL +
                 toggles live in IndexConfig (JSON-safe); API key
                 goes to the OS keychain via sync_cb_set_token. -->
            <div class="section-card">
                <label><Server size={16} /> {i18n.t.settings.index.cloud_backup_section ?? 'Cloud-backup sync'}</label>
                <p class="hint">{i18n.t.settings.index.cloud_backup_section_hint ?? ''}</p>

                <label for="index-cb-url" style="margin-top:10px;">
                    {i18n.t.settings.index.cloud_backup_url ?? 'Cloud-backup URL'}
                </label>
                <input id="index-cb-url" type="text" bind:value={indexCloudBackupUrl}
                       placeholder={i18n.t.settings.index.cloud_backup_url_placeholder ?? 'https://your-vps.example.com/cb'} />

                <label for="index-cb-key" style="margin-top:10px;">
                    {i18n.t.settings.index.cloud_backup_api_key ?? 'API key'}
                </label>
                <div style="display:flex; gap:8px; align-items:center;">
                    <input id="index-cb-key" type="password" autocomplete="off"
                           bind:value={indexCloudBackupApiKeyInput}
                           placeholder={i18n.t.settings.index.cloud_backup_api_key_placeholder ?? 'cbk_...'}
                           style="flex:1;" />
                    <button type="button" class="btn" onclick={saveCloudBackupToken}
                            disabled={!indexCloudBackupUrl.trim()}>
                        Save
                    </button>
                </div>
                {#if indexCloudBackupTokenSavedMsg}
                    <p class="hint" style="color: var(--color-success, #2a8);">{indexCloudBackupTokenSavedMsg}</p>
                {/if}
                <p class="hint">
                    {i18n.t.settings.index.cloud_backup_api_key_hint ?? ''}
                </p>

                <label class="cb-row" style="display:flex; align-items:center; gap:8px; margin-top:14px;">
                    <input type="checkbox" bind:checked={indexCloudBackupPushManifests} />
                    <span>{i18n.t.settings.index.cloud_backup_push_manifests ?? 'Push manifests'}</span>
                </label>
                <p class="hint">{i18n.t.settings.index.cloud_backup_push_manifests_hint ?? ''}</p>

                <label class="cb-row" style="display:flex; align-items:center; gap:8px; margin-top:8px;">
                    <input type="checkbox" bind:checked={indexCloudBackupPushEmbeddings} />
                    <span>{i18n.t.settings.index.cloud_backup_push_embeddings ?? 'Push embeddings'}</span>
                </label>
                <p class="hint">{i18n.t.settings.index.cloud_backup_push_embeddings_hint ?? ''}</p>

                <label class="cb-row" style="display:flex; align-items:center; gap:8px; margin-top:8px;">
                    <input type="checkbox" bind:checked={indexCloudBackupPullManifests} />
                    <span>{i18n.t.settings.index.cloud_backup_pull_manifests ?? 'Pull manifests'}</span>
                </label>
                <p class="hint">{i18n.t.settings.index.cloud_backup_pull_manifests_hint ?? ''}</p>

                <!-- Stage I — opt into body text on pull (off by
                     default = tiered-cache mode). -->
                <label class="cb-row" style="display:flex; align-items:center; gap:8px; margin-top:8px;">
                    <input type="checkbox" bind:checked={indexCloudBackupPullFullText} />
                    <span>{i18n.t.settings.index.cloud_backup_pull_full_text ?? 'Pull body text alongside metadata'}</span>
                </label>
                <p class="hint">{i18n.t.settings.index.cloud_backup_pull_full_text_hint ?? ''}</p>

                <!-- Stage U — thin-client switch. -->
                <label style="display:flex; align-items:center; gap:8px; margin-top:10px; cursor:pointer;">
                    <input type="checkbox" bind:checked={indexLocalExtractionEnabled} disabled={indexSkeletonOnly} />
                    <span>{i18n.t.settings.index.local_extraction_label}</span>
                </label>
                <p class="hint">{i18n.t.settings.index.local_extraction_hint}</p>

                <!-- Stage W — skeleton-only switch. -->
                <label style="display:flex; align-items:center; gap:8px; margin-top:10px; cursor:pointer;">
                    <input type="checkbox" bind:checked={indexSkeletonOnly} />
                    <span>{i18n.t.settings.index.skeleton_only_label}</span>
                </label>
                <p class="hint">{i18n.t.settings.index.skeleton_only_hint}</p>

                <!-- Stage P — local DB size cap. -->
                <div style="margin-top:14px;">
                    <label for="local-max-size" style="display:block; margin-bottom:4px;">
                        {i18n.t.settings.index.local_max_size ?? 'Local index size cap (GB)'}
                    </label>
                    <div style="display:flex; align-items:center; gap:10px;">
                        <input id="local-max-size" type="range" min="0" max="1000" step="1"
                               bind:value={indexLocalMaxSizeGb}
                               style="flex:1;" />
                        <span style="min-width:60px; text-align:right;">
                            {indexLocalMaxSizeGb > 0 ? `${indexLocalMaxSizeGb} GB` : 'Unlimited'}
                        </span>
                    </div>
                    <p class="hint">{i18n.t.settings.index.local_max_size_hint ?? 'When set, old rows are stripped and evicted hourly to stay within the cap. 0 = unlimited.'}</p>
                </div>

                <!-- Stage Q — shard backup to cloud drive. -->
                <div style="margin-top:14px; padding:10px; border:1px solid var(--color-border, #444); border-radius:6px;">
                    <strong>{i18n.t.settings.index.cloud_backup_shard_backup ?? 'Shard backup'}</strong>
                    <p class="hint" style="margin-top:4px;">
                        {i18n.t.settings.index.cloud_backup_shard_backup_hint ?? 'Back up VPS shards to a cloud drive. Only changed shards are re-uploaded (incremental).'}
                    </p>
                    <label for="backup-drive-id" style="margin-top:8px; display:block;">
                        {i18n.t.settings.index.cloud_backup_backup_drive ?? 'Backup drive ID'}
                    </label>
                    <input id="backup-drive-id" type="text"
                           bind:value={backupDriveId}
                           placeholder={i18n.t.settings.index.backup_drive_placeholder} />
                    <div style="display:flex; gap:12px; margin-top:8px; align-items:end;">
                        <div>
                            <label for="backup-keep-daily">{i18n.t.settings.index.cloud_backup_keep_daily ?? 'Keep daily backups'}</label>
                            <input id="backup-keep-daily" type="number" min="0" max="365"
                                   bind:value={backupKeepDaily}
                                   style="width:80px;" />
                        </div>
                        <button type="button" class="btn"
                                onclick={backupShardsNow}
                                disabled={backupNowBusy || !indexCloudBackupUrl.trim() || !backupDriveId.trim()}>
                            {backupNowBusy ? 'Backing up…' : 'Backup now'}
                        </button>
                    </div>
                    {#if backupNowMsg}
                        <p class="hint" style="margin-top:6px;">{backupNowMsg}</p>
                    {/if}
                </div>

                <!-- Stage N — volume-proportional auto-partition.
                     Pure operator surface; doesn't persist into
                     IndexConfig (the partition_map.db is the
                     persistent artefact). -->
                <div style="margin-top:14px; padding:10px; border:1px solid var(--color-border, #444); border-radius:6px;">
                    <strong>{i18n.t.settings.index.cloud_backup_partition ?? 'Auto-partition shards'}</strong>
                    <p class="hint" style="margin-top:4px;">
                        {i18n.t.settings.index.cloud_backup_partition_hint ?? ''}
                    </p>
                    <label for="cb-part-root" style="margin-top:8px;">Watched root</label>
                    <input id="cb-part-root" type="text"
                           bind:value={cloudBackupPartitionRoot}
                           placeholder="/path/to/watched/folder" />
                    <div style="display:flex; gap:12px; margin-top:8px; align-items:end;">
                        <div style="flex:1;">
                            <label for="cb-part-max">{i18n.t.settings.index.cloud_backup_partition_max ?? 'Max shards'}</label>
                            <input id="cb-part-max" type="number" min="1" max="256"
                                   bind:value={cloudBackupPartitionMax} />
                        </div>
                        <div style="flex:1;">
                            <label for="cb-part-depth">{i18n.t.settings.index.cloud_backup_partition_depth ?? 'Group depth'}</label>
                            <input id="cb-part-depth" type="number" min="1" max="6"
                                   bind:value={cloudBackupPartitionDepth} />
                        </div>
                        <button type="button" class="btn" onclick={runCloudBackupPartition}
                                disabled={!cloudBackupPartitionRoot.trim()}>
                            {i18n.t.settings.index.cloud_backup_partition_run ?? 'Re-partition now'}
                        </button>
                    </div>
                    {#if cloudBackupPartitionStatus}
                        <p class="hint" style="margin-top:6px;">{cloudBackupPartitionStatus}</p>
                    {/if}
                </div>

                <!-- Stage O — "Sync now" button: drain outbox then pull deltas. -->
                <div style="display:flex; gap:10px; align-items:center; margin-top:14px;">
                    <button type="button" class="btn"
                            onclick={syncNow}
                            disabled={syncNowBusy || !indexCloudBackupUrl.trim()}>
                        {syncNowBusy ? 'Syncing…' : 'Sync now'}
                    </button>
                    {#if syncNowMsg}
                        <span class="hint" style="margin:0;">{syncNowMsg}</span>
                    {/if}
                </div>

                <!-- Stage R — import from controller.py manifest SQLite. -->
                <div style="margin-top:14px; padding:10px; border:1px solid var(--color-border, #444); border-radius:6px;">
                    <strong>{i18n.t.settings.index.import_manifest_title}</strong>
                    <p class="hint" style="margin-top:4px;">{i18n.t.settings.index.import_manifest_hint}</p>
                    <label for="cb-import-path" style="margin-top:8px;">{i18n.t.settings.index.import_manifest_path_label}</label>
                    <input id="cb-import-path" type="text"
                           bind:value={importManifestPath}
                           placeholder="/path/to/index_manifest.db" />
                    <label for="cb-import-owner" style="margin-top:6px;">{i18n.t.settings.index.owner_id_optional}</label>
                    <input id="cb-import-owner" type="text"
                           bind:value={importManifestOwner}
                           placeholder={i18n.t.settings.index.owner_id_placeholder} />
                    <div style="margin-top:8px;">
                        <button type="button" class="btn"
                                onclick={importFromManifestDb}
                                disabled={importManifestBusy || !indexCloudBackupUrl.trim() || !importManifestPath.trim()}>
                            {importManifestBusy ? i18n.t.settings.index.importing : i18n.t.settings.index.import_now}
                        </button>
                    </div>
                    {#if importManifestMsg}
                        <p class="hint" style="margin-top:6px;">{importManifestMsg}</p>
                    {/if}
                </div>

                <!-- Compact status line — version + watermarks.  Refreshes
                     on save and on initial mount via refreshCloudBackupStatus. -->
                {#if indexCloudBackupStatus}
                    <p class="hint" style="margin-top:10px;">
                        {#if indexCloudBackupStatus.health?.ok}
                            ✅ connected to cloud-backup {indexCloudBackupStatus.health.version}
                            {#if indexCloudBackupStatus.health.shared_catalog}(shared){:else}(per-owner){/if}
                            · last push: {indexCloudBackupStatus.last_manifest_push_ts ?? '—'}
                            · last pull: {indexCloudBackupStatus.last_manifest_pull_ts ?? '—'}
                        {:else if indexCloudBackupStatus.configured && !indexCloudBackupStatus.token_present}
                            ⚠️ URL configured but no API key in keychain — paste one above and Save.
                        {:else if indexCloudBackupStatus.error}
                            ❌ {indexCloudBackupStatus.error}
                        {/if}
                    </p>
                {/if}

                <!-- Stage T — Admin key management (collapsible sub-section) -->
                <details class="cb-admin-panel">
                    <summary style="cursor:pointer; font-weight:600; margin-top:14px;">
                        {i18n.t.settings.index.admin_panel_title}
                    </summary>
                    <p class="hint">{i18n.t.settings.index.admin_panel_hint}</p>
                    <label class="cb-admin-label">{i18n.t.settings.index.admin_token_label}</label>
                    <input type="password" class="cb-admin-input"
                           bind:value={cbAdminToken}
                           placeholder="cbadm_…"
                           autocomplete="off" />

                    <div class="cb-admin-row">
                        <div class="cb-admin-group">
                            <strong>{i18n.t.settings.index.admin_mint_title}</strong>
                            <label class="cb-admin-label">{i18n.t.settings.index.admin_key_name_label}</label>
                            <input type="text" class="cb-admin-input"
                                   bind:value={cbAdminMintName}
                                   placeholder="my-laptop" />
                            <label class="cb-admin-label">{i18n.t.settings.index.owner_id_optional}</label>
                            <input type="text" class="cb-admin-input"
                                   bind:value={cbAdminMintOwner}
                                   placeholder={i18n.t.settings.index.admin_owner_placeholder} />
                            <button type="button" class="btn"
                                    onclick={cbAdminMint}
                                    disabled={cbAdminBusy || !cbAdminToken.trim() || !cbAdminMintName.trim()}>
                                {i18n.t.settings.index.admin_mint_btn}
                            </button>
                            {#if cbAdminMintedKey}
                                <div class="cb-admin-minted-key">
                                    <span class="hint">{i18n.t.settings.index.admin_minted_key_hint}</span>
                                    <code>{cbAdminMintedKey}</code>
                                </div>
                            {/if}
                        </div>

                        <div class="cb-admin-group">
                            <strong>{i18n.t.settings.index.admin_revoke_title}</strong>
                            <label class="cb-admin-label">{i18n.t.settings.index.admin_revoke_name_label}</label>
                            <input type="text" class="cb-admin-input"
                                   bind:value={cbAdminRevokeName}
                                   placeholder="my-laptop" />
                            <button type="button" class="btn btn-danger"
                                    onclick={cbAdminRevoke}
                                    disabled={cbAdminBusy || !cbAdminToken.trim() || !cbAdminRevokeName.trim()}>
                                {i18n.t.settings.index.admin_revoke_btn}
                            </button>
                        </div>
                    </div>

                    <button type="button" class="btn" style="margin-top:8px;"
                            onclick={cbAdminRefreshList}
                            disabled={cbAdminBusy || !cbAdminToken.trim()}>
                        {i18n.t.settings.index.admin_list_btn}
                    </button>

                    {#if cbAdminMsg}
                        <p class="hint" style="margin-top:6px;">{cbAdminMsg}</p>
                    {/if}

                    {#if cbAdminKeys.length > 0}
                        <table class="cb-admin-table">
                            <thead>
                                <tr>
                                    <th>Name</th>
                                    <th>Status</th>
                                    <th>Owner ID</th>
                                    <th>Created</th>
                                </tr>
                            </thead>
                            <tbody>
                                {#each cbAdminKeys as k}
                                    <tr class={k.revoked_at ? 'cb-admin-revoked' : ''}>
                                        <td>{k.name}</td>
                                        <td>{k.revoked_at ? 'revoked' : 'active'}</td>
                                        <td style="font-size:0.8em;">{k.owner_id ?? '—'}</td>
                                        <td style="font-size:0.8em;">{new Date(k.created_at).toLocaleDateString()}</td>
                                    </tr>
                                {/each}
                            </tbody>
                        </table>
                    {/if}
                </details>
            </div>

            <!-- Compute device — options depend on the selected engine.
                 ONNX (FastEmbed) supports Auto/CPU/Metal/CUDA via ORT
                 execution providers. GGUF (CrispEmbed) is bound at compile
                 time to ONE GPU backend (the `crispembed-{vulkan,cuda,metal}`
                 feature) plus CPU; we surface only what's actually linked. -->
            <div class="section-card">
                <label for="index-device-select"><Zap size={16} /> {i18n.t.settings.index.device}</label>
                {#if indexEmbedderBackend === 'onnx'}
                    <select id="index-device-select" bind:value={indexDevice} class="styled-select">
                        <option value="auto">{i18n.t.settings.index.device_auto}</option>
                        <option value="cpu">{i18n.t.settings.index.device_cpu}</option>
                        {#if isMacOS}<option value="metal">{i18n.t.settings.index.device_metal}</option>{/if}
                        <option value="cuda">{i18n.t.settings.index.device_cuda}</option>
                    </select>
                {:else}
                    <!-- GGUF: CPU is always available; the linked GPU
                         backend (or `null` if CrispEmbed itself is off) is
                         the second option. -->
                    <select id="index-device-select" bind:value={indexDevice} class="styled-select">
                        <option value="cpu">CPU</option>
                        {#if crispEmbedGpu === 'vulkan'}<option value="auto">Vulkan (linked)</option>{/if}
                        {#if crispEmbedGpu === 'cuda'}<option value="cuda">CUDA (linked)</option>{/if}
                        {#if crispEmbedGpu === 'metal'}<option value="metal">Metal (linked)</option>{/if}
                    </select>
                    <p class="hint" style="margin-top:6px;">
                        {#if crispEmbedGpu && crispEmbedGpu !== 'cpu'}
                            {@html i18n.t.crispembed_engine_built
                                .replace('{backend}', crispEmbedGpu)
                                .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
                                .replace(/`([^`]+)`/g, '<code>$1</code>')}
                        {:else}
                            {@html i18n.t.crispembed_engine_built_cpu
                                .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
                                .replace(/`([^`]+)`/g, '<code>$1</code>')}
                        {/if}
                    </p>
                {/if}
            </div>

            <!-- Reranker (cross-encoder, GGUF-only via CrispEmbed) -->
            <div class="section-card">
                <label for="index-reranker-select"><Cpu size={16} /> {i18n.t.settings.index.reranker_model}</label>
                <select id="index-reranker-select" bind:value={indexRerankerModel} onchange={(e) => handleRerankerChange(e, 'main')} class="styled-select">
                    <option value="">{i18n.t.settings.index.reranker_off}</option>
                    <option value="bge_v2_m3">{i18n.t.settings.index.reranker_bge_v2_m3}</option>
                    <option value="bge_base">{i18n.t.settings.index.reranker_bge_base}</option>
                    <option value="jina_v2_multi">{i18n.t.settings.index.reranker_jina_v2_multi}</option>
                </select>
                {#if indexRerankerModel}
                    <label for="index-reranker-topn" style="margin-top:10px;">
                        {i18n.t.settings.index.reranker_top_n}
                    </label>
                    <input id="index-reranker-topn" type="number" min="5" max="200" step="5"
                        bind:value={indexRerankerTopN} />
                    <p class="hint">{i18n.t.settings.index.reranker_hint}</p>
                {:else}
                    <!-- Bi-encoder fallback (P13.5 follow-up): only visible
                         when no dedicated cross-encoder is selected.  Reuses
                         the loaded dense embedder for cosine-similarity
                         reranking — zero extra disk / RAM. -->
                    <label for="index-bi-encoder-rerank" style="margin-top:10px; display:flex; align-items:center; gap:8px; cursor:pointer;">
                        <input id="index-bi-encoder-rerank" type="checkbox"
                            bind:checked={indexUseEmbedderAsReranker} />
                        <span>{i18n.t.settings.index.use_embedder_as_reranker ?? 'Use loaded embedder as bi-encoder reranker'}</span>
                    </label>
                    {#if indexUseEmbedderAsReranker}
                        <label for="index-bi-encoder-topn" style="margin-top:6px;">
                            {i18n.t.settings.index.reranker_top_n}
                        </label>
                        <input id="index-bi-encoder-topn" type="number" min="5" max="200" step="5"
                            bind:value={indexRerankerTopN} />
                    {/if}
                    <p class="hint">
                        {i18n.t.settings.index.use_embedder_as_reranker_hint ??
                         'Re-scores top-N hybrid hits by cosine similarity against the query, using the dense embedder you already loaded. Faster than a cross-encoder, less accurate per pair — good middle ground when you have not installed a dedicated reranker GGUF.'}
                    </p>
                {/if}
            </div>

            <!-- Stage Z — Multilingual reranker (CJK/Arabic/Cyrillic auto-routing) -->
            <div class="section-card">
                <label for="index-reranker-multi-select">
                    <Cpu size={16} /> {i18n.t.settings.index.reranker_multilingual_label}
                </label>
                <select id="index-reranker-multi-select" bind:value={indexRerankerModelMultilingual} onchange={(e) => handleRerankerChange(e, 'multi')} class="styled-select">
                    <option value="">{i18n.t.settings.index.reranker_off}</option>
                    <option value="bge_v2_m3">{i18n.t.settings.index.reranker_bge_v2_m3}</option>
                    <option value="bge_base">{i18n.t.settings.index.reranker_bge_base}</option>
                    <option value="jina_v2_multi">{i18n.t.settings.index.reranker_jina_v2_multi}</option>
                </select>
                <p class="hint" style="margin-top:6px;">{i18n.t.settings.index.reranker_multilingual_hint}</p>
            </div>

            <!-- P19 — GLiNER named-entity recognition (GGUF-only via CrispEmbed) -->
            <div class="section-card">
                <label for="index-ner-enabled" style="display:flex; align-items:center; gap:8px; cursor:pointer;">
                    <input id="index-ner-enabled" type="checkbox"
                        bind:checked={indexNerEnabled}
                        disabled={!crispEmbedCompiledIn} />
                    <Cpu size={16} /> <span>{i18n.t.settings.index.ner_enabled}</span>
                </label>
                {#if !crispEmbedCompiledIn}
                    <p class="hint">{i18n.t.settings.index.ner_requires_crispembed}</p>
                {:else}
                    <p class="hint">{i18n.t.settings.index.ner_enabled_hint}</p>
                {/if}
                {#if indexNerEnabled && crispEmbedCompiledIn}
                    <label for="index-ner-model" style="margin-top:10px;">{i18n.t.settings.index.ner_model}</label>
                    <select id="index-ner-model" bind:value={indexNerModel}
                        onchange={handleNerModelChange} class="styled-select">
                        <option value="sauerkraut-gliner-lfm">{i18n.t.settings.index.ner_model_sauerkraut}</option>
                        <option value="gliner-deberta">{i18n.t.settings.index.ner_model_deberta}</option>
                    </select>

                    <label for="index-ner-labels" style="margin-top:10px;">{i18n.t.settings.index.ner_labels}</label>
                    <textarea id="index-ner-labels" rows="2" bind:value={indexNerLabels}
                        placeholder={i18n.t.settings.index.ner_labels_placeholder}></textarea>
                    <p class="hint">{i18n.t.settings.index.ner_labels_hint}</p>

                    <label for="index-ner-threshold" style="margin-top:10px;">
                        {i18n.t.settings.index.ner_threshold} ({Number(indexNerThreshold).toFixed(2)})
                    </label>
                    <input id="index-ner-threshold" type="range" min="0" max="1" step="0.05"
                        bind:value={indexNerThreshold} style="width:100%;" />

                    <label for="index-ner-max-entities" style="margin-top:10px;">{i18n.t.settings.index.ner_max_entities}</label>
                    <input id="index-ner-max-entities" type="number" min="0" max="200" step="5"
                        bind:value={indexNerMaxEntities} />

                    <label for="index-ner-max-chars" style="margin-top:10px;">{i18n.t.settings.index.ner_max_chars}</label>
                    <input id="index-ner-max-chars" type="number" min="0" step="1000"
                        bind:value={indexNerMaxChars} />
                    <p class="hint">{i18n.t.settings.index.ner_hint}</p>
                {/if}
            </div>

            <!-- Model cache directory (shared by ONNX + GGUF + reranker downloads) -->
            <div class="section-card">
                <label for="index-model-cache-dir">
                    <FolderOpen size={16} /> {i18n.t.settings.index.model_cache_dir}
                </label>
                <div class="input-with-action">
                    <input id="index-model-cache-dir" type="text" bind:value={indexModelCacheDir}
                        placeholder={i18n.t.settings.index.model_cache_dir_placeholder} />
                    <button class="action-btn small" onclick={pickIndexModelCacheDir}>{i18n.t.settings.browse}</button>
                </div>
                <p class="hint">{i18n.t.settings.index.model_cache_dir_hint}</p>
            </div>

            <!-- Data directory -->
            {#if indexBackendType === 'local' || indexBackendType === 'hybrid'}
            <div class="section-card">
                <label for="index-data-dir"><FolderOpen size={16} /> {i18n.t.settings.index.data_dir}</label>
                <div class="input-with-action">
                    <input id="index-data-dir" type="text" bind:value={indexDataDir}
                        placeholder={i18n.t.settings.index.data_dir_placeholder} />
                    <button class="action-btn small" onclick={pickIndexDataDir}>{i18n.t.settings.browse}</button>
                </div>
                <p class="hint">{i18n.t.settings.index.data_dir_hint}</p>
            </div>
            {/if}

            <!-- Apply button + status -->
            <div class="section-card">
                <button class="save-btn" onclick={applyIndexConfig}
                    disabled={indexStatus === 'loading'}>
                    {#if indexStatus === 'loading'}<Loader2 size={16} class="spin" /> {i18n.t.settings.index.status_loading}
                    {:else}<Play size={16} /> {i18n.t.settings.index.apply}{/if}
                </button>
                <p class="hint">
                    Saves config and initializes the index.
                    {#if modelDownloadMb > 0}
                        First run downloads the embedder model (~{modelDownloadMb} MB).
                    {:else}
                        First run downloads the embedder model.
                    {/if}
                </p>

                {#if indexStatus === 'loading' && indexInitProgress}
                    <div class="init-progress-wrap">
                        <p class="init-progress-label"><Loader2 size={13} class="spin" /> {indexInitProgress}</p>
                        <!-- Step-level bar (5% / 40% / 70% / 90% / 100%). -->
                        <div class="init-progress-bar">
                            <div class="init-progress-fill" style="width:{indexInitPct}%"></div>
                        </div>
                        {#if indexDownloadProgress}
                            <!-- Bytes-level download bar (advances once per MB). -->
                            <p class="init-progress-label" style="margin-top:8px;">
                                {indexDownloadProgress.repo}/{indexDownloadProgress.file}
                                — {(indexDownloadProgress.bytes_done / 1024 / 1024).toFixed(1)} /
                                {(indexDownloadProgress.bytes_total / 1024 / 1024).toFixed(1)} MB
                                ({indexDownloadProgress.pct}%)
                            </p>
                            <div class="init-progress-bar">
                                <div class="init-progress-fill" style="width:{indexDownloadProgress.pct}%"></div>
                            </div>
                        {/if}
                        <p class="init-progress-note">{i18n.t.settings.index.init_progress_note}</p>
                    </div>
                {:else if indexStatus === 'ok'}
                    <p style="color:#22c55e; margin-top:8px; font-size:0.85rem;"><CheckCircle2 size={14} /> {i18n.t.settings.index.status_ok}</p>
                {:else if indexStatus === 'error'}
                    <p style="color:#ef4444; margin-top:8px; font-size:0.85rem;"><AlertCircle size={14} /> {indexStatusMsg}</p>
                {/if}
            </div>

            <!-- IVF-PQ build -->
            <div class="section-card">
                <label><Code size={16} /> {i18n.t.settings.index.build_ivf}</label>
                <p class="hint">{i18n.t.settings.index.build_ivf_hint}</p>
                <button class="action-btn secondary" onclick={buildIvfPq}
                    disabled={indexIvfRunning || indexStatus !== 'ok'}>
                    {#if indexIvfRunning}<Loader2 size={14} class="spin" />{/if}
                    {i18n.t.settings.index.build_ivf}
                </button>
            </div>

            <!-- Scalar index (parent_dir BTree) -->
            <div class="section-card">
                <label><Code size={16} /> {i18n.t.settings.index.build_scalar}</label>
                <p class="hint">{i18n.t.settings.index.build_scalar_hint}</p>
                <button class="action-btn secondary" onclick={buildScalarIndex}
                    disabled={indexScalarRunning || indexStatus !== 'ok'}>
                    {#if indexScalarRunning}<Loader2 size={14} class="spin" />{/if}
                    {i18n.t.settings.index.build_scalar}
                </button>
            </div>

        {:else if selectedProviderId === 'about'}
            <div class="header">
                <h1>{i18n.t.settings.about}</h1>
                <span class="version-pill" title="App version">CrispSorter v{appVersion}</span>
            </div>

            <div class="section-card">
                <label><Info size={16} /> {i18n.t.settings.legal.provider}</label>
                <div class="legal-text">
                    Christian Ströbele<br />
                    Nikolausstr. 5<br />
                    70190 Stuttgart<br />
                    Deutschland / Germany
                </div>
            </div>

            <div class="section-card">
                <label><Globe size={16} /> {i18n.t.settings.legal.contact}</label>
                <div class="legal-text">
                    Email: postmaster@crispstro.be<br />
                    Phone: +49 176 6421 8601
                </div>
            </div>

            <div class="section-card">
                <label><XCircle size={16} /> {i18n.t.settings.legal.disclaimer}</label>
                <div class="legal-text hint" style="color: #a1a1aa;">
                    {i18n.t.settings.legal.disclaimer_text}
                </div>
            </div>

            <div class="section-card">
                <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:8px;">
                    <h3>{i18n.t.settings.legal.licenses}</h3>
                    <div class="search-box small" style="background:#09090b; border:1px solid #27272a; border-radius:6px; padding:0 10px; width:240px; display:flex; align-items:center;">
                        <Search size={14} style="color:#71717a; margin-right:8px;" />
                        <input type="text" id="license-search-input" bind:value={licenseSearch} placeholder={i18n.t.settings.legal.search_licenses.replace('{count}', String(automatedLicenses.length))} style="border:none; background:transparent; color:white; padding:6px 0; font-size:0.75rem; width:100%; outline:none;" aria-label="Search licenses" />
                    </div>
                </div>
                {#if licensesGeneratedAt}
                    <div class="hint" style="color:#71717a; font-size:0.7rem; margin-bottom:12px;">
                        {i18n.t.settings.legal.generated_at
                            .replace('{timestamp}', new Date(licensesGeneratedAt).toLocaleString())
                            .replace('{count}', String(automatedLicenses.length))}
                    </div>
                {/if}
                <div class="license-list-scrollable">
                    {#if licensesLoading}
                        <div class="license-empty"><Loader2 size={16} class="loader-spin" /> Loading licenses…</div>
                    {:else if licensesError}
                        <div class="license-empty error">
                            <AlertCircle size={16} />
                            Could not load <code>licenses.json</code>: {licensesError}.
                            <br /><small>Run <code>npm run licenses:gen</code> to regenerate it.</small>
                        </div>
                    {:else if automatedLicenses.length === 0}
                        <div class="license-empty">
                            No license data found in <code>static/licenses.json</code>.
                            Run <code>npm run licenses:gen</code> to populate it.
                        </div>
                    {:else}
                        {#each filteredLicenses as lib}
                            <div class="license-item-auto">
                                <div class="license-item-header">
                                    <span class="lib-name"><strong>{lib.name}</strong> <small>v{lib.version}</small></span>
                                    <span class="lib-source-badge" class:rust={lib.source === 'Backend'}>{lib.source}</span>
                                </div>
                                <div class="license-item-meta">
                                    <span class="lib-type">{lib.license}</span>
                                    <span class="lib-author">{lib.author}</span>
                                    {#if lib.link}
                                        <button class="inline-link" onclick={() => opener.openUrl(lib.link)}>{i18n.t.settings.legal.source_link}</button>
                                    {/if}
                                </div>
                            </div>
                        {/each}
                    {/if}
                </div>
            </div>

        {:else}
            <!-- Provider Settings (Local/Remote) -->
            <div class="header">
                <h1>{selectedProvider.name}</h1>
                <div class="header-actions">
                    {#if saveIndicator}<span class="save-badge"><CheckCircle size={14} /> {i18n.t.settings.saved}</span>{/if}
                    <button class="action-btn small" class:active-btn={activeProviderId === selectedProvider.id} onclick={() => setActiveProvider(selectedProvider.id)}>
                        <Zap size={14} /> {activeProviderId === selectedProvider.id ? i18n.t.batch.selected_status : i18n.t.settings.set_active}
                    </button>
                    <button class="save-btn" onclick={handleSaveProvider}>{i18n.t.settings.save_all}</button>
                </div>
            </div>

            {#if !['mistralrs', 'webllm', 'ort'].includes(selectedProvider.id)}
                <div class="form-group">
                    <label for="base-url-input-{selectedProvider.id}">{i18n.t.settings.base_url}</label>
                    <input id="base-url-input-{selectedProvider.id}" type="text" bind:value={selectedProvider.baseUrl} />
                </div>
            {/if}

            {#if !['mistralrs', 'llamacpp', 'mlx', 'ollama', 'webllm', 'ort'].includes(selectedProvider.id)}
                <form onsubmit={e => e.preventDefault()}>
                    <input type="text" name="username" value="api-key" style="display:none;" autocomplete="username" />
                    <div class="form-group">
                        <label for="api-key-input-{selectedProvider.id}">{i18n.t.settings.api_key}</label>
                        <input id="api-key-input-{selectedProvider.id}" type="password" name="password" bind:value={selectedProvider.apiKey} autocomplete="current-password" />
                    </div>
                </form>
            {/if}

            {#if !['webllm', 'ort', 'ollama'].includes(selectedProvider.id)}
            <div class="form-group">
                <label for="model-select-input-{selectedProvider.id}">{i18n.t.settings.select_model}</label>
                <div class="input-with-action">
                    <select id="model-select-input-{selectedProvider.id}" bind:value={selectedProvider.selectedModel} class="styled-select" onchange={() => handleSave()}>
                        <option value="">-- {i18n.t.settings.select_model} --</option>
                        {#each availableModels as model}
                            <option value={model}>{model.split(/[\\/]/).pop()}</option>
                        {/each}
                    </select>
                    {#if selectedProvider.id !== 'mistralrs'}
                        <button class="action-btn small" onclick={handleRefreshModels} disabled={loadingModels} aria-label={i18n.t.settings.action_refresh_models}>
                            <RefreshCw size={14} class={loadingModels ? "loader-spin" : ""} />
                        </button>
                    {/if}
                </div>
            </div>
            {/if}

            {#if selectedProvider.id !== 'mistralrs'}
                {#if selectedProvider.id === 'ollama' && selectedProvider.selectedModel}
                    <p class="hint" style="margin: 8px 0 4px;">{i18n.t.settings.active_model} <strong>{selectedProvider.selectedModel}</strong></p>
                {/if}
                <div class="actions">
                    <button class="action-btn test-btn"
                            onclick={handleTestConnection}
                            disabled={!!testingProviders[selectedProvider.id] || !selectedProvider.selectedModel}>
                        <span class={testingProviders[selectedProvider.id] ? "loader-spin" : ""}>
                            {#if testingProviders[selectedProvider.id]}<Loader2 size={16} />{:else}<CheckCircle size={16} />{/if}
                        </span>
                        {i18n.t.settings.test_connection}
                    </button>
                </div>
                {#if testResult}
                    <div class="test-result-box" class:success={testResult.success} class:error={!testResult.success}>
                        <span>{testResult.message}</span>
                    </div>
                {/if}
            {/if}

            {#if selectedProvider.id === 'ollama'}
                <div class="section-card" style="margin-top: 24px;">
                    <div class="header">
                        <h2 style="font-size: 1rem; color: #a1a1aa;"><Cpu size={16} /> {i18n.t.settings.ollama_manager_title}</h2>
                        <div class="header-actions">
                            {#if ollamaStatus === 'starting'}
                                <span class="save-badge" style="color:#f59e0b;"><Loader2 size={14} /> {i18n.t.settings.sidecar.starting}</span>
                            {:else if ollamaStatus === 'ready'}
                                <span class="save-badge" style="color:#10b981;"><CheckCircle size={14} /> {i18n.t.settings.sidecar.running}</span>
                            {:else if ollamaStatus === 'error'}
                                <span class="save-badge" style="color:#ef4444;"><XCircle size={14} /> {i18n.t.settings.sidecar.failed}</span>
                            {/if}
                            {#if ollamaRunning}
                                <button class="action-btn small danger" onclick={stopOllamaService}>
                                    <Square size={14} /> {i18n.t.settings.sidecar.stop}
                                </button>
                            {:else}
                                <button class="action-btn small success" onclick={startOllamaService} disabled={ollamaStatus === 'starting'}>
                                    <Rocket size={14} /> {i18n.t.settings.ollama_start}
                                </button>
                            {/if}
                            <button class="action-btn small" onclick={handleRefreshModels} disabled={loadingModels} aria-label={i18n.t.settings.action_fetch_installed_ollama}>
                                <RefreshCw size={14} class={loadingModels ? "loader-spin" : ""} /> {i18n.t.settings.ollama_fetch_installed}
                            </button>
                        </div>
                    </div>
                    {#if ollamaLogs.length > 0}
                        <div style="margin-bottom: 14px;">
                            <button class="action-btn small" style="color:#71717a; border:none; background:none; padding:0; font-size:0.75rem; font-weight:700; gap:6px;" onclick={() => ollamaLogsVisible = !ollamaLogsVisible}>
                                {i18n.t.settings.ollama_logs}
                                {#if ollamaLogsVisible}<ChevronUp size={14} />{:else}<ChevronDown size={14} />{/if}
                            </button>
                            {#if ollamaLogsVisible}
                                <div style="margin-top: 8px; position: relative;">
                                    <textarea bind:this={ollamaLogEl} readonly class="log-viewer" value={ollamaLogs.join('\n')} rows="8" aria-label={i18n.t.settings.action_ollama_logs_aria}></textarea>
                                    <button class="log-clear-btn" onclick={() => ollamaLogs = []} title={i18n.t.settings.action_clear_log}><Trash2 size={12} /></button>
                                </div>
                            {/if}
                        </div>
                    {/if}
                    
                    <div class="form-group" style="margin-top: 20px;">
                        <label for="ollama-custom-id">{i18n.t.settings.ollama_custom_tag_label}</label>
                        <div class="input-with-action">
                            <input type="text" id="ollama-custom-id" placeholder={i18n.t.settings.ollama_custom_tag_placeholder} bind:value={ollamaCustomInput} />
                            <button class="action-btn small" onclick={addCustomOllamaModel} disabled={!ollamaCustomInput.trim()}>
                                <Plus size={14} /> {i18n.t.settings.ollama_add_pull}
                            </button>
                        </div>
                    </div>

                    <div class="models-grid">
                        {#each ollamaModelsList as model}
                            <div class="local-model-row" class:active-model-row={selectedProvider.selectedModel === model.tag}>
                                <div class="model-info">
                                    <div class="model-title-line">
                                        <strong>{model.tag}</strong>
                                        {#if selectedProvider.selectedModel === model.tag}<Zap size={12} style="color: #eab308;" />{/if}
                                    </div>
                                    <span class="model-path">{model.isInstalled ? i18n.t.settings.installed : i18n.t.settings.not_installed}</span>
                                    {#if ollamaPulling[model.tag] !== undefined}
                                        <div class="progress-container">
                                            <div class="progress-bar" style="width: {ollamaPulling[model.tag]}%"></div>    
                                            <span class="progress-text">{ollamaPulling[model.tag]}%</span>
                                        </div>
                                    {/if}
                                </div>
                                <div class="model-status">
                                    {#if model.isInstalled}
                                        <button class="action-btn small" onclick={() => { selectedProvider.selectedModel = model.tag; handleSave(); }}>
                                            {selectedProvider.selectedModel === model.tag ? i18n.t.batch.selected_status : i18n.t.batch.use_model}   
                                        </button>
                                    {:else if ollamaPulling[model.tag] === undefined}
                                        <button class="action-btn small primary" onclick={() => pullOllamaModel(model.tag)}>
                                            <Download size={14} /> {i18n.t.settings.ollama_pull}
                                        </button>
                                    {/if}
                                </div>
                            </div>
                        {/each}
                    </div>
                </div>
            {/if}

            {#if selectedProvider.id === 'mlx'}
                <div class="section-card" style="margin-top: 24px;">
                    <div class="header" style="margin-bottom: 12px;">
                        <h2 style="font-size: 1rem; color: #a1a1aa;"><HardDrive size={16} /> {i18n.t.settings.mlx_manager_title}</h2>
                        <div class="header-actions">
                            <input id="mlx-port-input" type="number" bind:value={mlxPort} style="width: 80px;" aria-label={i18n.t.settings.action_mlx_port_aria} />    
                            {#if mlxRunning}
                                <button class="action-btn small danger" onclick={stopMlxServer}>
                                    <Square size={14} /> {i18n.t.settings.mlx_stop}
                                </button>
                            {:else}
                                <button class="action-btn small success" onclick={startMlxServer}>
                                    <Rocket size={14} /> {i18n.t.settings.mlx_start}
                                </button>
                            {/if}
                            <button class="action-btn small" onclick={checkMlxModelsCache} title={i18n.t.settings.action_refresh_cache_status}>
                                <RefreshCw size={14} /> {i18n.t.batch.reanalyze_run}
                            </button>
                        </div>
                    </div>
                    <p class="hint">{i18n.t.settings.mlx_manager_hint}</p>
                    <p class="hint">{i18n.t.settings.mlx_cache_label}: <code>{mlxCacheDir}</code></p>

                    <div class="form-group" style="margin-top: 20px;">
                        <label for="mlx-custom-id">{i18n.t.settings.mlx_custom_repo}</label>
                        <div class="input-with-action">
                            <input type="text" id="mlx-custom-id" placeholder={i18n.t.settings.mlx_custom_placeholder} bind:value={mlxCustomInput} />
                            <button class="action-btn small" onclick={addMlxModel}><Plus size={14} /> {i18n.t.batch.add_files}</button>
                        </div>
                    </div>

                    <div class="models-grid">
                        {#each mlxModels as model}
                            <div class="local-model-row" class:active-model-row={selectedProvider.selectedModel === model.repoId}>
                                <div class="model-info">
                                    <div class="model-title-line">
                                        <strong>{model.name}</strong>
                                        {#if model.vision}<span class="vision-badge">VL</span>{/if}
                                        {#if selectedProvider.selectedModel === model.repoId}<Zap size={12} style="color: #eab308;" />{/if}
                                        <span class="cache-dot" class:cached={mlxModelCached[model.id]}></span>
                                    </div>
                                    <span class="model-path">{model.repoId}</span>
                                </div>
                                <div class="model-status">
                                    <span class="size-badge">{model.params}</span>
                                    <button class="action-btn small" onclick={() => setMlxModelActive(model.repoId)}>
                                        {selectedProvider.selectedModel === model.repoId ? i18n.t.batch.selected_status : i18n.t.batch.use_model}   
                                    </button>
                                    {#if mlxModelCached[model.id]}
                                        <button class="icon-btn danger" onclick={() => deleteMlxModelFromDisk(model)} title={i18n.t.settings.action_delete_from_disk}><Trash2 size={14} /></button>
                                    {/if}
                                    <button class="icon-btn" onclick={() => removeMlxModel(model.id)} title={i18n.t.settings.action_remove_from_list}><XCircle size={14} /></button>
                                </div>
                            </div>
                        {/each}
                    </div>
                </div>

                <div class="section-card" style="margin-top: 16px;">
                    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                    <div class="section-toggle-flat" onclick={() => mlxLogsVisible = !mlxLogsVisible} role="button" tabindex="0" onkeydown={e => e.key === 'Enter' && (mlxLogsVisible = !mlxLogsVisible)}>        
                        <span style="display:flex; align-items:center; gap:8px;">
                            <Brain size={14} /> {i18n.t.settings.mlx_server_log}
                            {#if mlxRunning}<span class="running-dot"></span>{/if}
                        </span>
                        <span style="display:flex; align-items:center; gap:8px;">
                            <span class="hint" style="margin:0;">{i18n.t.settings.mlx_log_lines.replace('{count}', String(mlxLogs.length))}</span>
                            {#if mlxLogsVisible}<ChevronUp size={14} />{:else}<ChevronDown size={14} />{/if}
                        </span>
                    </div>
                    {#if mlxLogsVisible}
                        <div style="margin-top: 10px; position: relative;">
                            <textarea id="mlx-log-viewer" bind:this={mlxLogEl} readonly class="log-viewer" value={mlxLogs.join('\n')} rows="14" aria-label={i18n.t.settings.action_mlx_logs_aria}></textarea>
                            <button class="log-clear-btn" onclick={() => mlxLogs = []} title={i18n.t.settings.action_clear_log}><Trash2 size={12} /></button>
                        </div>
                    {/if}
                </div>
            {/if}

            {#if selectedProvider.id === 'webllm'}
                <div class="section-card" style="margin-top: 24px;">
                    <div class="header" style="margin-bottom: 12px;">
                        <h2 style="font-size: 1rem; color: #a1a1aa;"><Cpu size={16} /> {i18n.t.settings.webllm_manager_title}</h2>
                        <div class="header-actions">
                            {#if webllmStatus === 'ready'}
                                <span class="save-badge" style="color:#10b981;"><CheckCircle size={14} /> {i18n.t.settings.webllm_ready}: {getWebLLMLoadedModel().split('-q4')[0]}</span>
                                <button class="action-btn small danger" onclick={handleUnloadWebLLM}>{i18n.t.settings.webllm_unload}</button>
                            {:else if webllmStatus === 'downloading' || webllmStatus === 'loading'}
                                <span class="save-badge" style="color:#f59e0b;"><Loader2 size={14} /> {webllmStatus === 'downloading' ? i18n.t.settings.webllm_downloading : i18n.t.settings.webllm_loading} {webllmProgress}%</span>
                            {:else if webllmStatus === 'error'}
                                <span class="save-badge" style="color:#ef4444;"><XCircle size={14} /> Error</span>
                            {/if}
                        </div>
                    </div>
                    <p class="hint">{i18n.t.settings.webllm_hint}</p>

                    {#if (webllmStatus === 'downloading' || webllmStatus === 'loading') && webllmProgressText}
                        <div style="margin: 8px 0 12px;">
                            <div class="progress-container">
                                <div class="progress-bar" style="width: {webllmProgress}%"></div>
                                <span class="progress-text">{webllmProgress}%</span>
                            </div>
                            <p class="hint" style="margin-top:4px; font-size:0.7rem; color:#52525b;">{webllmProgressText}</p>
                        </div>
                    {/if}

                    <div class="models-grid" style="margin-top: 12px;">
                        {#each WEBLLM_MODELS as model}
                            {@const isLoaded = webllmStatus === 'ready' && getWebLLMLoadedModel() === model.id}
                            {@const isActive = selectedProvider.selectedModel === model.id}
                            <div class="local-model-row" class:active-model-row={isActive}>
                                <div class="model-info">
                                    <div class="model-title-line">
                                        <strong>{model.name.split(' · ')[0]}</strong>
                                        {#if isActive}<Zap size={12} style="color: #eab308;" />{/if}
                                    </div>
                                    <span class="model-path">{model.name.split(' · ').slice(1).join(' · ')}</span>
                                </div>
                                <div class="model-status">
                                    {#if isLoaded}
                                        <button class="action-btn small" onclick={() => handleUseWebLLM(model.id)}>
                                            {isActive ? i18n.t.batch.selected_status : i18n.t.settings.webllm_use}
                                        </button>
                                    {:else}
                                        <button class="action-btn small primary"
                                            disabled={webllmStatus === 'downloading' || webllmStatus === 'loading'}
                                            onclick={() => handleLoadWebLLM(model.id)}>
                                            <Download size={14} /> {i18n.t.settings.webllm_load}
                                        </button>
                                    {/if}
                                </div>
                            </div>
                        {/each}
                    </div>
                </div>
            {/if}

            {#if selectedProvider.id === 'ort'}
                <div class="section-card" style="margin-top: 24px;">
                    <div class="header" style="margin-bottom: 12px;">
                        <h2 style="font-size: 1rem; color: #a1a1aa;"><Cpu size={16} /> {i18n.t.settings.ort_manager_title}</h2>
                        <div class="header-actions">
                            {#if ortStatus === 'ready'}
                                <span class="save-badge" style="color:#10b981;"><CheckCircle size={14} /> {i18n.t.settings.ort_ready}: {getORTLoadedModel().split('/').pop()} ({getORTDevice()})</span>
                                <button class="action-btn small danger" onclick={handleUnloadORT}>{i18n.t.settings.ort_unload}</button>
                            {:else if ortStatus === 'downloading' || ortStatus === 'loading'}
                                <span class="save-badge" style="color:#f59e0b;"><Loader2 size={14} /> {ortStatus === 'downloading' ? i18n.t.settings.ort_downloading : i18n.t.settings.ort_loading} {ortProgress}%</span>
                            {:else if ortStatus === 'error'}
                                <span class="save-badge" style="color:#ef4444;"><XCircle size={14} /> Error</span>
                            {/if}
                        </div>
                    </div>
                    <p class="hint">{i18n.t.settings.ort_hint}</p>

                    {#if (ortStatus === 'downloading' || ortStatus === 'loading') && ortProgressText}
                        <div style="margin: 8px 0 12px;">
                            <div class="progress-container">
                                <div class="progress-bar" style="width: {ortProgress}%"></div>
                                <span class="progress-text">{ortProgress}%</span>
                            </div>
                            <p class="hint" style="margin-top:4px; font-size:0.7rem; color:#52525b;">{ortProgressText}</p>
                        </div>
                    {/if}

                    <div class="models-grid" style="margin-top: 12px;">
                        {#each ORT_MODELS as model}
                            {@const isLoaded = ortStatus === 'ready' && getORTLoadedModel() === model.id}
                            {@const isActive = selectedProvider.selectedModel === model.id}
                            <div class="local-model-row" class:active-model-row={isActive}>
                                <div class="model-info">
                                    <div class="model-title-line">
                                        <strong>{model.name.split(' · ')[0]}</strong>
                                        {#if isActive}<Zap size={12} style="color: #eab308;" />{/if}
                                    </div>
                                    <span class="model-path">{model.name.split(' · ').slice(1).join(' · ')}</span>
                                </div>
                                <div class="model-status">
                                    {#if isLoaded}
                                        <button class="action-btn small" onclick={() => handleUseORT(model.id)}>
                                            {isActive ? i18n.t.batch.selected_status : i18n.t.settings.ort_use}
                                        </button>
                                    {:else}
                                        <button class="action-btn small primary"
                                            disabled={ortStatus === 'downloading' || ortStatus === 'loading'}
                                            onclick={() => handleLoadORT(model.id)}>
                                            <Download size={14} /> {i18n.t.settings.ort_load}
                                        </button>
                                    {/if}
                                </div>
                            </div>
                        {/each}
                    </div>
                </div>
            {/if}

            {#if ['mistralrs', 'llamacpp'].includes(selectedProvider.id)}
                <div class="section-card" style="margin-top: 24px;">
                    <div class="header" style="margin-bottom: 15px;">
                        <h2 style="font-size: 1rem; color: #a1a1aa;"><HardDrive size={16} /> {i18n.t.settings.local_manager_title}</h2>
                        <div class="header-actions" style="display: flex; gap: 8px; align-items: center;">
                            {#if selectedProvider.id === 'llamacpp'}
                                <div style="display:flex; align-items:center; gap:8px; margin-right:8px; background:#09090b; padding:2px 8px; border-radius:6px; border:1px solid #27272a;">
                                    <span style="font-size:0.7rem; color:#71717a; font-weight:700;">{i18n.t.settings.port}</span>
                                    <input type="number" bind:value={llamacppPort} style="width: 70px; border:none; padding:2px; height:24px; font-size:0.8125rem;" />
                                </div>
                                {#if sidecarStatus === 'starting'}
                                    <span class="save-badge" style="color:#f59e0b;"><Loader2 size={14} /> {i18n.t.settings.sidecar.starting}</span>
                                {:else if sidecarStatus === 'ready'}
                                    <span class="save-badge" style="color:#10b981;"><CheckCircle size={14} /> {i18n.t.settings.sidecar.running}</span>
                                {:else if sidecarStatus === 'error'}
                                    <span class="save-badge" style="color:#ef4444;"><XCircle size={14} /> {i18n.t.settings.sidecar.failed}</span>
                                {/if}
                                <button class="action-btn small primary" disabled={sidecarStatus === 'starting'} onclick={() => setLocalModelActive(selectedProvider.selectedModel)}>
                                    <Rocket size={14} /> {i18n.t.settings.local_manager_start}
                                </button>
                                <button class="action-btn small danger" onclick={async () => { await invoke('stop_llamacpp_sidecar'); sidecarStatus = ''; llamacppReady = false; }}>
                                    <Square size={14} /> {i18n.t.settings.sidecar.stop}
                                </button>
                            {/if}
                            <button class="action-btn small success" onclick={addLocalModel}>
                                <Plus size={14} /> {i18n.t.settings.local_manager_add}
                            </button>
                        </div>
                    </div>
                    {#if selectedProvider.id === 'llamacpp' && sidecarLogs.length > 0}
                        <div style="margin-bottom: 14px;">
                            <button class="action-btn small" style="color:#71717a; border:none; background:none; padding:0; font-size:0.75rem; font-weight:700; gap:6px;" onclick={() => sidecarLogsVisible = !sidecarLogsVisible}>
                                {i18n.t.settings.sidecar_logs}
                                {#if sidecarLogsVisible}<ChevronUp size={14} />{:else}<ChevronDown size={14} />{/if}
                            </button>
                            {#if sidecarLogsVisible}
                                <div style="margin-top: 8px; position: relative;">
                                    <textarea bind:this={sidecarLogEl} readonly class="log-viewer" value={sidecarLogs.join('\n')} rows="10" aria-label={i18n.t.settings.action_llamacpp_logs_aria}></textarea>
                                    <button class="log-clear-btn" onclick={() => sidecarLogs = []} title={i18n.t.settings.action_clear_log}><Trash2 size={12} /></button>
                                </div>
                            {/if}
                        </div>
                    {/if}

                    <div class="form-group">
                        <label for="custom-model-id-{selectedProvider.id}">{i18n.t.settings.custom_hf_repo_url}</label>
                        <div class="input-with-action">
                            <input type="text" id="custom-model-id-{selectedProvider.id}" placeholder={i18n.t.settings.custom_hf_placeholder} bind:value={customModelInput} />
                            <button class="action-btn small" onclick={addCustomModel}><Plus size={14} /> {i18n.t.batch.add_files}</button>
                        </div>
                        <p class="hint">{i18n.t.settings.local_manager_hf_hint}</p>
                    </div>

                    <div class="models-grid">
                        {#each localModels as model, i}
                            <div class="local-model-row" class:active-model-row={selectedProvider.selectedModel === model.path && model.path !== ''}>
                                <div class="model-info">
                                    <div class="model-title-line">
                                        <strong>{model.name}</strong>
                                        {#if selectedProvider.selectedModel === model.path && model.path !== ''}<Zap size={12} style="color: #eab308;" />{/if}
                                    </div>
                                    <span class="model-path">{model.path || i18n.t.settings.not_downloaded}</span>
                                    {#if model.progress !== undefined}
                                        <div class="progress-container">
                                            <div class="progress-bar" style="width: {model.progress}%"></div>    
                                            <span class="progress-text">{model.progress}%</span>
                                        </div>
                                    {/if}
                                </div>
                                <div class="model-status">
                                    <button class="action-btn small" onclick={() => setLocalModelActive(model.path)}>
                                        {selectedProvider.selectedModel === model.path && model.path !== '' ? i18n.t.batch.selected_status : i18n.t.batch.use_model} 
                                    </button>
                                    {#if model.isDownloaded}
                                        <span class="size-badge">{model.size}</span>
                                        <button class="icon-btn danger" onclick={() => removeLocalModel(i)} title={i18n.t.settings.action_delete_file}><Trash2 size={14} /></button>
                                    {:else if model.progress === undefined}
                                        <button class="action-btn small primary" onclick={() => downloadLocalModel(i)}>
                                            <Download size={14} /> {i18n.t.settings.download}
                                        </button>
                                        <button class="icon-btn" onclick={() => localModels.splice(i, 1)} title={i18n.t.settings.action_remove_from_list}><XCircle size={14} /></button>
                                    {/if}
                                </div>
                            </div>
                        {/each}
                    </div>
                </div>
            {/if}
        {/if}
    </div>
</div>

{#if benchModal}
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="bench-modal-overlay" onclick={() => benchModal = null} role="button" tabindex="0" onkeydown={e => e.key === 'Escape' && (benchModal = null)}>
        <div class="bench-modal" onclick={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
            <div class="bench-modal-header">
                <span>{benchModal.title}</span>
                <button class="bench-modal-close" onclick={() => benchModal = null} aria-label={i18n.t.settings.action_close_bench_modal}>✕</button>
            </div>
            <div class="bench-modal-body">
                {#each benchModal.runs as run, ri}
                    <div class="bench-response-block">
                        <span class="bench-run-label">
                            {i18n.t.settings.benchmark.run_label} {ri + 1} {ri === 0 ? i18n.t.settings.benchmark.run_cold_marker : i18n.t.settings.benchmark.run_warm_marker}
                            {#if run.error}{i18n.t.settings.benchmark.error_marker}{:else}— {run.latencyMs.toLocaleString()} ms / {run.tokensPerSec ?? '?'} t/s{/if}
                        </span>
                        <pre class="bench-response-pre" style="max-height:none;">{run.error || run.response || i18n.t.settings.benchmark.empty_response}</pre>
                    </div>
                {/each}
            </div>
        </div>
    </div>
{/if}

<style>
    .settings-container { display: flex; height: 100%; background: #09090b; color: #fafafa; font-family: 'Inter', sans-serif; overflow: hidden; }
    .sidebar { width: 200px; background: #18181b; border-right: 1px solid #27272a; padding: 0; display: flex; flex-direction: column; flex-shrink: 0; }
    .sidebar-scrollable { flex: 1; overflow-y: auto; padding: 20px 0; }
    .sidebar h2 { padding: 0 20px; font-size: 0.75rem; text-transform: uppercase; color: #71717a; margin-bottom: 12px; letter-spacing: 0.05em; }
    .sidebar-divider { height: 1px; background: #27272a; margin: 20px 0; }
    .provider-list { display: flex; flex-direction: column; }
    .provider-btn { padding: 8px 20px; text-align: left; border: none; background: transparent; cursor: pointer; font-size: 0.875rem; color: #a1a1aa; transition: all 0.2s; display: flex; align-items: center; justify-content: space-between; gap: 8px; width: 100%; }
    .provider-btn .prov-label { display: inline-flex; align-items: center; gap: 8px; min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .provider-btn:hover { background: #27272a; color: white; }
    .provider-btn.active { background: #27272a; color: white; font-weight: 600; border-left: 3px solid #3b82f6; }
    
    .sidebar-footer { padding: 16px; border-top: 1px solid #27272a; }

    .content { flex: 1; padding: 32px 48px; overflow-y: auto; }
    .header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 24px; }
    .header-actions { display: flex; gap: 12px; align-items: center; }
    h1 { font-size: 1.25rem; font-weight: 700; margin: 0; }
    
    .save-area { display: flex; align-items: center; gap: 12px; }
    .save-badge { font-size: 0.75rem; color: #10b981; font-weight: 600; display: flex; align-items: center; gap: 4px; animation: fadeIn 0.3s; }
    @keyframes fadeIn { from { opacity: 0; transform: translateY(5px); } to { opacity: 1; transform: translateY(0); } }

    .save-btn { background: #3b82f6; color: white; border: none; padding: 6px 12px; border-radius: 6px; font-weight: 600; cursor: pointer; font-size: 0.875rem; }
    .section-card { background: #18181b; border: 1px solid #27272a; padding: 16px; border-radius: 8px; margin-bottom: 16px; }
    .form-group { margin-bottom: 20px; max-width: 600px; }
    .checkbox-group { display: flex; align-items: center; gap: 12px; margin-bottom: 10px; }
    label { display: flex; align-items: center; gap: 8px; font-size: 0.8125rem; font-weight: 600; margin-bottom: 10px; color: #a1a1aa; text-transform: uppercase; letter-spacing: 0.02em; }
    input[type="text"], input[type="password"], input[type="number"], .styled-select, textarea { width: 100%; padding: 8px 12px; border: 1px solid #27272a; border-radius: 6px; font-size: 0.875rem; background: #09090b; color: white; }
    textarea { resize: vertical; min-height: 100px; }
    .input-with-action { display: flex; gap: 10px; }
    
    .toggle-group { display: flex; background: #09090b; border: 1px solid #27272a; border-radius: 6px; padding: 2px; width: fit-content; }
    .toggle-btn { padding: 4px 12px; border: none; background: transparent; color: #71717a; font-size: 0.75rem; font-weight: 600; cursor: pointer; border-radius: 4px; transition: all 0.2s; }
    .toggle-btn.active { background: #27272a; color: white; }

    .actions { display: flex; gap: 12px; margin-bottom: 24px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 6px 12px; border: 1px solid #27272a; background: #18181b; border-radius: 6px; font-size: 0.8125rem; font-weight: 600; color: #d4d4d8; transition: background 0.2s; }
    .action-btn:hover { background: #27272a; }
    .action-btn.active-btn { color: #eab308; border-color: #713f12; background: #42200633; }
    
    .test-result-box { padding: 10px; border-radius: 6px; font-size: 0.8125rem; margin-bottom: 24px; max-width: 600px; border: 1px solid #27272a; }
    .test-result-box.success { background: #064e3b33; color: #ecfdf5; border-color: #065f46; }
    .test-result-box.error { background: #450a0a33; color: #fef2f2; border-color: #7f1d1d; }
    
    .models-grid { display: flex; flex-direction: column; gap: 8px; }
    .local-model-row { display: flex; justify-content: space-between; align-items: center; padding: 12px; background: #09090b; border: 1px solid #27272a; border-radius: 6px; }
    .local-model-row.active-model-row { border-color: #3b82f6; background: #1e3a8a33; }
    .model-title-line { display: flex; align-items: center; gap: 8px; }
    .model-info { display: flex; flex-direction: column; gap: 4px; flex: 1; margin-right: 20px; overflow: hidden; }
    .model-path { font-size: 0.7rem; color: #71717a; font-family: monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .model-status { display: flex; align-items: center; gap: 12px; flex-shrink: 0; }
    .size-badge { font-size: 0.75rem; background: #27272a; padding: 2px 6px; border-radius: 4px; color: #a1a1aa; }
    
    .progress-container { margin-top: 8px; height: 16px; background: #18181b; border-radius: 8px; position: relative; overflow: hidden; border: 1px solid #27272a; }
    .progress-bar { height: 100%; background: #3b82f6; transition: width 0.3s; }
    .progress-text { position: absolute; top: 0; left: 0; width: 100%; text-align: center; font-size: 0.65rem; line-height: 16px; color: white; font-weight: 700; }

    .icon-btn { background: transparent; border: none; cursor: pointer; color: #71717a; display: flex; align-items: center; justify-content: center; padding: 4px; border-radius: 4px; }
    .icon-btn:hover { background: #27272a; color: white; }
    .icon-btn.danger:hover { background: #ef444433; color: #ef4444; }
    
    .hint { font-size: 0.75rem; color: #71717a; margin-top: 6px; display: block; line-height: 1.4; }
    .preset-chips { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 10px; }
    .chip { padding: 3px 10px; border: 1px solid #3f3f46; border-radius: 999px; background: transparent; color: #a1a1aa; font-size: 0.72rem; cursor: pointer; transition: all 0.15s; white-space: nowrap; }
    .chip:hover { border-color: #6366f1; color: #c7d2fe; }
    .chip.active { border-color: #6366f1; background: #6366f122; color: #c7d2fe; }
    .template-preview { font-family: monospace; word-break: break-all; }
    .loader-spin { display: inline-flex; animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

    .init-progress-wrap { margin-top: 12px; }
    .init-progress-label { font-size: 0.8rem; color: #a1a1aa; display: flex; align-items: center; gap: 6px; margin-bottom: 6px; }
    .init-progress-bar { height: 6px; background: #27272a; border-radius: 3px; overflow: hidden; }
    .init-progress-fill { height: 100%; background: #3b82f6; border-radius: 3px; transition: width 0.4s ease; }
    .init-progress-note { font-size: 0.75rem; color: #52525b; margin-top: 6px; font-style: italic; }

    .bench-modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.7); z-index: 1000; display: flex; align-items: center; justify-content: center; }
    .bench-modal { background: #18181b; border: 1px solid #3f3f46; border-radius: 8px; width: min(720px, 90vw); max-height: 80vh; display: flex; flex-direction: column; }
    .bench-modal-header { display: flex; justify-content: space-between; align-items: center; padding: 14px 18px; border-bottom: 1px solid #27272a; font-size: 0.875rem; font-weight: 600; color: #e2e8f0; }       
    .bench-modal-close { background: none; border: none; color: #71717a; cursor: pointer; font-size: 1rem; }
    .bench-modal-body { padding: 16px 18px; overflow-y: auto; }
    .bench-response-pre { margin: 0; white-space: pre-wrap; font-size: 0.8rem; color: #e2e8f0; font-family: monospace; background: #09090b; padding: 10px; border-radius: 4px; }

    .vision-badge { font-size: 0.6rem; font-weight: 700; background: #7c3aed33; color: #a78bfa; border: 1px solid #7c3aed55; border-radius: 3px; padding: 1px 5px; }
    .cache-dot { width: 7px; height: 7px; border-radius: 50%; background: #3f3f46; }
    .cache-dot.cached { background: #10b981; }
    .running-dot { width: 8px; height: 8px; border-radius: 50%; background: #10b981; animation: pulse 1.5s infinite; }
    @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.3; } }

    .license-list-scrollable { display: flex; flex-direction: column; gap: 8px; max-height: 400px; overflow-y: auto; background: #09090b; border: 1px solid #27272a; border-radius: 6px; padding: 12px; }
    .license-item-auto { border-bottom: 1px solid #18181b; padding-bottom: 8px; margin-bottom: 4px; }
    .license-item-auto:last-child { border-bottom: none; }
    .license-item-header { display: flex; justify-content: space-between; align-items: center; }
    .lib-source-badge { font-size: 0.65rem; font-weight: 700; background: #1e293b; color: #94a3b8; padding: 1px 6px; border-radius: 4px; }
    .lib-source-badge.rust { background: #450a0a33; color: #f87171; }
    .license-item-meta { display: flex; align-items: center; gap: 12px; font-size: 0.75rem; color: #71717a; }    
    .inline-link { background: none; border: none; color: #3b82f6; cursor: pointer; font-size: 0.8rem; padding: 0; text-decoration: underline; }
    .legal-text { font-size: 0.875rem; color: #e2e8f0; line-height: 1.6; }

    .catalog-list { display: flex; flex-direction: column; gap: 4px; }
    .catalog-row {
        display: flex; align-items: center; gap: 10px; padding: 8px 10px;
        background: #09090b; border: 1px solid #27272a; border-radius: 6px;
    }
    .catalog-row.active { border-color: #3b82f6; background: #1e3a8a14; }
    .catalog-row input[type="radio"] { cursor: pointer; flex-shrink: 0; accent-color: #3b82f6; }
    .catalog-name {
        background: none; border: none; color: #e4e4e7; font-size: 0.875rem;
        cursor: pointer; padding: 0; text-align: left;
    }
    .catalog-name:hover { color: white; }
    .catalog-rename-input {
        background: #18181b; border: 1px solid #3b82f6; border-radius: 4px;
        color: white; padding: 2px 6px; font-size: 0.85rem; min-width: 200px;
    }
    .catalog-meta { flex: 1; font-size: 0.72rem; color: #71717a; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .catalog-actions { display: flex; gap: 2px; flex-shrink: 0; }
    .version-pill { display: inline-flex; align-items: center; padding: 4px 10px; border-radius: 99px; background: #18181b; border: 1px solid #27272a; color: #a1a1aa; font-size: 0.75rem; font-family: monospace; font-weight: 600; }
    .license-empty { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; padding: 16px; color: #71717a; font-size: 0.85rem; }
    .license-empty.error { color: #fca5a5; }
    .license-empty code { background: #18181b; border: 1px solid #27272a; padding: 1px 5px; border-radius: 3px; font-size: 0.75rem; }

    /* Improved Benchmark UI Styles */
    .benchmark-ui .bench-config-label { width: 100px; font-size: 0.75rem; color: #71717a; font-weight: 700; }
    .bench-providers-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 12px; width: 100%; }
    .bench-provider-card { background: #09090b; border: 1px solid #27272a; border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 8px; transition: border-color 0.2s; }
    .bench-provider-card.selected { border-color: #3b82f6; background: #1e3a8a11; }
    .bench-provider-card .p-name { font-weight: 600; font-size: 0.875rem; }
    .bench-model-select { background: #18181b; border: 1px solid #27272a; color: white; padding: 4px 8px; border-radius: 4px; font-size: 0.75rem; font-family: monospace; }
    
    .bench-file-list { display: flex; flex-direction: column; gap: 4px; background: #09090b; border: 1px solid #27272a; border-radius: 8px; padding: 8px; max-height: 200px; overflow-y: auto; }
    .bench-check-label.file-item { display: flex; justify-content: space-between; align-items: center; padding: 6px 10px; border-radius: 6px; transition: background 0.2s; width: 100%; box-sizing: border-box; }
    .bench-check-label.file-item:hover { background: #18181b; }
    .bench-check-label.file-item .file-name { flex: 1; margin: 0 12px; font-size: 0.8125rem; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .bench-check-label.file-item .char-count { font-size: 0.7rem; color: #71717a; font-family: monospace; }
    
    .runs-select-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; width: 100%; }
    .runs-select-grid button { background: #09090b; border: 1px solid #27272a; color: #a1a1aa; padding: 10px; border-radius: 8px; cursor: pointer; display: flex; flex-direction: column; align-items: center; gap: 4px; transition: all 0.2s; }
    .runs-select-grid button:hover { border-color: #3f3f46; color: white; }
    .runs-select-grid button.active { border-color: #3b82f6; background: #1e3a8a33; color: white; }
    .runs-select-grid .r-num { font-size: 1.125rem; font-weight: 700; }
    .runs-select-grid .r-text { font-size: 0.65rem; text-transform: uppercase; font-weight: 600; opacity: 0.8; }
    
    .large-bench-btn { height: 50px; font-size: 1rem; margin-top: 20px; width: 100%; display: flex; align-items: center; justify-content: center; gap: 12px; }

    .rr-list { display: flex; flex-direction: column; gap: 4px; }
    .rr-row { display: flex; align-items: center; justify-content: space-between; padding: 6px 10px; border-radius: 6px; border: 1px solid transparent; }
    .rr-row.rr-enabled { border-color: #27272a; background: #1c1c1f; }
    .rr-label { display: flex; align-items: center; gap: 8px; font-size: 0.8125rem; color: #a1a1aa; cursor: pointer; }
    .rr-label input { cursor: pointer; }
    .rr-order { display: flex; align-items: center; gap: 4px; }
    .rr-idx { font-size: 0.7rem; color: #52525b; width: 20px; text-align: right; }
    .icon-btn-tiny { background: transparent; border: none; color: #52525b; cursor: pointer; padding: 2px 4px; border-radius: 3px; font-size: 0.65rem; line-height: 1; }
    .icon-btn-tiny:hover:not(:disabled) { color: #a1a1aa; background: #27272a; }
    .icon-btn-tiny:disabled { opacity: 0.3; cursor: not-allowed; }

    /* Stage T — admin panel */
    .cb-admin-panel { margin-top: 12px; }
    .cb-admin-label { display: block; font-size: 0.78rem; color: #a1a1aa; margin: 6px 0 2px; }
    .cb-admin-input { width: 100%; max-width: 340px; background: #18181b; border: 1px solid #3f3f46; border-radius: 6px; color: #e4e4e7; padding: 5px 8px; font-size: 0.82rem; }
    .cb-admin-row { display: flex; gap: 16px; flex-wrap: wrap; margin-top: 10px; }
    .cb-admin-group { flex: 1 1 200px; display: flex; flex-direction: column; gap: 4px; }
    .cb-admin-minted-key { margin-top: 6px; background: #09090b; border: 1px solid #3f3f46; border-radius: 6px; padding: 6px 8px; }
    .cb-admin-minted-key code { display: block; font-size: 0.75rem; color: #4ade80; word-break: break-all; margin-top: 2px; }
    .cb-admin-table { width: 100%; border-collapse: collapse; font-size: 0.8rem; margin-top: 10px; }
    .cb-admin-table th { text-align: left; color: #71717a; font-weight: 600; padding: 4px 8px; border-bottom: 1px solid #27272a; }
    .cb-admin-table td { padding: 4px 8px; border-bottom: 1px solid #1c1c1f; }
    .cb-admin-revoked td { opacity: 0.5; text-decoration: line-through; }
    /* Force readable text on admin-panel buttons.  Global .btn rules
       inherit grey text from the surrounding muted panel; on the
       red `.btn-danger` background that became unreadable, and the
       neutral `.btn` on the muted-grey card was nearly as bad.
       Pinning white text + slightly heavier weight fixes both. */
    .cb-admin-panel .btn,
    .cb-admin-panel .btn-danger { color: #fafafa; font-weight: 600; }
    .cb-admin-panel .btn:disabled,
    .cb-admin-panel .btn-danger:disabled { color: #a1a1aa; }
    .btn-danger { background: #7f1d1d; border-color: #991b1b; color: #fafafa; }
    .btn-danger:hover:not(:disabled) { background: #991b1b; }

    /* ── Responsive: phone (<768px) ────────────────────────────────── */
    @media (max-width: 767px) {
        .settings-container { flex-direction: column; }
        .sidebar { width: 100%; max-height: 48px; flex-direction: row; overflow-x: auto; overflow-y: hidden; border-right: none; border-bottom: 1px solid #27272a; }
        .sidebar-scrollable { display: flex; flex-direction: row; padding: 0; overflow-x: auto; }
        .sidebar h2 { display: none; }
        .sidebar-divider { display: none; }
        .sidebar-footer { display: none; }
        .provider-list { flex-direction: row; }
        .provider-btn { white-space: nowrap; padding: 8px 12px; font-size: 0.75rem; }
        .provider-btn.active { border-left: none; border-bottom: 2px solid #3b82f6; }
        .content { padding: 16px; }
        .form-group { max-width: 100%; }
        .input-with-action { flex-direction: column; }
        .info-grid { grid-template-columns: 1fr; gap: 8px; }
    }

    /* ── Responsive: tablet (768-1024px) ───────────────────────────── */
    @media (min-width: 768px) and (max-width: 1024px) {
        .sidebar { width: 160px; }
        .content { padding: 20px 24px; }
    }
</style>
