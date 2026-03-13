<script lang="ts">
    import { onMount } from 'svelte';
    import { DEFAULT_PROVIDERS, type LLMProvider, llmClient } from '../llm/client';
    import { getSetting, saveSetting } from '../store';
    import { i18n, type Language } from '../i18n.svelte';
    import { RefreshCw, CheckCircle, XCircle, Key, Globe, Cpu, Loader2, FolderOpen, Save, Languages, MessageSquare, Scan, Edit, Zap } from 'lucide-svelte';
    import { open } from '@tauri-apps/plugin-dialog';

    let providers = $state<LLMProvider[]>(JSON.parse(JSON.stringify(DEFAULT_PROVIDERS)));
    let selectedProviderId = $state(DEFAULT_PROVIDERS[0].id);
    let selectedProvider = $derived(providers.find(p => p.id === selectedProviderId) || providers[0]);

    // Global App Settings
    let activeProviderId = $state('ollama');
    let exportPath = $state('');
    let saveTxt = $state(true);
    let currentLanguage = $state<Language>('en');
    
    // LLM & OCR Settings
    let llmMaxChars = $state(5000);
    let llmPrompt = $state('Extract metadata from this document text. Return JSON ONLY. { "title": "...", "author": "...", "year": "..." }.');
    let ocrEnabled = $state(false);
    let authorSortEnabled = $state(false);

    let loadingModels = $state(false);
    let testingConnection = $state(false);
    let testResult = $state<{ success: boolean; message: string } | null>(null);

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
        llmPrompt = await getSetting('llmPrompt', 'Extract metadata from this document text. Return JSON ONLY. { "title": "...", "author": "...", "year": "..." }.');
        ocrEnabled = await getSetting('ocrEnabled', false);
        authorSortEnabled = await getSetting('authorSortEnabled', false);
    });

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
        i18n.setLanguage(currentLanguage);
        alert(i18n.t.settings.saved);
    }

    async function pickExportPath() {
        const selected = await open({
            directory: true,
            multiple: false,
            title: 'Select Export Directory'
        });
        if (typeof selected === 'string') {
            exportPath = selected;
        }
    }

    async function handleRefreshModels() {
        if (!selectedProvider.apiKey && selectedProvider.id !== 'ollama') {
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
            await saveSetting('providers', $state.snapshot(providers));
        } catch (error: any) {
            const msg = error instanceof Error ? error.message : String(error);
            alert(`${i18n.t.settings.fetch_failed}: ${msg}`);
        } finally {
            loadingModels = false;
        }
    }

    async function handleTestConnection() {
        if (!selectedProvider.selectedModel && selectedProvider.models.length > 0) {
            selectedProvider.selectedModel = selectedProvider.models[0];
        }
        
        if (!selectedProvider.selectedModel) {
            alert(i18n.t.settings.select_model);
            return;
        }

        testingConnection = true;
        testResult = null;
        try {
            const response = await llmClient.query(selectedProvider.id, selectedProvider.selectedModel, 'Hello, are you working?', selectedProvider.apiKey);
            testResult = { success: true, message: `${i18n.t.settings.test_success} Response: ${response.substring(0, 50)}...` };
        } catch (error: any) {
            const msg = error instanceof Error ? error.message : String(error);
            testResult = { success: false, message: `${i18n.t.settings.test_error}: ${msg}` };
        } finally {
            testingConnection = false;
        }
    }
</script>

