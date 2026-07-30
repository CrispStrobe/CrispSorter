<script lang="ts">
    // P30 — DOCX surgery surface.
    //
    // The eight `docx_*` commands shipped with a CLI verb each but no GUI,
    // so the OOXML work was reachable only from a terminal. They are
    // one-shot `path → output` operations (not an edit session like the PDF
    // editor), so this is a tool panel: pick a file, read what the document
    // actually says about itself, then run an operation that writes a new
    // file. Nothing here overwrites the input.

    import { invoke } from '@tauri-apps/api/core';
    import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
    import { i18n } from '$lib/i18n.svelte';
    import {
        FileUp, ShieldCheck, Ruler, Heading, Palette, BookOpen, Quote,
        Eraser, Check, X, Loader2, AlertTriangle, Plus, Trash2,
    } from 'lucide-svelte';

    // ── Wire types (mirror src-tauri/src/docx_tools.rs) ─────────────────
    interface DocxCheckResult { ok: string[]; issues: string[]; valid: boolean }
    interface DocxSection {
        page_width_pt: number | null;
        page_height_pt: number | null;
        left_margin_pt: number | null;
        right_margin_pt: number | null;
        top_margin_pt: number | null;
        bottom_margin_pt: number | null;
        orientation: string | null;
    }
    interface DocxBlueprint {
        sections: DocxSection[];
        default_font: string;
        default_font_size_pt: number;
        style_count: number;
    }
    interface InferredHeading { level: number; text: string }
    interface TransplantResult {
        output_path: string;
        source_paragraphs: number;
        blueprint_styles: number;
        styles_remapped: number;
    }

    // ── State ───────────────────────────────────────────────────────────
    let filePath = $state('');
    let busy = $state(false);
    let error = $state('');
    let success = $state('');

    let activeOp = $state<'check' | 'properties' | 'headings' | 'restyle' | 'notes' | 'quotes' | 'footnotes' | null>(null);

    let checkResult = $state<DocxCheckResult | null>(null);
    let blueprint = $state<DocxBlueprint | null>(null);
    let headings = $state<InferredHeading[] | null>(null);
    let transplant = $state<TransplantResult | null>(null);

    let blueprintPath = $state('');
    let notesTarget = $state<'footnotes' | 'endnotes'>('endnotes');
    let quoteStyle = $state('german');
    /** `[marker, text]` pairs for footnote injection. */
    let noteRows = $state<{ n: number; text: string }[]>([{ n: 1, text: '' }]);

    const QUOTE_STYLES = ['german', 'english', 'french', 'swiss', 'german_guillemets'];

    // ── Helpers ─────────────────────────────────────────────────────────
    async function pickDocx(): Promise<string | null> {
        const p = await openDialog({
            filters: [{ name: 'Word document', extensions: ['docx'] }],
            multiple: false,
        });
        return (p as string) ?? null;
    }

    async function openFile() {
        const p = await pickDocx();
        if (!p) return;
        filePath = p;
        // Everything on screen described the previous file.
        checkResult = null;
        blueprint = null;
        headings = null;
        transplant = null;
        error = '';
        success = '';
    }

    /** Ask where to write, defaulting next to the input with a suffix. */
    async function askOutput(suffix: string): Promise<string | null> {
        const base = filePath.replace(/\.docx$/i, '');
        const out = await saveDialog({
            filters: [{ name: 'Word document', extensions: ['docx'] }],
            defaultPath: `${base}-${suffix}.docx`,
        });
        return (out as string) ?? null;
    }

    /** Run `fn`, routing failures to the status line rather than the console. */
    async function run<T>(fn: () => Promise<T>): Promise<T | null> {
        if (!filePath) { error = i18n.t.docxtools.pick_first; return null; }
        busy = true;
        error = '';
        success = '';
        try {
            return await fn();
        } catch (e: any) {
            error = e?.message ?? String(e);
            return null;
        } finally {
            busy = false;
        }
    }

    function pt(v: number | null): string {
        // "unstated" is a real answer here and must not read as 0 — the
        // backend keeps it distinguishable, so the UI does too.
        return v === null ? i18n.t.docxtools.unstated : `${v.toFixed(0)} pt`;
    }

    // ── Operations ──────────────────────────────────────────────────────
    async function doCheck() {
        const r = await run(() => invoke<DocxCheckResult>('docx_check', { path: filePath }));
        if (r) {
            checkResult = r;
            success = r.valid ? i18n.t.docxtools.check_ok : '';
        }
    }

    async function doProperties() {
        const b = await run(() => invoke<DocxBlueprint>('docx_analyze', { path: filePath }));
        if (b) blueprint = b;
    }

    async function doHeadings() {
        const hs = await run(() => invoke<InferredHeading[]>('docx_infer_headings', { path: filePath }));
        if (hs) {
            headings = hs;
            if (hs.length === 0) success = i18n.t.docxtools.headings_none;
        }
    }

    async function pickBlueprint() {
        const p = await pickDocx();
        if (p) blueprintPath = p;
    }

    async function doRestyle() {
        if (!blueprintPath) { error = i18n.t.docxtools.blueprint_first; return; }
        const out = await askOutput('restyled');
        if (!out) return;
        const r = await run(() => invoke<TransplantResult>('docx_transplant', {
            source: filePath, blueprint: blueprintPath, output: out,
        }));
        if (r) {
            transplant = r;
            success = `${i18n.t.docxtools.written} ${out}`;
        }
    }

    async function doConvertNotes() {
        const out = await askOutput(notesTarget);
        if (!out) return;
        const r = await run(() => invoke<string>('docx_convert_notes', {
            path: filePath, targetKind: notesTarget, output: out,
        }));
        if (r) success = `${i18n.t.docxtools.written} ${out}`;
    }

    async function doNormalizeQuotes() {
        const out = await askOutput('quotes');
        if (!out) return;
        const r = await run(() => invoke<string>('docx_normalize_quotes', {
            path: filePath, style: quoteStyle, output: out,
        }));
        if (r) success = `${i18n.t.docxtools.written} ${out}`;
    }

    async function doStripRsids() {
        const out = await askOutput('clean');
        if (!out) return;
        const n = await run(() => invoke<number>('docx_strip_rsids', {
            path: filePath, output: out,
        }));
        if (n !== null) {
            success = i18n.t.docxtools.rsids_stripped
                .replace('{n}', String(n)).replace('{path}', out);
        }
    }

    function addNoteRow() {
        const next = Math.max(0, ...noteRows.map((r) => r.n)) + 1;
        noteRows = [...noteRows, { n: next, text: '' }];
    }

    function removeNoteRow(i: number) {
        noteRows = noteRows.filter((_, k) => k !== i);
        if (noteRows.length === 0) noteRows = [{ n: 1, text: '' }];
    }

    async function doInjectFootnotes() {
        const filled = noteRows.filter((r) => r.text.trim() !== '');
        if (filled.length === 0) { error = i18n.t.docxtools.notes_empty; return; }
        const seen = new Set<number>();
        for (const r of filled) {
            // Two texts for one marker means one of them gets dropped;
            // refuse rather than choose.
            if (seen.has(r.n)) { error = i18n.t.docxtools.notes_duplicate.replace('{n}', String(r.n)); return; }
            seen.add(r.n);
        }
        const out = await askOutput('noted');
        if (!out) return;
        const notes: Record<number, string> = {};
        for (const r of filled) notes[r.n] = r.text;
        const inserted = await run(() => invoke<number>('docx_inject_footnotes', {
            path: filePath, notes, output: out,
        }));
        if (inserted !== null) {
            success = i18n.t.docxtools.notes_injected
                .replace('{n}', String(inserted))
                .replace('{total}', String(filled.length))
                .replace('{path}', out);
        }
    }

    function toggle(op: typeof activeOp) {
        activeOp = activeOp === op ? null : op;
    }
