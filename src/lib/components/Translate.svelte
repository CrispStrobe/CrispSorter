<!--
  Translate — docx LLM translation panel.

  Wires the two Tauri commands exposed by src-tauri/src/translate/ into
  a single-page workflow: pick input → preview paragraph texts → choose
  a provider + model + target language → run → save the translated docx.

  Streams `translate://progress` events for a live progress indicator.
-->
<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { listen, type UnlistenFn } from '@tauri-apps/api/event';
    import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
    import { onMount, onDestroy } from 'svelte';
    import { FileText, Play, FolderOpen, AlertCircle, CheckCircle2, Loader2 } from 'lucide-svelte';

    type ProviderKind =
        | 'openai'
        | 'anthropic'
        | 'ollama'
        | 'groq'
        | 'openrouter'
        | 'together'
        | 'cerebras'
        | 'mistral'
        | 'nebius'
        | 'scaleway'
        | 'poe'
        | 'google'
        | 'nmt';

    interface ProviderSpec {
        kind: ProviderKind;
        model: string;
        api_key?: string | null;
        base_url?: string | null;
    }

    interface TranslateResult {
        total: number;
        succeeded: number;
        failed: number;
    }

    interface TranslateProgress {
        paragraph_index: number;
        total: number;
    }

    // ── Form state ────────────────────────────────────────────────────
    let inputPath = $state('');
    let outputPath = $state('');
    let sourceLang = $state('English');
    let targetLang = $state('German');

    let providerKind = $state<ProviderKind>('openai');
    let providerModel = $state('gpt-4o-mini');
    let providerApiKey = $state('');
    let providerBaseUrl = $state('');
    let concurrency = $state(4);
    // v0.2: preserve intra-paragraph runs (bold/italic) via word alignment.
    // Off by default. When on, requires a multilingual encoder GGUF + the
    // backend to be built with --features translate-align.
    let preserveFormatting = $state(false);
    let alignModelPath = $state('');

    // ── Runtime state ─────────────────────────────────────────────────
    let paragraphs = $state<string[]>([]);
    let previewLoading = $state(false);
    let translating = $state(false);
    let progress = $state<TranslateProgress | null>(null);
    let result = $state<TranslateResult | null>(null);
    let errorMessage = $state('');

    let progressUnsubscribe: UnlistenFn | null = null;

    onMount(async () => {
        progressUnsubscribe = await listen<TranslateProgress>(
            'translate://progress',
            (event) => {
                progress = event.payload;
            }
        );
    });

    onDestroy(() => {
        progressUnsubscribe?.();
    });

    // ── Default model per provider ────────────────────────────────────
    function defaultModelFor(kind: ProviderKind): string {
        switch (kind) {
            case 'openai':     return 'gpt-4o-mini';
            case 'anthropic':  return 'claude-3-5-sonnet-20241022';
            case 'ollama':     return 'llama3.2';
            case 'groq':       return 'llama-3.3-70b-versatile';
            case 'openrouter': return 'meta-llama/llama-3.3-70b-instruct';
            case 'together':   return 'meta-llama/Llama-3.3-70B-Instruct-Turbo';
            case 'cerebras':   return 'llama-3.3-70b';
            case 'mistral':    return 'mistral-large-latest';
            case 'nebius':     return 'meta-llama/Llama-3.3-70B-Instruct';
            case 'scaleway':   return 'llama-3.3-70b-instruct';
            case 'poe':        return 'GPT-4o-mini';
            case 'google':     return 'gemini-2.0-flash';
            case 'nmt':        return '';  // GGUF path entered as model
        }
    }

    function onProviderChange() {
        providerModel = defaultModelFor(providerKind);
    }

    // ── Pick input ────────────────────────────────────────────────────
    async function pickInput() {
        const path = await openDialog({
            multiple: false,
            directory: false,
            filters: [{ name: 'Word documents', extensions: ['docx'] }],
        });
        if (typeof path !== 'string') return;
        inputPath = path;
        if (!outputPath) {
            outputPath = path.replace(/\.docx$/i, `.${targetLang.toLowerCase()}.docx`);
        }
        await runPreview();
    }

    async function pickOutput() {
        const path = await saveDialog({
            defaultPath: outputPath || undefined,
            filters: [{ name: 'Word documents', extensions: ['docx'] }],
        });
        if (typeof path === 'string') outputPath = path;
    }

    async function runPreview() {
        if (!inputPath) return;
        previewLoading = true;
        errorMessage = '';
        try {
            paragraphs = await invoke<string[]>('translate_dry_run', {
                input: inputPath,
            });
        } catch (e) {
            errorMessage = String(e);
            paragraphs = [];
        } finally {
            previewLoading = false;
        }
    }

    async function runTranslate() {
        if (!inputPath || !outputPath) {
            errorMessage = 'Pick both input and output paths first.';
            return;
        }
        errorMessage = '';
        result = null;
        progress = null;
        translating = true;

        const providers: ProviderSpec[] = [{
            kind: providerKind,
            model: providerModel,
            // Local providers don't take an API key.
            api_key:
                providerKind === 'ollama' || providerKind === 'nmt'
                    ? null
                    : (providerApiKey || null),
            base_url: providerBaseUrl || null,
        }];

        try {
            result = await invoke<TranslateResult>('translate_docx', {
                input: inputPath,
                output: outputPath,
                sourceLang,
                targetLang,
                providers,
                concurrency,
                preserveFormatting,
                alignModelPath: preserveFormatting ? (alignModelPath || null) : null,
            });
        } catch (e) {
            errorMessage = String(e);
        } finally {
            translating = false;
        }
    }

    async function pickAlignModel() {
        const path = await openDialog({
            multiple: false,
            directory: false,
            filters: [{ name: 'GGUF models', extensions: ['gguf'] }],
        });
        if (typeof path === 'string') alignModelPath = path;
    }
