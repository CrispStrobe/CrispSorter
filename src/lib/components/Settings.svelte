<script lang="ts">
    import { onMount } from 'svelte';
    import { DEFAULT_PROVIDERS, type LLMProvider, llmClient } from '../llm/client';
    import { getSetting, saveSetting } from '../store';
    import { RefreshCw, CheckCircle, XCircle, Key, Globe, Cpu, Loader2 } from 'lucide-svelte';

    let providers = $state<LLMProvider[]>(JSON.parse(JSON.stringify(DEFAULT_PROVIDERS)));
    let selectedProviderId = $state(DEFAULT_PROVIDERS[0].id);
    let selectedProvider = $derived(providers.find(p => p.id === selectedProviderId) || providers[0]);

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
    });

    async function handleSave() {
        await saveSetting('providers', $state.snapshot(providers));
        alert('Settings saved!');
    }

    async function handleRefreshModels() {
        if (!selectedProvider.apiKey && selectedProvider.id !== 'ollama') {
            alert('Please enter an API key first.');
            return;
        }
        loadingModels = true;
        try {
            const models = await llmClient.fetchModels(selectedProvider.id, selectedProvider.apiKey, selectedProvider.baseUrl);
            selectedProvider.models = models;
            await handleSave();
        } catch (error: any) {
            alert(`Failed to fetch models: ${error.message}`);
        } finally {
            loadingModels = false;
        }
    }

    async function handleTestConnection() {
        testingConnection = true;
        testResult = null;
        try {
            const model = selectedProvider.models[0] || 'gpt-3.5-turbo';
            const response = await llmClient.query(selectedProvider.id, model, 'Hello, are you working?', selectedProvider.apiKey);
            testResult = { success: true, message: `Success! Response: ${response.substring(0, 50)}...` };
        } catch (error: any) {
            testResult = { success: false, message: `Error: ${error.message}` };
        } finally {
            testingConnection = false;
        }
    }
</script>

