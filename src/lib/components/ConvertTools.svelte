<script lang="ts">
    // Document conversion surface.
    //
    // The `convert` CLI verb shipped without a GUI, so office and e-book
    // conversion was terminal-only. Like the DOCX panel next door this is a
    // one-shot pick-a-file-then-run tool, not an edit session — but unlike
    // it, what the tool *can* do depends on both the file and the build, so
    // the panel asks the backend first (`convert_capabilities`) and disables
    // what is unreachable rather than offering it and failing.

    import { invoke } from '@tauri-apps/api/core';
    import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
    import { i18n } from '$lib/i18n.svelte';
    import {
        FileUp, Play, Save, Loader2, AlertTriangle, Check, Presentation,
    } from 'lucide-svelte';

    // ── Wire types (mirror src-tauri/src/convert_tools.rs) ──────────────
    interface ConvertCapabilities {
        ext: string;
        native_reader: boolean;
        anydoc_available: boolean;
        convertible: boolean;
        rich_output: boolean;
        slide_options: boolean;
        extensions: string[];
    }
    interface ConvertOutput {
        content: string | null;
        headings: string[];
        slides: number | null;
        engine_used: string;
        written_path: string | null;
    }

    let filePath = $state('');
    let caps = $state<ConvertCapabilities | null>(null);
    let busy = $state(false);
    let error = $state('');
    let success = $state('');
    let result = $state<ConvertOutput | null>(null);

    let emit = $state<'md' | 'text' | 'headings' | 'docx' | 'rtf'>('md');
    let engine = $state<'auto' | 'native' | 'anydoc'>('auto');
    let wrapText = $state(0);
    let includeNotes = $state(true);
    let includeComments = $state(true);
    let visualOrder = $state(true);

    /** Word only ever lands in a file, so "Preview" cannot apply to it. */
    const emitIsBinary = $derived(emit === 'docx');
    /** The slide knobs describe a model only the native reader has. */
    const slideKnobsApply = $derived(!!caps?.slide_options && engine !== 'anydoc');
    const richBlocked = $derived(
        (emit === 'docx' || emit === 'rtf') && (!caps?.rich_output || engine === 'anydoc'),
    );

    async function pickFile() {
        const exts = caps?.extensions ?? ['pptx'];
        const picked = await openDialog({
            multiple: false,
            filters: [{ name: i18n.t.converttools?.filter_documents ?? 'Documents', extensions: exts }],
        });
        if (typeof picked === 'string') {
            filePath = picked;
            await refreshCaps();
        }
    }

    async function refreshCaps() {
        try {
            caps = await invoke<ConvertCapabilities>('convert_capabilities', {
                path: filePath || null,
            });
            // Fall back to an output the file can actually produce, rather
            // than leaving a selection that will only fail on Run.
            if ((emit === 'docx' || emit === 'rtf') && !caps.rich_output) emit = 'md';
        } catch (e) {
            error = String(e);
        }
    }

    async function run(save: boolean) {
        if (!filePath) return;
        error = ''; success = ''; result = null;

        let outPath: string | null = null;
        if (save || emitIsBinary) {
            const ext = emit === 'docx' ? 'docx' : emit === 'rtf' ? 'rtf' : emit === 'md' ? 'md' : 'txt';
            const chosen = await saveDialog({ filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
            if (typeof chosen !== 'string') return;   // cancelled
            outPath = chosen;
        }

        busy = true;
        try {
            result = await invoke<ConvertOutput>('convert_document', {
                path: filePath,
                emit,
                engine,
                wrapText,
                includeNotes,
                includeComments,
                visualOrder,
                outPath,
            });
            success = result.written_path
                ? `${i18n.t.converttools?.wrote ?? 'Wrote'} ${result.written_path}`
                : (i18n.t.converttools?.converted ?? 'Converted');
        } catch (e) {
            error = String(e);
        } finally {
            busy = false;
        }
    }

    $effect(() => { if (!caps) refreshCaps(); });
</script>

<div class="ct">
    <div class="ct-row">
        <button class="ct-btn" onclick={pickFile} disabled={busy}>
            <FileUp size={13} /> {i18n.t.converttools?.pick ?? 'Choose document…'}
        </button>
        <span class="ct-path" title={filePath}>{filePath || (i18n.t.converttools?.none ?? 'No file selected')}</span>
    </div>

    {#if caps && filePath && !caps.convertible}
        <p class="ct-warn">
            <AlertTriangle size={13} />
            {i18n.t.converttools?.not_convertible ??
             `This build cannot convert .${caps.ext} files.`}
            {#if !caps.anydoc_available}
                {i18n.t.converttools?.needs_feature ??
                 'Only PowerPoint (.pptx) is available — the build lacks the office/e-book converter.'}
            {/if}
        </p>
    {/if}

    <div class="ct-grid">
        <label class="ct-field">
            <span>{i18n.t.converttools?.emit ?? 'Output'}</span>
            <select bind:value={emit} disabled={busy} class="ct-select">
                <option value="md">Markdown</option>
                <option value="text">{i18n.t.converttools?.emit_text ?? 'Plain text'}</option>
                <option value="headings">{i18n.t.converttools?.emit_headings ?? 'Outline only'}</option>
                <option value="docx" disabled={!caps?.rich_output}>Word (.docx)</option>
                <option value="rtf" disabled={!caps?.rich_output}>Rich Text (.rtf)</option>
            </select>
        </label>

        <label class="ct-field">
            <span>{i18n.t.converttools?.engine ?? 'Converter'}</span>
            <select bind:value={engine} disabled={busy} class="ct-select">
                <option value="auto">{i18n.t.converttools?.engine_auto ?? 'Automatic (recommended)'}</option>
                <option value="native" disabled={!caps?.native_reader}>
                    {i18n.t.converttools?.engine_native ?? 'Dedicated reader'}
                </option>
                <option value="anydoc" disabled={!caps?.anydoc_available}>
                    {i18n.t.converttools?.engine_anydoc ?? 'Generic converter'}
                </option>
            </select>
        </label>

        <label class="ct-field">
            <span>{i18n.t.converttools?.wrap ?? 'Wrap width (0 = off)'}</span>
            <input type="number" min="0" max="200" bind:value={wrapText} disabled={busy} class="ct-input" />
        </label>
    </div>

    {#if slideKnobsApply}
        <div class="ct-slide">
            <p class="ct-slide-head"><Presentation size={12} /> {i18n.t.converttools?.slide_opts ?? 'Slide options'}</p>
            <label class="ct-cb"><input type="checkbox" bind:checked={includeNotes} disabled={busy} />
                <span>{i18n.t.converttools?.notes ?? 'Include speaker notes'}</span></label>
            <label class="ct-cb"><input type="checkbox" bind:checked={includeComments} disabled={busy} />
                <span>{i18n.t.converttools?.comments ?? 'Include comments'}</span></label>
            <label class="ct-cb"><input type="checkbox" bind:checked={visualOrder} disabled={busy} />
                <span>{i18n.t.converttools?.visual ?? 'Read shapes in visual order (off = authoring order)'}</span></label>
        </div>
    {:else if caps?.slide_options && engine === 'anydoc'}
        <p class="ct-hint">
            {i18n.t.converttools?.slide_opts_ignored ??
             'The generic converter models neither comments nor shape geometry, so the slide options do not apply to it.'}
        </p>
    {/if}

    <div class="ct-row">
        <button class="ct-btn primary" onclick={() => run(false)}
                disabled={busy || !filePath || !caps?.convertible || richBlocked || emitIsBinary}>
            {#if busy}<Loader2 size={13} class="spin" />{:else}<Play size={13} />{/if}
            {i18n.t.converttools?.preview ?? 'Preview'}
        </button>
        <button class="ct-btn" onclick={() => run(true)}
                disabled={busy || !filePath || !caps?.convertible || richBlocked}>
            <Save size={13} /> {i18n.t.converttools?.save ?? 'Convert and save…'}
        </button>
    </div>

    {#if error}<p class="ct-warn"><AlertTriangle size={13} /> {error}</p>{/if}
    {#if success}<p class="ct-ok"><Check size={13} /> {success}</p>{/if}

    {#if result}
        <p class="ct-meta">
            {i18n.t.converttools?.used ?? 'Converter used'}:
            <strong>{result.engine_used}</strong>
            {#if result.slides !== null} · {result.slides} {i18n.t.converttools?.slides ?? 'slides'}{/if}
            {#if result.headings.length} · {result.headings.length} {i18n.t.converttools?.headings ?? 'headings'}{/if}
        </p>
        {#if result.content}
            <pre class="ct-out">{result.content}</pre>
        {/if}
    {/if}
</div>

<style>
    .ct { display: flex; flex-direction: column; gap: 10px; padding: 12px; overflow: auto; }
    .ct-row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
    .ct-path { font-size: 11px; color: #a1a1aa; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
    .ct-btn {
        display: inline-flex; align-items: center; gap: 5px; padding: 5px 11px;
        font-size: 12px; border-radius: 6px; cursor: pointer;
        border: 1px solid #27272a; background: #18181b; color: #e4e4e7;
    }
    .ct-btn:disabled { opacity: .45; cursor: not-allowed; }
    .ct-btn.primary { border-color: #3f3f46; background: #27272a; }
    .ct-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 10px; }
    .ct-field { display: flex; flex-direction: column; gap: 4px; font-size: 11px; color: #a1a1aa; }
    .ct-select, .ct-input {
        padding: 5px 8px; font-size: 12px; border-radius: 6px;
        border: 1px solid #27272a; background: #09090b; color: #e4e4e7;
    }
    .ct-slide { display: flex; flex-direction: column; gap: 5px; padding: 8px 10px; border: 1px solid #27272a; border-radius: 6px; }
    .ct-slide-head { display: flex; align-items: center; gap: 5px; margin: 0 0 2px; font-size: 11px; color: #a1a1aa; }
    .ct-cb { display: flex; align-items: center; gap: 7px; font-size: 12px; color: #e4e4e7; }
    .ct-hint, .ct-meta { font-size: 11px; color: #a1a1aa; margin: 0; }
    .ct-warn { display: flex; align-items: center; gap: 6px; font-size: 12px; color: #fca5a5; margin: 0; }
    .ct-ok { display: flex; align-items: center; gap: 6px; font-size: 12px; color: #86efac; margin: 0; }
    .ct-out {
        margin: 0; padding: 10px; max-height: 320px; overflow: auto;
        font-size: 11px; line-height: 1.5; white-space: pre-wrap; word-break: break-word;
        border: 1px solid #27272a; border-radius: 6px; background: #09090b; color: #d4d4d8;
    }
</style>
