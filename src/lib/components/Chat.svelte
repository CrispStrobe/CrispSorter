<script lang="ts">
    import { batchManager } from '../batch/store.svelte';
    import { DEFAULT_PROVIDERS, llmClient, type LLMProvider } from '../llm/client';
    import { getSetting } from '../store';
    import { i18n } from '../i18n.svelte';
    import { 
        Send, User, Bot, Trash2, FileText, 
        ChevronRight, ChevronLeft, Cpu, Zap, Search, MessageSquare, Brain,
        Loader2, Info, ChevronDown, ChevronUp
    } from 'lucide-svelte';
    import { onMount, tick } from 'svelte';
    import { getWebLLMLoadedModel } from '../llm/webllm';
    import { getORTLoadedModel } from '../llm/ort';
    import { saveSetting } from '../store';
    import katex from 'katex';
    import 'katex/dist/katex.min.css';
    import 'deep-chat';

    // Global KaTeX for Deep Chat
    if (typeof window !== 'undefined') {
        (window as any).katex = katex;
    }

    // Engine Selection & Models
    let providers = $state<LLMProvider[]>(JSON.parse(JSON.stringify(DEFAULT_PROVIDERS)));
    let activeProviderId = $state('ollama');
    let selectedModel = $state('');
    let localModels = $state<any[]>([]);
    let llmContextLimit = $state(4096);
    let llmTemperature = $state(0.7);
    let systemInstruction = $state('You are a helpful AI assistant. Use the provided context to answer questions accurately.');
    
    // UI State
    let selectedIds = $state<string[]>([]);
    let sidebarCollapsed = $state(false);
    let settingsCollapsed = $state(true);
    let chatElement = $state<any>();
    let chatHistorySize = $state(0);
    let fileSearchTerm = $state('');

    // Derived Context Info
    const filteredContextItems = $derived.by(() => {
        if (!fileSearchTerm.trim()) return batchManager.items;
        const term = fileSearchTerm.toLowerCase();
        return batchManager.items.filter(i => i.originalName.toLowerCase().includes(term));
    });

    const selectedItems = $derived(batchManager.items.filter(i => selectedIds.includes(i.id)));
    const docContextSize = $derived.by(() => {
        let total = 0;
        selectedItems.forEach(item => {
            if (item.extractedText) total += new TextEncoder().encode(item.extractedText).length;
        });
        return total;
    });

    function formatSize(bytes: number) {
        if (bytes === 0) return "0 B";
        if (bytes < 4096) return `${bytes} B`;
        return `${(bytes / 1024).toFixed(1)} KB`;
    }

    // --- Deep Chat Configuration ---
    
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
        "placeholder": {"text": "Type your message...", "style": {"color": "#71717a"}},
        "style": {"backgroundColor": "#09090b", "color": "white", "border": "1px solid #27272a", "borderRadius": "8px", "fontSize": "1rem", "padding": "12px"}
    };

    onMount(async () => {
        const savedProviders = await getSetting('providers');
        if (savedProviders) providers = savedProviders as LLMProvider[];
        activeProviderId = await getSetting('activeProviderId', 'ollama');
        localModels = await getSetting('localModels', []) as any[];
        llmContextLimit = await getSetting('llmContextLimit', 4096);
        llmTemperature = await getSetting('llmTemperature', 0.7);
        systemInstruction = await getSetting('systemInstruction', 'You are a helpful AI assistant. Use the provided context to answer questions accurately.');

        const currentProv = providers.find(p => p.id === activeProviderId);
        if (currentProv) {
            selectedModel = currentProv.selectedModel || currentProv.models[0] || '';
        }

        await tick();
        if (chatElement) {
            chatElement.connect = { handler: handleRequest };
            chatElement.messageStyles = messageStyles;
            chatElement.submitButtonStyles = submitButtonStyles;
            chatElement.textInput = textInputConfig;
            chatElement.inputAreaStyle = { "backgroundColor": "#18181b", "borderTop": "1px solid #27272a", "padding": "15px" };
            chatElement.introPanel = { display: false };
            chatElement.remarkable = { math: true, html: true, breaks: true };
            
            chatElement.onMessage = () => {
                const msgs = chatElement.getMessages();
                let size = 0;
                msgs.forEach((m: any) => {
                    if (m.text) size += new TextEncoder().encode(m.text).length;
                });
                chatHistorySize = size;
            };
        }
    });

    async function handleRequest(body: any, signals: any) {
        try {
            const userMsg = body.messages[body.messages.length - 1].text;
            // For browser-based engines, fall back to the loaded model even if selectedModel is empty
            let effectiveModel = selectedModel;
            if (activeProviderId === 'webllm') effectiveModel = getWebLLMLoadedModel() || selectedModel;
            else if (activeProviderId === 'ort') effectiveModel = getORTLoadedModel() || selectedModel;
            if (!activeProviderId || !effectiveModel) {
                const noModelMsg = activeProviderId === 'webllm' ? 'WebLLM: no model loaded — open Settings and click Load.'
                    : activeProviderId === 'ort' ? 'ORT: no model loaded — open Settings and click Load.'
                    : 'No engine selected.';
                return signals.onResponse({ error: noModelMsg });
            }

            let contextPrompt = systemInstruction + "\n\n";
            const selectedItems = batchManager.items.filter(i => selectedIds.includes(i.id));
            if (selectedItems.length > 0) {
                contextPrompt += "Use the document contexts below to answer correctly.\n\n";
                // Evenly distribute context window across selected items
                const perDocLimit = Math.floor(llmContextLimit / selectedItems.length);
                const halfLimit = Math.floor(perDocLimit / 2);

                selectedItems.forEach(item => {
                    if (item.extractedText) {
                        let text = item.extractedText;
                        if (text.length > perDocLimit) {
                            text = text.substring(0, halfLimit) + "\n... [TRUNCATED] ...\n" + text.substring(text.length - halfLimit);
                        }
                        contextPrompt += `--- DOC: ${item.originalName} ---\n${text}\n\n`;
                    }
                });
                contextPrompt += "User Question: ";
            }

            const activeProv = providers.find(p => p.id === activeProviderId) || providers[0];
            const responseText = await llmClient.query(activeProviderId, effectiveModel, contextPrompt + userMsg, activeProv.apiKey, llmTemperature);
            
            // Normalize math for Remarkable (Deep Chat)
            let fixedText = responseText
                .replace(/\\\[([\s\S]*?)\\\]/g, (m, f) => `\n$$\n${f.trim()}\n$$\n`)
                .replace(/\\\(([\s\S]*?)\\\)/g, (m, f) => `$${f.trim()}$`)
                .replace(/(^|\n)\[([\s\S]*?)\]($|\n)/g, (m, pre, f, post) => `${pre}\n$$\n${f.trim()}\n$$\n${post}`);

            signals.onResponse({ text: fixedText });
        } catch (error: any) {
            signals.onResponse({ error: error.message });
        }
    }

    function toggleContext(id: string) {
        if (selectedIds.includes(id)) selectedIds = selectedIds.filter(i => i !== id);
        else selectedIds = [...selectedIds, id];
    }

    function clearChat() { 
        if (chatElement) chatElement.clearMessages();
        chatHistorySize = 0;
    }

    const activeProvider = $derived(providers.find(p => p.id === activeProviderId) || providers[0]);
    let availableModels = $derived.by(() => {
        if (['mistralrs', 'llamacpp'].includes(activeProviderId)) return localModels.filter(m => m.isDownloaded).map(m => m.path);
        if (activeProviderId === 'webllm') {
            const loaded = getWebLLMLoadedModel();
            return loaded ? [loaded] : [];
        }
        if (activeProviderId === 'ort') {
            const loaded = getORTLoadedModel();
            return loaded ? [loaded] : [];
        }
        return activeProvider.models;
    });
