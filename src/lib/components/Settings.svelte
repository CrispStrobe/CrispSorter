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
        Rocket, FileText, Brain, Square
    } from 'lucide-svelte';
    import { open, save } from '@tauri-apps/plugin-dialog';
    import { stat, remove } from '@tauri-apps/plugin-fs';
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';

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

    let providers = $state<LLMProvider[]>(JSON.parse(JSON.stringify(DEFAULT_PROVIDERS)));
    let selectedProviderId = $state('global'); 
    let selectedProvider = $derived(providers.find(p => p.id === selectedProviderId) || providers[0]);

    // Global App Settings
    let activeProviderId = $state('ollama');
    let exportPath = $state('');
    let saveTxt = $state(true);
    let currentLanguage = $state<Language>('en');
    
    // LLM & OCR Settings
    let llmMaxChars = $state(5000);
    let llmContextLimit = $state(4096);
    let llmPrompt = $state(''); // Loaded from store
    let ocrEnabled = $state(false);
    let authorSortEnabled = $state(false);
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
    let saveIndicator = $state(false);
    let customModelInput = $state('');

    // Consolidate current provider models
    let availableModels = $derived.by(() => {
        if (['mistralrs', 'llamacpp'].includes(selectedProviderId)) {
            return localModels.filter(m => m.isDownloaded).map(m => m.path);
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

        listen('download-progress', (event: any) => {
            const { id, received, total } = event.payload;
            const model = localModels.find(m => m.id === id);
            if (model) {
                model.progress = Math.round((received / total) * 100);
            }
        }).then(fn => { unlistenDownload = fn; });
    });

    let unlistenDownload: (() => void) | undefined;
    onDestroy(() => unlistenDownload?.());

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
        await saveSetting('saveTxt', saveTxt);
        await saveSetting('language', currentLanguage);
        await saveSetting('llmMaxChars', llmMaxChars);
        await saveSetting('llmContextLimit', llmContextLimit);
        await saveSetting('llmPrompt', llmPrompt);
        await saveSetting('ocrEnabled', ocrEnabled);
        await saveSetting('authorSortEnabled', authorSortEnabled);
        await saveSetting('pdfBackend', pdfBackend);
        await saveSetting('parsingFormat', parsingFormat);
        await saveSetting('localModels', $state.snapshot(localModels));
        i18n.setLanguage(currentLanguage);
        
        saveIndicator = true;
        setTimeout(() => saveIndicator = false, 2000);
    }

    async function pickExportPath() {
        const selected = await open({ directory: true, multiple: false });
        if (typeof selected === 'string') exportPath = selected;
    }

    async function addLocalModel() {
        const selected = await open({
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
            const msg = await invoke<string>('start_mlx_server', { modelPath: model, port: mlxPort });
            setTimeout(() => alert(msg + '\nWait ~5s for the server to initialize, then test connection.'), 100);
        } catch (e: any) {
            mlxRunning = false;
            alert(`MLX start failed: ${e?.message || e}`);
        }
    }

    async function stopMlxServer() {
        try {
            await invoke('stop_mlx_server');
            mlxRunning = false;
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
    interface BenchmarkResult {
        providerId: string;
        providerName: string;
        model: string;
        tokensPerSec: number | null;
        latencyMs: number;
        error?: string;
    }
    const BENCH_PROMPT = 'List the first 10 prime numbers. Be concise.';
    let benchmarkRunning = $state(false);
    let benchmarkProgress = $state('');
    let benchmarkResults = $state<BenchmarkResult[]>([]);

    async function runBenchmark() {
        benchmarkRunning = true;
        benchmarkResults = [];

        const candidates = providers.filter(p => {
            if (p.selectedModel && p.selectedModel.trim()) return true;
            if (['mistralrs', 'llamacpp', 'mlx', 'ollama'].includes(p.id) && p.models.length > 0) return true;
            return false;
        });

        for (const p of candidates) {
            const model = p.selectedModel || p.models[0] || '';
            if (!model) continue;
            benchmarkProgress = `Testing ${p.name} / ${model.split(/[\\/]/).pop()}...`;
            const start = performance.now();
            try {
                const response = await llmClient.query(p.id, model, BENCH_PROMPT, p.apiKey);
                const elapsed = performance.now() - start;
                const approxTokens = Math.max(1, Math.round(response.length / 4));
                benchmarkResults.push({
                    providerId: p.id,
                    providerName: p.name,
                    model: model.split(/[\\/]/).pop() || model,
                    tokensPerSec: Math.round(approxTokens / (elapsed / 1000)),
                    latencyMs: Math.round(elapsed)
                });
            } catch (e: any) {
                benchmarkResults.push({
                    providerId: p.id,
                    providerName: p.name,
                    model: model.split(/[\\/]/).pop() || model,
                    tokensPerSec: null,
                    latencyMs: Math.round(performance.now() - start),
                    error: e.message || String(e)
                });
            }
        }

        if (candidates.length === 0) {
            benchmarkProgress = 'No providers with a selected model found. Configure a model first.';
        } else {
            benchmarkProgress = '';
        }
        benchmarkRunning = false;
    }
</script>

<div class="settings-container">
    <div class="sidebar">
        <h2>{i18n.t.settings.app_settings}</h2>
        <button class="provider-btn" class:active={selectedProviderId === 'global'} onclick={() => selectedProviderId = 'global'}>
            {i18n.t.settings.general}
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
                    <input id="ocr-check" type="checkbox" bind:checked={ocrEnabled} />
                    <label for="ocr-check"><Scan size={16} /> {i18n.t.settings.ocr_enabled}</label>
                </div>
                <p class="hint">{i18n.t.settings.ocr_hint}</p>
            </div>

            <div class="header" style="margin-top: 40px;">
                <h1><Zap size={18} /> Benchmark</h1>
            </div>
            <div class="section-card">
                <p class="hint" style="margin-bottom: 12px;">Runs a fixed prompt through every provider that has a model selected. Estimates output tokens/sec (response chars ÷ 4 ÷ seconds). Tests run sequentially for fair comparison.</p>
                <div style="display:flex; align-items:center; gap: 12px; margin-bottom: 12px;">
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
                {#if benchmarkResults.length > 0}
                    <table class="bench-table">
                        <thead>
                            <tr>
                                <th>Provider</th>
                                <th>Model</th>
                                <th>Latency</th>
                                <th>Tokens/sec</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each benchmarkResults.sort((a,b) => (b.tokensPerSec ?? 0) - (a.tokensPerSec ?? 0)) as r}
                                <tr class:bench-error={!!r.error}>
                                    <td>{r.providerName}</td>
                                    <td class="bench-model">{r.model}</td>
                                    <td class="bench-num">{r.latencyMs} ms</td>
                                    <td class="bench-num">
                                        {#if r.error}
                                            <span class="bench-err-msg" title={r.error}>error</span>
                                        {:else}
                                            <strong>{r.tokensPerSec}</strong>
                                        {/if}
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                {/if}
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
                <div class="form-group">
                    <label for="api-key-input">{i18n.t.settings.api_key}</label>
                    <input id="api-key-input" type="password" bind:value={selectedProvider.apiKey} />
                </div>
            {/if}

            <div class="form-group">
                <label for="model-select">{i18n.t.settings.select_model}</label>
                <div class="input-with-action">
                    <select id="model-select" bind:value={selectedProvider.selectedModel} class="styled-select" onchange={() => setLocalModelActive(selectedProvider.selectedModel)}>
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
                        {:else}
                            <button class="action-btn small success" onclick={startMlxServer}>
                                <Rocket size={14} /> Start MLX
                            </button>
                        {/if}
                    </div>
                    <p class="hint">Requires: <code>pip install mlx-lm</code>. Model path can be a local directory or HF repo ID (e.g. <code>mlx-community/Mistral-7B-Instruct-v0.3-4bit</code>).</p>
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

    .bench-table { width: 100%; border-collapse: collapse; font-size: 0.8125rem; }
    .bench-table th { text-align: left; padding: 6px 10px; border-bottom: 2px solid #27272a; color: #71717a; font-size: 0.7rem; text-transform: uppercase; letter-spacing: 0.05em; }
    .bench-table td { padding: 6px 10px; border-bottom: 1px solid #1e1e1e; color: #e2e8f0; }
    .bench-table tr:hover td { background: #1e293b; }
    .bench-table tr.bench-error td { color: #71717a; }
    .bench-num { font-family: monospace; text-align: right; }
    .bench-model { font-family: monospace; font-size: 0.7rem; color: #a1a1aa; max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .bench-err-msg { color: #ef4444; font-style: italic; cursor: help; }
    .action-btn.primary { background: #3b82f6; color: white; border-color: #3b82f6; }
    .action-btn.small { padding: 4px 10px; font-size: 0.75rem; }
    .action-btn.success { background: #10b981; color: white; border-color: #10b981; }
    .action-btn.danger { color: #ef4444; }
    .action-btn.danger:hover { background: #ef444433; }
    .mlx-control-row { display: flex; align-items: center; gap: 10px; margin-top: 8px; flex-wrap: wrap; }
    .mlx-control-row input { padding: 4px 8px; background: #09090b; border: 1px solid #27272a; color: white; border-radius: 4px; font-size: 0.8125rem; }
</style>