</script>

<div class="dt">
    <div class="dt-toolbar">
        <button class="dt-btn dt-open" onclick={openFile}>
            <FileUp size={14} /> {i18n.t.docxtools.open}
        </button>
        {#if filePath}
            <span class="dt-file" title={filePath}>{filePath.split('/').pop()}</span>
            <span class="dt-sep"></span>
            <button class="dt-btn" class:active={activeOp === 'check'} disabled={busy}
                    onclick={() => { toggle('check'); if (activeOp === 'check') doCheck(); }}
                    title={i18n.t.docxtools.check_hint}>
                <ShieldCheck size={14} /> {i18n.t.docxtools.check}
            </button>
            <button class="dt-btn" class:active={activeOp === 'properties'} disabled={busy}
                    onclick={() => { toggle('properties'); if (activeOp === 'properties') doProperties(); }}>
                <Ruler size={14} /> {i18n.t.docxtools.properties}
            </button>
            <button class="dt-btn" class:active={activeOp === 'headings'} disabled={busy}
                    onclick={() => { toggle('headings'); if (activeOp === 'headings') doHeadings(); }}
                    title={i18n.t.docxtools.headings_hint}>
                <Heading size={14} /> {i18n.t.docxtools.headings}
            </button>
            <span class="dt-sep"></span>
            <button class="dt-btn" class:active={activeOp === 'restyle'} disabled={busy}
                    onclick={() => toggle('restyle')} title={i18n.t.docxtools.restyle_hint}>
                <Palette size={14} /> {i18n.t.docxtools.restyle}
            </button>
            <button class="dt-btn" class:active={activeOp === 'notes'} disabled={busy}
                    onclick={() => toggle('notes')}>
                <BookOpen size={14} /> {i18n.t.docxtools.notes}
            </button>
            <button class="dt-btn" class:active={activeOp === 'footnotes'} disabled={busy}
                    onclick={() => toggle('footnotes')} title={i18n.t.docxtools.footnotes_hint}>
                <Plus size={14} /> {i18n.t.docxtools.footnotes}
            </button>
            <button class="dt-btn" class:active={activeOp === 'quotes'} disabled={busy}
                    onclick={() => toggle('quotes')}>
                <Quote size={14} /> {i18n.t.docxtools.quotes}
            </button>
            <button class="dt-btn" disabled={busy} onclick={doStripRsids}
                    title={i18n.t.docxtools.rsids_hint}>
                <Eraser size={14} /> {i18n.t.docxtools.rsids}
            </button>
        {/if}
    </div>

    {#if error}<div class="dt-status dt-err"><X size={13} /> {error}</div>{/if}
    {#if success && !error}<div class="dt-status dt-ok"><Check size={13} /> {success}</div>{/if}
    {#if busy}<div class="dt-status dt-busy"><Loader2 size={13} class="spin" /> {i18n.t.docxtools.working}</div>{/if}

    {#if filePath && activeOp}
        <div class="dt-panel">
            {#if activeOp === 'restyle'}
                <button class="dt-btn" onclick={pickBlueprint}>{i18n.t.docxtools.choose_blueprint}</button>
                <span class="dt-hint" title={blueprintPath}>
                    {blueprintPath ? blueprintPath.split('/').pop() : i18n.t.docxtools.no_blueprint}
                </span>
                <button class="dt-btn dt-go" disabled={busy || !blueprintPath} onclick={doRestyle}>
                    {i18n.t.docxtools.restyle_apply}
                </button>
                <span class="dt-hint">{i18n.t.docxtools.restyle_caveat}</span>

            {:else if activeOp === 'notes'}
                <label>{i18n.t.docxtools.notes_target}
                    <select class="dt-input" bind:value={notesTarget}>
                        <option value="footnotes">{i18n.t.docxtools.footnotes_label}</option>
                        <option value="endnotes">{i18n.t.docxtools.endnotes_label}</option>
                    </select>
                </label>
                <button class="dt-btn dt-go" disabled={busy} onclick={doConvertNotes}>
                    {i18n.t.docxtools.convert_apply}
                </button>

            {:else if activeOp === 'quotes'}
                <label>{i18n.t.docxtools.quote_style}
                    <select class="dt-input" bind:value={quoteStyle}>
                        {#each QUOTE_STYLES as s}<option value={s}>{s}</option>{/each}
                    </select>
                </label>
                <button class="dt-btn dt-go" disabled={busy} onclick={doNormalizeQuotes}>
                    {i18n.t.docxtools.quotes_apply}
                </button>

            {:else if activeOp === 'footnotes'}
                <div class="dt-notes">
                    {#each noteRows as row, i}
                        <div class="dt-noterow">
                            <input type="number" min="1" class="dt-input-sm" bind:value={row.n} />
                            <input type="text" class="dt-input dt-grow"
                                   placeholder={i18n.t.docxtools.note_text}
                                   bind:value={row.text} />
                            <button class="dt-mini" title={i18n.t.docxtools.note_remove}
                                    onclick={() => removeNoteRow(i)}>
                                <Trash2 size={11} />
                            </button>
                        </div>
                    {/each}
                    <button class="dt-btn-sm" onclick={addNoteRow}>
                        <Plus size={11} /> {i18n.t.docxtools.note_add}
                    </button>
                </div>
                <button class="dt-btn dt-go" disabled={busy} onclick={doInjectFootnotes}>
                    {i18n.t.docxtools.footnotes_apply}
                </button>
                <span class="dt-hint">{i18n.t.docxtools.footnotes_caveat}</span>
            {/if}
        </div>
    {/if}

    <div class="dt-body">
        {#if !filePath}
            <div class="dt-empty">
                <FileUp size={44} strokeWidth={1} />
                <p>{i18n.t.docxtools.empty_hint}</p>
                <button class="dt-btn dt-open" onclick={openFile}>{i18n.t.docxtools.open}</button>
            </div>
        {:else}
            {#if activeOp === 'check' && checkResult}
                <section class="dt-card">
                    <h3>
                        {#if checkResult.valid}
                            <Check size={14} /> {i18n.t.docxtools.check_ok}
                        {:else}
                            <AlertTriangle size={14} /> {i18n.t.docxtools.check_issues
                                .replace('{n}', String(checkResult.issues.length))}
                        {/if}
                    </h3>
                    {#if checkResult.issues.length > 0}
                        <ul class="dt-list dt-list-bad">
                            {#each checkResult.issues as issue}<li>{issue}</li>{/each}
                        </ul>
                    {/if}
                    <ul class="dt-list dt-list-ok">
                        {#each checkResult.ok as ok}<li>{ok}</li>{/each}
                    </ul>
                </section>
            {/if}

            {#if activeOp === 'properties' && blueprint}
                <section class="dt-card">
                    <h3><Ruler size={14} /> {i18n.t.docxtools.properties}</h3>
                    <div class="dt-kv">
                        <span>{i18n.t.docxtools.default_font}</span>
                        <span>{blueprint.default_font || i18n.t.docxtools.unstated}
                            {#if blueprint.default_font_size_pt}· {blueprint.default_font_size_pt} pt{/if}</span>
                        <span>{i18n.t.docxtools.style_count}</span>
                        <span>{blueprint.style_count}</span>
                        <span>{i18n.t.docxtools.section_count}</span>
                        <span>{blueprint.sections.length}</span>
                    </div>
                    {#each blueprint.sections as s, i}
                        <div class="dt-section">
                            <h4>{i18n.t.docxtools.section} {i + 1}
                                {#if s.orientation}· {s.orientation}{/if}</h4>
                            <div class="dt-kv">
                                <span>{i18n.t.docxtools.page_size}</span>
                                <span>{pt(s.page_width_pt)} × {pt(s.page_height_pt)}</span>
                                <span>{i18n.t.docxtools.margins}</span>
                                <span>
                                    {i18n.t.docxtools.margin_short_top} {pt(s.top_margin_pt)} ·
                                    {i18n.t.docxtools.margin_short_right} {pt(s.right_margin_pt)} ·
                                    {i18n.t.docxtools.margin_short_bottom} {pt(s.bottom_margin_pt)} ·
                                    {i18n.t.docxtools.margin_short_left} {pt(s.left_margin_pt)}
                                </span>
                            </div>
                        </div>
                    {/each}
                </section>
            {/if}

            {#if activeOp === 'headings' && headings}
                <section class="dt-card">
                    <h3><Heading size={14} /> {i18n.t.docxtools.headings}</h3>
                    {#if headings.length === 0}
                        <p class="dt-hint">{i18n.t.docxtools.headings_none}</p>
                    {:else}
                        <p class="dt-hint">{i18n.t.docxtools.headings_explain}</p>
                        <ul class="dt-outline">
                            {#each headings as h}
                                <li style:padding-left="{(h.level - 1) * 16}px">
                                    <span class="dt-level">H{h.level}</span> {h.text}
                                </li>
                            {/each}
                        </ul>
                    {/if}
                </section>
            {/if}

            {#if transplant}
                <section class="dt-card">
                    <h3><Palette size={14} /> {i18n.t.docxtools.restyle}</h3>
                    <div class="dt-kv">
                        <span>{i18n.t.docxtools.paragraphs_moved}</span>
                        <span>{transplant.source_paragraphs}</span>
                        <span>{i18n.t.docxtools.blueprint_styles}</span>
                        <span>{transplant.blueprint_styles}</span>
                        <span>{i18n.t.docxtools.styles_remapped}</span>
                        <span>{transplant.styles_remapped}</span>
                    </div>
                </section>
            {/if}
        {/if}
    </div>
</div>

<style>
    .dt { display: flex; flex-direction: column; height: 100%; background: #09090b; color: #e4e4e7; }

    .dt-toolbar {
        display: flex; align-items: center; gap: 4px; flex-wrap: wrap;
        padding: 6px 10px; background: #18181b; border-bottom: 1px solid #27272a; flex-shrink: 0;
    }
    .dt-file { font-size: 0.72rem; color: #a1a1aa; max-width: 220px; overflow: hidden;
               text-overflow: ellipsis; white-space: nowrap; }
    .dt-sep { width: 1px; height: 20px; background: #3f3f46; margin: 0 2px; }

    .dt-btn {
        display: inline-flex; align-items: center; gap: 4px; padding: 4px 10px;
        border: 1px solid #3f3f46; border-radius: 5px; background: #27272a; color: #d4d4d8;
        font-size: 0.78rem; cursor: pointer; white-space: nowrap;
    }
    .dt-btn:hover:not(:disabled) { background: #3f3f46; }
    .dt-btn:disabled { opacity: 0.4; cursor: default; }
    .dt-btn.active { background: #1d4ed8; border-color: #2563eb; color: #fff; }
    .dt-open { background: #1d4ed8; border-color: #2563eb; color: #fff; }
    .dt-go { background: #15803d; border-color: #16a34a; color: #fff; }
    .dt-btn-sm {
        display: inline-flex; align-items: center; gap: 3px; padding: 3px 8px;
        border: 1px solid #3f3f46; border-radius: 4px; background: #27272a;
        color: #d4d4d8; font-size: 0.72rem; cursor: pointer;
    }
    .dt-mini {
        display: inline-flex; align-items: center; justify-content: center;
        width: 22px; height: 22px; border-radius: 4px; cursor: pointer;
        border: 1px solid #3f3f46; background: #27272a; color: #d4d4d8;
    }
    .dt-mini:hover { background: #7f1d1d; border-color: #b91c1c; }

    .dt-status { display: flex; align-items: center; gap: 6px; padding: 5px 12px;
                 font-size: 0.78rem; flex-shrink: 0; }
    .dt-err { background: #450a0a; color: #fca5a5; }
    .dt-ok { background: #052e16; color: #86efac; }
    .dt-busy { background: #111827; color: #93c5fd; }

    .dt-panel {
        display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
        padding: 7px 12px; background: #111114; border-bottom: 1px solid #27272a;
        font-size: 0.78rem; flex-shrink: 0;
    }
    .dt-panel label { display: inline-flex; align-items: center; gap: 5px; color: #a1a1aa; }
    .dt-input, .dt-input-sm, .dt-panel select {
        padding: 3px 7px; background: #18181b; border: 1px solid #3f3f46;
        border-radius: 4px; color: #d4d4d8; font-size: 0.75rem;
    }
    .dt-input-sm { width: 58px; }
    .dt-grow { flex: 1; min-width: 180px; }
    .dt-hint { color: #71717a; font-size: 0.72rem; }

    .dt-notes { display: flex; flex-direction: column; gap: 4px; flex: 1 1 320px; }
    .dt-noterow { display: flex; align-items: center; gap: 6px; }

    .dt-body { flex: 1; overflow-y: auto; padding: 12px; }
    .dt-empty {
        display: flex; flex-direction: column; align-items: center; justify-content: center;
        gap: 14px; height: 100%; color: #52525b;
    }

    .dt-card {
        background: #111114; border: 1px solid #27272a; border-radius: 8px;
        padding: 12px 14px; margin-bottom: 12px;
    }
    .dt-card h3 {
        display: flex; align-items: center; gap: 6px;
        margin: 0 0 10px; font-size: 0.85rem; color: #e4e4e7; font-weight: 600;
    }
    .dt-card h4 { margin: 10px 0 5px; font-size: 0.78rem; color: #a1a1aa; font-weight: 600; }

    .dt-kv {
        display: grid; grid-template-columns: minmax(120px, max-content) 1fr;
        gap: 3px 12px; font-size: 0.78rem;
    }
    .dt-kv span:nth-child(odd) { color: #71717a; }

    .dt-section { border-top: 1px solid #27272a; margin-top: 8px; padding-top: 4px; }

    .dt-list { margin: 0 0 6px; padding-left: 18px; font-size: 0.75rem; }
    .dt-list li { margin: 2px 0; }
    .dt-list-bad li { color: #fca5a5; }
    .dt-list-ok li { color: #71717a; }

    .dt-outline { list-style: none; margin: 0; padding: 0; font-size: 0.8rem; }
    .dt-outline li { padding: 2px 0; }
    .dt-level {
        display: inline-block; min-width: 24px; margin-right: 6px;
        font-size: 0.68rem; color: #93c5fd; font-variant-numeric: tabular-nums;
    }

    :global(.spin) { animation: dt-spin 1s linear infinite; }
    @keyframes dt-spin { to { transform: rotate(360deg); } }
</style>
