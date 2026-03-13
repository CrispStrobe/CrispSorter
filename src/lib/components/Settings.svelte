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
    let llmPrompt = $state('Extract metadata from this document text. Use the provided context tags.');
    let ocrEnabled = $state(false);
    let authorSortEnabled = $state(false);
    let localModelPath = $state('');

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
        localModelPath = await getSetting('localModelPath', '');
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
        await saveSetting('localModelPath', localModelPath);
        i18n.setLanguage(currentLanguage);
        alert(i18n.t.settings.saved);
    }

    async function pickExportPath() {
        const selected = await open({ directory: true, multiple: false });
        if (typeof selected === 'string') exportPath = selected;
    }

    async function pickLocalModel() {
        const selected = await open({
            multiple: false,
            filters: [{ name: 'GGUF Models', extensions: ['gguf'] }]
        });
        if (typeof selected === 'string') localModelPath = selected;
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
            await saveSetting('providers', $state.snapshot(providers));
        } catch (error: any) {
            alert(`${i18n.t.settings.fetch_failed}: ${error.message}`);
        } finally {
            loadingModels = false;
        }
    }

    async function handleTestConnection() {
        const model = selectedProvider.selectedModel || selectedProvider.models[0];
        if (!model && selectedProvider.id !== 'mistralrs') {
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
                        <Zap size={12} class="active-zap" />
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

            <div class="section-card">
                <label>{i18n.t.settings.active_provider}</label>
                <select bind:value={activeProviderId} class="styled-select">
                    {#each providers as provider}
                        <option value={provider.id}>{provider.name}</option>
                    {/each}
                </select>
            </div>

            <div class="section-card">
                <label><Languages size={16} /> {i18n.t.settings.language}</label>
                <select bind:value={currentLanguage} class="styled-select">
                    <option value="en">English</option>
                    <option value="de">Deutsch</option>
                </select>
            </div>

            <div class="section-card">
                <label><FolderOpen size={16} /> {i18n.t.settings.export_dir}</label>
                <div class="input-with-action">
                    <input type="text" bind:value={exportPath} placeholder="Path..." />
                    <button class="action-btn small" onclick={pickExportPath}>{i18n.t.settings.browse}</button>
                </div>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <label><Save size={16} /> {i18n.t.settings.save_txt}</label>
                    <input type="checkbox" bind:checked={saveTxt} />
                </div>
            </div>

            <div class="header" style="margin-top: 40px;">
                <h1>{i18n.t.settings.llm_options}</h1>
            </div>

            <div class="section-card">
                <label><MessageSquare size={16} /> {i18n.t.settings.llm_max_chars}</label>
                <input type="number" bind:value={llmMaxChars} min="500" step="500" class="styled-input" />
            </div>

            <div class="section-card">
                <label><MessageSquare size={16} /> {i18n.t.settings.llm_prompt}</label>
                <textarea bind:value={llmPrompt} rows="4" class="styled-textarea"></textarea>
                <p class="hint">{i18n.t.settings.llm_prompt_hint}</p>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <label><Edit size={16} /> {i18n.t.settings.author_sort}</label>
                    <input type="checkbox" bind:checked={authorSortEnabled} />
                </div>
            </div>

            <div class="header" style="margin-top: 40px;">
                <h1>{i18n.t.settings.ocr_options}</h1>
            </div>

            <div class="section-card">
                <div class="checkbox-group">
                    <label><Scan size={16} /> {i18n.t.settings.ocr_enabled}</label>
                    <input type="checkbox" bind:checked={ocrEnabled} />
                </div>
                <p class="hint">{i18n.t.settings.ocr_hint}</p>
            </div>

        {:else}
            <div class="header">
                <h1>{selectedProvider.name}</h1>
                <button class="save-btn" onclick={handleSave}>{i18n.t.settings.save_all}</button>
            </div>

            {#if selectedProvider.id === 'mistralrs'}
                <div class="section-card">
                    <label>{i18n.t.settings.local_model_path}</label>
                    <div class="input-with-action">
                        <input type="text" bind:value={localModelPath} placeholder="Path to .gguf..." />
                        <button class="action-btn small" onclick={pickLocalModel}>{i18n.t.settings.browse}</button>
                    </div>
                    <p class="hint">{i18n.t.settings.local_model_hint}</p>
                </div>
            {:else}
                <div class="form-group">
                    <label>{i18n.t.settings.base_url}</label>
                    <input type="text" bind:value={selectedProvider.baseUrl} />
                </div>

                <div class="form-group">
                    <label>{i18n.t.settings.api_key}</label>
                    <input type="password" bind:value={selectedProvider.apiKey} />
                </div>

                <div class="form-group">
                    <label>{i18n.t.settings.select_model}</label>
                    <select bind:value={selectedProvider.selectedModel} class="styled-select">
                        <option value="">-- {i18n.t.settings.select_model} --</option>
                        {#each selectedProvider.models as model}
                            <option value={model}>{model}</option>
                        {/each}
                    </select>
                </div>

                <div class="actions">
                    <button class="action-btn" onclick={handleRefreshModels} disabled={loadingModels}>
                        <span class:loader-spin={loadingModels}>
                            {#if loadingModels}<Loader2 size={16} />{:else}<RefreshCw size={16} />{/if}
                        </span>
                        {i18n.t.settings.refresh_models}
                    </button>
                    <button class="action-btn test-btn" onclick={handleTestConnection} disabled={testingConnection}>
                        <span class:loader-spin={testingConnection}>
                            {#if testingConnection}<Loader2 size={16} />{:else}<CheckCircle size={16} />{/if}
                        </span>
                        {i18n.t.settings.test_connection}
                    </button>
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
    .active-zap { color: #eab308; }
    .content { flex: 1; padding: 32px 48px; overflow-y: auto; }
    .header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 24px; }
    h1 { font-size: 1.25rem; font-weight: 700; margin: 0; }
    .save-btn { background: #3b82f6; color: white; border: none; padding: 6px 12px; border-radius: 6px; font-weight: 600; cursor: pointer; font-size: 0.875rem; }
    .section-card { background: #18181b; border: 1px solid #27272a; padding: 16px; border-radius: 8px; margin-bottom: 16px; }
    .form-group { margin-bottom: 20px; max-width: 600px; }
    .checkbox-group { display: flex; align-items: center; gap: 12px; }
    label { display: flex; align-items: center; gap: 8px; font-size: 0.8125rem; font-weight: 600; margin-bottom: 10px; color: #a1a1aa; text-transform: uppercase; letter-spacing: 0.02em; }
    input[type="text"], input[type="password"], input[type="number"], .styled-select, .styled-textarea { width: 100%; padding: 8px 12px; border: 1px solid #27272a; border-radius: 6px; font-size: 0.875rem; background: #09090b; color: white; }
    .styled-textarea { font-family: inherit; resize: vertical; }
    input:focus, .styled-select:focus, .styled-textarea:focus { outline: 2px solid #3b82f6; border-color: transparent; }
    .input-with-action { display: flex; gap: 10px; }
    .actions { display: flex; gap: 12px; margin-bottom: 24px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 6px 12px; border: 1px solid #27272a; background: #18181b; border-radius: 6px; font-size: 0.8125rem; font-weight: 600; cursor: pointer; color: #d4d4d8; }
    .action-btn:hover { background: #27272a; }
    .test-btn { color: #10b981; border-color: #064e3b; }
    .test-result { padding: 10px; border-radius: 6px; font-size: 0.8125rem; margin-bottom: 24px; max-width: 600px; }
    .test-result.success { background: #064e3b; color: #ecfdf5; }
    .test-result.error { background: #450a0a; color: #fef2f2; }
    .models-list { background: #09090b; border: 1px solid #27272a; border-radius: 8px; padding: 12px; max-height: 250px; overflow: auto; }
    .models-list li { padding: 6px 10px; font-size: 0.8125rem; border-bottom: 1px solid #27272a; color: #d4d4d8; display: flex; justify-content: space-between; align-items: center; }
    .models-list li.active-item { color: white; background: #27272a; border-radius: 4px; }
    .hint { font-size: 0.75rem; color: #71717a; margin-top: 6px; display: block; line-height: 1.4; }
    .loader-spin { display: inline-flex; animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
