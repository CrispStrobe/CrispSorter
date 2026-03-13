<script lang="ts">
    import { onMount } from 'svelte';
    import { DEFAULT_PROVIDERS, type LLMProvider, llmClient } from '../llm/client';
    import { getSetting, saveSetting } from '../store';
    import { i18n, type Language } from '../i18n.svelte';
    import { 
        RefreshCw, CheckCircle, XCircle, Key, Globe, Cpu, 
        Loader2, FolderOpen, Save, Languages, MessageSquare, 
        Scan, Edit, Zap, Trash2, Download, Plus, HardDrive
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
    let llmPrompt = $state('Extract metadata from this document. Respond ONLY in this exact format:\n<TITLE>...</TITLE>\n<YEAR>YYYY</YEAR>\n<AUTHOR>Lastname Firstname</AUTHOR>\n<LANGUAGE>ISO</LANGUAGE>');
    let ocrEnabled = $state(false);
    let authorSortEnabled = $state(false);

    // mistral.rs / Local Model Management
    let localModels = $state<LocalModel[]>([
        { 
            id: 'qwen-0.8b',
            name: 'Qwen 3.5 0.8B (Q4_K_M)', 
            path: '', 
            isDownloaded: false,
            isActive: true,
            downloadUrl: 'https://huggingface.co/bartowski/Qwen_Qwen3.5-0.8B-GGUF/resolve/main/Qwen_Qwen3.5-0.8B-Q4_K_M.gguf'
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
    let saveIndicator = $state(false);

    // Consolidate current provider models
    let availableModels = $derived.by(() => {
        if (selectedProviderId === 'mistralrs') {
            return localModels.filter(m => m.isDownloaded).map(m => m.path);
        }
        return selectedProvider.models;
    });

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
        llmPrompt = await getSetting('llmPrompt', 'Extract metadata from this document. Respond ONLY in this exact format:\n<TITLE>...</TITLE>\n<YEAR>YYYY</YEAR>\n<AUTHOR>Lastname Firstname</AUTHOR>\n<LANGUAGE>ISO</LANGUAGE>');
        ocrEnabled = await getSetting('ocrEnabled', false);
        authorSortEnabled = await getSetting('authorSortEnabled', false);
        
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

        const unlisten = await listen('download-progress', (event: any) => {
            const { id, received, total } = event.payload;
            const model = localModels.find(m => m.id === id);
            if (model) {
                model.progress = Math.round((received / total) * 100);
            }
        });

        return () => unlisten();
    });

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
        await saveSetting('llmPrompt', llmPrompt);
        await saveSetting('ocrEnabled', ocrEnabled);
        await saveSetting('authorSortEnabled', authorSortEnabled);
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
        await handleSave();
    }

    async function setActiveProvider(id: string) {
        activeProviderId = id;
        await handleSave();
    }

    async function handleRefreshModels() {
        if (!selectedProvider.apiKey && !['ollama', 'mistralrs'].includes(selectedProvider.id)) {
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
            alert(`${i18n.t.settings.fetch_failed}: ${error.message}`);
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
                        <span class="save-badge"><Check size={14} /> {i18n.t.settings.saved}</span>
                    {/if}
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
            </div>

            <div class="section-card">
                <label for="prompt-textarea"><MessageSquare size={16} /> {i18n.t.settings.llm_prompt}</label>
                <textarea id="prompt-textarea" bind:value={llmPrompt} rows="4" class="styled-textarea"></textarea>
                <p class="hint">{i18n.t.settings.llm_prompt_hint}</p>
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

                <div class="form-group">
                    <label for="api-key-input">{i18n.t.settings.api_key}</label>
                    <input id="api-key-input" type="password" bind:value={selectedProvider.apiKey} />
                </div>
            {/if}

            <div class="form-group">
                <label for="model-select">{i18n.t.settings.select_model}</label>
                <div class="input-with-action">
                    <select id="model-select" bind:value={selectedProvider.selectedModel} class="styled-select">
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
                </div>
            </div>

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

            {#if selectedProvider.id === 'mistralrs'}
                <div class="section-card" style="margin-top: 24px;">
                    <div class="header" style="margin-bottom: 15px;">
                        <h2 style="font-size: 1rem; color: #a1a1aa;"><HardDrive size={16} /> Local Model Manager</h2>
                        <button class="action-btn small success" onclick={addLocalModel}>
                            <Plus size={14} /> Add Model File
                        </button>
                    </div>
                    
                    <div class="model-manager-list">
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
    .actions { display: flex; gap: 12px; margin-bottom: 24px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 6px 12px; border: 1px solid #27272a; background: #18181b; border-radius: 6px; font-size: 0.8125rem; font-weight: 600; cursor: pointer; color: #d4d4d8; transition: background 0.2s; }
    .action-btn:hover { background: #27272a; }
    .action-btn.active-btn { color: #eab308; border-color: #713f12; background: #42200633; }
    
    .test-result-box { padding: 10px; border-radius: 6px; font-size: 0.8125rem; margin-bottom: 24px; max-width: 600px; border: 1px solid #27272a; }
    .test-result-box.success { background: #064e3b33; color: #ecfdf5; border-color: #065f46; }
    .test-result-box.error { background: #450a0a33; color: #fef2f2; border-color: #7f1d1d; }
    
    .model-manager-list { display: flex; flex-direction: column; gap: 10px; }
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
</style>