<div class="settings-container">
    <div class="sidebar">
        <h2>Providers</h2>
        <div class="provider-list">
            {#each providers as provider}
                <button 
                    class="provider-btn" 
                    class:active={selectedProviderId === provider.id}
                    onclick={() => selectedProviderId = provider.id}
                >
                    {provider.name}
                </button>
            {/each}
        </div>
    </div>

    <div class="content">
        <div class="header">
            <h1>{selectedProvider.name} Settings</h1>
            <button class="save-btn" onclick={handleSave}>Save Settings</button>
        </div>

        <div class="form-group">
            <label for="base-url">
                <Globe size={16} /> Base URL
            </label>
            <input id="base-url" type="text" bind:value={selectedProvider.baseUrl} placeholder="https://api..." />
        </div>

        <div class="form-group">
            <label for="api-key">
                <Key size={16} /> API Key
            </label>
            <div class="input-with-action">
                <input id="api-key" type="password" bind:value={selectedProvider.apiKey} placeholder="sk-..." />
            </div>
        </div>

        <div class="actions">
            <button class="action-btn" onclick={handleRefreshModels} disabled={loadingModels}>
                {#if loadingModels}
                    <Loader2 class="loader-icon" size={16} />
                {:else}
                    <RefreshCw size={16} />
                {/if}
                Refresh Models
            </button>

            <button class="action-btn test-btn" onclick={handleTestConnection} disabled={testingConnection}>
                {#if testingConnection}
                    <Loader2 class="loader-icon" size={16} />
                {:else}
                    <CheckCircle size={16} />
                {/if}
                Test Connection
            </button>
        </div>

        {#if testResult}
            <div class="test-result" class:success={testResult.success} class:error={!testResult.success}>
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
                <Cpu size={16} /> Available Models ({selectedProvider.models.length})
            </label>
            <div class="models-list">
                {#if selectedProvider.models.length > 0}
                    <ul>
                        {#each selectedProvider.models as model}
                            <li>{model}</li>
                        {/each}
                    </ul>
                {:else}
                    <p class="empty-hint">No models found. Click "Refresh Models" to fetch them.</p>
                {/if}
            </div>
        </div>
    </div>
</div>

<style>
    .settings-container { display: flex; height: 100%; background: var(--bg-main, #fff); color: var(--text-main, #333); font-family: 'Inter', sans-serif; }
    .sidebar { width: 240px; background: #f4f4f5; border-right: 1px solid #e4e4e7; padding: 20px 0; display: flex; flex-direction: column; }
    .sidebar h2 { padding: 0 20px; font-size: 0.875rem; text-transform: uppercase; color: #71717a; margin-bottom: 12px; }
    .provider-list { display: flex; flex-direction: column; }
    .provider-btn { padding: 10px 20px; text-align: left; border: none; background: transparent; cursor: pointer; font-size: 0.9375rem; color: #3f3f46; transition: all 0.2s; }
    .provider-btn:hover { background: #e4e4e7; }
    .provider-btn.active { background: #fff; color: #18181b; font-weight: 600; border-left: 3px solid #3b82f6; }
    .content { flex: 1; padding: 32px 48px; overflow-y: auto; }
    .header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 32px; }
    h1 { font-size: 1.5rem; font-weight: 700; margin: 0; }
    .save-btn { background: #3b82f6; color: white; border: none; padding: 8px 16px; border-radius: 6px; font-weight: 600; cursor: pointer; }
    .form-group { margin-bottom: 24px; max-width: 600px; }
    label { display: flex; align-items: center; gap: 8px; font-size: 0.875rem; font-weight: 600; margin-bottom: 8px; color: #4b5563; }
    input { width: 100%; padding: 10px 12px; border: 1px solid #d1d5db; border-radius: 6px; font-size: 0.9375rem; }
    input:focus { outline: 2px solid #3b82f6; border-color: transparent; }
    .actions { display: flex; gap: 12px; margin-bottom: 24px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 8px 14px; border: 1px solid #d1d5db; background: white; border-radius: 6px; font-size: 0.875rem; font-weight: 500; cursor: pointer; transition: background 0.2s; }
    .action-btn:hover { background: #f9fafb; }
    .test-btn { color: #059669; border-color: #a7f3d0; }
    .test-btn:hover { background: #ecfdf5; }
    .test-result { padding: 12px; border-radius: 6px; font-size: 0.875rem; display: flex; align-items: center; gap: 8px; margin-bottom: 24px; max-width: 600px; }
    .test-result.success { background: #ecfdf5; color: #065f46; border: 1px solid #a7f3d0; }
    .test-result.error { background: #fef2f2; color: #991b1b; border: 1px solid #fecaca; }
    .models-section { margin-top: 32px; max-width: 600px; }
    .models-list { background: #f9fafb; border: 1px solid #e5e7eb; border-radius: 8px; padding: 12px; max-height: 300px; overflow-y: auto; }
    .models-list ul { list-style: none; padding: 0; margin: 0; }
    .models-list li { padding: 6px 0; font-size: 0.875rem; border-bottom: 1px solid #f3f4f6; }
    .models-list li:last-child { border-bottom: none; }
    .empty-hint { color: #9ca3af; font-size: 0.875rem; text-align: center; margin: 20px 0; }
    .loader-icon { animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
    @media (prefers-color-scheme: dark) {
        .sidebar { background: #18181b; border-color: #27272a; }
        .sidebar h2 { color: #a1a1aa; }
        .provider-btn { color: #d4d4d8; }
        .provider-btn:hover { background: #27272a; }
        .provider-btn.active { background: #09090b; color: #fafafa; }
        .content { background: #09090b; color: #fafafa; }
        label { color: #a1a1aa; }
        input { background: #18181b; border-color: #27272a; color: white; }
        .action-btn { background: #18181b; border-color: #27272a; color: #d4d4d8; }
        .action-btn:hover { background: #27272a; }
        .models-list { background: #18181b; border-color: #27272a; }
        .models-list li { border-color: #27272a; }
    }
</style>