</script>

<div class="chat-container">
    <div class="chat-sidebar" class:collapsed={sidebarCollapsed}>
        <button class="sidebar-toggle" onclick={() => sidebarCollapsed = !sidebarCollapsed}>
            {#if sidebarCollapsed}<ChevronRight size={14} />{:else}<ChevronLeft size={14} />{/if}
        </button>

        {#if !sidebarCollapsed}
            <!-- Collapsible Settings Section -->
            <div class="sidebar-section">
                <button class="section-toggle-btn" onclick={() => settingsCollapsed = !settingsCollapsed}>
                    <div class="sidebar-header">
                        <h3>{i18n.t.chat.session_settings}</h3>
                        {#if settingsCollapsed}<ChevronDown size={14} />{:else}<ChevronUp size={14} />{/if}
                    </div>
                </button>

                {#if !settingsCollapsed}
                    <div class="engine-selectors">
                        <div class="select-group">
                            <label for="chat-prov"><Zap size={12} /> {i18n.t.chat.provider}</label>
                            <select id="chat-prov" bind:value={activeProviderId} class="styled-select chat-select">
                                {#each providers as provider}<option value={provider.id}>{provider.name}</option>{/each}
                            </select>
                        </div>
                        <div class="select-group">
                            <label for="chat-model"><Cpu size={12} /> {i18n.t.chat.model}</label>
                            <select id="chat-model" bind:value={selectedModel} class="styled-select chat-select">
                                <option value="">-- {i18n.t.settings.select_model} --</option>
                                {#each availableModels as model}<option value={model}>{model.split(/[\\/]/).pop()}</option>{/each}
                            </select>
                        </div>
                        <div class="select-group">
                            <label for="sys-instr"><MessageSquare size={12} /> {i18n.t.chat.system_instructions}</label>
                            <textarea id="sys-instr" bind:value={systemInstruction} class="chat-area" onchange={() => saveSetting('systemInstruction', systemInstruction)} placeholder="Persona..."></textarea>
                        </div>
                        <div class="select-group">
                            <label for="ctx-lim"><Brain size={12} /> {i18n.t.chat.context_limit} ({llmContextLimit})</label>
                            <input id="ctx-lim" type="number" bind:value={llmContextLimit} min="1024" step="1024" class="chat-input" onchange={() => saveSetting('llmContextLimit', llmContextLimit)} />
                        </div>
                        <div class="select-group">
                            <label for="temp-slider"><Zap size={12} /> {i18n.t.chat.temperature} ({llmTemperature.toFixed(1)})</label>
                            <input id="temp-slider" type="range" bind:value={llmTemperature} min="0" max="1.5" step="0.1" class="styled-range" onchange={() => saveSetting('llmTemperature', llmTemperature)} />
                        </div>
                    </div>
                {/if}
            </div>

            <div class="sidebar-section scrollable">
                <div class="sidebar-header">
                    <h3>{i18n.t.chat.context}</h3>
                    <div class="sidebar-search-box">
                        <Search size={14} />
                        <input type="text" bind:value={fileSearchTerm} placeholder={i18n.t.chat.filter_files} class="sidebar-search-input" />
                    </div>
                </div>
                <div class="context-list">
                    {#each filteredContextItems as item}
                        <button class="context-item" class:selected={selectedIds.includes(item.id)} onclick={() => toggleContext(item.id)}>
                            <FileText size={14} />
                            <div class="context-item-info">
                                <span class="file-name">{item.originalName}</span>
                                <span class="file-size-hint">{item.size > 0 ? formatSize(item.size) : (item.extractedText ? formatSize(item.extractedText.length) : '—')}</span>
                            </div>
                        </button>
                    {/each}
                </div>
            </div>
        {/if}
    </div>

    <div class="chat-main">
        <div class="chat-header" class:extra-pad={sidebarCollapsed}>
            <div class="header-info">
                <h2>{i18n.t.chat.title}</h2>
                <div class="context-stats">
                    <span class="stat-badge">Docs: {selectedIds.length} ({formatSize(docContextSize)})</span>
                    <span class="stat-badge history">Chat: {formatSize(chatHistorySize)}</span>
                </div>
            </div>
            <button class="icon-btn danger" onclick={clearChat} title="Clear Messages"><Trash2 size={16} /></button>
        </div>

        <div class="chat-content">
            <deep-chat 
                bind:this={chatElement} 
                style="width: 100%; height: 100%; border: none; background-color: #09090b; position: absolute; top: 0; left: 0;"
            ></deep-chat>
        </div>
    </div>
</div>

<style>
    .chat-container { display: flex; height: 100%; width: 100%; background: #09090b; position: relative; overflow: hidden; }
    .chat-sidebar { width: 260px; background: #18181b; border-right: 1px solid #27272a; display: flex; flex-direction: column; transition: width 0.3s ease; position: relative; flex-shrink: 0; }
    .chat-sidebar.collapsed { width: 0; border-right: none; }
    .sidebar-toggle { position: absolute; top: 12px; right: -12px; width: 24px; height: 24px; background: #27272a; border: 1px solid #3f3f46; border-radius: 50%; display: flex; align-items: center; justify-content: center; color: #a1a1aa; cursor: pointer; z-index: 100; }
    .chat-sidebar.collapsed .sidebar-toggle { right: -45px; top: 12px; }
    .sidebar-section { border-bottom: 1px solid #27272a; display: flex; flex-direction: column; width: 260px; overflow: hidden; }
    .sidebar-section.scrollable { flex: 1; overflow-y: auto; }
    .section-toggle-btn { width: 100%; background: transparent; border: none; padding: 0; cursor: pointer; text-align: left; color: inherit; transition: background 0.2s; }
    .section-toggle-btn:hover { background: #27272a; }
    .sidebar-header { padding: 16px 20px; display: flex; justify-content: space-between; align-items: center; }
    .sidebar-search-box { display: flex; align-items: center; gap: 8px; background: #09090b; border: 1px solid #27272a; border-radius: 6px; padding: 4px 10px; margin-top: 8px; flex: 1; }
    .sidebar-search-input { background: transparent; border: none; color: white; font-size: 0.75rem; width: 100%; outline: none; }
    .sidebar-header h3 { margin: 0; font-size: 0.75rem; text-transform: uppercase; color: #a1a1aa; letter-spacing: 0.05em; }
    .engine-selectors { padding: 0 20px 20px; display: flex; flex-direction: column; gap: 12px; }
    .select-group { display: flex; flex-direction: column; gap: 6px; }
    .select-group label { display: flex; align-items: center; gap: 6px; font-size: 0.7rem; color: #71717a; font-weight: 600; }
    .chat-select { background: #09090b !important; color: white !important; padding: 6px 8px !important; font-size: 0.8125rem !important; border: 1px solid #27272a !important; width: 100% !important; }
    .chat-area { background: #09090b; color: white; border: 1px solid #27272a; border-radius: 6px; padding: 8px; font-size: 0.75rem; width: 100%; resize: vertical; min-height: 60px; font-family: inherit; }
    .chat-input { background: #09090b; color: white; border: 1px solid #27272a; border-radius: 6px; padding: 4px 8px; font-size: 0.8125rem; width: 100%; }
    .styled-range { width: 100%; height: 6px; background: #27272a; border-radius: 3px; appearance: none; outline: none; margin: 10px 0; }
    .styled-range::-webkit-slider-thumb { appearance: none; width: 14px; height: 14px; background: #3b82f6; border-radius: 50%; cursor: pointer; }
    .context-list { padding: 10px; display: flex; flex-direction: column; gap: 4px; }
    .context-item { display: flex; align-items: center; gap: 8px; padding: 8px 12px; background: transparent; border: 1px solid transparent; border-radius: 6px; color: #a1a1aa; cursor: pointer; text-align: left; font-size: 0.8125rem; width: 100%; }
    .context-item:hover { background: #27272a; color: white; }
    .context-item.selected { background: #1e3a8a33; border-color: #1e3a8a; color: #3b82f6; }
    .context-item-info { display: flex; flex-direction: column; align-items: flex-start; overflow: hidden; flex: 1; }
    .file-size-hint { font-size: 0.65rem; color: #52525b; margin-top: 1px; }
    .file-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
    .chat-main { flex: 1; display: flex; flex-direction: column; background: #09090b; height: 100%; width: 100%; overflow: hidden; min-width: 0; position: relative; }
    .chat-header { height: 64px; padding: 0 24px; background: #18181b; border-bottom: 1px solid #27272a; display: flex; justify-content: space-between; align-items: center; transition: padding-left 0.3s ease; flex-shrink: 0; }
    .chat-header.extra-pad { padding-left: 64px; }
    .header-info { display: flex; align-items: center; gap: 16px; }
    .header-info h2 { margin: 0; font-size: 1rem; font-weight: 700; }
    .context-stats { display: flex; gap: 8px; }
    .stat-badge { font-size: 0.7rem; color: #a1a1aa; background: #27272a; padding: 2px 8px; border-radius: 10px; white-space: nowrap; border: 1px solid #3f3f46; }
    .stat-badge.history { color: #3b82f6; border-color: #1e3a8a; }
    .chat-content { flex: 1; width: 100%; position: relative; min-height: 0; }
    .icon-btn { background: transparent; border: none; color: #71717a; cursor: pointer; padding: 6px; border-radius: 6px; display: flex; align-items: center; justify-content: center; }
    .icon-btn:hover { background: #27272a; color: white; }
</style>
