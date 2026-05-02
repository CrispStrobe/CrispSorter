<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import { DEFAULT_PROVIDERS, type LLMProvider, llmClient } from '../llm/client';
    import { getSetting, saveSetting } from '../store';
    import { i18n, type Language } from '../i18n.svelte';
    import { getDefaultPrompt, batchManager } from '../batch/store.svelte';
    import {
        RefreshCw, CheckCircle, XCircle, Key, Globe, Cpu,
        Loader2, FolderOpen, Save, Languages, MessageSquare,
        Scan, Edit, Zap, Trash2, Download, Plus, HardDrive, Code,
        Rocket, FileText, Brain, Square, ChevronUp, ChevronDown, Info,
        RotateCcw, Search, CheckCircle2, AlertCircle, Beaker, Play, Check
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
    let authorSortEnabled = $state(false);
    let noThinking = $state(true);
    let pdfBackend = $state<'js' | 'rust'>('js');
    let parsingFormat = $state<'xml' | 'json'>('xml');
    // Auto-speak chat replies via the platform's native TTS synth (macOS
    // `say` / Windows SAPI / Linux espeak). Off by default — voice mode
    // is opt-in.
    let autoSpeakReplies = $state(false);

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
    let indexBackendType    = $state<'local' | 'remote'>('local');
    let indexRemoteUrl      = $state('');
    let indexRemoteApiKey   = $state('');
    let indexEmbedderModel  = $state<string>('bge_m3');
    let indexEmbedderBackend = $state<'onnx' | 'gguf'>('onnx');
    let indexDevice         = $state<'auto' | 'cpu' | 'metal' | 'cuda'>('auto');
    // Reranker: empty string = disabled (maps to null on the Rust side).
    // Other values are UI keys; mapped to Rust kebab-case via rerankerToRust.
    let indexRerankerModel  = $state<string>('');
    let indexRerankerTopN   = $state<number>(50);
    // Empty = use default ({data_dir}/models). Override is shared by
    // ONNX (fastembed/OrtPath) AND GGUF (CrispEmbed embedder + reranker)
    // downloads, so one setting controls every model weight on disk.
    let indexModelCacheDir  = $state<string>('');
    // Matryoshka truncation dim. 0 = use model default (no truncation).
    // Honored only on GGUF backend; ignored otherwise. Quality only holds
    // for MRL-trained models.
    let indexMatryoshkaDim  = $state<number>(0);

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
        // Added in fastembed-rs/CrispEmbed registry sync (May 2026):
        'multilingual_e5_small', 'multilingual_e5_base', 'multilingual_e5_large',
        'bge_small_en_v15', 'bge_base_en_v15', 'bge_large_en_v15',
        'nomic_embed_v15', 'mxbai_large_v1', 'minilm_l6_v2',
        'embedding_gemma_300m', 'gte_base_en_v15', 'gte_large_en_v15',
    ]);
    function supportsGguf(uiModel: string): boolean {
        return GGUF_CAPABLE_MODELS.has(uiModel);
    }

    async function handleEmbedderChange(e: Event) {
        const val = (e.target as HTMLSelectElement).value;
        if (val === 'jina_nano') {
            const confirmed = await ask(i18n.t.settings.index.non_commercial_confirm, { 
                title: 'Jina-v5 License Confirmation',
                kind: 'warning'
            });
            if (!confirmed) {
                // Revert to previous value or default if user cancels
                (e.target as HTMLSelectElement).value = indexEmbedderModel;
                return;
            }
        }
        indexEmbedderModel = val;
    }
    let indexDataDir        = $state('');
    let indexStatus         = $state<'idle' | 'loading' | 'ok' | 'error'>('idle');
    let indexStatusMsg      = $state('');
    let indexInitProgress   = $state('');
    let indexInitPct        = $state(0);
    let indexIvfRunning     = $state(false);

    // Benchmarking
    let benchProviders = $state<string[]>([]);
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
    let licenseSearch = $state('');
    let filteredLicenses = $derived(automatedLicenses.filter(l =>
        l.name.toLowerCase().includes(licenseSearch.toLowerCase()) ||
        l.author?.toLowerCase().includes(licenseSearch.toLowerCase()) ||
        (l.license ?? '').toLowerCase().includes(licenseSearch.toLowerCase())
    ));

    onMount(() => {
        let cleanup = () => {};
        (async () => {
        const savedProviders = await getSetting('providers');
        if (savedProviders) {
            const merged = DEFAULT_PROVIDERS.map(def => {
                const saved = (savedProviders as LLMProvider[]).find(p => p.id === def.id);
                return saved ? { ...def, ...saved } : def;
            });
            providers = merged;
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
        authorSortEnabled = await getSetting('authorSortEnabled', false);
        noThinking = await getSetting('noThinking', true);
        autoSpeakReplies = await getSetting('autoSpeakReplies', false);
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
        indexRerankerModel = await getSetting('indexRerankerModel', '') as any;
        indexRerankerTopN  = await getSetting('indexRerankerTopN', 50) as number;
        indexModelCacheDir = await getSetting('indexModelCacheDir', '');
        indexMatryoshkaDim = await getSetting('indexMatryoshkaDim', 0) as number;
        indexDataDir       = await getSetting('indexDataDir', '');
        // Sync saved config into the backend
        try {
            await invoke('index_get_config').then(() => {}).catch(() => {});
        } catch { /* index not yet wired */ }
        // Check if index is already initialized (e.g. after navigating back to Settings)
        try {
            const ready = await invoke<boolean>('index_is_ready');
            if (ready) indexStatus = 'ok';
        } catch { /* command not available */ }

        try {
            const resp = await fetch('/licenses.json');
            const raw = await resp.json();
            if (Array.isArray(raw)) {
                // Legacy shape: bare array of license entries.
                automatedLicenses = raw;
                licensesGeneratedAt = null;
            } else if (raw && Array.isArray(raw.licenses)) {
                automatedLicenses = raw.licenses;
                licensesGeneratedAt = raw.generatedAt ?? null;
            } else {
                console.warn('Unexpected licenses.json shape', raw);
            }
        } catch(e) { console.error('Failed to load automated licenses', e); }

        checkMlxModelsCache();
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

            cleanup = () => {
                unlistenMlx();
                unlistenSidecar();
                unlistenSidecarFailed();
                unlistenSidecarLog();
                unlistenOllamaReady();
                unlistenOllamaFailed();
                unlistenOllamaLog();
                unlistenIndexProgress();
            };
        })();
        return () => cleanup();
    });

    // Tesseract Management
    let tesseractModels = $state<{ id: string; name: string; isDownloaded: boolean }[]>([
        { id: 'eng', name: 'English', isDownloaded: true },
        { id: 'deu', name: 'German', isDownloaded: true },
        { id: 'fra', name: 'French', isDownloaded: false },
        { id: 'spa', name: 'Spanish', isDownloaded: false },
        { id: 'ita', name: 'Italian', isDownloaded: false },
    ]);

    // Save all settings without showing the "Gespeichert!" badge
    async function saveSettingsSilent() {
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
        await saveSetting('authorSortEnabled', authorSortEnabled);
        await saveSetting('noThinking', noThinking);
        await saveSetting('autoSpeakReplies', autoSpeakReplies);
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
        await saveSetting('indexRerankerModel', indexRerankerModel);
        await saveSetting('indexRerankerTopN',  indexRerankerTopN);
        await saveSetting('indexModelCacheDir', indexModelCacheDir);
        await saveSetting('indexMatryoshkaDim', indexMatryoshkaDim);
        await saveSetting('indexDataDir',       indexDataDir);
        llmClient.setKeys(providers.reduce((acc, p) => ({ ...acc, [p.id]: p.apiKey }), {}));
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
        return b === 'remote' ? 'remote' : 'local';
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
        }[m] ?? 'bge-m3';
    }
    function indexDeviceToRust(d: string): string {
        return { auto: 'auto', cpu: 'cpu', metal: 'metal', cuda: 'cuda' }[d] ?? 'auto';
    }

    async function applyIndexConfig() {
        await saveSettingsSilent();
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
                    reranker_model:   rerankerToRust(indexRerankerModel),
                    rerank_top_n:     Number(indexRerankerTopN) || 50,
                    model_cache_dir:  indexModelCacheDir.trim() || null,
                    matryoshka_dim:   (indexEmbedderBackend === 'gguf' && Number(indexMatryoshkaDim) > 0)
                        ? Number(indexMatryoshkaDim)
                        : null,
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

    async function pickIndexModelCacheDir() {
        const selected = await openDialog({ directory: true, multiple: false });
        if (selected) { indexModelCacheDir = selected as string; }
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
        llmClient.setKeys(providers.reduce((acc, p) => ({ ...acc, [p.id]: p.apiKey }), {}));
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
            const models = await llmClient.fetchModels(selectedProvider.id, selectedProvider.apiKey, selectedProvider.baseUrl);
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

    async function handleTestConnection() {
        testingConnection = true;
        testResult = null;
        try {
            const prompt = "Reply with 'OK' and nothing else.";
            const response = await llmClient.query(selectedProvider.id, selectedProvider.selectedModel, prompt, selectedProvider.apiKey);
            testResult = { success: true, message: `Connected! Response: ${response}` };
        } catch (e: any) {
            testResult = { success: false, message: e.message };
        } finally {
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
        if (benchProviders.length === 0) return alert('Select at least one provider.');
        if (benchDocuments.length === 0 && benchPromptMode === 'batch') return alert('Add documents to the batch or use a custom prompt.');
        
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
                for (let i = 0; i < benchRuns; i++) {
                    const start = Date.now();
                    try {
                        const response = await llmClient.query(pid, model, prompt, prov.apiKey);
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
                <Globe size={16} /> {i18n.t.settings.general}
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'llm'} onclick={() => selectedProviderId = 'llm'}>
                <Zap size={16} /> {i18n.t.settings.llm_options}
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'bench'} onclick={() => selectedProviderId = 'bench'}>
                <Beaker size={16} /> {i18n.t.settings.benchmark.title}
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'index'} onclick={() => selectedProviderId = 'index'}>
                <Search size={16} /> {i18n.t.settings.index.title}
                {#if indexStatus === 'ok'}<CheckCircle2 size={12} style="color:#22c55e; margin-left:auto;" />{/if}
                {#if indexStatus === 'error'}<AlertCircle size={12} style="color:#ef4444; margin-left:auto;" />{/if}
            </button>
            <button class="provider-btn" class:active={selectedProviderId === 'about'} onclick={() => selectedProviderId = 'about'}>
                <Info size={16} /> {i18n.t.settings.about}
            </button>

            <div class="sidebar-divider"></div>
            <h2>{i18n.t.settings.providers}</h2>
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
                    <input id="export-path-input" type="text" bind:value={exportPath} placeholder="Path..." />
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
                <div class="checkbox-group">
                    <input id="save-txt-check" type="checkbox" bind:checked={saveTxt} />
                    <label for="save-txt-check"><FileText size={16} /> {i18n.t.settings.save_txt}</label>
                </div>
                <p class="hint">{i18n.t.settings.save_txt_hint}</p>
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
                <label for="max-chars-input">{i18n.t.settings.llm_max_chars}</label>
                <input id="max-chars-input" type="number" bind:value={llmMaxChars} step="500" min="500" />
                <p class="hint">{i18n.t.settings.llm_max_chars_hint}</p>
            </div>

            <div class="section-card">
                <label for="pdf-engine-select">{i18n.t.settings.pdf_engine}</label>
                <div class="toggle-group" id="pdf-engine-select">
                    <button class:active={pdfBackend === 'js'} class="toggle-btn" onclick={() => pdfBackend = 'js'}>{i18n.t.settings.pdf_engine_js}</button>
                    <button class:active={pdfBackend === 'rust'} class="toggle-btn" onclick={() => pdfBackend = 'rust'}>{i18n.t.settings.pdf_engine_rust}</button>
                </div>
                <p class="hint">{i18n.t.settings.pdf_engine_hint}</p>
            </div>

            <div class="section-card">
                <div class="header" style="margin-bottom: 12px;">
                    <h2 style="font-size: 1rem; color: #a1a1aa;"><Scan size={16} /> {i18n.t.settings.ocr_tesseract_title}</h2>
                </div>
                <div class="checkbox-group">
                    <input id="ocr-enabled-check" type="checkbox" bind:checked={ocrEnabled} />
                    <label for="ocr-enabled-check">{i18n.t.settings.ocr_enabled}</label>
                </div>
                <p class="hint" style="margin-bottom: 16px;">{i18n.t.settings.ocr_tesseract_hint}</p>
                
                <div class="models-grid">
                    {#each tesseractModels as model}
                        <div class="local-model-row">
                            <div class="model-info">
                                <div class="model-title-line">
                                    <strong>{model.name}</strong>
                                    <span class="size-badge" style="font-size: 0.6rem;">{model.id}</span>
                                </div>
                                <span class="model-path">{model.isDownloaded ? 'Language pack ready' : 'Not installed'}</span>
                            </div>
                            <div class="model-status">
                                {#if model.isDownloaded}
                                    <span class="save-badge" style="color: #10b981;"><Check size={14} /> Installed</span>
                                {:else}
                                    <button class="action-btn small primary">
                                        <Download size={14} /> Download
                                    </button>
                                {/if}
                            </div>
                        </div>
                    {/each}
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
                                        <button class="icon-btn-tiny" onclick={() => rrMoveUp(rrPos)} disabled={rrPos === 0} title="Move up">▲</button>
                                        <button class="icon-btn-tiny" onclick={() => rrMoveDown(rrPos)} disabled={rrPos === roundRobinProviders.length - 1} title="Move down">▼</button>
                                    </div>
                                {/if}
                            </div>
                        {/each}
                    </div>
                {/if}
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
                                <select id="bench-model-select-{p.id}" class="bench-model-select" bind:value={benchModels[p.id]} disabled={!benchProviders.includes(p.id)} aria-label="Select benchmark model">
                                    <option value="">(Select model)</option>
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
                                <div class="empty-docs">No documents in current batch.</div>
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
                            <textarea id="bench-custom-prompt" class="bench-prompt-input" bind:value={benchCustomPrompt} rows="3" aria-label="Custom benchmark prompt"></textarea>
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
                                <th>Provider</th>
                                <th class="bench-num">Avg Latency</th>
                                <th class="bench-num">Runs</th>
                                <th>Details</th>
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
                                    <td><button class="bench-view-btn" onclick={() => benchModal = { title: `${res.providerName} — ${res.model}`, runs: res.runs }}>View</button></td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </div>
            {/if}

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
                </select>
            </div>

            {#if indexBackendType === 'remote'}
            <!-- Remote URL + Key -->
            <div class="section-card">
                <label for="index-remote-url">{i18n.t.settings.index.remote_url}</label>
                <input id="index-remote-url" type="text" bind:value={indexRemoteUrl}
                    placeholder={i18n.t.settings.index.remote_url_placeholder} />
                <label for="index-remote-key" style="margin-top:10px;"><Key size={14} /> {i18n.t.settings.index.remote_api_key}</label>
                <input id="index-remote-key" type="password" bind:value={indexRemoteApiKey} placeholder="••••••••" />
            </div>
            {/if}

            <!-- Embedder model -->
            <div class="section-card">
                <label for="index-model-select"><Cpu size={16} /> {i18n.t.settings.index.embedder_model}</label>
                <select id="index-model-select" value={indexEmbedderModel} onchange={handleEmbedderChange} class="styled-select">
                    <option value="bge_m3">{i18n.t.settings.index.model_bge_m3}</option>
                    <optgroup label="PIXIE-Rune-v1.0 (cstr/PIXIE-Rune-v1.0-ONNX)">
                        <option value="pixie_q">{i18n.t.settings.index.model_pixie_q}</option>
                        <option value="pixie_int4">{i18n.t.settings.index.model_pixie_int4}</option>
                        <option value="pixie_int4_full">{i18n.t.settings.index.model_pixie_int4_full}</option>
                        <option value="pixie">{i18n.t.settings.index.model_pixie}</option>
                    </optgroup>
                    <optgroup label="Snowflake Arctic Embed L v2.0">
                        <option value="snowflake_l">{i18n.t.settings.index.model_snowflake_l}</option>
                        <option value="snowflake_l_int8">{i18n.t.settings.index.model_snowflake_l_int8}</option>
                        <option value="snowflake_l_fp16">{i18n.t.settings.index.model_snowflake_l_fp16}</option>
                        <option value="snowflake_l_q4">{i18n.t.settings.index.model_snowflake_l_q4}</option>
                        <option value="snowflake_l_q4f16">{i18n.t.settings.index.model_snowflake_l_q4f16}</option>
                        <option value="snowflake_l_o4">{i18n.t.settings.index.model_snowflake_l_o4}</option>
                        <option value="snowflake_l_fp32">{i18n.t.settings.index.model_snowflake_l_fp32}</option>
                    </optgroup>
                    <option value="octen">{i18n.t.settings.index.model_octen}</option>
                    <option value="jina_nano">{i18n.t.settings.index.model_jina_nano}</option>
                    <option value="multilingual_mini_lm">{i18n.t.settings.index.model_mini_lm}</option>
                    <optgroup label="Multilingual E5 (intfloat)">
                        <option value="multilingual_e5_small">{i18n.t.settings.index.model_multilingual_e5_small}</option>
                        <option value="multilingual_e5_base">{i18n.t.settings.index.model_multilingual_e5_base}</option>
                        <option value="multilingual_e5_large">{i18n.t.settings.index.model_multilingual_e5_large}</option>
                    </optgroup>
                    <optgroup label="BGE en-v1.5 (BAAI)">
                        <option value="bge_small_en_v15">{i18n.t.settings.index.model_bge_small_en_v15}</option>
                        <option value="bge_base_en_v15">{i18n.t.settings.index.model_bge_base_en_v15}</option>
                        <option value="bge_large_en_v15">{i18n.t.settings.index.model_bge_large_en_v15}</option>
                    </optgroup>
                    <optgroup label="GTE en-v1.5 (Alibaba)">
                        <option value="gte_base_en_v15">{i18n.t.settings.index.model_gte_base_en_v15}</option>
                        <option value="gte_large_en_v15">{i18n.t.settings.index.model_gte_large_en_v15}</option>
                    </optgroup>
                    <option value="nomic_embed_v15">{i18n.t.settings.index.model_nomic_embed_v15}</option>
                    <option value="mxbai_large_v1">{i18n.t.settings.index.model_mxbai_large_v1}</option>
                    <option value="minilm_l6_v2">{i18n.t.settings.index.model_minilm_l6_v2}</option>
                    <option value="embedding_gemma_300m">{i18n.t.settings.index.model_embedding_gemma_300m}</option>
                </select>

                {#if supportsGguf(indexEmbedderModel)}
                    <label for="index-backend-select" style="margin-top:10px;">
                        <Cpu size={14} /> Inference Backend
                    </label>
                    <select id="index-backend-select" bind:value={indexEmbedderBackend} class="styled-select">
                        <option value="onnx">ONNX (fastembed / ORT)</option>
                        <option value="gguf">GGUF (CrispEmbed, experimental)</option>
                    </select>
                    <div style="font-size: 12px; color: var(--muted, #888); margin-top: 4px;">
                        GGUF reuses the llama.cpp GPU backends (Vulkan/Metal/CUDA) — smaller files, unified GPU stack. Only available for models with a verified GGUF equivalent.
                    </div>

                    {#if indexEmbedderBackend === 'gguf'}
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
                {/if}
            </div>

            <!-- Compute device -->
            <div class="section-card">
                <label for="index-device-select"><Zap size={16} /> {i18n.t.settings.index.device}</label>
                <select id="index-device-select" bind:value={indexDevice} class="styled-select">
                    <option value="auto">{i18n.t.settings.index.device_auto}</option>
                    <option value="cpu">{i18n.t.settings.index.device_cpu}</option>
                    {#if isMacOS}<option value="metal">{i18n.t.settings.index.device_metal}</option>{/if}
                    <option value="cuda">{i18n.t.settings.index.device_cuda}</option>
                </select>
            </div>

            <!-- Reranker (cross-encoder, GGUF-only via CrispEmbed) -->
            <div class="section-card">
                <label for="index-reranker-select"><Cpu size={16} /> {i18n.t.settings.index.reranker_model}</label>
                <select id="index-reranker-select" bind:value={indexRerankerModel} class="styled-select">
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
                {/if}
            </div>

            <!-- Model cache directory (shared by ONNX + GGUF + reranker downloads) -->
            <div class="section-card">
                <label for="index-model-cache-dir">
                    <FolderOpen size={16} /> {i18n.t.settings.index.model_cache_dir}
                </label>
                <div class="input-with-action">
                    <input id="index-model-cache-dir" type="text" bind:value={indexModelCacheDir}
                        placeholder="(default: app data dir / models)" />
                    <button class="action-btn small" onclick={pickIndexModelCacheDir}>{i18n.t.settings.browse}</button>
                </div>
                <p class="hint">{i18n.t.settings.index.model_cache_dir_hint}</p>
            </div>

            <!-- Data directory -->
            {#if indexBackendType === 'local'}
            <div class="section-card">
                <label for="index-data-dir"><FolderOpen size={16} /> {i18n.t.settings.index.data_dir}</label>
                <div class="input-with-action">
                    <input id="index-data-dir" type="text" bind:value={indexDataDir}
                        placeholder="(app data dir)" />
                    <button class="action-btn small" onclick={pickIndexDataDir}>{i18n.t.settings.browse}</button>
                </div>
                <p class="hint">{i18n.t.settings.index.data_dir_hint}</p>
            </div>
            {/if}

            <!-- Apply button + status -->
            <div class="section-card">
                <button class="save-btn" onclick={applyIndexConfig}
                    disabled={indexStatus === 'loading'}>
                    {#if indexStatus === 'loading'}<Loader2 size={16} class="spin" /> Initialisiere …
                    {:else}<Play size={16} /> {i18n.t.settings.index.apply}{/if}
                </button>
                <p class="hint">{i18n.t.settings.index.apply_hint}</p>

                {#if indexStatus === 'loading' && indexInitProgress}
                    <div class="init-progress-wrap">
                        <p class="init-progress-label"><Loader2 size={13} class="spin" /> {indexInitProgress}</p>
                        <div class="init-progress-bar">
                            <div class="init-progress-fill" style="width:{indexInitPct}%"></div>
                        </div>
                        <p class="init-progress-note">Beim ersten Start wird das Embedder-Modell heruntergeladen (~500 MB). Bitte warten …</p>
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

        {:else if selectedProviderId === 'about'}
            <div class="header">
                <h1>{i18n.t.settings.about}</h1>
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
                <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:16px;">
                    <h3>{i18n.t.settings.legal.licenses}</h3>
                    <div class="search-box small" style="background:#09090b; border:1px solid #27272a; border-radius:6px; padding:0 10px; width:240px; display:flex; align-items:center;">
                        <Search size={14} style="color:#71717a; margin-right:8px;" />
                        <input type="text" id="license-search-input" bind:value={licenseSearch} placeholder={i18n.t.settings.legal.search_licenses.replace('{count}', String(automatedLicenses.length))} style="border:none; background:transparent; color:white; padding:6px 0; font-size:0.75rem; width:100%; outline:none;" aria-label="Search licenses" />
                    </div>
                </div>
                <div class="license-list-scrollable">
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
                                    <button class="inline-link" onclick={() => opener.openUrl(lib.link)}>Source</button>
                                {/if}
                            </div>
                        </div>
                    {/each}
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
                        <button class="action-btn small" onclick={handleRefreshModels} disabled={loadingModels} aria-label="Refresh models">
                            <RefreshCw size={14} class={loadingModels ? "loader-spin" : ""} />
                        </button>
                    {/if}
                </div>
            </div>
            {/if}

            {#if selectedProvider.id !== 'mistralrs'}
                {#if selectedProvider.id === 'ollama' && selectedProvider.selectedModel}
                    <p class="hint" style="margin: 8px 0 4px;">Active model: <strong>{selectedProvider.selectedModel}</strong></p>
                {/if}
                <div class="actions">
                    <button class="action-btn test-btn" onclick={handleTestConnection} disabled={testingConnection || !selectedProvider.selectedModel}>
                        <span class={testingConnection ? "loader-spin" : ""}>
                            {#if testingConnection}<Loader2 size={16} />{:else}<CheckCircle size={16} />{/if}
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
                                <span class="save-badge" style="color:#f59e0b;"><Loader2 size={14} /> Starting...</span>
                            {:else if ollamaStatus === 'ready'}
                                <span class="save-badge" style="color:#10b981;"><CheckCircle size={14} /> Running</span>
                            {:else if ollamaStatus === 'error'}
                                <span class="save-badge" style="color:#ef4444;"><XCircle size={14} /> Failed</span>
                            {/if}
                            {#if ollamaRunning}
                                <button class="action-btn small danger" onclick={stopOllamaService}>
                                    <Square size={14} /> Stop
                                </button>
                            {:else}
                                <button class="action-btn small success" onclick={startOllamaService} disabled={ollamaStatus === 'starting'}>
                                    <Rocket size={14} /> Start Ollama
                                </button>
                            {/if}
                            <button class="action-btn small" onclick={handleRefreshModels} disabled={loadingModels} aria-label="Fetch installed Ollama models">
                                <RefreshCw size={14} class={loadingModels ? "loader-spin" : ""} /> Fetch Installed
                            </button>
                        </div>
                    </div>
                    {#if ollamaLogs.length > 0}
                        <div style="margin-bottom: 14px;">
                            <button class="action-btn small" style="color:#71717a; border:none; background:none; padding:0; font-size:0.75rem; font-weight:700; gap:6px;" onclick={() => ollamaLogsVisible = !ollamaLogsVisible}>
                                OLLAMA LOGS
                                {#if ollamaLogsVisible}<ChevronUp size={14} />{:else}<ChevronDown size={14} />{/if}
                            </button>
                            {#if ollamaLogsVisible}
                                <div style="margin-top: 8px; position: relative;">
                                    <textarea bind:this={ollamaLogEl} readonly class="log-viewer" value={ollamaLogs.join('\n')} rows="8" aria-label="Ollama Logs"></textarea>
                                    <button class="log-clear-btn" onclick={() => ollamaLogs = []} title="Clear log"><Trash2 size={12} /></button>
                                </div>
                            {/if}
                        </div>
                    {/if}
                    
                    <div class="form-group" style="margin-top: 20px;">
                        <label for="ollama-custom-id">Custom Model Tag</label>
                        <div class="input-with-action">
                            <input type="text" id="ollama-custom-id" placeholder="e.g. llama3:8b" bind:value={ollamaCustomInput} />
                            <button class="action-btn small" onclick={addCustomOllamaModel} disabled={!ollamaCustomInput.trim()}>
                                <Plus size={14} /> Add/Pull
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
                                    <span class="model-path">{model.isInstalled ? 'Installed' : 'Not installed'}</span>
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
                                            <Download size={14} /> Pull
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
                            <input id="mlx-port-input" type="number" bind:value={mlxPort} style="width: 80px;" aria-label="MLX Port" />    
                            {#if mlxRunning}
                                <button class="action-btn small danger" onclick={stopMlxServer}>
                                    <Square size={14} /> Stop MLX
                                </button>
                            {:else}
                                <button class="action-btn small success" onclick={startMlxServer}>
                                    <Rocket size={14} /> Start MLX
                                </button>
                            {/if}
                            <button class="action-btn small" onclick={checkMlxModelsCache} title="Refresh cache status">
                                <RefreshCw size={14} /> {i18n.t.batch.reanalyze_run}
                            </button>
                        </div>
                    </div>
                    <p class="hint">{i18n.t.settings.mlx_manager_hint}</p>
                    <p class="hint">{i18n.t.settings.mlx_cache_label}: <code>{mlxCacheDir}</code></p>

                    <div class="form-group" style="margin-top: 20px;">
                        <label for="mlx-custom-id">Custom HF Repo ID or local path</label>
                        <div class="input-with-action">
                            <input type="text" id="mlx-custom-id" placeholder="e.g. mlx-community/Mistral-7B-Instruct-v0.3-4bit" bind:value={mlxCustomInput} />
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
                                        <button class="icon-btn danger" onclick={() => deleteMlxModelFromDisk(model)} title="Delete from disk"><Trash2 size={14} /></button>
                                    {/if}
                                    <button class="icon-btn" onclick={() => removeMlxModel(model.id)} title="Remove from list"><XCircle size={14} /></button>
                                </div>
                            </div>
                        {/each}
                    </div>
                </div>

                <div class="section-card" style="margin-top: 16px;">
                    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                    <div class="section-toggle-flat" onclick={() => mlxLogsVisible = !mlxLogsVisible} role="button" tabindex="0" onkeydown={e => e.key === 'Enter' && (mlxLogsVisible = !mlxLogsVisible)}>        
                        <span style="display:flex; align-items:center; gap:8px;">
                            <Brain size={14} /> MLX Server Log
                            {#if mlxRunning}<span class="running-dot"></span>{/if}
                        </span>
                        <span style="display:flex; align-items:center; gap:8px;">
                            <span class="hint" style="margin:0;">{mlxLogs.length} lines</span>
                            {#if mlxLogsVisible}<ChevronUp size={14} />{:else}<ChevronDown size={14} />{/if}     
                        </span>
                    </div>
                    {#if mlxLogsVisible}
                        <div style="margin-top: 10px; position: relative;">
                            <textarea id="mlx-log-viewer" bind:this={mlxLogEl} readonly class="log-viewer" value={mlxLogs.join('\n')} rows="14" aria-label="MLX Server Logs"></textarea>
                            <button class="log-clear-btn" onclick={() => mlxLogs = []} title="Clear log"><Trash2 size={12} /></button>
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
                                    <span style="font-size:0.7rem; color:#71717a; font-weight:700;">PORT</span>
                                    <input type="number" bind:value={llamacppPort} style="width: 70px; border:none; padding:2px; height:24px; font-size:0.8125rem;" />
                                </div>
                                {#if sidecarStatus === 'starting'}
                                    <span class="save-badge" style="color:#f59e0b;"><Loader2 size={14} /> Starting...</span>
                                {:else if sidecarStatus === 'ready'}
                                    <span class="save-badge" style="color:#10b981;"><CheckCircle size={14} /> Running</span>
                                {:else if sidecarStatus === 'error'}
                                    <span class="save-badge" style="color:#ef4444;"><XCircle size={14} /> Failed</span>
                                {/if}
                                <button class="action-btn small primary" disabled={sidecarStatus === 'starting'} onclick={() => setLocalModelActive(selectedProvider.selectedModel)}>
                                    <Rocket size={14} /> {i18n.t.settings.local_manager_start}
                                </button>
                                <button class="action-btn small danger" onclick={async () => { await invoke('stop_llamacpp_sidecar'); sidecarStatus = ''; llamacppReady = false; }}>
                                    <Square size={14} /> Stop
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
                                SIDECAR LOGS
                                {#if sidecarLogsVisible}<ChevronUp size={14} />{:else}<ChevronDown size={14} />{/if}
                            </button>
                            {#if sidecarLogsVisible}
                                <div style="margin-top: 8px; position: relative;">
                                    <textarea bind:this={sidecarLogEl} readonly class="log-viewer" value={sidecarLogs.join('\n')} rows="10" aria-label="llama-server Logs"></textarea>
                                    <button class="log-clear-btn" onclick={() => sidecarLogs = []} title="Clear log"><Trash2 size={12} /></button>
                                </div>
                            {/if}
                        </div>
                    {/if}

                    <div class="form-group">
                        <label for="custom-model-id-{selectedProvider.id}">Custom HF Repo ID or URL</label>
                        <div class="input-with-action">
                            <input type="text" id="custom-model-id-{selectedProvider.id}" placeholder="REPO_ID/FILENAME.GGUF" bind:value={customModelInput} />
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
                                    <span class="model-path">{model.path || 'Not downloaded yet'}</span>
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
                                        <button class="icon-btn danger" onclick={() => removeLocalModel(i)} title="Delete file"><Trash2 size={14} /></button>
                                    {:else if model.progress === undefined}
                                        <button class="action-btn small primary" onclick={() => downloadLocalModel(i)}>
                                            <Download size={14} /> Download
                                        </button>
                                        <button class="icon-btn" onclick={() => localModels.splice(i, 1)} title="Remove from list"><XCircle size={14} /></button>
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
                <button class="bench-modal-close" onclick={() => benchModal = null} aria-label="Close benchmark modal">✕</button>
            </div>
            <div class="bench-modal-body">
                {#each benchModal.runs as run, ri}
                    <div class="bench-response-block">
                        <span class="bench-run-label">
                            Run {ri + 1} {ri === 0 ? '(cold)' : '(warm)'}
                            {#if run.error}— ERROR{:else}— {run.latencyMs.toLocaleString()} ms / {run.tokensPerSec ?? '?'} t/s{/if}
                        </span>
                        <pre class="bench-response-pre" style="max-height:none;">{run.error || run.response || '(empty response)'}</pre>
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
    .provider-btn { padding: 8px 20px; text-align: left; border: none; background: transparent; cursor: pointer; font-size: 0.875rem; color: #a1a1aa; transition: all 0.2s; display: flex; align-items: center; justify-content: space-between; width: 100%; }
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
</style>
