<script lang="ts">
    import { batchManager } from '../batch/store.svelte';
    import { DEFAULT_PROVIDERS, llmClient, type LLMProvider } from '../llm/client';
    import { getSetting } from '../store';
    import { i18n } from '../i18n.svelte';
    import { 
        Send, User, Bot, Trash2, FileText, 
        ChevronRight, Loader2, Info, Cpu, Zap
    } from 'lucide-svelte';
    import { onMount } from 'svelte';

    interface Message {
        role: 'user' | 'assistant';
        content: string;
        timestamp: number;
    }

    let messages = $state<Message[]>([]);
    let input = $state('');
    let isTyping = $state(false);
    
    // Use an array for selected IDs to ensure clean Svelte 5 reactivity
    let selectedIds = $state<string[]>([]);
    let scrollContainer: HTMLDivElement;

    // Chat-specific engine selection
    let providers = $state<LLMProvider[]>(JSON.parse(JSON.stringify(DEFAULT_PROVIDERS)));
    let activeProviderId = $state('ollama');
    let selectedModel = $state('');
    let localModels = $state<any[]>([]);

    onMount(async () => {
        const savedProviders = await getSetting('providers');
        if (savedProviders) providers = savedProviders as LLMProvider[];
        
        activeProviderId = await getSetting('activeProviderId', 'ollama');
        localModels = await getSetting('localModels', []) as any[];

        const currentProv = providers.find(p => p.id === activeProviderId);
        if (currentProv) {
            selectedModel = currentProv.selectedModel || currentProv.models[0] || '';
        }
    });

    const activeProvider = $derived(providers.find(p => p.id === activeProviderId) || providers[0]);
    
    let availableModels = $derived.by(() => {
        if (['mistralrs', 'llamacpp'].includes(activeProviderId)) {
            return localModels.filter(m => m.isDownloaded).map(m => m.path);
        }
        return activeProvider.models;
    });

    const selectedItems = $derived(
        batchManager.items.filter(i => selectedIds.includes(i.id))
    );

    function toggleContext(id: string) {
        if (selectedIds.includes(id)) {
            selectedIds = selectedIds.filter(i => i !== id);
        } else {
            selectedIds = [...selectedIds, id];
        }
    }

    async function handleSend() {
        if (!input.trim() || isTyping) return;

        const userMsg = input.trim();
        messages.push({ role: 'user', content: userMsg, timestamp: Date.now() });
        input = '';
        isTyping = true;

        try {
            if (!activeProviderId || !selectedModel) {
                throw new Error("No AI Provider or Model selected.");
            }

            // Build context from selected documents
            let contextPrompt = "";
            if (selectedItems.length > 0) {
                contextPrompt = "Use the following document context to answer the user's question:\n\n";
                selectedItems.forEach(item => {
                    if (item.extractedText) {
                        contextPrompt += `--- DOCUMENT: ${item.originalName} ---\n${item.extractedText.substring(0, 3000)}\n\n`;
                    }
                });
                contextPrompt += "--- END OF CONTEXT ---\n\nUser Question: ";
            }

            const fullPrompt = contextPrompt + userMsg;
            const response = await llmClient.query(activeProviderId, selectedModel, fullPrompt, activeProvider.apiKey);
            
            messages.push({ role: 'assistant', content: response, timestamp: Date.now() });
        } catch (error: any) {
            messages.push({ role: 'assistant', content: `Error: ${error.message}`, timestamp: Date.now() });
        } finally {
            isTyping = false;
            setTimeout(scrollToBottom, 50);
        }
    }

    function scrollToBottom() {
        if (scrollContainer) {
            scrollContainer.scrollTop = scrollContainer.scrollHeight;
        }
    }

    function clearChat() {
        messages = [];
    }
</script>