</script>

<section class="translate">
    <header>
        <h1><FileText size={22} /> Translate document</h1>
        <p class="subtitle">
            Translate every paragraph of a <code>.docx</code> via an LLM.
            Paragraph styles, sections, bookmarks and footnote references
            are preserved.
        </p>
    </header>

    <div class="grid">
        <!-- ── INPUT ─────────────────────────────────────────────── -->
        <div class="field">
            <label for="t-input">Input</label>
            <div class="path-row">
                <input id="t-input" type="text" bind:value={inputPath} placeholder="Pick a .docx..." />
                <button onclick={pickInput} title="Browse">
                    <FolderOpen size={16} />
                </button>
            </div>
        </div>

        <div class="field">
            <label for="t-output">Output</label>
            <div class="path-row">
                <input id="t-output" type="text" bind:value={outputPath} placeholder="Output .docx path" />
                <button onclick={pickOutput} title="Browse">
                    <FolderOpen size={16} />
                </button>
            </div>
        </div>

        <!-- ── LANGUAGE ─────────────────────────────────────────── -->
        <div class="field">
            <label for="t-src">Source language</label>
            <input id="t-src" type="text" bind:value={sourceLang} />
        </div>

        <div class="field">
            <label for="t-tgt">Target language</label>
            <input id="t-tgt" type="text" bind:value={targetLang} />
        </div>

        <!-- ── PROVIDER ─────────────────────────────────────────── -->
        <div class="field">
            <label for="t-provider">Provider</label>
            <select id="t-provider" bind:value={providerKind} onchange={onProviderChange}>
                <optgroup label="Cloud LLM">
                    <option value="openai">OpenAI</option>
                    <option value="anthropic">Anthropic</option>
                    <option value="groq">Groq</option>
                    <option value="openrouter">OpenRouter</option>
                    <option value="together">Together</option>
                    <option value="cerebras">Cerebras</option>
                    <option value="mistral">Mistral</option>
                    <option value="nebius">Nebius</option>
                    <option value="scaleway">Scaleway</option>
                    <option value="poe">Poe</option>
                    <option value="google">Google (Gemini)</option>
                </optgroup>
                <optgroup label="Local / offline">
                    <option value="ollama">Ollama (HTTP server)</option>
                    <option value="nmt">CrispASR NMT (GGUF, offline)</option>
                </optgroup>
            </select>
        </div>

        <div class="field">
            <label for="t-model">
                {providerKind === 'nmt' ? 'Model GGUF path' : 'Model'}
            </label>
            <input id="t-model" type="text" bind:value={providerModel}
                placeholder={providerKind === 'nmt'
                    ? '/path/to/m2m100-418m-q8_0.gguf'
                    : ''} />
        </div>

        {#if providerKind !== 'ollama' && providerKind !== 'nmt'}
            <div class="field">
                <label for="t-key">API key</label>
                <input id="t-key" type="password" bind:value={providerApiKey} placeholder="(or leave blank to use env)" />
            </div>
        {/if}

        <div class="field">
            <label for="t-base">Base URL (optional)</label>
            <input id="t-base" type="text" bind:value={providerBaseUrl} placeholder="e.g. http://localhost:11434/api" />
        </div>

        <div class="field">
            <label for="t-conc">Concurrency</label>
            <input id="t-conc" type="number" min="1" max="32" bind:value={concurrency} />
        </div>

        <!-- ── FORMAT PRESERVATION (v0.2) ───────────────────────── -->
        <div class="field span-2">
            <label>
                <input type="checkbox" bind:checked={preserveFormatting} />
                Preserve intra-paragraph formatting (bold / italic)
                <span class="badge">v0.2 — requires <code>translate-align</code> build</span>
            </label>
        </div>

        {#if preserveFormatting}
            <div class="field span-2">
                <label for="t-align">Alignment encoder GGUF</label>
                <div class="path-row">
                    <input id="t-align" type="text" bind:value={alignModelPath}
                        placeholder="Path to a multilingual encoder GGUF (e.g. paraphrase-multilingual-MiniLM-L12-v2.gguf)" />
                    <button onclick={pickAlignModel} title="Browse">
                        <FolderOpen size={16} />
                    </button>
                </div>
                <p class="hint">
                    Per-paragraph: aligns source ↔ translated words via the
                    encoder, then maps each source run's rPr onto the matching
                    target word range. Adds ~50ms / paragraph on a typical
                    multilingual MiniLM model.
                </p>
            </div>
        {/if}
    </div>

    <div class="actions">
        <button class="primary" disabled={!inputPath || !outputPath || translating} onclick={runTranslate}>
            {#if translating}
                <Loader2 size={16} class="spin" />
                Translating...
            {:else}
                <Play size={16} />
                Translate
            {/if}
        </button>
    </div>

    {#if errorMessage}
        <div class="banner error">
            <AlertCircle size={18} />
            <div>
                <strong>Failed</strong>
                <div>{errorMessage}</div>
            </div>
        </div>
    {/if}

    {#if progress && translating}
        <div class="banner info">
            <Loader2 size={18} class="spin" />
            <div>
                Paragraph {progress.paragraph_index} / {progress.total}
                <div class="progress-bar">
                    <div class="progress-fill" style:width="{(progress.paragraph_index / progress.total) * 100}%"></div>
                </div>
            </div>
        </div>
    {/if}

    {#if result}
        <div class="banner success">
            <CheckCircle2 size={18} />
            <div>
                <strong>Done</strong> — {result.succeeded}/{result.total} paragraphs translated{#if result.failed > 0}; {result.failed} failed (left as original){/if}.
                <div class="hint">Output: <code>{outputPath}</code></div>
            </div>
        </div>
    {/if}

    {#if paragraphs.length > 0 && !translating}
        <details class="preview" open={paragraphs.length <= 5}>
            <summary>Preview: {paragraphs.length} paragraph(s)</summary>
            <ol>
                {#each paragraphs as p, i}
                    <li>
                        <span class="idx">{i + 1}</span>
                        <span class="text">{p.length > 200 ? p.slice(0, 200) + '…' : p}</span>
                    </li>
                {/each}
            </ol>
        </details>
    {/if}

    {#if previewLoading}
        <div class="banner info">
            <Loader2 size={18} class="spin" />
            Loading paragraphs from input...
        </div>
    {/if}
</section>

<style>
    .translate {
        display: flex;
        flex-direction: column;
        gap: 1rem;
        padding: 1.5rem;
        max-width: 60rem;
        margin: 0 auto;
    }
    header h1 {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        margin: 0;
    }
    .subtitle {
        color: var(--text-muted, #888);
        margin: 0.25rem 0 0 0;
        font-size: 0.9em;
    }
    code {
        background: var(--bg-subtle, #f4f4f4);
        padding: 1px 4px;
        border-radius: 3px;
        font-family: monospace;
    }

    .grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 0.75rem 1rem;
    }
    .field {
        display: flex;
        flex-direction: column;
        gap: 0.25rem;
    }
    .field label {
        font-size: 0.85em;
        color: var(--text-muted, #666);
    }
    .field.span-2 {
        grid-column: 1 / -1;
    }
    .field input[type="checkbox"] {
        margin-right: 0.4rem;
    }
    .badge {
        display: inline-block;
        padding: 1px 5px;
        background: var(--bg-subtle, #f0f0f0);
        color: var(--text-muted, #666);
        border-radius: 3px;
        font-size: 0.75em;
        margin-left: 0.4rem;
    }
    .field input:not([type="checkbox"]), .field select {
        padding: 0.4rem 0.5rem;
        border: 1px solid var(--border, #ccc);
        border-radius: 4px;
        background: var(--bg, #fff);
        color: var(--text, #111);
        font: inherit;
    }
    .path-row {
        display: flex;
        gap: 0.25rem;
    }
    .path-row input {
        flex: 1;
    }
    .path-row button {
        padding: 0.4rem 0.5rem;
        border: 1px solid var(--border, #ccc);
        border-radius: 4px;
        background: var(--bg, #fff);
        cursor: pointer;
    }

    .actions {
        display: flex;
        gap: 0.5rem;
        justify-content: flex-end;
        margin-top: 0.5rem;
    }
    .actions button.primary {
        padding: 0.5rem 1rem;
        border: 0;
        border-radius: 4px;
        background: var(--accent, #4a7);
        color: white;
        cursor: pointer;
        display: inline-flex;
        align-items: center;
        gap: 0.4rem;
        font-weight: 500;
    }
    .actions button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .banner {
        display: flex;
        align-items: flex-start;
        gap: 0.6rem;
        padding: 0.6rem 0.8rem;
        border-radius: 4px;
        border-left: 3px solid;
    }
    .banner.error  { background: rgba(220,80,80,0.08); border-left-color: #c33; }
    .banner.info   { background: rgba(80,130,220,0.08); border-left-color: #36c; }
    .banner.success{ background: rgba(80,180,80,0.08); border-left-color: #393; }
    .banner > div { flex: 1; }
    .hint { font-size: 0.85em; color: var(--text-muted, #888); margin-top: 0.2rem; }

    .progress-bar {
        margin-top: 0.4rem;
        height: 6px;
        background: var(--bg-subtle, #e0e0e0);
        border-radius: 3px;
        overflow: hidden;
    }
    .progress-fill {
        height: 100%;
        background: var(--accent, #4a7);
        transition: width 0.2s ease;
    }

    .preview ol {
        margin: 0.5rem 0 0 0;
        padding-left: 0;
        list-style: none;
    }
    .preview li {
        display: flex;
        gap: 0.5rem;
        padding: 0.3rem 0;
        border-bottom: 1px solid var(--border-subtle, #eee);
    }
    .preview .idx {
        color: var(--text-muted, #888);
        min-width: 2.2rem;
        text-align: right;
        font-variant-numeric: tabular-nums;
    }
    .preview .text {
        white-space: pre-wrap;
        word-break: break-word;
    }

    :global(.spin) {
        animation: spin 1s linear infinite;
    }
    @keyframes spin {
        from { transform: rotate(0deg); }
        to   { transform: rotate(360deg); }
    }
</style>