<div class="settings-container">
    <div class="sidebar">
        <h2>{i18n.t.settings.providers}</h2>
        <div class="provider-list">
            {#each providers as provider}
                <button 
                    class="provider-btn" 
                    class:active={selectedProviderId === provider.id}
                    onclick={() => selectedProviderId = provider.id}
                >
                    {provider.name}
                    {#if activeProviderId === provider.id}
                        <Zap size={12} style="color: #eab308;" />
                    {/if}
                </button>
            {/each}
        </div>
        
        <div class="sidebar-divider"></div>
        <h2>{i18n.t.settings.app_settings}</h2>
        <button class="provider-btn" class:active={selectedProviderId === 'global'} onclick={() => selectedProviderId = 'global'}>
            {i18n.t.settings.general}
        </button>
    </div>

    <div class="content">
        {#if selectedProviderId === 'global'}
            <div class="header">
                <h1>{i18n.t.settings.general}</h1>
                <button class="save-btn" onclick={handleSave}>{i18n.t.settings.save_all}</button>
            </div>

            <div class="form-group">
                <label for="active-prov-select">{i18n.t.settings.active_provider}</label>
                <select id="active-prov-select" bind:value={activeProviderId} class="styled-select">
                    {#each providers as provider}
                        <option value={provider.id}>{provider.name}</option>
                    {/each}
                </select>
            </div>

            <div class="form-group">
                <label for="lang-select"><Languages size={16} /> {i18n.t.settings.language}</label>
                <select id="lang-select" bind:value={currentLanguage} class="styled-select">
                    <option value="en">English</option>
                    <option value="de">Deutsch</option>
                </select>
            </div>

            <div class="form-group">
                <label for="export-path-input"><FolderOpen size={16} /> {i18n.t.settings.export_dir}</label>
                <div class="input-with-action">
                    <input id="export-path-input" type="text" bind:value={exportPath} placeholder="Path..." />
                    <button class="action-btn" onclick={pickExportPath}>{i18n.t.settings.browse}</button>
                </div>
                <p class="hint">{i18n.t.settings.dir_hint}</p>
            </div>

            <div class="form-group checkbox-group">
                <input id="save-txt-check" type="checkbox" bind:checked={saveTxt} />
                <label for="save-txt-check"><Save size={16} /> {i18n.t.settings.save_txt}</label>
                <span class="hint">{i18n.t.settings.save_txt_hint}</span>
            </div>

            <div class="header" style="margin-top: 40px;">
                <h1>{i18n.t.settings.llm_options}</h1>
            </div>

            <div class="form-group">
                <label for="max-chars-input"><MessageSquare size={16} /> {i18n.t.settings.llm_max_chars}</label>
                <input id="max-chars-input" type="number" bind:value={llmMaxChars} min="500" step="500" class="styled-input" />
            </div>

            <div class="form-group">
                <label for="prompt-textarea"><MessageSquare size={16} /> {i18n.t.settings.llm_prompt}</label>
                <textarea id="prompt-textarea" bind:value={llmPrompt} rows="4" class="styled-textarea"></textarea>
                <p class="hint">{i18n.t.settings.llm_prompt_hint}</p>
            </div>

            <div class="form-group checkbox-group">
                <input id="author-sort-check" type="checkbox" bind:checked={authorSortEnabled} />
                <label for="author-sort-check"><Edit size={16} /> {i18n.t.settings.author_sort}</label>
                <span class="hint">{i18n.t.settings.author_sort_hint}</span>
            </div>

            <div class="header" style="margin-top: 40px;">
                <h1>{i18n.t.settings.ocr_options}</h1>
            </div>

            <div class="form-group checkbox-group">
                <input id="ocr-check" type="checkbox" bind:checked={ocrEnabled} />
                <label for="ocr-check"><Scan size={16} /> {i18n.t.settings.ocr_enabled}</label>
                <p class="hint">{i18n.t.settings.ocr_hint}</p>
            </div>

        {:else}
            <div class="header">
                <h1>{selectedProvider.name} Settings</h1>
                <button class="save-btn" onclick={handleSave}>{i18n.t.settings.save_all}</button>
            </div>

            <div class="form-group">
                <label for="base-url-input">
                    <Globe size={16} /> {i18n.t.settings.base_url}
                </label>
                <input id="base-url-input" type="text" bind:value={selectedProvider.baseUrl} placeholder="https://api..." />
            </div>

            <div class="form-group">
                <label for="api-key-input">
                    <Key size={16} /> {i18n.t.settings.api_key}
                </label>
                <div class="input-with-action">
                    <input id="api-key-input" type="password" bind:value={selectedProvider.apiKey} placeholder="sk-..." />
                </div>
            </div>

            <div class="form-group">
                <label for="model-select">
                    <Cpu size={16} /> {i18n.t.settings.select_model}
                </label>
                <select id="model-select" bind:value={selectedProvider.selectedModel} class="styled-select">
                    <option value="">-- {i18n.t.settings.select_model} --</option>
                    {#each selectedProvider.models as model}
                        <option value={model}>{model}</option>
                    {/each}
                </select>
            </div>

            <div class="actions">
                <button class="action-btn" onclick={handleRefreshModels} disabled={loadingModels}>
                    {#if loadingModels}
                        <Loader2 class="loader-spin" size={16} />
                    {:else}
                        <RefreshCw size={16} />
                    {/if}
                    {i18n.t.settings.refresh_models}
                </button>

                <button class="action-btn test-btn" onclick={handleTestConnection} disabled={testingConnection}>
                    {#if testingConnection}
                        <Loader2 class="loader-spin" size={16} />
                    {:else}
                        <CheckCircle size={16} />
                    {/if}
                    {i18n.t.settings.test_connection}
                </button>
            </div>

            {#if testResult}
                <div class="test-result-box" class:success={testResult.success} class:error={!testResult.success}>
                    {#if testResult.success}
                        <CheckCircle size={16} />
                    {:else}
                        <XCircle size={16} />
                    {/if}
                    <span>{testResult.message}</span>
                </div>
            {/if}

            <div class="models-section">
                <label>
                    <Cpu size={16} /> {i18n.t.settings.available_models} ({selectedProvider.models.length})
                </label>
                <div class="models-list-view">
                    {#if selectedProvider.models.length > 0}
                        <ul>
                            {#each selectedProvider.models as model}
                                <li class:active-item-row={selectedProvider.selectedModel === model}>
                                    {model}
                                    {#if selectedProvider.selectedModel === model}
                                        <CheckCircle size={12} style="color: #3b82f6;" />
                                    {/if}
                                </li>
                            {/each}
                        </ul>
                    {:else}
                        <p class="empty-hint">{i18n.t.settings.no_models}</p>
                    {/if}
                </div>
            </div>
        {/if}
    </div>
</div>

<style>
    .settings-container { display: flex; height: 100%; background: #09090b; color: #fafafa; font-family: 'Inter', sans-serif; overflow: hidden; }
    .sidebar { width: 240px; background: #18181b; border-right: 1px solid #27272a; padding: 20px 0; display: flex; flex-direction: column; flex-shrink: 0; }
    .sidebar h2 { padding: 0 20px; font-size: 0.875rem; text-transform: uppercase; color: #71717a; margin-bottom: 12px; }
    .sidebar-divider { height: 1px; background: #27272a; margin: 20px 0; }
    .provider-list { display: flex; flex-direction: column; }
    .provider-btn { padding: 10px 20px; text-align: left; border: none; background: transparent; cursor: pointer; font-size: 0.9375rem; color: #a1a1aa; transition: all 0.2s; display: flex; align-items: center; justify-content: space-between; }
    .provider-btn:hover { background: #27272a; color: white; }
    .provider-btn.active { background: #27272a; color: white; font-weight: 600; border-left: 3px solid #3b82f6; }
    .content { flex: 1; padding: 32px 48px; overflow-y: auto; }
    .header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 32px; }
    h1 { font-size: 1.5rem; font-weight: 700; margin: 0; }
    .save-btn { background: #3b82f6; color: white; border: none; padding: 8px 16px; border-radius: 6px; font-weight: 600; cursor: pointer; }
    .form-group { margin-bottom: 24px; max-width: 600px; }
    .checkbox-group { display: flex; align-items: center; gap: 12px; }
    .checkbox-group label { margin-bottom: 0; }
    label { display: flex; align-items: center; gap: 8px; font-size: 0.875rem; font-weight: 600; margin-bottom: 8px; color: #a1a1aa; }
    input[type="text"], input[type="password"], input[type="number"], .styled-select, .styled-textarea { width: 100%; padding: 10px 12px; border: 1px solid #27272a; border-radius: 6px; font-size: 0.9375rem; background: #18181b; color: white; }
    .styled-textarea { font-family: inherit; resize: vertical; }
    input:focus, .styled-select:focus, .styled-textarea:focus { outline: 2px solid #3b82f6; border-color: transparent; }
    .input-with-action { display: flex; gap: 10px; }
    .actions { display: flex; gap: 12px; margin-bottom: 24px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 8px 14px; border: 1px solid #27272a; background: #18181b; border-radius: 6px; font-size: 0.875rem; font-weight: 500; cursor: pointer; color: #d4d4d8; transition: background 0.2s; }
    .action-btn:hover { background: #27272a; }
    .test-btn { color: #10b981; border-color: #064e3b; }
    .test-result-box { padding: 12px; border-radius: 6px; font-size: 0.875rem; display: flex; align-items: center; gap: 8px; margin-bottom: 24px; max-width: 600px; border: 1px solid #27272a; }
    .test-result-box.success { background: #064e3b33; color: #ecfdf5; border-color: #065f46; }
    .test-result-box.error { background: #450a0a33; color: #fef2f2; border-color: #7f1d1d; }
    .models-list-view { background: #18181b; border: 1px solid #27272a; border-radius: 8px; padding: 12px; max-height: 300px; overflow-y: auto; }
    .models-list-view ul { list-style: none; padding: 0; margin: 0; }
    .models-list-view li { padding: 8px 12px; font-size: 0.875rem; border-bottom: 1px solid #27272a; color: #d4d4d8; display: flex; justify-content: space-between; align-items: center; }
    .models-list-view li.active-item-row { color: white; background: #27272a; }
    .hint { font-size: 0.75rem; color: #71717a; margin-top: 4px; display: block; }
    .loader-spin { animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