<div class="chat-container">
    <div class="chat-sidebar">
        <div class="sidebar-section">
            <div class="sidebar-header">
                <h3>Engine</h3>
            </div>
            <div class="engine-selectors">
                <div class="select-group">
                    <label for="chat-prov"><Zap size={12} /> Provider</label>
                    <select id="chat-prov" bind:value={activeProviderId} class="styled-select chat-select">
                        {#each providers as provider}
                            <option value={provider.id}>{provider.name}</option>
                        {/each}
                    </select>
                </div>
                <div class="select-group">
                    <label for="chat-model"><Cpu size={12} /> Model</label>
                    <select id="chat-model" bind:value={selectedModel} class="styled-select chat-select">
                        <option value="">-- Select --</option>
                        {#each availableModels as model}
                            <option value={model}>{model.split(/[\\/]/).pop()}</option>
                        {/each}
                    </select>
                </div>
            </div>
        </div>

        <div class="sidebar-section scrollable">
            <div class="sidebar-header">
                <h3>{i18n.t.chat.context}</h3>
                <p class="hint">{i18n.t.chat.context_hint}</p>
            </div>
            <div class="context-list">
                {#each batchManager.items as item}
                    <button 
                        class="context-item" 
                        class:selected={selectedIds.includes(item.id)}
                        onclick={() => toggleContext(item.id)}
                    >
                        <FileText size={14} />
                        <span class="file-name">{item.originalName}</span>
                        {#if selectedIds.includes(item.id)}
                            <ChevronRight size={14} class="indicator" />
                        {/if}
                    </button>
                {/each}
                {#if batchManager.items.length === 0}
                    <p class="empty-hint">{i18n.t.batch.empty}</p>
                {/if}
            </div>
        </div>
    </div>

    <div class="chat-main">
        <div class="chat-header">
            <div class="header-info">
                <h2>{i18n.t.chat.title}</h2>
                <span class="context-count">
                    {selectedItems.length} {i18n.t.chat.context}
                </span>
            </div>
            <button class="icon-btn danger" onclick={clearChat} title={i18n.t.chat.clear}>
                <Trash2 size={16} />
            </button>
        </div>

        <div class="message-list" bind:this={scrollContainer}>
            {#if messages.length === 0}
                <div class="welcome-msg">
                    <Bot size={48} />
                    <p>{i18n.t.chat.placeholder}</p>
                    {#if selectedItems.length === 0}
                        <div class="warning-box">
                            <Info size={14} />
                            <span>{i18n.t.chat.no_context}</span>
                        </div>
                    {/if}
                </div>
            {/if}

            {#each messages as msg}
                <div class="message-row" class:user-row={msg.role === 'user'}>
                    <div class="avatar" class:user-avatar={msg.role === 'user'}>
                        {#if msg.role === 'user'}<User size={16} />{:else}<Bot size={16} />{/if}
                    </div>
                    <div class="message-bubble">
                        <div class="msg-content">{@html msg.content.replace(/\n/g, '<br>')}</div>
                        <div class="msg-meta">{new Date(msg.timestamp).toLocaleTimeString()}</div>
                    </div>
                </div>
            {/each}

            {#if isTyping}
                <div class="message-row">
                    <div class="avatar"><Bot size={16} /></div>
                    <div class="message-bubble typing">
                        <Loader2 size={16} class="loader-anim" />
                    </div>
                </div>
            {/if}
        </div>

        <div class="input-area">
            <textarea 
                bind:value={input} 
                placeholder={i18n.t.chat.placeholder}
                onkeydown={e => e.key === 'Enter' && !e.shiftKey && (e.preventDefault(), handleSend())}
            ></textarea>
            <button class="send-btn" onclick={handleSend} disabled={!input.trim() || isTyping}>
                <Send size={18} />
            </button>
        </div>
    </div>
</div>

<style>
    .chat-container { display: flex; height: 100%; background: #09090b; }
    
    .chat-sidebar { width: 260px; background: #18181b; border-right: 1px solid #27272a; display: flex; flex-direction: column; }
    .sidebar-section { border-bottom: 1px solid #27272a; display: flex; flex-direction: column; flex-shrink: 0; }
    .sidebar-section.scrollable { flex: 1; overflow-y: auto; }
    .sidebar-header { padding: 16px 20px; }
    .sidebar-header h3 { margin: 0; font-size: 0.75rem; text-transform: uppercase; color: #a1a1aa; letter-spacing: 0.05em; }
    .hint { font-size: 0.7rem; color: #71717a; margin-top: 4px; }
    
    .engine-selectors { padding: 0 20px 20px; display: flex; flex-direction: column; gap: 12px; }
    .select-group { display: flex; flex-direction: column; gap: 6px; }
    .select-group label { display: flex; align-items: center; gap: 6px; font-size: 0.7rem; color: #71717a; font-weight: 600; text-transform: uppercase; }
    
    /* Fix blank black selectors */
    .chat-select { 
        background: #09090b !important; 
        color: white !important; 
        padding: 6px 8px !important; 
        font-size: 0.8125rem !important;
        border: 1px solid #27272a !important;
        width: 100% !important;
        height: auto !important;
        min-height: 32px !important;
    }
    .chat-select option { background: #18181b; color: white; }

    .context-list { padding: 10px; display: flex; flex-direction: column; gap: 4px; }
    .context-item { display: flex; align-items: center; gap: 8px; padding: 8px 12px; background: transparent; border: 1px solid transparent; border-radius: 6px; color: #a1a1aa; cursor: pointer; text-align: left; font-size: 0.8125rem; transition: all 0.2s; }
    .context-item:hover { background: #27272a; color: white; }
    .context-item.selected { background: #1e3a8a33; border-color: #1e3a8a; color: #3b82f6; }
    .file-name { flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    :global(.indicator) { color: #3b82f6; }
    .empty-hint { padding: 20px; text-align: center; color: #71717a; font-size: 0.8125rem; }

    .chat-main { flex: 1; display: flex; flex-direction: column; background: #09090b; }
    .chat-header { padding: 12px 24px; background: #18181b; border-bottom: 1px solid #27272a; display: flex; justify-content: space-between; align-items: center; }
    .header-info { display: flex; align-items: center; gap: 16px; }
    .header-info h2 { margin: 0; font-size: 1rem; font-weight: 700; }
    .context-count { font-size: 0.75rem; color: #71717a; background: #27272a; padding: 2px 8px; border-radius: 10px; }

    .message-list { flex: 1; overflow-y: auto; padding: 24px; display: flex; flex-direction: column; gap: 24px; }
    .welcome-msg { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; color: #71717a; gap: 16px; }
    .warning-box { display: flex; align-items: center; gap: 8px; background: #42200633; color: #fbbf24; padding: 8px 12px; border-radius: 6px; font-size: 0.75rem; border: 1px solid #713f12; }

    .message-row { display: flex; gap: 16px; max-width: 800px; width: 100%; }
    .user-row { flex-direction: row-reverse; align-self: flex-end; }
    
    .avatar { width: 32px; height: 32px; border-radius: 8px; background: #27272a; display: flex; align-items: center; justify-content: center; color: #a1a1aa; flex-shrink: 0; }
    .user-avatar { background: #3b82f6; color: white; }
    
    .message-bubble { display: flex; flex-direction: column; gap: 4px; max-width: calc(100% - 48px); }
    .msg-content { padding: 12px 16px; background: #18181b; border-radius: 12px; border-top-left-radius: 2px; color: #e2e8f0; font-size: 0.9375rem; line-height: 1.5; }
    .user-row .msg-content { background: #3b82f6; color: white; border-radius: 12px; border-top-right-radius: 2px; }
    .msg-meta { font-size: 0.65rem; color: #71717a; align-self: flex-start; }
    .user-row .msg-meta { align-self: flex-end; }

    .typing { padding: 8px 16px; }
    :global(.loader-anim) { animation: spin 1s linear infinite; color: #3b82f6; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

    .input-area { padding: 20px 24px; background: #18181b; border-top: 1px solid #27272a; display: flex; gap: 12px; align-items: flex-end; }
    .input-area textarea { flex: 1; background: #09090b; border: 1px solid #27272a; border-radius: 8px; color: white; padding: 12px; font-size: 0.9375rem; resize: none; min-height: 44px; max-height: 200px; font-family: inherit; }
    .input-area textarea:focus { outline: 2px solid #3b82f6; border-color: transparent; }
    
    .send-btn { width: 44px; height: 44px; border-radius: 8px; background: #3b82f6; color: white; border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; transition: all 0.2s; }
    .send-btn:hover:not(:disabled) { background: #2563eb; }
    .send-btn:disabled { opacity: 0.5; cursor: not-allowed; background: #27272a; }

    .icon-btn { background: transparent; border: none; color: #71717a; cursor: pointer; padding: 6px; border-radius: 6px; display: flex; align-items: center; justify-content: center; }
    .icon-btn:hover { background: #27272a; color: white; }
    .icon-btn.danger:hover { background: #ef444433; color: #ef4444; }
</style>
