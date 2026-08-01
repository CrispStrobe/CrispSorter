<script lang="ts">
    import IntendedPurposeGate from './IntendedPurposeGate.svelte';
    import AiGeneratedBadge from './AiGeneratedBadge.svelte';
    import { batchManager } from '../batch/store.svelte';
    import { DEFAULT_PROVIDERS, llmClient, type LLMProvider } from '../llm/client';
    import { getSetting } from '../store';
    import { i18n } from '../i18n.svelte';
    import {
        Send, User, Bot, Trash2, FileText,
        ChevronRight, ChevronLeft, Cpu, Zap, Search, MessageSquare, Brain,
        Loader2, Info, ChevronDown, ChevronUp,
        Mic, MicOff, VolumeX
    } from 'lucide-svelte';
    import { onMount, tick } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { getWebLLMLoadedModel } from '../llm/webllm';
    import { getORTLoadedModel } from '../llm/ort';
    import { saveSetting } from '../store';
    import katex from 'katex';
    import 'katex/dist/katex.min.css';
    import 'deep-chat';
    import { logInfo, logWarn } from '../log';

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
        autoSpeakReplies = await getSetting('autoSpeakReplies', false);

        const currentProv = providers.find(p => p.id === activeProviderId);
        if (currentProv) {
            selectedModel = currentProv.selectedModel || currentProv.models[0] || '';
        }

        // deep-chat is a custom element; the post-mount property
        // assignment below depends on it being upgraded by the time
        // we run. Two ticks is paranoid but reliable -- one for
        // Svelte's `bind:this` to fire, one for the custom-element
        // upgrade lifecycle to complete on the freshly-imported
        // module (HMR / vite re-optimisation paths in particular
        // can race the upgrade).
        await tick();
        if (typeof customElements !== 'undefined' && customElements.whenDefined) {
            try { await customElements.whenDefined('deep-chat'); }
            catch { /* element already registered or registry doesn't track it */ }
        }
        await tick();
        if (chatElement) {
            // Belt + suspenders: assign as a property AND set a
            // boolean attribute so deep-chat sees a config either
            // way. Pre-this-fix, when vite re-optimised after a
            // package-lock churn the property assignment seemed to
            // arrive too early or too late and the element rendered
            // its "no config" demo screen.
            chatElement.connect = { handler: handleRequest };
            try { chatElement.setAttribute('connected', ''); } catch { /* attribute is informational */ }
            logInfo(`Chat: deep-chat connected (provider=${activeProviderId}, model=${selectedModel || 'auto'})`);
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

                // Auto-speak: fire on the first onMessage tick that lands a
                // bot reply we haven't spoken yet. Skips streaming partials
                // by keying on stable index transitions; mid-stream updates
                // either don't trigger onMessage (deep-chat dispatches only
                // for completed messages by default) or land at the same
                // index and are safely no-op'd by the lastSpokenIndex check.
                if (autoSpeakReplies && msgs.length > 0) {
                    const lastIdx = msgs.length - 1;
                    const last = msgs[lastIdx];
                    const role = (last?.role ?? '').toLowerCase();
                    const isBot = role === 'ai' || role === 'assistant';
                    const text = (last?.text ?? '').trim();
                    if (isBot && text && lastIdx > lastSpokenIndex) {
                        lastSpokenIndex = lastIdx;
                        speakBotReply(text);
                    }
                }
            };
        } else {
            // Couldn't bind. Pre-this-fix this would silently render
            // the deep-chat demo screen ("To remove this message set
            // the demo property to true."); now we yell.
            logWarn('Chat: chatElement was null after tick()+whenDefined() -- deep-chat demo screen will show. Try clearing node_modules/.vite and restart.');
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

    // ── Voice input (CrispASR push-to-talk) ──────────────────────────────────
    // The mic button captures via WebAudio, resamples to 16 kHz mono Float32
    // PCM (the format CrispASR expects), invokes the asr_transcribe Tauri
    // command, and submits the result through deep-chat. Backend availability
    // depends on the `crispasr*` cargo feature being enabled at build time;
    // the Rust command returns a clear error otherwise so the user gets a
    // toast instead of a silent no-op.

    let asrState = $state<'idle' | 'recording' | 'transcribing'>('idle');
    let asrAudioCtx: AudioContext | null = null;
    let asrMediaStream: MediaStream | null = null;
    let asrSamples: Float32Array[] = [];
    let asrSampleRate = 48000;
    let asrError = $state<string | null>(null);

    // ── TTS auto-speak state ─────────────────────────────────────────────────
    // Watches deep-chat onMessage and, when the toggle is on, ships the
    // latest bot reply to the platform-native synth (macOS say / Windows
    // SAPI / Linux espeak). The Rust handler kills any in-flight utterance
    // when a new one arrives.
    let autoSpeakReplies = $state(false);
    let ttsSpeaking = $state(false);
    let lastSpokenIndex = -1;

    async function startVoiceCapture() {
        asrError = null;
        try {
            asrMediaStream = await navigator.mediaDevices.getUserMedia({ audio: true });
        } catch (e: any) {
            asrError = `Microphone access denied: ${e?.message ?? e}`;
            return;
        }

        // Use a single AudioContext per recording session; the browser may
        // pick its own native rate (typically 48000) which we resample on stop.
        asrAudioCtx = new AudioContext();
        asrSampleRate = asrAudioCtx.sampleRate;
        asrSamples = [];

        const source = asrAudioCtx.createMediaStreamSource(asrMediaStream);
        // ScriptProcessorNode is deprecated but universally supported and
        // sufficient for short PTT clips. AudioWorklet would be cleaner for
        // streaming but adds a worklet-module load which complicates packaging.
        const processor = asrAudioCtx.createScriptProcessor(4096, 1, 1);
        processor.onaudioprocess = (ev) => {
            const ch = ev.inputBuffer.getChannelData(0);
            // Copy because the buffer gets reused across callbacks.
            asrSamples.push(new Float32Array(ch));
        };
        source.connect(processor);
        processor.connect(asrAudioCtx.destination);

        asrState = 'recording';
    }

    async function stopVoiceCapture() {
        if (asrState !== 'recording') return;
        asrState = 'transcribing';

        // Tear down the capture pipeline before resampling — frees the mic LED.
        try { asrMediaStream?.getTracks().forEach(t => t.stop()); } catch {}
        asrMediaStream = null;

        // Concatenate captured chunks.
        const total = asrSamples.reduce((n, c) => n + c.length, 0);
        const merged = new Float32Array(total);
        let off = 0;
        for (const chunk of asrSamples) {
            merged.set(chunk, off);
            off += chunk.length;
        }
        asrSamples = [];

        try {
            // Resample to 16 kHz via OfflineAudioContext — handles
            // anti-aliasing properly. Typical mic rate is 48000.
            const targetRate = 16000;
            let pcm16k: Float32Array = merged;
            if (Math.abs(asrSampleRate - targetRate) > 1) {
                const offline = new OfflineAudioContext(
                    1,
                    Math.max(1, Math.ceil((merged.length * targetRate) / asrSampleRate)),
                    targetRate
                );
                const buf = offline.createBuffer(1, merged.length, asrSampleRate);
                buf.copyToChannel(merged, 0);
                const src = offline.createBufferSource();
                src.buffer = buf;
                src.connect(offline.destination);
                src.start();
                const rendered = await offline.startRendering();
                pcm16k = rendered.getChannelData(0);
            }

            // tauri's invoke serializes Float32Array as Vec<f32> on the Rust
            // side, so this just works without an intermediate JSON encode.
            const text = await invoke<string>('asr_transcribe', { pcm: Array.from(pcm16k) });
            const trimmed = (text ?? '').trim();
            if (trimmed && chatElement) {
                // deep-chat exposes submitUserMessage to inject a chat-as-user
                // message that goes through the normal request handler.
                try { chatElement.submitUserMessage({ text: trimmed }); }
                catch (e) { console.warn('[asr] submitUserMessage failed', e); }
            } else if (!trimmed) {
                asrError = 'No speech detected';
            }
        } catch (e: any) {
            asrError = `Transcription failed: ${e?.message ?? e}`;
            console.error('[asr]', e);
        } finally {
            try { asrAudioCtx?.close(); } catch {}
            asrAudioCtx = null;
            asrState = 'idle';
        }
    }

    function toggleVoiceCapture() {
        if (asrState === 'idle') startVoiceCapture();
        else if (asrState === 'recording') stopVoiceCapture();
    }

    // ── TTS bridge ───────────────────────────────────────────────────────────
    // Strips Markdown/HTML before handing off so the synth pronounces words,
    // not asterisks and angle brackets. Crude but good enough for chat replies;
    // a fully native speech-friendly transformation would need a proper MD AST.
    function plainifyForSpeech(text: string): string {
        return text
            // Code fences and inline code
            .replace(/```[\s\S]*?```/g, ' ')
            .replace(/`([^`]+)`/g, '$1')
            // Headings + emphasis
            .replace(/^#{1,6}\s*/gm, '')
            .replace(/\*\*([^*]+)\*\*/g, '$1')
            .replace(/\*([^*]+)\*/g, '$1')
            .replace(/__([^_]+)__/g, '$1')
            .replace(/_([^_]+)_/g, '$1')
            // Links: keep the link text only
            .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
            // HTML tags
            .replace(/<[^>]+>/g, ' ')
            // Collapse whitespace
            .replace(/\s+/g, ' ')
            .trim();
    }

    async function speakBotReply(text: string) {
        const clean = plainifyForSpeech(text);
        if (!clean) return;
        try {
            ttsSpeaking = true;
            await invoke('tts_speak', { text: clean });
        } catch (e) {
            console.warn('[tts] speak failed', e);
        } finally {
            // We can't cleanly tell when the synth finishes (it runs detached
            // on the Rust side). Reset the flag a moment later so the Stop
            // button stays enabled while plausibly speaking.
            setTimeout(() => { ttsSpeaking = false; }, 500);
        }
    }

    async function stopSpeaking() {
        try {
            await invoke('tts_stop');
        } catch (e) {
            console.warn('[tts] stop failed', e);
        } finally {
            ttsSpeaking = false;
        }
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
        const fetched = activeProvider.models || [];
        // Always keep the saved selectedModel as an option so bind:value doesn't reset to ''
        const saved = activeProvider.selectedModel;
        if (saved && !fetched.includes(saved)) return [saved, ...fetched];
        return fetched;
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
                                {#if item.suggestedTitle}
                                    <span class="file-name">{item.suggestedTitle}</span>
                                    {#if item.suggestedAuthor}
                                        <span class="file-meta-hint">{item.suggestedAuthor}</span>
                                    {/if}
                                    <span class="file-size-hint">{item.originalName}</span>
                                {:else}
                                    <span class="file-name">{item.originalName}</span>
                                    <span class="file-size-hint">{item.size > 0 ? formatSize(item.size) : (item.extractedText ? formatSize(item.extractedText.length) : '—')}</span>
                                {/if}
                            </div>
                        </button>
                    {/each}
                </div>
            </div>
        {/if}
    </div>

    <div class="chat-main">
        <!-- Chat completions go straight from `deep-chat` to the provider and
             never reach a Tauri command, so the Rust gate cannot see them. This
             overlay IS the gate for this surface. -->
        <IntendedPurposeGate />
        <div class="chat-header" class:extra-pad={sidebarCollapsed}>
            <div class="header-info">
                <h2>{i18n.t.chat.title}</h2>
                <div class="context-stats">
                    <span class="stat-badge">{i18n.t.chat.docs} {selectedIds.length} ({formatSize(docContextSize)})</span>
                    <span class="stat-badge history">{i18n.t.chat.history}: {formatSize(chatHistorySize)}</span>
                    <!-- Art 50(1)+(2): a panel-level disclosure rather than a
                         per-message one. Answers render inside the `deep-chat`
                         web component, so marking each bubble would mean
                         reaching into its shadow DOM; a persistent notice on the
                         surface that produces them informs the user just as
                         well and cannot drift out of sync with the messages. -->
                    <AiGeneratedBadge compact />
                </div>
            </div>
            <div style="display:flex; align-items:center; gap:6px;">
                {#if ttsSpeaking}
                    <button
                        class="icon-btn tts-speaking"
                        onclick={stopSpeaking}
                        title={i18n.t.chat.tts_stop}>
                        <VolumeX size={16} />
                    </button>
                {/if}
                <button
                    class="icon-btn"
                    class:asr-recording={asrState === 'recording'}
                    onclick={toggleVoiceCapture}
                    disabled={asrState === 'transcribing'}
                    title={asrState === 'recording' ? i18n.t.chat.voice_stop : (asrState === 'transcribing' ? i18n.t.chat.voice_busy : i18n.t.chat.voice_start)}>
                    {#if asrState === 'transcribing'}
                        <Loader2 size={16} class="spin" />
                    {:else if asrState === 'recording'}
                        <MicOff size={16} />
                    {:else}
                        <Mic size={16} />
                    {/if}
                </button>
                <button class="icon-btn danger" onclick={clearChat} title={i18n.t.chat.clear}><Trash2 size={16} /></button>
            </div>
        </div>
        {#if asrError}
            <div class="asr-error" role="alert">{asrError}</div>
        {/if}

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
    .file-size-hint { font-size: 0.65rem; color: #52525b; margin-top: 1px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
    .file-meta-hint { font-size: 0.65rem; color: #71717a; margin-top: 1px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
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
    .icon-btn:disabled { opacity: 0.5; cursor: not-allowed; }
    .icon-btn.asr-recording { color: #ef4444; background: #7f1d1d33; }
    .icon-btn.asr-recording:hover { background: #7f1d1d55; }
    .icon-btn.tts-speaking { color: #3b82f6; background: #1e3a8a33; }
    .icon-btn.tts-speaking:hover { background: #1e3a8a55; }
    .asr-error { padding: 6px 16px; background: #7f1d1d33; color: #fca5a5; font-size: 0.75rem; border-bottom: 1px solid #7f1d1d; }
    :global(.spin) { animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

    @media (max-width: 767px) {
        .chat-sidebar { display: none; }
        .chat-sidebar.collapsed { display: none; }
        .chat-header { height: 48px; padding: 0 12px; }
        .chat-header h2 { font-size: 0.875rem; }
        .context-stats { display: none; }
    }
</style>
