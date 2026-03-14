<script lang="ts">
    import { batchManager } from '../batch/store.svelte';
    import { DEFAULT_PROVIDERS, llmClient, type LLMProvider } from '../llm/client';
    import { getSetting } from '../store';
    import { i18n } from '../i18n.svelte';
    import { 
        Bot, Trash2, FileText, ChevronRight, ChevronLeft, Cpu, Zap
    } from 'lucide-svelte';
    import { onMount, tick } from 'svelte';
    import katex from 'katex';
    import 'katex/dist/katex.min.css';
    import 'deep-chat';

    // Set KaTeX globally for Deep Chat native math support
    if (typeof window !== 'undefined') {
        (window as any).katex = katex;
    }

    // Engine Selection & Models
    let providers = $state<LLMProvider[]>(JSON.parse(JSON.stringify(DEFAULT_PROVIDERS)));
    let activeProviderId = $state('ollama');
    let selectedModel = $state('');
    let localModels = $state<any[]>([]);
    
    // UI State
    let selectedIds = $state<string[]>([]);
    let sidebarCollapsed = $state(false);
    let chatElement = $state<any>();

    // --- Configuration Objects ---
    
    const messageStyles = {
        "default": {
            "shared": {
                "bubble": { "maxWidth": "100%", "backgroundColor": "unset", "marginTop": "12px", "marginBottom": "12px", "fontSize": "1rem", "lineHeight": "1.6" }
            },
            "user": {
                "bubble": { "marginLeft": "0px", "color": "#fafafa", "backgroundColor": "#3b82f6", "padding": "12px 16px", "borderRadius": "12px" }
            },
            "ai": {
                "outerContainer": { "backgroundColor": "#18181b", "borderTop": "1px solid #27272a", "borderBottom": "1px solid #27272a" },
                "bubble": { "color": "#e2e8f0", "padding": "12px 16px" }
            }
        }
    };

    const submitButtonStyles = {
        "submit": {
            "container": {
                "default": {"backgroundColor": "#3b82f6"},
                "hover": {"backgroundColor": "#2563eb"}
            },
            "svg": {
                "content": '<?xml version="1.0" ?> <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"> <g> <path d="M21.66,12a2,2,0,0,1-1.14,1.81L5.87,20.75A2.08,2.08,0,0,1,5,21a2,2,0,0,1-1.82-2.82L5.46,13H11a1,1,0,0,0,0-2H5.46L3.18,5.87A2,2,0,0,1,5.86,3.25h0l14.65,6.94A2,2,0,0,1,21.66,12Z" fill="white"> </path> </g> </svg>',
                "styles": { "default": { "width": "1.3em", "marginTop": "0.15em" } }
            }
        }
    };

    const textInputConfig = {
        "placeholder": {"text": "Type your question...", "style": {"color": "#71717a"}},
        "style": {"backgroundColor": "#09090b", "color": "white", "border": "1px solid #27272a", "borderRadius": "8px", "fontSize": "1rem", "padding": "12px"}
    };

    onMount(async () => {
        const savedProviders = await getSetting('providers');
        if (savedProviders) providers = savedProviders as LLMProvider[];
        activeProviderId = await getSetting('activeProviderId', 'ollama');
        localModels = await getSetting('localModels', []) as any[];

        const currentProv = providers.find(p => p.id === activeProviderId);
        if (currentProv) {
            selectedModel = currentProv.selectedModel || currentProv.models[0] || '';
        }

        await tick();
        if (chatElement) {
            // Apply config ONCE to prevent input clearing on re-render
            chatElement.connect = { handler: handleRequest };
            chatElement.messageStyles = messageStyles;
            chatElement.submitButtonStyles = submitButtonStyles;
            chatElement.textInput = textInputConfig;
            chatElement.inputAreaStyle = { "backgroundColor": "#18181b", "borderTop": "1px solid #27272a", "padding": "15px" };
            chatElement.introPanel = { display: false };
            chatElement.remarkable = { math: true, html: true, breaks: true };
        }
    });

    async function handleRequest(body: any, signals: any) {
        try {
            const userMsg = body.messages[body.messages.length - 1].text;
            if (!activeProviderId || !selectedModel) {
                return signals.onResponse({ error: "No AI engine selected." });
            }

            let contextPrompt = "";
            const selectedItems = batchManager.items.filter(i => selectedIds.includes(i.id));
            if (selectedItems.length > 0) {
                contextPrompt = "Use these contexts to answer:\n\n";
                selectedItems.forEach(item => {
                    if (item.extractedText) {
                        let text = item.extractedText;
                        if (text.length > 3000) {
                            text = text.substring(0, 1500) + "\n... [TRUNCATED] ...\n" + text.substring(text.length - 1500);
                        }
                        contextPrompt += `--- DOC: ${item.originalName} ---\n${text}\n\n`;
                    }
                });
                contextPrompt += "User Question: ";
            }

            const responseText = await llmClient.query(activeProviderId, selectedModel, contextPrompt + userMsg, activeProvider.apiKey);
            
            // Normalize ALL common math delimiters to $ and $$ for Deep Chat Remarkable plugin
            let fixedText = responseText
                // Handle \( ... \) -> $...$
                .replace(/\\\(([\s\S]*?)\\\)/g, '$$$1$$')
                // Handle \[ ... \] -> $$$...$$$
                .replace(/\\\[([\s\S]*?)\\\]/g, '$$$$$1$$$$')
                // Handle raw [ ... ] blocks if they look like math
                .replace(/(^|\n)\[([\s\S]*?)\]($|\n)/g, '$1$$$$$2$$$$$3');

            signals.onResponse({ text: fixedText });
        } catch (error: any) {
            signals.onResponse({ error: error.message });
        }
    }

    function toggleContext(id: string) {
        if (selectedIds.includes(id)) selectedIds = selectedIds.filter(i => i !== id);
        else selectedIds = [...selectedIds, id];
    }

    function clearChat() { if (chatElement) chatElement.clearMessages(); }

    const activeProvider = $derived(providers.find(p => p.id === activeProviderId) || providers[0]);
    let availableModels = $derived.by(() => {
        if (['mistralrs', 'llamacpp'].includes(activeProviderId)) return localModels.filter(m => m.isDownloaded).map(m => m.path);
        return activeProvider.models;
    });
