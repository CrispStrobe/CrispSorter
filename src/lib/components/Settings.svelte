<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import { DEFAULT_PROVIDERS, type LLMProvider, llmClient } from '../llm/client';
    import { getSetting, saveSetting } from '../store';
    import { i18n, type Language } from '../i18n.svelte';
    import { getDefaultPrompt } from '../batch/store.svelte';
    import {
        RefreshCw, CheckCircle, XCircle, Key, Globe, Cpu,
        Loader2, FolderOpen, Save, Languages, MessageSquare,
        Scan, Edit, Zap, Trash2, Download, Plus, HardDrive, Code,
        Rocket, FileText, Brain, Square, ChevronUp, ChevronDown, Info
    } from 'lucide-svelte';
    import { open as openDialog, save } from '@tauri-apps/plugin-dialog';
    import * as opener from '@tauri-apps/plugin-opener';
    import { stat, remove } from '@tauri-apps/plugin-fs';
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';
    import { fetch as tauriFetch } from '@tauri-apps/plugin-http';

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
        repoId: string;   // HF repo ID or local path passed to mlx_lm.server
        name: string;
        params: string;   // e.g. "3.3B", "9B"
        vision?: boolean;
    }

    const DEFAULT_MLX_MODELS: MlxModel[] = [
        // Stable models — work with any recent mlx-lm release
        // ~2 GB
        { id: 'llama32-3b',         repoId: 'mlx-community/Llama-3.2-3B-Instruct-4bit',               name: 'Llama 3.2 3B',           params: '3B' },
        { id: 'ministral-3b',       repoId: 'mlx-community/Ministral-3-3B-Instruct-2512-4bit',         name: 'Ministral 3.3B',         params: '3.3B' },
        { id: 'phi35-mini',         repoId: 'mlx-community/Phi-3.5-mini-instruct-4bit',                name: 'Phi-3.5 Mini',           params: '3.8B' },
        // ~2.5–4 GB
        { id: 'gemma3-4b',          repoId: 'mlx-community/gemma-3-4b-it-4bit',                        name: 'Gemma 3 4B',             params: '4B' },
        { id: 'mistral-7b',         repoId: 'mlx-community/Mistral-7B-Instruct-v0.3-4bit',             name: 'Mistral 7B v0.3',        params: '7B' },
        { id: 'ministral-8b',       repoId: 'mlx-community/Ministral-3-8B-Instruct-2512-4bit',         name: 'Ministral 8B',           params: '8B' },
        // 14B+
        { id: 'phi4',               repoId: 'mlx-community/Phi-4-4bit',                                name: 'Phi-4 14B',              params: '14B' },
        // Qwen 3.5 — requires mlx-lm >= 0.31.0 (pip install -U mlx-lm)
        { id: 'qwen35-0.8b-4bit',   repoId: 'mlx-community/Qwen3.5-0.8B-4bit',                        name: 'Qwen 3.5 0.8B ⚠',        params: '0.8B' },
        { id: 'qwen35-0.8b-optiq',  repoId: 'mlx-community/Qwen3.5-0.8B-OptiQ-4bit',                  name: 'Qwen 3.5 0.8B OptiQ ⚠',  params: '0.8B' },
        { id: 'qwen35-2b-optiq',    repoId: 'mlx-community/Qwen3.5-2B-OptiQ-4bit',                    name: 'Qwen 3.5 2B OptiQ ⚠',    params: '2B' },
        { id: 'qwen35-4b-4bit',     repoId: 'mlx-community/Qwen3.5-4B-4bit',                          name: 'Qwen 3.5 4B ⚠',          params: '4B' },
        { id: 'qwen35-4b-optiq',    repoId: 'mlx-community/Qwen3.5-4B-OptiQ-4bit',                    name: 'Qwen 3.5 4B OptiQ ⚠',    params: '4B' },
        { id: 'qwen35-9b-optiq',    repoId: 'mlx-community/Qwen3.5-9B-OptiQ-4bit',                    name: 'Qwen 3.5 9B OptiQ ⚠',    params: '9B' },
    ];

    let providers = $state<LLMProvider[]>(JSON.parse(JSON.stringify(DEFAULT_PROVIDERS)));
    let selectedProviderId = $state('global'); 
    let selectedProvider = $derived(providers.find(p => p.id === selectedProviderId) || providers[0]);

    // Global App Settings
    let activeProviderId = $state('ollama');
    let exportPath = $state('');
    let exportPathMode = $state<'absolute' | 'relative'>('absolute');
    let saveTxt = $state(true);
    let currentLanguage = $state<Language>('en');
    
    // LLM & OCR Settings
    let llmMaxChars = $state(5000);
    let llmContextLimit = $state(4096);
    let llmPrompt = $state(''); // Loaded from store
    let ocrEnabled = $state(false);
    let authorSortEnabled = $state(false);
    let noThinking = $state(true);
    let pdfBackend = $state<'js' | 'rust'>('js');
    let parsingFormat = $state<'xml' | 'json'>('xml');

    // mistral.rs / Local Model Management
    let localModels = $state<LocalModel[]>([
        {
            id: 'qwen3-0.6b',
            name: 'Qwen 3 0.6B (Q4_K_M)',
            path: '',
            isDownloaded: false,
            isActive: true,
            downloadUrl: 'https://huggingface.co/Mungert/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-q4_k_m.gguf'
        },
        {
            id: 'ministral-3b',
            name: 'Ministral 3B (Q4_K_M)',
            path: '',
            isDownloaded: false,
            isActive: false,
            downloadUrl: 'https://huggingface.co/bartowski/Ministral-3b-instruct-GGUF/resolve/main/Ministral-3b-instruct-Q4_K_M.gguf'
        }
    ]);
    let loadingModels = $state(false);
    let testingConnection = $state(false);
    let testResult = $state<{ success: boolean; message: string } | null>(null);
    let mlxPort = $state(8000);
    let mlxRunning = $state(false);
    let mlxReady = $state(false);
    let saveIndicator = $state(false);
    let customModelInput = $state('');

    // MLX Model Management
    let mlxModels = $state<MlxModel[]>(DEFAULT_MLX_MODELS.map(m => ({ ...m })));
    let mlxCustomInput = $state('');
    let mlxLogs = $state<string[]>([]);
    let mlxLogsVisible = $state(false);
    let mlxModelCached = $state<Record<string, boolean>>({});
    let mlxCacheDir = $state('~/.cache/huggingface/hub');
    let mlxLogEl = $state<HTMLTextAreaElement>();

    // Consolidate current provider models
    let availableModels = $derived.by(() => {
        if (['mistralrs', 'llamacpp'].includes(selectedProviderId)) {
            return localModels.filter(m => m.isDownloaded).map(m => m.path);
        }
        if (selectedProviderId === 'mlx') {
            return mlxModels.map(m => m.repoId);
        }
        return selectedProvider.models;
    });

    async function addCustomModel() {
        if (!customModelInput.trim()) return;
        const input = customModelInput.trim();
        const fileName = input.split(/[\\/]/).pop() || 'Custom Model';
        const id = 'custom-' + Date.now();
        
        localModels.push({
            id,
            name: fileName,
            path: input,
            isDownloaded: true, // We treat remote IDs as ready-to-load
            isActive: false,
            downloadUrl: input.startsWith('http') ? input : undefined
        });
        
        customModelInput = '';
        await saveSetting('localModels', localModels);
        saveIndicator = true;
        setTimeout(() => saveIndicator = false, 2000);
    }

    async function deleteLocalModel(id: string) {
        localModels = localModels.filter(m => m.id !== id);
        await saveSetting('localModels', localModels);
        saveIndicator = true;
        setTimeout(() => saveIndicator = false, 2000);
    }

    function setMlxModelActive(repoId: string) {
        selectedProvider.selectedModel = repoId;
        saveSetting('providers', $state.snapshot(providers));
    }

    async function addMlxModel() {
        if (!mlxCustomInput.trim()) return;
        const input = mlxCustomInput.trim();
        const name = input.split('/').pop() || input;
        mlxModels.push({ id: 'custom-' + Date.now(), repoId: input, name, params: '?' });
        mlxCustomInput = '';
        await saveSetting('mlxModels', $state.snapshot(mlxModels));
        saveIndicator = true;
        setTimeout(() => saveIndicator = false, 2000);
    }

    async function removeMlxModel(id: string) {
        mlxModels = mlxModels.filter(m => m.id !== id);
        await saveSetting('mlxModels', $state.snapshot(mlxModels));
        saveIndicator = true;
        setTimeout(() => saveIndicator = false, 2000);
    }

    async function checkMlxModelsCache() {
        const repoIds = mlxModels.map(m => m.repoId);
        const results = await invoke<boolean[]>('check_mlx_models_cached', { repoIds }).catch(() => repoIds.map(() => false));
        const map: Record<string, boolean> = {};
        mlxModels.forEach((m, i) => { map[m.id] = results[i]; });
        mlxModelCached = map;
    }

    async function deleteMlxModelFromDisk(model: MlxModel) {
        if (!confirm(`Delete "${model.name}" from disk?\n${mlxCacheDir}/models--${model.repoId.replaceAll('/', '--')}`)) return;
        try {
            const msg = await invoke<string>('delete_mlx_model', { repoId: model.repoId });
            mlxModelCached = { ...mlxModelCached, [model.id]: false };
            alert(msg);
        } catch (e: any) {
            alert(`Delete failed: ${e}`);
        }
    }

    onMount(async () => {
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
        saveTxt = await getSetting('saveTxt', true);
        currentLanguage = await getSetting('language', 'en') as Language;
        llmMaxChars = await getSetting('llmMaxChars', 5000);
        llmContextLimit = await getSetting('llmContextLimit', 4096);
        parsingFormat = await getSetting('parsingFormat', 'xml') as 'xml' | 'json';
        llmPrompt = await getSetting('llmPrompt', getDefaultPrompt(parsingFormat, currentLanguage));
        ocrEnabled = await getSetting('ocrEnabled', false);
        authorSortEnabled = await getSetting('authorSortEnabled', false);
        noThinking = await getSetting('noThinking', true);
        llmClient.noThinking = noThinking;
        pdfBackend = await getSetting('pdfBackend', 'js') as 'js' | 'rust';
        
        const savedLocalModels = await getSetting('localModels');
        if (savedLocalModels) {
            const saved = savedLocalModels as LocalModel[];
            localModels = localModels.map(def => {
                const s = saved.find(m => m.id === def.id);
                return s ? { ...def, ...s } : def;
            });
            saved.forEach(s => {
                if (!localModels.find(m => m.id === s.id)) localModels.push(s);
            });
        }
        
        await updateLocalModelSizes();

        const savedMlxModels = await getSetting('mlxModels');
        if (savedMlxModels) {
            const saved = savedMlxModels as MlxModel[];
            mlxModels = DEFAULT_MLX_MODELS.map(def => {
                const s = saved.find(m => m.id === def.id);
                return s ? { ...def, ...s } : def;
            });
            saved.forEach(s => {
                if (!mlxModels.find(m => m.id === s.id)) mlxModels.push(s);
            });
        }

        mlxCacheDir = await invoke<string>('get_mlx_cache_dir').catch(() => '~/.cache/huggingface/hub');
        checkMlxModelsCache();
        loadBenchBatchItems();
        loadAutomatedLicenses();

        listen('mlx-log', (event: any) => {
            mlxLogs = [...mlxLogs.slice(-499), event.payload as string];
            if (mlxLogEl) { mlxLogEl.scrollTop = mlxLogEl.scrollHeight; }
        }).then(fn => { unlistenMlx = fn; });

        listen('mlx-ready', () => {
            mlxReady = true;
            mlxLogs = [...mlxLogs, '[CrispSorter] Server ready — accepting requests'];
        }).then(fn => { unlistenMlxReady = fn; });

        listen('download-progress', (event: any) => {
            const { id, received, total } = event.payload;
            const model = localModels.find(m => m.id === id);
            if (model) {
                model.progress = Math.round((received / total) * 100);
            }
        }).then(fn => { unlistenDownload = fn; });
    });

    let unlistenDownload: (() => void) | undefined;
    let unlistenMlx: (() => void) | undefined;
    let unlistenMlxReady: (() => void) | undefined;
    onDestroy(() => { unlistenDownload?.(); unlistenMlx?.(); unlistenMlxReady?.(); });

    async function updateLocalModelSizes() {
        for (let m of localModels) {
            if (m.path) {
                try {
                    const s = await stat(m.path);
                    m.size = (s.size / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
                    m.isDownloaded = true;
                } catch {
                    m.isDownloaded = false;
                    m.size = undefined;
                }
            }
        }
    }

    async function handleSave() {
        await saveSetting('providers', $state.snapshot(providers));
        await saveSetting('activeProviderId', activeProviderId);
        await saveSetting('exportPath', exportPath);
        await saveSetting('exportPathMode', exportPathMode);
        await saveSetting('saveTxt', saveTxt);
        await saveSetting('language', currentLanguage);
        await saveSetting('llmMaxChars', llmMaxChars);
        await saveSetting('llmContextLimit', llmContextLimit);
        await saveSetting('llmPrompt', llmPrompt);
        await saveSetting('ocrEnabled', ocrEnabled);
        await saveSetting('authorSortEnabled', authorSortEnabled);
        await saveSetting('noThinking', noThinking);
        llmClient.noThinking = noThinking;
        await saveSetting('pdfBackend', pdfBackend);
        await saveSetting('parsingFormat', parsingFormat);
        await saveSetting('localModels', $state.snapshot(localModels));
        await saveSetting('mlxModels', $state.snapshot(mlxModels));
        i18n.setLanguage(currentLanguage);
        
        saveIndicator = true;
        setTimeout(() => saveIndicator = false, 2000);
    }

    async function pickExportPath() {
        const selected = await openDialog({ directory: true, multiple: false });
        if (typeof selected === 'string') exportPath = selected;
    }

    async function addLocalModel() {
        const selected = await openDialog({
            multiple: false,
            filters: [{ name: 'GGUF Models', extensions: ['gguf'] }]
        });
        if (typeof selected === 'string') {
            const name = selected.split(/[\\/]/).pop() || 'Unknown Model';
            localModels.push({ id: crypto.randomUUID(), name, path: selected, isDownloaded: true, isActive: false });
            await updateLocalModelSizes();
            await handleSave();
        }
    }

    async function removeLocalModel(index: number) {
        const model = localModels[index];
        if (model.path && confirm(`Delete model file from disk? ${model.path}`)) {
            try { await remove(model.path); } catch(e) { console.error(e); }
        }
        localModels.splice(index, 1);
        await handleSave();
    }

    async function downloadLocalModel(index: number) {
        const model = localModels[index];
        if (!model.downloadUrl) return;

        const dest = await save({
            defaultPath: model.name + '.gguf',
            filters: [{ name: 'GGUF', extensions: ['gguf'] }]
        });

        if (dest) {
            try {
                model.progress = 0;
                await invoke('download_file', { 
                    id: model.id,
                    url: model.downloadUrl,
                    path: dest
                });
                model.path = dest;
                model.isDownloaded = true;
                model.progress = undefined;
                await updateLocalModelSizes();
                await handleSave();
            } catch (error: any) {
                alert(`Download failed: ${error}`);
                model.progress = undefined;
            }
        }
    }

    async function setLocalModelActive(path: string) {
        localModels.forEach(m => m.isActive = m.path === path);
        selectedProvider.selectedModel = path;
        
        if (selectedProviderId === 'llamacpp') {
            if (!path) {
                try {
                    await invoke('stop_llamacpp_sidecar');
                } catch (e) { console.error(e); }
                return;
            }
            try {
                await invoke('start_llamacpp_sidecar', { modelPath: path });
                // Give it a moment to initialize
                setTimeout(() => {
                    alert("llama.cpp Sidecar starting with Metal acceleration! It will be ready in a few seconds.");
                }, 1000);
            } catch (e) {
                alert(`Failed to start sidecar: ${e}`);
            }
        }
        
        await handleSave();
    }

    async function setActiveProvider(id: string) {
        activeProviderId = id;
        await handleSave();
    }

    async function startMlxServer() {
        const model = selectedProvider.selectedModel;
        if (!model) { alert('Select a model first.'); return; }
        try {
            mlxRunning = true;
            mlxReady = false;
            mlxLogs = [`[CrispSorter] Starting mlx_lm.server with model: ${model}`];
            mlxLogsVisible = true;
            const msg = await invoke<string>('start_mlx_server', { modelPath: model, port: mlxPort });
            mlxLogs = [...mlxLogs, `[CrispSorter] ${msg} — waiting for model to load...`];
        } catch (e: any) {
            mlxRunning = false;
            mlxReady = false;
            mlxLogs = [...mlxLogs, `[ERROR] ${e?.message || e}`];
            alert(`MLX start failed: ${e?.message || e}`);
        }
    }

    async function stopMlxServer() {
        try {
            await invoke('stop_mlx_server');
            mlxRunning = false;
            mlxReady = false;
        } catch (e: any) {
            alert(`MLX stop failed: ${e?.message || e}`);
        }
    }

    async function resetProviders() {
        if (!confirm("Are you sure you want to reset all providers to defaults? This will clear your API keys.")) return;
        providers = JSON.parse(JSON.stringify(DEFAULT_PROVIDERS));
        await handleSave();
        alert("Providers reset to defaults.");
    }

    async function handleRefreshModels() {
        const localProviders = ['ollama', 'mistralrs', 'llamacpp', 'mlx'];
        if (!selectedProvider.apiKey && !localProviders.includes(selectedProvider.id)) {
            alert(i18n.t.settings.key_required);
            return;
        }
        loadingModels = true;
        try {
            const models = await llmClient.fetchModels(selectedProvider.id, selectedProvider.apiKey, selectedProvider.baseUrl);
            selectedProvider.models = models;
            if (models.length > 0 && !selectedProvider.selectedModel) {
                selectedProvider.selectedModel = models[0];
            }
            await handleSave();
        } catch (error: any) {
            const msg = error?.message || String(error) || 'Connection refused';
            alert(`${i18n.t.settings.fetch_failed}: ${msg}`);
        } finally {
            loadingModels = false;
        }
    }

    async function handleTestConnection() {
        const model = selectedProvider.selectedModel;
        if (!model) {
            alert(i18n.t.settings.select_model);
            return;
        }
        testingConnection = true;
        testResult = null;
        try {
            const response = await llmClient.query(selectedProvider.id, model, 'Hello!', selectedProvider.apiKey);
            testResult = { success: true, message: `Success! ${response.substring(0, 50)}...` };
        } catch (error: any) {
            testResult = { success: false, message: `Error: ${error.message}` };
        } finally {
            testingConnection = false;
        }
    }

    function switchParsingFormat(format: 'xml' | 'json') {
        parsingFormat = format;
        llmPrompt = getDefaultPrompt(format, currentLanguage);
    }

    function resetPromptToDefault() {
        llmPrompt = getDefaultPrompt(parsingFormat, currentLanguage);
    }

    // Benchmark
    interface BenchmarkRunResult {
        latencyMs: number;
        tokensPerSec: number | null;
        response: string;
        error?: string;
    }
    interface BenchmarkResult {
        providerId: string;
        providerName: string;
        model: string;
        runs: BenchmarkRunResult[];
    }
    interface BenchModal {
        title: string;
        runs: BenchmarkRunResult[];
    }
    let benchModal = $state<BenchModal | null>(null);
    let benchmarkRunning = $state(false);
    let benchmarkProgress = $state('');
    let benchmarkResults = $state<BenchmarkResult[]>([]);
    let benchNumRuns = $state(2);
    let benchPromptMode = $state<'batch' | 'custom'>('custom');
    let benchCustomPrompt = $state('List the first 10 prime numbers. Be concise.');
    let benchEnabledProviders = $state<Record<string, boolean>>({});
    let benchModelByProvider = $state<Record<string, string>>({});
    let benchBatchItems = $state<Array<{path: string; name: string; text: string}>>([]);
    let benchSelectedFiles = $state<Set<string>>(new Set());

    interface AutomatedLicense {
        name: string;
        version: string;
        license: string;
        author: string;
        link: string;
        source: 'Frontend' | 'Backend';
    }
    let automatedLicenses = $state<AutomatedLicense[]>([]);
    let licenseSearch = $state('');

    let filteredLicenses = $derived(
        automatedLicenses.filter(l => 
            l.name.toLowerCase().includes(licenseSearch.toLowerCase()) ||
            l.license.toLowerCase().includes(licenseSearch.toLowerCase()) ||
            l.author.toLowerCase().includes(licenseSearch.toLowerCase())
        )
    );

    async function loadAutomatedLicenses() {
        try {
            const res = await fetch('/licenses.json');
            if (res.ok) automatedLicenses = await res.json();
        } catch (e) { console.error('Failed to load licenses.json', e); }
    }

    function getBenchModelOptions(p: LLMProvider): string[] {
        if (['mistralrs', 'llamacpp'].includes(p.id)) return localModels.filter(m => m.isDownloaded && m.path).map(m => m.path);
        if (p.id === 'mlx') return mlxModels.map(m => m.repoId);
        return p.models;
    }
    function getBenchModel(p: LLMProvider): string {
        return benchModelByProvider[p.id] || p.selectedModel || getBenchModelOptions(p)[0] || '';
    }

    async function loadBenchBatchItems() {
        const sessions = await getSetting('sessions', {}) as Record<string, any>;
        const seen = new Set<string>();
        const items: typeof benchBatchItems = [];
        for (const session of Object.values(sessions)) {
            for (const item of (session.items || [])) {
                if (item.extractedText && item.originalPath && !seen.has(item.originalPath)) {
                    seen.add(item.originalPath);
                    items.push({ path: item.originalPath, name: item.originalName || item.originalPath.split(/[\\/]/).pop() || '', text: item.extractedText });
                }
            }
        }
        benchBatchItems = items;
    }

    function getBenchCandidates() {
        return providers.filter(p => {
            if (benchEnabledProviders[p.id] === false) return false;
            return !!getBenchModel(p);
        });
    }

    async function pollUntilReady(url: string, intervalMs: number, maxAttempts: number): Promise<boolean> {
        for (let i = 0; i < maxAttempts; i++) {
            await new Promise(r => setTimeout(r, intervalMs));
            try {
                const r = await tauriFetch(url, { method: 'GET', connectTimeout: 3000 });
                if (r.ok) return true;
            } catch { /* not ready yet */ }
        }
        return false;
    }

    async function ensureBackendRunning(providerId: string, model: string): Promise<void> {
        if (providerId === 'llamacpp') {
            const health = 'http://localhost:8080/v1/models';
            try {
                const r = await tauriFetch(health, { method: 'GET', connectTimeout: 2000 });
                if (r.ok) return;
            } catch { /* not running */ }
            benchmarkProgress = 'Starting llama.cpp sidecar…';
            await invoke('start_llamacpp_sidecar', { modelPath: model });
            const ready = await pollUntilReady(health, 1500, 20);
            if (!ready) throw new Error('llama.cpp sidecar did not become ready');
            benchmarkProgress = 'llama.cpp ready';
            return;
        }
        if (providerId === 'mlx') {
            const health = `http://localhost:${mlxPort}/v1/models`;
            try {
                const r = await tauriFetch(health, { method: 'GET', connectTimeout: 2000 });
                if (r.ok) { mlxRunning = true; mlxReady = true; return; }
            } catch { /* not running */ }
            benchmarkProgress = 'Starting MLX server — loading model, please wait…';
            mlxRunning = true;
            mlxReady = false;
            await invoke('start_mlx_server', { modelPath: model, port: mlxPort });
            // MLX can take 60+ seconds to load a model
            const ready = await pollUntilReady(health, 2000, 90);
            if (!ready) throw new Error('MLX server did not become ready within 3 min');
            mlxReady = true;
            benchmarkProgress = 'MLX ready';
            return;
        }
    }

    async function runBenchmark() {
        benchmarkRunning = true;
        benchmarkResults = [];
        const basePrompt = benchPromptMode === 'batch'
            ? getDefaultPrompt(parsingFormat, currentLanguage)
            : benchCustomPrompt;
        // Prepend selected document texts as context
        let fullPrompt = basePrompt;
        if (benchSelectedFiles.size > 0) {
            const parts = [...benchSelectedFiles].map(path => {
                const item = benchBatchItems.find(i => i.path === path);
                return item ? `--- ${item.name} ---\n${item.text.substring(0, llmMaxChars)}` : null;
            }).filter(Boolean);
            if (parts.length > 0) fullPrompt = parts.join('\n\n') + '\n\n' + basePrompt;
        }
        const candidates = getBenchCandidates();
        if (candidates.length === 0) {
            benchmarkProgress = 'No enabled providers with a model configured.';
            benchmarkRunning = false;
            return;
        }
        for (const p of candidates) {
            const model = getBenchModel(p);
            if (!model) continue;
            const shortModel = model.split(/[\\/]/).pop() || model;
            const result: BenchmarkResult = { providerId: p.id, providerName: p.name, model: shortModel, runs: [] };
            // Auto-start local servers if not running
            try {
                await ensureBackendRunning(p.id, model);
            } catch (e: any) {
                result.runs.push({ latencyMs: 0, tokensPerSec: null, response: '', error: e.message });
                benchmarkResults = [...benchmarkResults, result];
                continue;
            }
            for (let i = 0; i < benchNumRuns; i++) {
                benchmarkProgress = `Run ${i + 1}/${benchNumRuns} — ${p.name} / ${shortModel}`;
                const start = performance.now();
                try {
                    const response = await llmClient.query(p.id, model, fullPrompt, p.apiKey, 0.3);
                    const elapsed = performance.now() - start;
                    const tokens = Math.max(1, Math.round(response.length / 3.5));
                    result.runs.push({ latencyMs: Math.round(elapsed), tokensPerSec: Math.round(tokens / (elapsed / 1000)), response });
                } catch (e: any) {
                    result.runs.push({ latencyMs: Math.round(performance.now() - start), tokensPerSec: null, response: '', error: e.message || String(e) });
                }
            }
            benchmarkResults = [...benchmarkResults, result];
        }
        benchmarkProgress = `Done — ${candidates.length} provider${candidates.length !== 1 ? 's' : ''} tested`;
        benchmarkRunning = false;
    }
</script>

<div class="settings-container">
    <div class="sidebar">
        <h2>{i18n.t.settings.app_settings}</h2>
        <button class="provider-btn" class:active={selectedProviderId === 'global'} onclick={() => selectedProviderId = 'global'}>
            {i18n.t.settings.general}
        </button>
        <button class="provider-btn" class:active={selectedProviderId === 'about'} onclick={() => selectedProviderId = 'about'}>
            {i18n.t.settings.about}
        </button>

        <div class="sidebar-divider"></div>
        <h2>{i18n.t.settings.providers}</h2>
        <div class="provider-list">
            {#each providers as provider}
                <button 
                    class="provider-btn" 
                    class:active={selectedProviderId === provider.id}
                    onclick={() => selectedProviderId = provider.id}
                >
                    <span>{provider.name}</span>
                    {#if activeProviderId === provider.id}
                        <Zap size={12} style="color: #eab308;" />
                    {/if}
                </button>
            {/each}
        </div>
    </div>

    <div class="content">
        {#if selectedProviderId === 'global'}
            <div class="header">
                <h1>{i18n.t.settings.general}</h1>
                <div class="save-area">
                    {#if saveIndicator}
                        <span class="save-badge"><CheckCircle size={14} /> {i18n.t.settings.saved}</span>
                    {/if}
                    <button class="action-btn small danger" onclick={resetProviders}>
                        <RefreshCw size={14} /> Reset to Defaults
                    </button>
                    <button class="save-btn" onclick={handleSave}>{i18n.t.settings.save_all}</button>
                </div>
            </div>

            <div class="section-card">
                <label for="active-prov-select">{i18n.t.settings.active_provider}</label>
                <select id="active-prov-select" bind:value={activeProviderId} class="styled-select">
                    {#each providers as provider}
                        <option value={provider.id}>{provider.name}</option>
                    {/each}
                </select>
            </div>

            <div class="section-card">
                <label for="lang-select"><Languages size={16} /> {i18n.t.settings.language}</label>
                <select id="lang-select" bind:value={currentLanguage} class="styled-select">
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
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <input id="save-txt-check" type="checkbox" bind:checked={saveTxt} />
                    <label for="save-txt-check"><Save size={16} /> {i18n.t.settings.save_txt}</label>
                </div>
            </div>

            <div class="header" style="margin-top: 40px;">
                <h1>{i18n.t.settings.llm_options}</h1>
            </div>

            <div class="section-card">
                <label for="max-chars-input"><MessageSquare size={16} /> {i18n.t.settings.llm_max_chars}</label>
                <input id="max-chars-input" type="number" bind:value={llmMaxChars} min="500" step="500" class="styled-input" />
                <p class="hint">Maximum characters to extract per document for analysis.</p>
            </div>

            <div class="section-card">
                <label for="pdf-backend-select"><FileText size={16} /> PDF Extraction Engine</label>
                <div class="toggle-group">
                    <button class="toggle-btn" class:active={pdfBackend === 'js'} onclick={() => pdfBackend = 'js'}>
                        JS-Native (PDF.js)
                    </button>
                    <button class="toggle-btn" class:active={pdfBackend === 'rust'} onclick={() => pdfBackend = 'rust'}>
                        Rust-Native (Fast)
                    </button>
                </div>
                <p class="hint">Rust engine is faster and better at preserving layout, but doesn't support OCR.</p>
            </div>

            <div class="section-card">
                <label for="format-select"><Code size={16} /> {i18n.t.settings.parsing_format}</label>
                <div class="toggle-group">
                    <button class="toggle-btn" class:active={parsingFormat === 'xml'} onclick={() => switchParsingFormat('xml')}>
                        {i18n.t.settings.parsing_xml}
                    </button>
                    <button class="toggle-btn" class:active={parsingFormat === 'json'} onclick={() => switchParsingFormat('json')}>
                        {i18n.t.settings.parsing_json}
                    </button>
                </div>
            </div>

            <div class="section-card">
                <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:10px;">
                    <label for="prompt-textarea" style="margin-bottom:0;"><MessageSquare size={16} /> {i18n.t.settings.llm_prompt}</label>
                    <button class="action-btn small" onclick={resetPromptToDefault} title={currentLanguage === 'de' ? 'Standard wiederherstellen' : 'Reset to default'}>
                        <RefreshCw size={12} /> {currentLanguage === 'de' ? 'Standard' : 'Default'}
                    </button>
                </div>
                <textarea id="prompt-textarea" bind:value={llmPrompt} rows="10" class="styled-textarea"></textarea>
                <p class="hint">{currentLanguage === 'de' ? `Aktuelle Variante: ${parsingFormat.toUpperCase()} / DE` : `Current variant: ${parsingFormat.toUpperCase()} / EN`}</p>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <input id="author-sort-check" type="checkbox" bind:checked={authorSortEnabled} />
                    <label for="author-sort-check"><Edit size={16} /> {i18n.t.settings.author_sort}</label>
                </div>
            </div>

            <div class="header" style="margin-top: 40px;">
                <h1>{i18n.t.settings.ocr_options}</h1>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <input id="no-thinking-check" type="checkbox" bind:checked={noThinking}
                        onchange={() => { llmClient.noThinking = noThinking; }} />
                    <label for="no-thinking-check"><Brain size={16} /> Suppress thinking mode</label>
                </div>
                <p class="hint">Sends <code>/no_think</code> as a system message (<strong>Qwen3, Qwen3.5</strong>) and strips <code>&lt;think&gt;</code>/<code>&lt;thinking&gt;</code> blocks from all responses. Qwen3.5-0.8B is non-thinking by default; larger Qwen3 models think by default and benefit most. DeepSeek-R1 variants leak <code>&lt;think&gt;</code> blocks regardless — stripping handles that. <em>No known model uses <code>/nothink</code> without the underscore.</em></p>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <input id="ocr-check" type="checkbox" bind:checked={ocrEnabled} />
                    <label for="ocr-check"><Scan size={16} /> {i18n.t.settings.ocr_enabled}</label>
                </div>
                <p class="hint">{i18n.t.settings.ocr_hint}</p>
            </div>

            <div class="header" style="margin-top: 40px;">
                <h1><Zap size={18} /> Benchmark</h1>
            </div>
            <div class="section-card">
                <!-- Providers + Models -->
                <div class="bench-config-row" style="align-items:flex-start;">
                    <span class="bench-config-label" style="padding-top:5px;">Providers</span>
                    <div style="display:flex; flex-direction:column; gap:5px; flex:1;">
                        {#each providers.filter(p => getBenchModelOptions(p).length > 0) as p}
                            {@const opts = getBenchModelOptions(p)}
                            {@const enabled = benchEnabledProviders[p.id] !== false}
                            <div class="bench-provider-row">
                                <label class="bench-check-label" style="flex-shrink:0; width:160px;">
                                    <input type="checkbox"
                                        checked={enabled}
                                        onchange={(e) => { benchEnabledProviders[p.id] = e.currentTarget.checked; }}
                                    />
                                    {p.name}
                                </label>
                                <select class="bench-model-select" disabled={!enabled}
                                    value={getBenchModel(p)}
                                    onchange={(e) => { benchModelByProvider[p.id] = e.currentTarget.value; }}
                                >
                                    {#each opts as m}
                                        <option value={m}>{m.split(/[\\/]/).pop() || m}</option>
                                    {/each}
                                </select>
                            </div>
                        {/each}
                    </div>
                </div>
                <!-- Documents from batch -->
                <div class="bench-config-row" style="align-items:flex-start;">
                    <span class="bench-config-label" style="padding-top:4px;">Documents</span>
                    <div style="flex:1;">
                        {#if benchBatchItems.length === 0}
                            <span class="hint" style="margin:0;">No extracted documents in current batch. <button class="inline-link" onclick={loadBenchBatchItems}>Reload</button></span>
                        {:else}
                            <div class="bench-file-list">
                                {#each benchBatchItems as item}
                                    <label class="bench-check-label">
                                        <input type="checkbox"
                                            checked={benchSelectedFiles.has(item.path)}
                                            onchange={(e) => {
                                                const s = new Set(benchSelectedFiles);
                                                e.currentTarget.checked ? s.add(item.path) : s.delete(item.path);
                                                benchSelectedFiles = s;
                                            }}
                                        />
                                        <span style="flex:1; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;">{item.name}</span>
                                        <span class="hint" style="margin:0; flex-shrink:0;">{Math.round(item.text.length / 1000)}k chars</span>
                                    </label>
                                {/each}
                            </div>
                            <button class="inline-link" onclick={loadBenchBatchItems} style="margin-top:4px;">↻ Reload</button>
                            <p class="hint" style="margin:4px 0 0;">Selected documents are sent as context on every run (same prompt each time). This correctly benchmarks cold vs warm performance on an identical task.</p>
                        {/if}
                    </div>
                </div>
                <!-- Prompt -->
                <div class="bench-config-row">
                    <span class="bench-config-label">Prompt</span>
                    <div style="display:flex; flex-direction:column; gap:6px; flex:1; min-width:0;">
                        <div style="display:flex; gap:16px; flex-wrap:wrap;">
                            <label class="bench-check-label">
                                <input type="radio" bind:group={benchPromptMode} value="custom" /> Custom
                            </label>
                            <label class="bench-check-label">
                                <input type="radio" bind:group={benchPromptMode} value="batch" /> Batch prompt ({parsingFormat.toUpperCase()})
                            </label>
                        </div>
                        {#if benchPromptMode === 'custom'}
                            <textarea class="bench-prompt-input" bind:value={benchCustomPrompt} rows="2" placeholder="Enter benchmark prompt..."></textarea>
                        {:else}
                            <p class="hint" style="margin:0; font-style:italic; word-break:break-word;">{getDefaultPrompt(parsingFormat, currentLanguage).slice(0, 140)}…</p>
                        {/if}
                    </div>
                </div>
                <!-- Runs -->
                <div class="bench-config-row">
                    <span class="bench-config-label">Runs</span>
                    <div style="display:flex; gap:16px; flex-wrap:wrap;">
                        {#each [[1,'cold only'],[2,'cold + warm'],[3,'cold + 2× warm']] as [n, label]}
                            <label class="bench-check-label">
                                <input type="radio" bind:group={benchNumRuns} value={n} /> {n} <span class="hint" style="margin:0;">({label})</span>
                            </label>
                        {/each}
                    </div>
                </div>
                <!-- Run button -->
                <div style="display:flex; align-items:center; gap:12px; margin-top:4px;">
                    <button class="action-btn small primary" onclick={runBenchmark} disabled={benchmarkRunning}>
                        {#if benchmarkRunning}
                            <Loader2 size={14} class="loader-spin" /> Running...
                        {:else}
                            <Zap size={14} /> Run Benchmark
                        {/if}
                    </button>
                    {#if benchmarkProgress}
                        <span class="hint" style="margin:0;">{benchmarkProgress}</span>
                    {/if}
                </div>
                <!-- Results -->
                {#if benchmarkResults.length > 0}
                    <table class="bench-table" style="margin-top:16px;">
                        <thead>
                            <tr>
                                <th>Provider</th>
                                <th>Model</th>
                                {#each Array.from({length: benchNumRuns}, (_, i) => i) as ri}
                                    <th class="bench-num">#{ri + 1} ms</th>
                                    <th class="bench-num">#{ri + 1} t/s</th>
                                {/each}
                                <th></th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each benchmarkResults as r}
                                <tr class:bench-error={r.runs.every(x => !!x.error)}>
                                    <td>{r.providerName}</td>
                                    <td class="bench-model">{r.model}</td>
                                    {#each r.runs as run}
                                        <td class="bench-num">{run.latencyMs.toLocaleString()} ms</td>
                                        <td class="bench-num">
                                            {#if run.error}
                                                <span class="bench-err-msg" title={run.error} style="cursor:help;">✗</span>
                                            {:else}
                                                <strong>{run.tokensPerSec}</strong>
                                            {/if}
                                        </td>
                                    {/each}
                                    {#each Array.from({length: benchNumRuns - r.runs.length}) as _}
                                        <td class="bench-num">—</td><td class="bench-num">—</td>
                                    {/each}
                                    <td>
                                        <button class="bench-view-btn"
                                            onclick={() => benchModal = { title: `${r.providerName} — ${r.model}`, runs: r.runs }}>
                                            View
                                        </button>
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                {/if}
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

            <div class="sidebar-divider" style="margin: 32px 0;"></div>

            <div class="section-card">
                <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:12px;">
                    <div style="display:flex; flex-direction:column; gap:2px;">
                        <label style="margin-bottom:0;"><Code size={16} /> {i18n.t.settings.legal.licenses}</label>
                        <span class="hint" style="margin:0;">{i18n.t.settings.legal.license_total.replace('{count}', String(automatedLicenses.length))}</span>
                    </div>
                    <div class="search-box small">
                        <input type="text" placeholder="Search {automatedLicenses.length} licenses..." bind:value={licenseSearch} />
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
                                    <button class="inline-link" onclick={() => opener.open(lib.link)}>Source</button>
                                {/if}
                            </div>
                        </div>
                    {/each}
                    {#if filteredLicenses.length === 0}
                        <div class="hint" style="text-align:center; padding: 20px;">No matching licenses found.</div>
                    {/if}
                </div>
            </div>

        {:else}
            <div class="header">
                <h1>{selectedProvider.name}</h1>
                <div class="header-actions">
                    {#if saveIndicator}
                        <span class="save-badge"><CheckCircle size={14} /> {i18n.t.settings.saved}</span>
                    {/if}
                    <button class="action-btn small" 
                            class:active-btn={activeProviderId === selectedProvider.id}
                            onclick={() => setActiveProvider(selectedProvider.id)}>
                        <Zap size={14} /> {activeProviderId === selectedProvider.id ? 'Active' : 'Set Active'}
                    </button>
                    <button class="save-btn" onclick={handleSave}>{i18n.t.settings.save_all}</button>
                </div>
            </div>

            {#if selectedProvider.id !== 'mistralrs'}
                <div class="form-group">
                    <label for="base-url-input">{i18n.t.settings.base_url}</label>
                    <input id="base-url-input" type="text" bind:value={selectedProvider.baseUrl} />
                </div>
            {/if}

            {#if !['mistralrs', 'llamacpp', 'mlx', 'ollama'].includes(selectedProvider.id)}
                <form onsubmit={e => e.preventDefault()}>
                    <div class="form-group">
                        <label for="api-key-input">{i18n.t.settings.api_key}</label>
                        <input id="api-key-input" type="password" bind:value={selectedProvider.apiKey} autocomplete="current-password" />
                    </div>
                </form>
            {/if}

            <div class="form-group">
                <label for="model-select">{i18n.t.settings.select_model}</label>
                <div class="input-with-action">
                    <select id="model-select" bind:value={selectedProvider.selectedModel} class="styled-select" onchange={() => selectedProviderId === 'mlx' ? setMlxModelActive(selectedProvider.selectedModel) : setLocalModelActive(selectedProvider.selectedModel)}>
                        <option value="">-- {i18n.t.settings.select_model} --</option>
                        {#each availableModels as model}
                            <option value={model}>{model.split(/[\\/]/).pop()}</option>
                        {/each}
                    </select>
                    {#if selectedProvider.id !== 'mistralrs'}
                        <button class="action-btn small" onclick={handleRefreshModels} disabled={loadingModels}>
                            <RefreshCw size={14} class={loadingModels ? "loader-spin" : ""} />
                        </button>
                    {/if}
                    {#if selectedProvider.id === 'llamacpp'}
                        <button class="action-btn small primary" onclick={() => setLocalModelActive(selectedProvider.selectedModel)} title="Start Sidecar">
                            <Rocket size={14} />
                        </button>
                    {/if}
                </div>
                {#if selectedProvider.id === 'mlx'}
                    <div class="mlx-control-row">
                        <label for="mlx-port-input" style="width:auto; margin:0;">Port</label>
                        <input id="mlx-port-input" type="number" bind:value={mlxPort} style="width: 80px;" />
                        {#if mlxRunning}
                            <button class="action-btn small danger" onclick={stopMlxServer}>
                                <Square size={14} /> Stop MLX
                            </button>
                            {#if !mlxReady}
                                <span class="hint" style="margin:0; display:flex; align-items:center; gap:4px;">
                                    <Loader2 size={12} class="loader-spin" /> Loading model...
                                </span>
                            {:else}
                                <span class="hint" style="margin:0; color:#22c55e;">● Ready</span>
                            {/if}
                        {:else}
                            <button class="action-btn small success" onclick={startMlxServer}>
                                <Rocket size={14} /> Start MLX
                            </button>
                        {/if}
                    </div>
                    <p class="hint">Requires: <code>pip install -U mlx-lm</code> (≥ 0.31.0). Models marked ⚠ require the latest version. Path can be a local dir or HF repo ID.</p>
                {/if}
            </div>

            {#if selectedProvider.id !== 'mistralrs'}
                <div class="actions">
                    <button class="action-btn test-btn" onclick={handleTestConnection} disabled={testingConnection}>
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

            {#if selectedProvider.id === 'mlx'}
                <div class="section-card" style="margin-top: 24px;">
                    <div class="header" style="margin-bottom: 12px;">
                        <h2 style="font-size: 1rem; color: #a1a1aa;"><HardDrive size={16} /> MLX Model Manager</h2>
                        <button class="action-btn small" onclick={checkMlxModelsCache} title="Refresh cache status">
                            <RefreshCw size={14} /> Refresh
                        </button>
                    </div>
                    <p class="hint" style="margin-bottom: 4px;">Models auto-download from HuggingFace on first use via mlx_lm.</p>
                    <p class="hint" style="margin-bottom: 16px;">Cache: <code>{mlxCacheDir}</code></p>

                    <div class="form-group" style="margin-bottom: 20px;">
                        <label for="mlx-custom-id">Custom HF Repo ID or local path</label>
                        <div class="input-with-action">
                            <input
                                type="text"
                                id="mlx-custom-id"
                                placeholder="e.g. mlx-community/Mistral-7B-Instruct-v0.3-4bit"
                                bind:value={mlxCustomInput}
                            />
                            <button class="action-btn small" onclick={addMlxModel}>
                                <Plus size={14} /> Add
                            </button>
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
                                        <span class="cache-dot" class:cached={mlxModelCached[model.id]} title={mlxModelCached[model.id] ? 'Cached on disk' : 'Not downloaded yet'}></span>
                                    </div>
                                    <span class="model-path">{model.repoId}</span>
                                </div>
                                <div class="model-status">
                                    <span class="size-badge">{model.params}</span>
                                    <button class="action-btn small" onclick={() => setMlxModelActive(model.repoId)}>
                                        {selectedProvider.selectedModel === model.repoId ? 'Selected' : 'Use'}
                                    </button>
                                    {#if mlxModelCached[model.id]}
                                        <button class="icon-btn danger" onclick={() => deleteMlxModelFromDisk(model)} title="Delete from disk"><Trash2 size={14} /></button>
                                    {/if}
                                    {#if !DEFAULT_MLX_MODELS.find(d => d.id === model.id)}
                                        <button class="icon-btn" onclick={() => removeMlxModel(model.id)} title="Remove from list"><XCircle size={14} /></button>
                                    {/if}
                                </div>
                            </div>
                        {/each}
                    </div>
                </div>

                <!-- MLX Log Viewer -->
                <div class="section-card" style="margin-top: 16px;">
                    <button class="section-toggle-flat" onclick={() => mlxLogsVisible = !mlxLogsVisible}>
                        <span style="display:flex; align-items:center; gap:8px;">
                            <Brain size={14} /> MLX Server Log
                            {#if mlxRunning}<span class="running-dot"></span>{/if}
                        </span>
                        <span style="display:flex; align-items:center; gap:8px;">
                            <span class="hint" style="margin:0;">{mlxLogs.length} lines</span>
                            {#if mlxLogsVisible}<ChevronUp size={14} />{:else}<ChevronDown size={14} />{/if}
                        </span>
                    </button>
                    {#if mlxLogsVisible}
                        <div style="margin-top: 10px; position: relative;">
                            <textarea
                                bind:this={mlxLogEl}
                                readonly
                                class="log-viewer"
                                value={mlxLogs.join('\n')}
                                rows="14"
                            ></textarea>
                            <button class="log-clear-btn" onclick={() => mlxLogs = []} title="Clear log">
                                <Trash2 size={12} />
                            </button>
                        </div>
                    {/if}
                </div>
            {/if}

            {#if ['mistralrs', 'llamacpp'].includes(selectedProvider.id)}
                <div class="section-card" style="margin-top: 24px;">
                    <div class="header" style="margin-bottom: 15px;">
                        <h2 style="font-size: 1rem; color: #a1a1aa;"><HardDrive size={16} /> Local Model Manager</h2>
                        <div class="header-actions" style="display: flex; gap: 8px;">
                            {#if selectedProvider.id === 'llamacpp'}
                                <button class="action-btn small primary" onclick={() => setLocalModelActive(selectedProvider.selectedModel)}>
                                    <Rocket size={14} /> Start Sidecar
                                </button>
                            {/if}
                            <button class="action-btn small success" onclick={addLocalModel}>
                                <Plus size={14} /> Add File
                            </button>
                        </div>
                    </div>

                    <div class="form-group" style="margin-bottom: 20px;">
                        <label for="custom-model-id">Custom HF Repo ID or URL</label>
                        <div class="input-with-action">
                            <input 
                                type="text" 
                                id="custom-model-id" 
                                placeholder="e.g. bartowski/Llama-3.2-1B-Instruct-GGUF/Llama-3.2-1B-Instruct-Q4_K_M.gguf" 
                                bind:value={customModelInput}
                            />
                            <button class="action-btn small" onclick={addCustomModel}>
                                <Plus size={14} /> Add
                            </button>
                        </div>
                        <p class="hint">For GGUF on HF use Format: REPO_ID/FILENAME.GGUF</p>
                    </div>

                    <div class="models-grid">
                        {#each localModels as model, i}
                            <div class="local-model-row" class:active-model-row={selectedProvider.selectedModel === model.path}>
                                <div class="model-info">
                                    <div class="model-title-line">
                                        <strong>{model.name}</strong>
                                        {#if selectedProvider.selectedModel === model.path}<Zap size={12} style="color: #eab308;" />{/if}
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
                                    {#if model.isDownloaded}
                                        <span class="size-badge">{model.size}</span>
                                        <button class="action-btn small" onclick={() => setLocalModelActive(model.path)}>
                                            {selectedProvider.selectedModel === model.path ? 'Selected' : 'Use'}
                                        </button>
                                        <button class="icon-btn danger" onclick={() => removeLocalModel(i)} title="Delete file"><Trash2 size={14} /></button>
                                    {:else if model.progress === undefined}
                                        <button class="action-btn small primary" onclick={() => downloadLocalModel(i)}>
                                            <Download size={14} /> Download
                                        </button>
                                        <button class="icon-btn" onclick={() => localModels.splice(i, 1)}><XCircle size={14} /></button>
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
    <div class="bench-modal-overlay" onclick={() => benchModal = null}>
        <div class="bench-modal" onclick={(e) => e.stopPropagation()}>
            <div class="bench-modal-header">
                <span>{benchModal.title}</span>
                <button class="bench-modal-close" onclick={() => benchModal = null}>✕</button>
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
    .sidebar { width: 200px; background: #18181b; border-right: 1px solid #27272a; padding: 20px 0; display: flex; flex-direction: column; flex-shrink: 0; }
    .sidebar h2 { padding: 0 20px; font-size: 0.75rem; text-transform: uppercase; color: #71717a; margin-bottom: 12px; letter-spacing: 0.05em; }
    .sidebar-divider { height: 1px; background: #27272a; margin: 20px 0; }
    .provider-list { display: flex; flex-direction: column; }
    .provider-btn { padding: 8px 20px; text-align: left; border: none; background: transparent; cursor: pointer; font-size: 0.875rem; color: #a1a1aa; transition: all 0.2s; display: flex; align-items: center; justify-content: space-between; }
    .provider-btn:hover { background: #27272a; color: white; }
    .provider-btn.active { background: #27272a; color: white; font-weight: 600; border-left: 3px solid #3b82f6; }
    
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
    input[type="text"], input[type="password"], input[type="number"], .styled-select, .styled-textarea { width: 100%; padding: 8px 12px; border: 1px solid #27272a; border-radius: 6px; font-size: 0.875rem; background: #09090b; color: white; }
    .styled-textarea { font-family: inherit; resize: vertical; }
    input:focus, .styled-select:focus, .styled-textarea:focus { outline: 2px solid #3b82f6; border-color: transparent; }
    .input-with-action { display: flex; gap: 10px; }
    
    .toggle-group { display: flex; background: #09090b; border: 1px solid #27272a; border-radius: 6px; padding: 2px; width: fit-content; }
    .toggle-btn { padding: 4px 12px; border: none; background: transparent; color: #71717a; font-size: 0.75rem; font-weight: 600; cursor: pointer; border-radius: 4px; transition: all 0.2s; }
    .toggle-btn.active { background: #27272a; color: white; }

    .actions { display: flex; gap: 12px; margin-bottom: 24px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 6px 12px; border: 1px solid #27272a; background: #18181b; border-radius: 6px; font-size: 0.8125rem; font-weight: 600; cursor: pointer; color: #d4d4d8; transition: background 0.2s; }
    .action-btn:hover { background: #27272a; }
    .action-btn.active-btn { color: #eab308; border-color: #713f12; background: #42200633; }
    
    .test-result-box { padding: 10px; border-radius: 6px; font-size: 0.8125rem; margin-bottom: 24px; max-width: 600px; border: 1px solid #27272a; }
    .test-result-box.success { background: #064e3b33; color: #ecfdf5; border-color: #065f46; }
    .test-result-box.error { background: #450a0a33; color: #fef2f2; border-color: #7f1d1d; }
    
    :global(.model-manager-list) { display: flex; flex-direction: column; gap: 10px; }
    .local-model-row { display: flex; justify-content: space-between; align-items: center; padding: 12px; background: #09090b; border: 1px solid #27272a; border-radius: 6px; }
    .local-model-row.active-model-row { border-color: #3b82f6; background: #1e3a8a33; }
    .model-title-line { display: flex; align-items: center; gap: 8px; }
    .model-info { display: flex; flex-direction: column; gap: 4px; flex: 1; margin-right: 20px; }
    .model-path { font-size: 0.7rem; color: #71717a; font-family: monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 300px; }
    .model-status { display: flex; align-items: center; gap: 12px; }
    .size-badge { font-size: 0.75rem; background: #27272a; padding: 2px 6px; border-radius: 4px; color: #a1a1aa; }
    
    .progress-container { margin-top: 8px; height: 16px; background: #18181b; border-radius: 8px; position: relative; overflow: hidden; border: 1px solid #27272a; }
    .progress-bar { height: 100%; background: #3b82f6; transition: width 0.3s; }
    .progress-text { position: absolute; top: 0; left: 0; width: 100%; text-align: center; font-size: 0.65rem; line-height: 16px; color: white; font-weight: 700; }

    .icon-btn { background: transparent; border: none; cursor: pointer; color: #71717a; display: flex; align-items: center; justify-content: center; padding: 4px; border-radius: 4px; }
    .icon-btn:hover { background: #27272a; color: white; }
    .icon-btn.danger:hover { background: #ef444433; color: #ef4444; }
    
    .hint { font-size: 0.75rem; color: #71717a; margin-top: 6px; display: block; line-height: 1.4; }
    .loader-spin { display: inline-flex; animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

    .bench-config-row { display: flex; align-items: flex-start; gap: 12px; margin-bottom: 12px; }
    .bench-config-label { width: 68px; flex-shrink: 0; font-size: 0.7rem; color: #71717a; text-transform: uppercase; letter-spacing: 0.05em; padding-top: 3px; }
    .bench-provider-row { display: flex; align-items: center; gap: 8px; }
    .bench-model-select { flex: 1; min-width: 0; background: #09090b; border: 1px solid #27272a; color: #e2e8f0; border-radius: 4px; padding: 3px 6px; font-size: 0.75rem; font-family: monospace; }
    .bench-model-select:disabled { opacity: 0.4; }
    .bench-file-list { display: flex; flex-direction: column; gap: 4px; max-height: 120px; overflow-y: auto; padding: 6px 8px; background: #09090b; border: 1px solid #27272a; border-radius: 4px; }
    .bench-check-label { display: flex; align-items: center; gap: 5px; font-size: 0.8125rem; color: #e2e8f0; cursor: pointer; }
    .bench-check-label input { accent-color: #3b82f6; cursor: pointer; flex-shrink: 0; }
    .bench-prompt-input { width: 100%; background: #09090b; border: 1px solid #27272a; color: #e2e8f0; border-radius: 4px; padding: 6px 8px; font-size: 0.8rem; font-family: monospace; resize: vertical; }
    .inline-link { background: none; border: none; color: #3b82f6; cursor: pointer; font-size: 0.8rem; padding: 0; text-decoration: underline; }
    .inline-link:hover { color: #60a5fa; }
    .bench-table { width: 100%; border-collapse: collapse; font-size: 0.8125rem; }
    .bench-table th { text-align: left; padding: 6px 10px; border-bottom: 2px solid #27272a; color: #71717a; font-size: 0.7rem; text-transform: uppercase; letter-spacing: 0.05em; }
    .bench-table td { padding: 6px 10px; border-bottom: 1px solid #1e1e1e; color: #e2e8f0; vertical-align: top; }
    .bench-table tr:hover td { background: #1e293b; }
    .bench-table tr.bench-error td { color: #71717a; }
    .bench-num { font-family: monospace; text-align: right; white-space: nowrap; }
    .bench-model { font-family: monospace; font-size: 0.7rem; color: #a1a1aa; max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .bench-err-msg { color: #ef4444; font-style: italic; cursor: help; }
    .bench-view-btn { background: #27272a; border: 1px solid #3f3f46; color: #a1a1aa; cursor: pointer; font-size: 0.7rem; padding: 2px 8px; border-radius: 3px; white-space: nowrap; }
    .bench-view-btn:hover { background: #3f3f46; color: #e2e8f0; }
    .bench-response-block { margin-bottom: 16px; }
    .bench-response-block:last-child { margin-bottom: 0; }
    .bench-run-label { font-size: 0.65rem; color: #52525b; text-transform: uppercase; letter-spacing: 0.06em; display: block; margin-bottom: 6px; }
    .bench-response-pre { margin: 0; white-space: pre-wrap; font-size: 0.8rem; color: #e2e8f0; font-family: monospace; line-height: 1.6; background: #09090b; border: 1px solid #27272a; border-radius: 4px; padding: 10px; overflow-y: auto; }
    .bench-modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.7); z-index: 1000; display: flex; align-items: center; justify-content: center; }
    .bench-modal { background: #18181b; border: 1px solid #3f3f46; border-radius: 8px; width: min(720px, 90vw); max-height: 80vh; display: flex; flex-direction: column; box-shadow: 0 20px 60px rgba(0,0,0,0.5); }
    .bench-modal-header { display: flex; justify-content: space-between; align-items: center; padding: 14px 18px; border-bottom: 1px solid #27272a; font-size: 0.875rem; font-weight: 600; color: #e2e8f0; flex-shrink: 0; }
    .bench-modal-close { background: none; border: none; color: #71717a; cursor: pointer; font-size: 1rem; padding: 0 4px; line-height: 1; }
    .bench-modal-close:hover { color: #e2e8f0; }
    .bench-modal-body { padding: 16px 18px; overflow-y: auto; }
    .action-btn.primary { background: #3b82f6; color: white; border-color: #3b82f6; }
    .action-btn.small { padding: 4px 10px; font-size: 0.75rem; }
    .action-btn.success { background: #10b981; color: white; border-color: #10b981; }
    .action-btn.danger { color: #ef4444; }
    .action-btn.danger:hover { background: #ef444433; }
    .mlx-control-row { display: flex; align-items: center; gap: 10px; margin-top: 8px; flex-wrap: wrap; }
    .mlx-control-row input { padding: 4px 8px; background: #09090b; border: 1px solid #27272a; color: white; border-radius: 4px; font-size: 0.8125rem; }
    .vision-badge { font-size: 0.6rem; font-weight: 700; background: #7c3aed33; color: #a78bfa; border: 1px solid #7c3aed55; border-radius: 3px; padding: 1px 5px; letter-spacing: 0.05em; }
    .cache-dot { width: 7px; height: 7px; border-radius: 50%; background: #3f3f46; flex-shrink: 0; transition: background 0.3s; }
    .cache-dot.cached { background: #10b981; }
    .running-dot { width: 8px; height: 8px; border-radius: 50%; background: #10b981; animation: pulse 1.5s infinite; flex-shrink: 0; }
    @keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.3; } }
    .section-toggle-flat { width: 100%; display: flex; justify-content: space-between; align-items: center; background: transparent; border: none; color: #a1a1aa; cursor: pointer; font-size: 0.8125rem; font-weight: 600; padding: 0; }
    .section-toggle-flat:hover { color: white; }
    .log-viewer { width: 100%; background: #09090b; color: #a1a1aa; border: 1px solid #27272a; border-radius: 6px; font-family: monospace; font-size: 0.7rem; line-height: 1.5; padding: 10px; resize: vertical; white-space: pre; overflow-wrap: normal; }
    .log-clear-btn { position: absolute; top: 8px; right: 8px; background: #27272a; border: none; color: #71717a; cursor: pointer; border-radius: 4px; padding: 3px 5px; display: flex; align-items: center; }
    .log-clear-btn:hover { color: #ef4444; background: #ef444422; }

    .legal-text { font-size: 0.875rem; color: #e2e8f0; line-height: 1.6; }

    .license-list-scrollable { display: flex; flex-direction: column; gap: 8px; max-height: 400px; overflow-y: auto; background: #09090b; border: 1px solid #27272a; border-radius: 6px; padding: 12px; }
    .license-item-auto { border-bottom: 1px solid #18181b; padding-bottom: 8px; margin-bottom: 4px; }
    .license-item-auto:last-child { border-bottom: none; margin-bottom: 0; padding-bottom: 0; }
    .license-item-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 2px; }
    .lib-name { font-size: 0.875rem; color: #f4f4f5; }
    .lib-name small { font-size: 0.75rem; color: #71717a; margin-left: 4px; }
    .lib-source-badge { font-size: 0.65rem; font-weight: 700; background: #1e293b; color: #94a3b8; padding: 1px 6px; border-radius: 4px; border: 1px solid #334155; }
    .lib-source-badge.rust { background: #450a0a33; color: #f87171; border-color: #7f1d1d; }
    .license-item-meta { display: flex; align-items: center; gap: 12px; font-size: 0.75rem; color: #71717a; }
    .lib-type { font-weight: 600; color: #a1a1aa; }
    .lib-author { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .search-box.small input { padding: 4px 10px; font-size: 0.75rem; width: 220px; background: #09090b; border: 1px solid #27272a; color: white; border-radius: 4px; }
</style>
