<script lang="ts">
    // Options bar for the PDF editor's document-level tools: everything that
    // used to live on the separate "Tools" tab (merge, split, watermark,
    // properties, security, sanitise) plus the editor's own non-drawing
    // panels (insert, page numbers, compress, forms, Kindle, annotation
    // store, image export). Drawing tools (crop, redact, text, annotations)
    // stay in the editor because they act on the page canvas.
    //
    // Undoable work goes through the edit session (`ctx.apply`); tools that
    // produce a new file run on a temp copy of the *edited* document, so
    // pending edits are always part of what gets written.

    import { invoke } from '@tauri-apps/api/core';
    import { onDestroy, untrack } from 'svelte';
    import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
    import { writeFile } from '@tauri-apps/plugin-fs';
    import { i18n } from '$lib/i18n.svelte';
    import { X } from 'lucide-svelte';
    import type { DocTool, DocToolContext, SessionState } from './types';
    import { parseRange, parseSplitRanges } from './types';
    import { requireCurrentDocument } from './actionRunner';
    import type { OperationName } from '$lib/operationLog';

    let { tool, session, selected, ctx, busy = false }: {
        tool: DocTool;
        session: SessionState;
        selected: number[];
        ctx: DocToolContext;
        busy?: boolean;
    } = $props();

    let disposed = false;
    let choosing = $state(false);
    const initialSessionId = untrack(() => session.id);
    const current = (expected = session) => !disposed && expected.id === initialSessionId && ctx.isCurrent(expected);
    onDestroy(() => { disposed = true; });

    async function run<T>(label: string, fn: () => Promise<T>, operation: OperationName = 'pdf.tool', writesOutput = false) {
        if (!current() || busy) return undefined;
        return await ctx.run(label, fn, operation, writesOutput);
    }
    async function apply(op: Record<string, unknown>, label: string, note: string) {
        return current() && !busy ? await ctx.apply(op, label, note) : false;
    }

    const t = $derived(i18n.t.pdfeditor);
    const tt = $derived(i18n.t.pdftools);

    const stem = $derived(session.source.split(/[/\\]/).pop()?.replace(/\.pdf$/i, '') ?? 'document');
    const dir = $derived.by(() => {
        const s = session.source;
        const i = Math.max(s.lastIndexOf('/'), s.lastIndexOf('\\'));
        return i > 0 ? s.slice(0, i) : '.';
    });

    async function askPdf(defaultName: string): Promise<string | null> {
        if (!current() || busy || choosing) return null;
        const expected = session;
        choosing = true;
        try {
            const out = await saveDialog({
                filters: [{ name: 'PDF', extensions: ['pdf'] }],
                defaultPath: `${dir}/${defaultName}`,
            });
            return current(expected) && typeof out === 'string' ? out : null;
        } finally { choosing = false; }
    }

    // ── Insert pages from another PDF ───────────────────────────────────
    let insertPath = $state('');
    let insertRange = $state('');
    let insertAt = $state(untrack(() => session.origins.length));

    async function pickInsertFile() {
        const p = await openDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], multiple: false });
        if (p) insertPath = p as string;
    }

    async function doInsertFrom() {
        if (!insertPath) { ctx.fail(t.insert_pages, t.pick_file_first); return; }
        const pages = parseRange(insertRange);
        if (pages === null) { ctx.fail(t.insert_pages, t.split_bad_range); return; }
        const ok = await apply({
            kind: 'insert_from',
            path: insertPath,
            pages,
            position: Math.max(0, Math.min(insertAt, session.origins.length)),
        }, t.insert_pages, t.inserted);
        if (ok) ctx.close();
    }

    // ── Merge: append whole PDFs at the end ─────────────────────────────
    let mergeFiles = $state<string[]>([]);

    async function addMergeFiles() {
        const p = await openDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], multiple: true });
        if (!p) return;
        const list = Array.isArray(p) ? p : [p];
        mergeFiles = [...mergeFiles, ...list.map(String)];
    }

    async function doMerge() {
        if (mergeFiles.length === 0) { ctx.fail(t.merge_append, tt.add_files_first); return; }
        let appended = 0;
        for (const path of mergeFiles) {
            const ok = await apply(
                { kind: 'insert_from', path, pages: [], position: session.origins.length },
                `${t.merge_append} ${path}`,
                t.appended.replace('{n}', String(appended + 1)),
            );
            if (!ok) break;
            appended += 1;
        }
        if (appended === mergeFiles.length) { mergeFiles = []; ctx.close(); }
    }

    // ── Split ───────────────────────────────────────────────────────────
    let splitSpec = $state('');

    async function doSplit() {
        const ranges = parseSplitRanges(splitSpec, session.origins.length);
        if (!ranges) { ctx.fail(t.split, t.split_bad_range); return; }
        const outputs = await run(t.split, async () => {
            const tmp = await ctx.exportTemp();
            return await invoke<string[]>('pdf_split', { path: tmp, ranges, outDir: dir, stem });
        }, 'pdf.split', true);
        if (outputs) ctx.notify(`${t.split_done.replace('{n}', String(outputs.length))}: ${outputs.join(', ')}`);
    }

    // ── Page numbers ────────────────────────────────────────────────────
    let numPosition = $state('bottom-center');
    let numFormat = $state('arabic');
    let numFontSize = $state(10);
    let numStart = $state(1);
    let numSkipFirst = $state(0);

    async function doPageNumbers() {
        const ok = await apply({
            kind: 'page_numbers',
            config: { position: numPosition, font_size: numFontSize, format: numFormat, start_number: numStart, skip_first: numSkipFirst },
        }, t.page_numbers, t.numbers_added);
        if (ok) ctx.close();
    }

    // ── Watermark (undoable session op) ─────────────────────────────────
    let wmText = $state('CONFIDENTIAL');
    let wmFontSize = $state(48);
    let wmAngle = $state(45);
    let wmOpacity = $state(0.15);
    let wmSelectedOnly = $state(false);

    async function doWatermark() {
        if (!wmText.trim()) { ctx.fail(t.watermark, t.text_required); return; }
        const ok = await apply({
            kind: 'watermark',
            config: { text: wmText, font_size: wmFontSize, angle: wmAngle, opacity: wmOpacity, color: [0.5, 0.5, 0.5] },
            pages: wmSelectedOnly && selected.length > 0 ? selected : null,
        }, t.watermark, t.watermark_added);
        if (ok) ctx.close();
    }

    // ── Metadata (undoable session op) ──────────────────────────────────
    let metaTitle = $state(untrack(() => session.info.title ?? ''));
    let metaAuthor = $state(untrack(() => session.info.author ?? ''));
    let metaSubject = $state(untrack(() => session.info.subject ?? ''));
    let metaKeywords = $state(untrack(() => session.info.keywords ?? ''));

    async function doMetadata() {
        const ok = await apply({
            kind: 'metadata',
            edits: {
                title: metaTitle || null, author: metaAuthor || null,
                subject: metaSubject || null, keywords: metaKeywords || null,
            },
        }, t.metadata, t.metadata_saved);
        if (ok) ctx.close();
    }

    // ── Security ────────────────────────────────────────────────────────
    let encOwnerPw = $state('');
    let encUserPw = $state('');
    let encNoPrint = $state(false);
    let encNoCopy = $state(false);
    let encNoModify = $state(false);
    let decryptPassword = $state('');

    async function doEncrypt() {
        if (!encOwnerPw) { ctx.fail(t.encrypt, tt.enter_owner_password); return; }
        const expected = session;
        const out = await askPdf(`${stem}-protected.pdf`);
        if (!out || !current(expected)) return;
        const ok = await run(t.encrypt, async () => {
            const tmp = await ctx.exportTemp();
            await invoke('pdf_encrypt', {
                path: tmp,
                config: {
                    owner_password: encOwnerPw, user_password: encUserPw,
                    allow_print: !encNoPrint, allow_copy: !encNoCopy, allow_modify: !encNoModify,
                    allow_annotate: !encNoModify, allow_fill_forms: true,
                    allow_assemble: !encNoModify, allow_high_quality_print: !encNoPrint,
                },
                outPath: out,
            });
            return true;
        }, 'pdf.encrypt', true);
        if (ok) {
            encOwnerPw = ''; encUserPw = '';
            ctx.notify(`${tt.encrypted} → ${out}`);
        }
    }

    async function doDecrypt() {
        if (!decryptPassword) { ctx.fail(t.decrypt, tt.enter_password); return; }
        const expected = session;
        const out = await askPdf(`${stem}-decrypted.pdf`);
        if (!out || !current(expected)) return;
        const ok = await run(t.decrypt, async () => {
            await invoke('pdf_decrypt', { path: session.source, password: decryptPassword, outPath: out });
            return true;
        }, 'pdf.decrypt', true);
        if (ok) { decryptPassword = ''; ctx.notify(`${tt.decrypted} → ${out}`, out); }
    }

    // ── Sanitise ────────────────────────────────────────────────────────
    let san = $state({
        strip_info: true, strip_xmp: true, strip_javascript: true, strip_embedded_files: true,
        strip_open_action: true, strip_thumbnails: true, strip_annotations: false,
    });

    async function doSanitise() {
        const expected = session;
        const out = await askPdf(`${stem}-clean.pdf`);
        if (!out || !current(expected)) return;
        const stripped = await run(t.sanitise, async () => {
            const tmp = await ctx.exportTemp();
            return await invoke<string[]>('pdf_sanitise', { path: tmp, options: { ...san }, outPath: out });
        }, 'pdf.sanitise', true);
        if (stripped) {
            ctx.notify(stripped.length > 0
                ? `${tt.sanitised}: ${stripped.join(', ')} → ${out}`
                : `${tt.no_metadata_found} → ${out}`, out);
        }
    }

    // ── Compression ─────────────────────────────────────────────────────
    interface CompressReport { bytes_before: number; bytes_after: number; streams_compressed: number; used_object_streams: boolean; }
    let compressObjectStreams = $state(true);
    let compressReport = $state<CompressReport | null>(null);

    function formatBytes(n: number): string {
        if (n < 1024) return `${n} B`;
        if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} kB`;
        return `${(n / (1024 * 1024)).toFixed(2)} MB`;
    }

    async function doCompress() {
        const expected = session;
        const out = await askPdf(`${stem}-compressed.pdf`);
        if (!out || !current(expected)) return;
        compressReport = null;
        const r = await run(t.compress, async () => {
            const tmp = await ctx.exportTemp();
            return await invoke<CompressReport>('pdf_compress', {
                path: tmp,
                options: { compress_streams: true, use_object_streams: compressObjectStreams, max_objects_per_stream: null },
                outPath: out,
            });
        }, 'pdf.compress', true);
        if (r) { compressReport = r; ctx.notify(`${t.compressed} ${out}`, out); }
    }

    // ── Forms ───────────────────────────────────────────────────────────
    interface FormField {
        name: string; kind: string; value: string | null; on_value: string | null;
        options: string[]; read_only: boolean; required: boolean;
        page: number | null; rect: [number, number, number, number] | null;
    }
    let formFields = $state<FormField[]>([]);
    let formEdits = $state<Record<string, string>>({});
    let formLoaded = $state(false);
    let formAttempted = $state(false);
    let formLoading = $state(false);

    async function loadForm() {
        if (!current() || busy || formLoading) return;
        formAttempted = true;
        formLoading = true;
        try {
            const fields = await run(t.form, async () => {
                const tmp = await ctx.exportTemp();
                return await invoke<FormField[]>('pdf_read_form_fields', { path: tmp });
            }, 'pdf.form');
            if (!fields || !current()) return;
            formFields = fields;
            formEdits = Object.fromEntries(fields.map((f) => [f.name, f.value ?? '']));
            formLoaded = true;
        } finally { formLoading = false; }
    }

    $effect(() => { if (tool === 'form' && !formAttempted) untrack(() => { void loadForm(); }); });

    function changedFormValues(): Record<string, string> {
        const out: Record<string, string> = {};
        for (const f of formFields) {
            const now = formEdits[f.name] ?? '';
            if (now !== (f.value ?? '')) out[f.name] = now;
        }
        return out;
    }

    async function doFillForm(flatten: boolean) {
        const expected = session;
        const values = changedFormValues();
        if (!flatten && Object.keys(values).length === 0) { ctx.fail(t.form, t.form_no_changes); return; }
        const out = await askPdf(flatten ? `${stem}-filled-flat.pdf` : `${stem}-filled.pdf`);
        if (!out || !current(expected)) return;
        const msg = await run(t.form, async () => {
            const tmp = await ctx.exportTemp();
            let n = 0;
            if (Object.keys(values).length > 0) n = await invoke<number>('pdf_fill_form', { path: tmp, values, outPath: out });
            if (flatten) {
                const src = Object.keys(values).length > 0 ? out : tmp;
                const k = await invoke<number>('pdf_flatten_form', { path: src, outPath: out });
                return t.form_flattened.replace('{n}', String(k)).replace('{path}', out);
            }
            return t.form_filled.replace('{n}', String(n)).replace('{path}', out);
        }, 'pdf.form', true);
        if (msg) ctx.notify(msg, out);
    }

    // ── Kindle clippings ────────────────────────────────────────────────
    interface ImportSummary { parsed: number; deduped: number; imported: number; matched: number; fuzzy_matched: number; titles: string[]; duplicates_skipped: number; }
    let clippingsPath = $state('');
    let kindleBooks = $state<string[]>([]);
    let kindleTitle = $state('');
    let kindleAnchor = $state(true);
    let kindleSummary = $state<ImportSummary | null>(null);

    async function pickClippings() {
        const p = await openDialog({ filters: [{ name: 'Kindle clippings', extensions: ['txt'] }], multiple: false });
        if (!p) return;
        clippingsPath = p as string;
        kindleSummary = null;
        if (!current()) return;
        const books = await run(t.kindle, () => invoke<string[]>('kindle_list_books', { clippingsPath }), 'pdf.kindle');
        if (books) { kindleBooks = books; kindleTitle = books[0] ?? ''; }
    }

    async function doKindleImport() {
        if (!clippingsPath) { ctx.fail(t.kindle, t.kindle_pick_first); return; }
        const r = await run(t.kindle, async () => {
            const documentPath = kindleAnchor ? await ctx.exportTemp() : null;
            return await invoke<ImportSummary>('kindle_import', {
                clippingsPath, docId: session.source, titleFilter: kindleTitle || null, documentPath, minScore: null,
            });
        }, 'pdf.kindle', true);
        if (r) {
            kindleSummary = r;
            ctx.notify(t.kindle_result.replace('{imported}', String(r.imported)).replace('{matched}', String(r.matched)).replace('{dupes}', String(r.duplicates_skipped)));
        }
    }

    // ── Annotation store (search index) ─────────────────────────────────
    let annotCount = $state<number | null>(null);

    async function doImportAnnots() {
        const n = await run(t.annots_import, async () => {
            const tmp = await ctx.exportTemp();
            return await invoke<number>('pdf_import_annotations', { path: tmp, docId: session.source });
        }, 'pdf.annotations', true);
        if (n !== undefined) { annotCount = n; ctx.notify(t.annots_imported.replace('{n}', String(n))); }
    }

    async function doStampAnnots() {
        const expected = session;
        const out = await askPdf(`${stem}-annotated.pdf`);
        if (!out || !current(expected)) return;
        const n = await run(t.annots_stamp, async () => {
            const tmp = await ctx.exportTemp();
            return await invoke<number>('pdf_stamp_annotations', { path: tmp, docId: session.source, outPath: out });
        }, 'pdf.annotations', true);
        if (n !== undefined) ctx.notify(t.annots_stamped.replace('{n}', String(n)).replace('{path}', out), out);
    }

    // ── Pages as images ─────────────────────────────────────────────────
    let imgDpi = $state(150);
    let imgScope = $state<'selected' | 'all'>(untrack(() => selected.length > 0) ? 'selected' : 'all');

    async function doExportImages() {
        if (!current() || busy) return;
        const expected = session;
        const folder = await openDialog({ directory: true, multiple: false });
        if (!folder || Array.isArray(folder) || !current(expected)) return;
        const pages = imgScope === 'selected' && selected.length > 0
            ? selected
            : session.origins.map((_, i) => i);
        const dpi = imgDpi;
        const outputStem = stem;
        const n = await run(t.export_images, async () => {
            let written = 0;
            for (const i of pages) {
                requireCurrentDocument(() => current(expected));
                const bytes = await ctx.renderPagePng(i, dpi);
                requireCurrentDocument(() => current(expected));
                const name = `${outputStem}-p${String(i + 1).padStart(3, '0')}.png`;
                await writeFile(`${folder}/${name}`, bytes);
                written += 1;
            }
            return written;
        }, 'pdf.images', true);
        if (n !== undefined) ctx.notify(t.images_exported.replace('{n}', String(n)).replace('{path}', folder));
    }
</script>

<fieldset class="dt-bar" disabled={busy || choosing} aria-label={tool}>
    {#if tool === 'insert'}
        <button class="dt-btn" onclick={pickInsertFile}>{t.choose_pdf}</button>
        <span class="dt-hint dt-path" title={insertPath}>{insertPath || t.no_file}</span>
        <label>{t.range} <input type="text" bind:value={insertRange} placeholder="1-3,7" class="dt-in-sm" /></label>
        <label>{t.at_position} <input type="number" min="0" max={session.origins.length} bind:value={insertAt} class="dt-in-sm" /></label>
        <button class="dt-btn dt-go" onclick={doInsertFrom}>{t.apply}</button>

    {:else if tool === 'merge'}
        <button class="dt-btn" onclick={addMergeFiles}>{tt.add_files}</button>
        {#each mergeFiles as f, i (i)}
            <span class="dt-chip">{f.split(/[/\\]/).pop()}
                <button class="dt-chip-x" aria-label={t.dismiss} onclick={() => mergeFiles = mergeFiles.filter((_, j) => j !== i)}><X size={10} /></button>
            </span>
        {/each}
        <button class="dt-btn dt-go" disabled={mergeFiles.length === 0} onclick={doMerge}>{tt.merge_all}</button>
        <span class="dt-hint">{t.insert_pages}</span>

    {:else if tool === 'split'}
        <label>{tt.ranges} <input type="text" bind:value={splitSpec} placeholder="1-5,6-10" class="dt-in" /></label>
        <button class="dt-btn dt-go" onclick={doSplit}>{tt.split_now}</button>
        <span class="dt-hint">{t.split_hint}</span>

    {:else if tool === 'numbers'}
        <label>{t.position}
            <select bind:value={numPosition} class="dt-in">
                <option value="bottom-center">bottom-center</option>
                <option value="bottom-left">bottom-left</option>
                <option value="bottom-right">bottom-right</option>
                <option value="top-center">top-center</option>
                <option value="top-left">top-left</option>
                <option value="top-right">top-right</option>
            </select>
        </label>
        <label>{t.format}
            <select bind:value={numFormat} class="dt-in">
                <option value="arabic">1, 2, 3</option>
                <option value="roman">i, ii, iii</option>
                <option value="page-of">Page 1 of N</option>
            </select>
        </label>
        <label>{t.size} <input type="number" min="6" max="48" bind:value={numFontSize} class="dt-in-sm" /></label>
        <label>{t.start_at} <input type="number" min="1" bind:value={numStart} class="dt-in-sm" /></label>
        <label>{t.skip_first} <input type="number" min="0" bind:value={numSkipFirst} class="dt-in-sm" /></label>
        <button class="dt-btn dt-go" onclick={doPageNumbers}>{t.apply}</button>

    {:else if tool === 'watermark'}
        <label class="dt-grow">{t.text} <input type="text" bind:value={wmText} class="dt-in" /></label>
        <label>{t.size} <input type="number" min="6" max="200" bind:value={wmFontSize} class="dt-in-sm" /></label>
        <label>{t.angle} <input type="number" bind:value={wmAngle} class="dt-in-sm" />°</label>
        <label>{t.opacity} <input type="range" min="0.05" max="1" step="0.05" bind:value={wmOpacity} /> {(wmOpacity * 100).toFixed(0)}%</label>
        <label><input type="checkbox" bind:checked={wmSelectedOnly} disabled={selected.length === 0} /> {t.export_scope_selected}</label>
        <button class="dt-btn dt-go" onclick={doWatermark}>{t.apply}</button>

    {:else if tool === 'metadata'}
        <label>{t.meta_title} <input type="text" bind:value={metaTitle} class="dt-in" /></label>
        <label>{t.meta_author} <input type="text" bind:value={metaAuthor} class="dt-in" /></label>
        <label>{t.meta_subject} <input type="text" bind:value={metaSubject} class="dt-in" /></label>
        <label>{t.meta_keywords} <input type="text" bind:value={metaKeywords} class="dt-in" /></label>
        <button class="dt-btn dt-go" onclick={doMetadata}>{t.apply}</button>

    {:else if tool === 'encrypt'}
        <label>{tt.owner_pw} <input type="password" bind:value={encOwnerPw} class="dt-in" autocomplete="new-password" /></label>
        <label>{tt.user_pw} <input type="password" bind:value={encUserPw} class="dt-in" autocomplete="new-password" /></label>
        <label><input type="checkbox" bind:checked={encNoPrint} /> {tt.no_print}</label>
        <label><input type="checkbox" bind:checked={encNoCopy} /> {tt.no_copy}</label>
        <label><input type="checkbox" bind:checked={encNoModify} /> {tt.no_modify}</label>
        <button class="dt-btn dt-go" onclick={doEncrypt}>{tt.encrypt}</button>

    {:else if tool === 'decrypt'}
        <span class="dt-hint">{t.encrypted_open}</span>
        <label>{tt.password} <input type="password" bind:value={decryptPassword} class="dt-in" autocomplete="current-password" /></label>
        <button class="dt-btn dt-go" onclick={doDecrypt}>{tt.decrypt}</button>

    {:else if tool === 'sanitise'}
        <label><input type="checkbox" bind:checked={san.strip_info} /> /Info</label>
        <label><input type="checkbox" bind:checked={san.strip_xmp} /> XMP</label>
        <label><input type="checkbox" bind:checked={san.strip_javascript} /> JavaScript</label>
        <label><input type="checkbox" bind:checked={san.strip_embedded_files} /> Embedded files</label>
        <label><input type="checkbox" bind:checked={san.strip_open_action} /> OpenAction</label>
        <label><input type="checkbox" bind:checked={san.strip_thumbnails} /> Thumbnails</label>
        <label><input type="checkbox" bind:checked={san.strip_annotations} /> {t.annotations}</label>
        <button class="dt-btn dt-go" onclick={doSanitise}>{tt.sanitise}</button>

    {:else if tool === 'compress'}
        <label><input type="checkbox" bind:checked={compressObjectStreams} /> {t.compress_object_streams}</label>
        <button class="dt-btn dt-go" onclick={doCompress}>{t.compress_apply}</button>
        <span class="dt-hint">{t.compress_images_hint}</span>
        {#if compressReport}
            <span class="dt-hint">
                {#if compressReport.bytes_after < compressReport.bytes_before}
                    {t.compress_result
                        .replace('{before}', formatBytes(compressReport.bytes_before))
                        .replace('{after}', formatBytes(compressReport.bytes_after))
                        .replace('{pct}', (100 * (1 - compressReport.bytes_after / compressReport.bytes_before)).toFixed(1))}
                {:else}
                    {t.compress_no_gain}
                {/if}
            </span>
        {/if}

    {:else if tool === 'form'}
        {#if formFields.length === 0}
            <span class="dt-hint">{formLoading ? t.working : formLoaded ? t.form_none : t.form_reload}</span>
            <button class="dt-btn" disabled={formLoading} onclick={loadForm}>{t.form_reload}</button>
        {:else}
            <div class="dt-formgrid">
                {#each formFields as f (f.name)}
                    <label class="dt-formrow">
                        <span class="dt-formname" title={f.name}>{f.name}{#if f.required}<span class="dt-req">*</span>{/if}</span>
                        {#if f.kind === 'checkbox' || f.kind === 'radio'}
                            <input type="checkbox" disabled={f.read_only}
                                checked={!!formEdits[f.name] && formEdits[f.name].toLowerCase() !== 'off'}
                                onchange={(e) => formEdits[f.name] = (e.currentTarget as HTMLInputElement).checked ? 'true' : 'false'} />
                        {:else if f.options.length > 0}
                            <select class="dt-in" disabled={f.read_only} bind:value={formEdits[f.name]}>
                                {#each f.options as o}<option value={o}>{o}</option>{/each}
                            </select>
                        {:else}
                            <input type="text" class="dt-in" disabled={f.read_only} bind:value={formEdits[f.name]} />
                        {/if}
                    </label>
                {/each}
            </div>
            <button class="dt-btn dt-go" onclick={() => doFillForm(false)}>{t.form_save}</button>
            <button class="dt-btn" onclick={() => doFillForm(true)} title={t.form_flatten_hint}>{t.form_flatten}</button>
        {/if}

    {:else if tool === 'kindle'}
        <button class="dt-btn" onclick={pickClippings}>{t.kindle_choose}</button>
        <span class="dt-hint dt-path">{clippingsPath || t.no_file}</span>
        {#if kindleBooks.length > 0}
            <label>{t.kindle_book}
                <select class="dt-in" bind:value={kindleTitle}>
                    {#each kindleBooks as b}<option value={b}>{b}</option>{/each}
                </select>
            </label>
            <label><input type="checkbox" bind:checked={kindleAnchor} /> {t.kindle_anchor}</label>
            <button class="dt-btn dt-go" onclick={doKindleImport}>{t.kindle_import}</button>
        {/if}
        {#if kindleSummary}
            <span class="dt-hint">{t.kindle_result.replace('{imported}', String(kindleSummary.imported)).replace('{matched}', String(kindleSummary.matched)).replace('{dupes}', String(kindleSummary.duplicates_skipped))}</span>
        {/if}

    {:else if tool === 'annots'}
        <button class="dt-btn" onclick={doImportAnnots}>{t.annots_import}</button>
        <button class="dt-btn" onclick={doStampAnnots}>{t.annots_stamp}</button>
        {#if annotCount !== null}<span class="dt-hint">{t.annots_in_store}: {annotCount}</span>{/if}

    {:else if tool === 'images'}
        <label>{t.export_images_dpi}
            <select class="dt-in" bind:value={imgDpi}>
                <option value={72}>72 dpi</option>
                <option value={150}>150 dpi</option>
                <option value={300}>300 dpi</option>
            </select>
        </label>
        <label><input type="radio" bind:group={imgScope} value="selected" disabled={selected.length === 0} /> {t.export_scope_selected} ({selected.length})</label>
        <label><input type="radio" bind:group={imgScope} value="all" /> {t.export_scope_all} ({session.origins.length})</label>
        <button class="dt-btn dt-go" onclick={doExportImages}>{t.export_images_go}</button>
    {/if}

    <span class="dt-spacer"></span>
    <button class="dt-close" onclick={() => ctx.close()} aria-label={t.close_tool} title={t.close_tool}><X size={13} /></button>
</fieldset>

<style>
    .dt-bar {
        margin: 0; min-width: 0; border: 0;
        display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
        padding: 7px 12px; background: #18181b; border-bottom: 1px solid #3f3f46; font-size: 12px;
    }
    .dt-bar label { display: inline-flex; align-items: center; gap: 5px; color: #a1a1aa; }
    .dt-grow { flex: 1; min-width: 180px; }
    .dt-grow .dt-in { flex: 1; }
    .dt-in, .dt-in-sm {
        background: #27272a; border: 1px solid #3f3f46; border-radius: 5px;
        color: #e4e4e7; padding: 4px 7px; font-size: 12px;
    }
    .dt-in-sm { width: 64px; }
    .dt-btn {
        display: inline-flex; align-items: center; gap: 5px;
        padding: 4px 10px; font-size: 12px; border-radius: 6px;
        border: 1px solid #3f3f46; background: #27272a; color: #e4e4e7; cursor: pointer;
    }
    .dt-btn:hover:not(:disabled) { background: #3f3f46; }
    .dt-btn:disabled { opacity: 0.45; cursor: default; }
    .dt-go { background: #3b82f6; border-color: #3b82f6; color: #fff; }
    .dt-go:hover:not(:disabled) { background: #2563eb; }
    .dt-hint { color: #a1a1aa; font-size: 11px; }
    .dt-path { max-width: 280px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .dt-spacer { flex: 1; }
    .dt-close {
        display: inline-flex; align-items: center; justify-content: center;
        width: 24px; height: 24px; border-radius: 5px; cursor: pointer;
        border: 1px solid transparent; background: none; color: #a1a1aa;
    }
    .dt-close:hover { background: #27272a; color: #e4e4e7; }
    .dt-chip {
        display: inline-flex; align-items: center; gap: 4px; padding: 2px 4px 2px 8px;
        border-radius: 10px; background: #27272a; color: #e4e4e7; font-size: 11px;
    }
    .dt-chip-x { display: inline-flex; background: none; border: none; color: #a1a1aa; cursor: pointer; padding: 1px; }
    .dt-formgrid {
        display: flex; flex-direction: column; gap: 4px;
        max-height: 200px; overflow-y: auto; flex: 1 1 320px; min-width: 260px;
    }
    .dt-formrow { display: flex; align-items: center; gap: 8px; }
    .dt-formname { flex: 0 0 150px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .dt-formrow .dt-in { flex: 1; }
    .dt-req { color: #f87171; margin-left: 2px; }
</style>