</script>

<div class="chat-container">
    <div class="chat-sidebar" class:collapsed={sidebarCollapsed}>
        <button class="sidebar-toggle" onclick={() => sidebarCollapsed = !sidebarCollapsed}>
            {#if sidebarCollapsed}<ChevronRight size={14} />{:else}<ChevronLeft size={14} />{/if}
        </button>

        {#if !sidebarCollapsed}
            <div class="sidebar-section">
                <div class="sidebar-header"><h3>Engine</h3></div>
                <div class="engine-selectors">
                    <div class="select-group">
                        <label for="chat-prov"><Zap size={12} /> Provider</label>
                        <select id="chat-prov" bind:value={activeProviderId} class="styled-select chat-select">
                            {#each providers as provider}<option value={provider.id}>{provider.name}</option>{/each}
                        </select>
                    </div>
                    <div class="select-group">
                        <label for="chat-model"><Cpu size={12} /> Model</label>
                        <select id="chat-model" bind:value={selectedModel} class="styled-select chat-select">
                            <option value="">-- Select --</option>
                            {#each availableModels as model}<option value={model}>{model.split(/[\\/]/).pop()}</option>{/each}
                        </select>
                    </div>
                </div>
            </div>

            <div class="sidebar-section scrollable">
                <div class="sidebar-header"><h3>{i18n.t.chat.context}</h3></div>
                <div class="context-list">
                    {#each batchManager.items as item}
                        <button class="context-item" class:selected={selectedIds.includes(item.id)} onclick={() => toggleContext(item.id)}>
                            <FileText size={14} />
                            <span class="file-name">{item.originalName}</span>
                        </button>
                    {/each}
                </div>
            </div>
        {/if}
    </div>

    <div class="chat-main">
        <div class="chat-header">
            <div class="header-info">
                <h2>{i18n.t.chat.title}</h2>
                <span class="context-count">{selectedIds.length} context</span>
            </div>
            <button class="icon-btn danger" onclick={clearChat} title="Clear Messages"><Trash2 size={16} /></button>
        </div>

        <div class="chat-content">
            {#if selectedIds.length === 0}
                <div class="welcome-overlay">
                    <Bot size={48} color="#71717a" />
                    <p>{i18n.t.chat.no_context}</p>
                </div>
            {/if}
            <deep-chat bind:this={chatElement} style="width: 100%; height: 100%; border: none; background-color: #09090b;"></deep-chat>
        </div>
    </div>
</div>

<style>
    .chat-container { display: flex; height: 100%; background: #09090b; position: relative; overflow: hidden; }
    .chat-sidebar { width: 260px; background: #18181b; border-right: 1px solid #27272a; display: flex; flex-direction: column; transition: width 0.3s ease; position: relative; flex-shrink: 0; }
    .chat-sidebar.collapsed { width: 0; border-right: none; }
    .sidebar-toggle { position: absolute; top: 12px; right: -12px; width: 24px; height: 24px; background: #27272a; border: 1px solid #3f3f46; border-radius: 50%; display: flex; align-items: center; justify-content: center; color: #a1a1aa; cursor: pointer; z-index: 100; }
    .chat-sidebar.collapsed .sidebar-toggle { right: -30px; top: 20px; }
    .sidebar-section { border-bottom: 1px solid #27272a; display: flex; flex-direction: column; width: 260px; overflow: hidden; }
    .sidebar-section.scrollable { flex: 1; overflow-y: auto; }
    .sidebar-header { padding: 16px 20px; }
    .sidebar-header h3 { margin: 0; font-size: 0.75rem; text-transform: uppercase; color: #a1a1aa; letter-spacing: 0.05em; }
    .engine-selectors { padding: 0 20px 20px; display: flex; flex-direction: column; gap: 12px; }
    .select-group { display: flex; flex-direction: column; gap: 6px; }
    .select-group label { display: flex; align-items: center; gap: 6px; font-size: 0.7rem; color: #71717a; font-weight: 600; }
    .chat-select { background: #09090b !important; color: white !important; padding: 6px 8px !important; font-size: 0.8125rem !important; border: 1px solid #27272a !important; width: 100% !important; }
    .context-list { padding: 10px; display: flex; flex-direction: column; gap: 4px; }
    .context-item { display: flex; align-items: center; gap: 8px; padding: 8px 12px; background: transparent; border: 1px solid transparent; border-radius: 6px; color: #a1a1aa; cursor: pointer; text-align: left; font-size: 0.8125rem; width: 100%; }
    .context-item:hover { background: #27272a; color: white; }
    .context-item.selected { background: #1e3a8a33; border-color: #1e3a8a; color: #3b82f6; }
    .file-name { flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .chat-main { flex: 1; display: flex; flex-direction: column; background: #09090b; overflow: hidden; }
    .chat-header { padding: 12px 24px; background: #18181b; border-bottom: 1px solid #27272a; display: flex; justify-content: space-between; align-items: center; }
    .header-info { display: flex; align-items: center; gap: 16px; }
    .header-info h2 { margin: 0; font-size: 1rem; font-weight: 700; }
    .context-count { font-size: 0.75rem; color: #71717a; background: #27272a; padding: 2px 8px; border-radius: 10px; }
    .chat-content { flex: 1; position: relative; display: flex; flex-direction: column; }
    .welcome-overlay { position: absolute; top: 50%; left: 50%; transform: translate(-50%, -50%); display: flex; flex-direction: column; align-items: center; color: #71717a; gap: 16px; pointer-events: none; z-index: 10; text-align: center; width: 80%; }
    .icon-btn { background: transparent; border: none; color: #71717a; cursor: pointer; padding: 6px; border-radius: 6px; display: flex; align-items: center; justify-content: center; }
    .icon-btn:hover { background: #27272a; color: white; }
</style>
