<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
    import { i18n } from '$lib/i18n.svelte';
    import DocumentViewer from './viewer/DocumentViewer.svelte';
    import {
        FileUp, FilePlus2, Scissors, Trash2, RotateCw, Crop, Hash,
        Stamp, FileText, Merge, ChevronLeft, ChevronRight, Check,
        Loader2, X, Info, Download, Plus, Lock, Unlock, Shield
    } from 'lucide-svelte';

    // ── Types ──────────────────────────────────────────────────────────
    interface PdfPageInfo { page_number: number; width_pt: number; height_pt: number; rotation: number; }
    interface PdfInfo {
        page_count: number; pages: PdfPageInfo[];
        title?: string; author?: string; subject?: string; keywords?: string;
        producer?: string; creator?: string;
    }

    // ── State ──────────────────────────────────────────────────────────
    let filePath = $state('');
    let info = $state<PdfInfo | null>(null);
    let loading = $state(false);
    let error = $state('');
    let success = $state('');
    let selected = $state<Set<number>>(new Set()); // 0-based page indices

    // Operation panels
    let activeOp = $state<string | null>(null);

    // Operation-specific state
    let mergeFiles = $state<string[]>([]);
    let splitSpec = $state('');
    let rotateDeg = $state(90);
    let cropRect = $state('0,0,400,600');
    let numPosition = $state('bottom-center');
    let numFormat = $state('arabic');
    let numFontSize = $state(10);
    let numSkipFirst = $state(0);
    let wmText = $state('CONFIDENTIAL');
    let wmFontSize = $state(48);
    let wmAngle = $state(45);
    let wmOpacity = $state(0.15);
    let metaTitle = $state('');
    let metaAuthor = $state('');
    let metaSubject = $state('');
    let metaKeywords = $state('');
    let decryptPassword = $state('');
    let encOwnerPw = $state('');
    let encUserPw = $state('');
    let encNoPrint = $state(false);
    let encNoCopy = $state(false);
    let encNoModify = $state(false);
    let isEncrypted = $state(false);
    // Sanitise options
    let sanStripInfo = $state(true);
    let sanStripXmp = $state(true);
    let sanStripJs = $state(true);
    let sanStripFiles = $state(true);
    let sanStripOpen = $state(true);
    let sanStripThumbs = $state(true);
    let sanStripAnnots = $state(true);

    // ── File loading ───────────────────────────────────────────────────
    async function openFile() {
        const path = await openDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            multiple: false,
        });
        if (path) loadPdf(path as string);
    }

    async function loadPdf(path: string) {
        filePath = path;
        loading = true;
        error = '';
        success = '';
        info = null;
        selected = new Set();
        activeOp = null;
        try {
            info = await invoke<PdfInfo>('pdf_info', { path });
            // Pre-fill metadata
            metaTitle = info.title ?? '';
            metaAuthor = info.author ?? '';
            metaSubject = info.subject ?? '';
            metaKeywords = info.keywords ?? '';
            // Check encryption
            try { isEncrypted = await invoke<boolean>('pdf_is_encrypted', { path }); } catch { isEncrypted = false; }
        } catch (e: any) {
            error = e?.message ?? String(e);
        }
        loading = false;
    }

    // ── Selection helpers ──────────────────────────────────────────────
    function togglePage(idx: number) {
        const s = new Set(selected);
        if (s.has(idx)) s.delete(idx); else s.add(idx);
        selected = s;
    }
    function selectAll() {
        if (!info) return;
        selected = new Set(Array.from({ length: info.page_count }, (_, i) => i));
    }
    function selectNone() { selected = new Set(); }
    function selectedSorted(): number[] { return [...selected].sort((a, b) => a - b); }

    // ── Save dialog helper ─────────────────────────────────────────────
    async function pickSavePath(defaultName?: string): Promise<string | null> {
        return await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: defaultName,
        }) as string | null;
    }

    // ── Operations ─────────────────────────────────────────────────────
    async function doExtract() {
        if (selected.size === 0) { error = i18n.t.pdftools.select_pages_first; return; }
        const out = await pickSavePath('extracted.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_extract_pages', { path: filePath, pageIndices: selectedSorted(), outPath: out });
            success = `${i18n.t.pdftools.extracted} ${selected.size} ${i18n.t.pdftools.pages_to} ${out}`;
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doRemove() {
        if (selected.size === 0) { error = i18n.t.pdftools.select_pages_first; return; }
        const out = await pickSavePath('trimmed.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_remove_pages', { path: filePath, pageIndices: selectedSorted(), outPath: out });
            success = `${i18n.t.pdftools.removed} ${selected.size} ${i18n.t.pdftools.pages_saved} ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doRotate() {
        const pages = selected.size > 0 ? selectedSorted() : Array.from({ length: info?.page_count ?? 0 }, (_, i) => i);
        const out = await pickSavePath('rotated.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_rotate_pages', { path: filePath, pageIndices: pages, degrees: rotateDeg, outPath: out });
            success = `${i18n.t.pdftools.rotated} ${pages.length} ${i18n.t.pdftools.pages_saved} ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doCrop() {
        const pages = selected.size > 0 ? selectedSorted() : Array.from({ length: info?.page_count ?? 0 }, (_, i) => i);
        const parts = cropRect.split(',').map(Number);
        if (parts.length !== 4 || parts.some(isNaN)) { error = 'Rect must be x,y,w,h'; return; }
        const out = await pickSavePath('cropped.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_crop_pages', { path: filePath, pageIndices: pages, x: parts[0], y: parts[1], w: parts[2], h: parts[3], outPath: out });
            success = `${i18n.t.pdftools.cropped} → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doMerge() {
        if (mergeFiles.length === 0) { error = i18n.t.pdftools.add_files_first; return; }
        const allPaths = [filePath, ...mergeFiles];
        const out = await pickSavePath('merged.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            const total = await invoke<number>('pdf_merge', { paths: allPaths, outPath: out });
            success = `${i18n.t.pdftools.merged} ${allPaths.length} files → ${total} pages → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function addMergeFile() {
        const path = await openDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], multiple: true });
        if (path) {
            const paths = Array.isArray(path) ? path : [path];
            mergeFiles = [...mergeFiles, ...paths.map(String)];
        }
    }

    async function doSplit() {
        if (!splitSpec.trim()) { error = 'Enter page ranges (e.g. 1-5,6-10)'; return; }
        const dir = filePath.substring(0, filePath.lastIndexOf('/') || filePath.lastIndexOf('\\')) || '.';
        const stem = filePath.split(/[/\\]/).pop()?.replace('.pdf', '') ?? 'doc';
        loading = true; error = ''; success = '';
        try {
            const outputs = await invoke<string[]>('pdf_split', { path: filePath, ranges: parseSplitRanges(splitSpec), outDir: dir, stem });
            success = `${i18n.t.pdftools.split_into} ${outputs.length} files`;
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    function parseSplitRanges(spec: string): [number, number][] {
        return spec.split(',').map(part => {
            const [a, b] = part.trim().split('-').map(Number);
            return [a - 1, b] as [number, number]; // 0-based start, exclusive end
        });
    }

    async function doPageNumbers() {
        const out = await pickSavePath('numbered.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_add_page_numbers', {
                path: filePath,
                config: { position: numPosition, font_size: numFontSize, format: numFormat, start_number: 1, skip_first: numSkipFirst },
                outPath: out,
            });
            success = `${i18n.t.pdftools.page_numbers_added} → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doWatermark() {
        const out = await pickSavePath('watermarked.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_add_watermark', {
                path: filePath,
                config: { text: wmText, font_size: wmFontSize, angle: wmAngle, opacity: wmOpacity, color: [0.5, 0.5, 0.5] },
                pageIndices: selected.size > 0 ? selectedSorted() : null,
                outPath: out,
            });
            success = `${i18n.t.pdftools.watermark_added} → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doInsertBlank() {
        const pos = selected.size === 1 ? [...selected][0] : (info?.page_count ?? 0);
        const out = await pickSavePath('with-blank.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_insert_blank_page', { path: filePath, position: pos, outPath: out });
            success = `${i18n.t.pdftools.blank_inserted} → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doMetadata() {
        const out = await pickSavePath('updated.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_edit_metadata', {
                path: filePath,
                edits: {
                    title: metaTitle || null,
                    author: metaAuthor || null,
                    subject: metaSubject || null,
                    keywords: metaKeywords || null,
                },
                outPath: out,
            });
            success = `${i18n.t.pdftools.metadata_saved} → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doReorder() {
        // Simple reverse for now — full drag-and-drop is a follow-up
        if (!info) return;
        const order = Array.from({ length: info.page_count }, (_, i) => info!.page_count - 1 - i);
        const out = await pickSavePath('reordered.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_reorder_pages', { path: filePath, newOrder: order, outPath: out });
            success = `${i18n.t.pdftools.reordered} → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doDecrypt() {
        if (!decryptPassword) { error = i18n.t.pdftools.enter_password; return; }
        const out = await pickSavePath('decrypted.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_decrypt', { path: filePath, password: decryptPassword, outPath: out });
            success = `${i18n.t.pdftools.decrypted} → ${out}`;
            decryptPassword = '';
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doEncrypt() {
        if (!encOwnerPw) { error = i18n.t.pdftools.enter_owner_password; return; }
        const out = await pickSavePath('encrypted.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_encrypt', {
                path: filePath,
                config: {
                    owner_password: encOwnerPw, user_password: encUserPw,
                    allow_print: !encNoPrint, allow_copy: !encNoCopy, allow_modify: !encNoModify,
                    allow_annotate: !encNoModify, allow_fill_forms: true,
                    allow_assemble: !encNoModify, allow_high_quality_print: !encNoPrint,
                },
                outPath: out,
            });
            success = `${i18n.t.pdftools.encrypted} → ${out}`;
            encOwnerPw = ''; encUserPw = '';
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doDetectSignatures() {
        loading = true; error = ''; success = '';
        try {
            const sigs = await invoke<any[]>('pdf_detect_signatures', { path: filePath });
            if (sigs.length === 0) {
                success = 'No digital signatures found.';
            } else {
                success = `${sigs.length} signature(s): ${sigs.map((s: any) => s.name || s.filter || 'unsigned').join(', ')}`;
            }
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doPdfa() {
        const out = await pickSavePath('archival.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            await invoke('pdf_convert_pdfa', { path: filePath, outPath: out });
            success = `PDF/A-2b → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }

    async function doSanitise() {
        const out = await pickSavePath('sanitised.pdf');
        if (!out) return;
        loading = true; error = ''; success = '';
        try {
            const options = {
                strip_info: sanStripInfo, strip_xmp: sanStripXmp,
                strip_javascript: sanStripJs, strip_embedded_files: sanStripFiles,
                strip_open_action: sanStripOpen, strip_thumbnails: sanStripThumbs,
                strip_annotations: sanStripAnnots,
            };
            const stripped = await invoke<string[]>('pdf_sanitise', { path: filePath, options, outPath: out });
            success = stripped.length > 0
                ? `${i18n.t.pdftools.sanitised}: ${stripped.join(', ')} → ${out}`
                : `${i18n.t.pdftools.no_metadata_found} → ${out}`;
            loadPdf(out);
        } catch (e: any) { error = String(e); }
        loading = false;
    }
</script>

<div class="pdf-tools">
    <!-- Toolbar -->
    <div class="pt-toolbar">
        <button class="pt-btn pt-open" onclick={openFile}>
            <FileUp size={16} /> {i18n.t.pdftools.open}
        </button>
        {#if info}
            <span class="pt-info-badge">{info.page_count} {i18n.t.pdftools.pages}</span>
            <span class="pt-sep"></span>
            <button class="pt-btn" class:active={activeOp === 'extract'} onclick={() => activeOp = activeOp === 'extract' ? null : 'extract'} title={i18n.t.pdftools.extract_pages}>
                <Download size={14} /> {i18n.t.pdftools.extract}
            </button>
            <button class="pt-btn" class:active={activeOp === 'remove'} onclick={() => activeOp = activeOp === 'remove' ? null : 'remove'} title={i18n.t.pdftools.remove_pages}>
                <Trash2 size={14} /> {i18n.t.pdftools.remove}
            </button>
            <button class="pt-btn" class:active={activeOp === 'rotate'} onclick={() => activeOp = activeOp === 'rotate' ? null : 'rotate'}>
                <RotateCw size={14} /> {i18n.t.pdftools.rotate}
            </button>
            <button class="pt-btn" class:active={activeOp === 'crop'} onclick={() => activeOp = activeOp === 'crop' ? null : 'crop'}>
                <Crop size={14} /> {i18n.t.pdftools.crop}
            </button>
            <button class="pt-btn" class:active={activeOp === 'merge'} onclick={() => activeOp = activeOp === 'merge' ? null : 'merge'}>
                <Merge size={14} /> {i18n.t.pdftools.merge}
            </button>
            <button class="pt-btn" class:active={activeOp === 'split'} onclick={() => activeOp = activeOp === 'split' ? null : 'split'}>
                <Scissors size={14} /> {i18n.t.pdftools.split}
            </button>
            <button class="pt-btn" class:active={activeOp === 'number'} onclick={() => activeOp = activeOp === 'number' ? null : 'number'}>
                <Hash size={14} /> {i18n.t.pdftools.page_numbers}
            </button>
            <button class="pt-btn" class:active={activeOp === 'watermark'} onclick={() => activeOp = activeOp === 'watermark' ? null : 'watermark'}>
                <Stamp size={14} /> {i18n.t.pdftools.watermark}
            </button>
            <button class="pt-btn" class:active={activeOp === 'metadata'} onclick={() => activeOp = activeOp === 'metadata' ? null : 'metadata'}>
                <Info size={14} /> {i18n.t.pdftools.metadata}
            </button>
            <button class="pt-btn" onclick={doInsertBlank} title={i18n.t.pdftools.insert_blank}>
                <Plus size={14} />
            </button>
            <button class="pt-btn" onclick={doReorder} title={i18n.t.pdftools.reverse_order}>
                &#8693;
            </button>
            <span class="pt-sep"></span>
            {#if isEncrypted}
                <button class="pt-btn" class:active={activeOp === 'decrypt'} onclick={() => activeOp = activeOp === 'decrypt' ? null : 'decrypt'}>
                    <Unlock size={14} /> {i18n.t.pdftools.decrypt}
                </button>
            {:else}
                <button class="pt-btn" class:active={activeOp === 'encrypt'} onclick={() => activeOp = activeOp === 'encrypt' ? null : 'encrypt'}>
                    <Lock size={14} /> {i18n.t.pdftools.encrypt}
                </button>
            {/if}
            <button class="pt-btn" class:active={activeOp === 'sanitise'} onclick={() => activeOp = activeOp === 'sanitise' ? null : 'sanitise'} title={i18n.t.pdftools.sanitise}>
                <Shield size={14} /> {i18n.t.pdftools.sanitise}
            </button>
            <button class="pt-btn" onclick={doDetectSignatures} title="Detect digital signatures">
                ✎
            </button>
            <button class="pt-btn" onclick={doPdfa} title="Convert to PDF/A-2b">
                A
            </button>
        {/if}
    </div>

    <!-- Status bar -->
    {#if error}<div class="pt-status pt-error">{error}</div>{/if}
    {#if success}<div class="pt-status pt-success"><Check size={14} /> {success}</div>{/if}
    {#if loading}<div class="pt-status pt-loading"><Loader2 size={14} class="spin" /> {i18n.t.viewer.loading}</div>{/if}

    <!-- Operation panels -->
    {#if activeOp && info}
        <div class="pt-op-panel">
            {#if activeOp === 'extract' || activeOp === 'remove'}
                <p class="pt-op-hint">{i18n.t.pdftools.click_pages_hint} ({selected.size} {i18n.t.pdftools.selected})</p>
                <button class="pt-btn-sm" onclick={selectAll}>{i18n.t.pdftools.select_all}</button>
                <button class="pt-btn-sm" onclick={selectNone}>{i18n.t.pdftools.select_none}</button>
                {#if activeOp === 'extract'}
                    <button class="pt-btn-sm pt-go" onclick={doExtract}>{i18n.t.pdftools.extract_selected}</button>
                {:else}
                    <button class="pt-btn-sm pt-go pt-danger" onclick={doRemove}>{i18n.t.pdftools.remove_selected}</button>
                {/if}
            {:else if activeOp === 'rotate'}
                <label>
                    {i18n.t.pdftools.degrees}:
                    <select bind:value={rotateDeg}><option value={90}>90°</option><option value={180}>180°</option><option value={270}>270°</option></select>
                </label>
                <button class="pt-btn-sm pt-go" onclick={doRotate}>{i18n.t.pdftools.apply}</button>
            {:else if activeOp === 'crop'}
                <label>x,y,w,h (pt): <input type="text" bind:value={cropRect} class="pt-input" /></label>
                <button class="pt-btn-sm pt-go" onclick={doCrop}>{i18n.t.pdftools.apply}</button>
            {:else if activeOp === 'merge'}
                <button class="pt-btn-sm" onclick={addMergeFile}><FilePlus2 size={12} /> {i18n.t.pdftools.add_files}</button>
                {#each mergeFiles as f, i (i)}
                    <span class="pt-merge-file">{f.split(/[/\\]/).pop()} <button onclick={() => mergeFiles = mergeFiles.filter((_, j) => j !== i)}>×</button></span>
                {/each}
                <button class="pt-btn-sm pt-go" onclick={doMerge}>{i18n.t.pdftools.merge_all}</button>
            {:else if activeOp === 'split'}
                <label>{i18n.t.pdftools.ranges}: <input type="text" bind:value={splitSpec} placeholder="1-5,6-10" class="pt-input" /></label>
                <button class="pt-btn-sm pt-go" onclick={doSplit}>{i18n.t.pdftools.split_now}</button>
            {:else if activeOp === 'number'}
                <label>{i18n.t.pdftools.position}: <select bind:value={numPosition}>
                    <option value="bottom-center">Bottom center</option>
                    <option value="bottom-left">Bottom left</option>
                    <option value="bottom-right">Bottom right</option>
                    <option value="top-center">Top center</option>
                    <option value="top-left">Top left</option>
                    <option value="top-right">Top right</option>
                </select></label>
                <label>{i18n.t.pdftools.format_label}: <select bind:value={numFormat}>
                    <option value="arabic">1, 2, 3</option>
                    <option value="roman">i, ii, iii</option>
                    <option value="page-of">Page 1 of N</option>
                </select></label>
                <label>Skip: <input type="number" bind:value={numSkipFirst} min="0" class="pt-input-sm" /></label>
                <button class="pt-btn-sm pt-go" onclick={doPageNumbers}>{i18n.t.pdftools.apply}</button>
            {:else if activeOp === 'watermark'}
                <label>{i18n.t.pdftools.text}: <input type="text" bind:value={wmText} class="pt-input" /></label>
                <label>{i18n.t.pdftools.opacity}: <input type="range" min="0.05" max="1" step="0.05" bind:value={wmOpacity} /> {(wmOpacity * 100).toFixed(0)}%</label>
                <label>Angle: <input type="number" bind:value={wmAngle} class="pt-input-sm" />°</label>
                <button class="pt-btn-sm pt-go" onclick={doWatermark}>{i18n.t.pdftools.apply}</button>
            {:else if activeOp === 'metadata'}
                <label>Title: <input type="text" bind:value={metaTitle} class="pt-input" /></label>
                <label>Author: <input type="text" bind:value={metaAuthor} class="pt-input" /></label>
                <label>Subject: <input type="text" bind:value={metaSubject} class="pt-input" /></label>
                <label>Keywords: <input type="text" bind:value={metaKeywords} class="pt-input" /></label>
                <button class="pt-btn-sm pt-go" onclick={doMetadata}>{i18n.t.pdftools.save}</button>
            {:else if activeOp === 'decrypt'}
                <label>{i18n.t.pdftools.password}: <input type="password" bind:value={decryptPassword} class="pt-input" /></label>
                <button class="pt-btn-sm pt-go" onclick={doDecrypt}><Unlock size={12} /> {i18n.t.pdftools.decrypt}</button>
            {:else if activeOp === 'encrypt'}
                <label>{i18n.t.pdftools.owner_pw}: <input type="password" bind:value={encOwnerPw} class="pt-input" /></label>
                <label>{i18n.t.pdftools.user_pw}: <input type="password" bind:value={encUserPw} class="pt-input" placeholder="(optional)" /></label>
                <label><input type="checkbox" bind:checked={encNoPrint} /> {i18n.t.pdftools.no_print}</label>
                <label><input type="checkbox" bind:checked={encNoCopy} /> {i18n.t.pdftools.no_copy}</label>
                <label><input type="checkbox" bind:checked={encNoModify} /> {i18n.t.pdftools.no_modify}</label>
                <button class="pt-btn-sm pt-go" onclick={doEncrypt}><Lock size={12} /> {i18n.t.pdftools.encrypt}</button>
            {:else if activeOp === 'sanitise'}
                <label><input type="checkbox" bind:checked={sanStripInfo} /> /Info (title, author…)</label>
                <label><input type="checkbox" bind:checked={sanStripXmp} /> XMP metadata</label>
                <label><input type="checkbox" bind:checked={sanStripJs} /> JavaScript</label>
                <label><input type="checkbox" bind:checked={sanStripFiles} /> Embedded files</label>
                <label><input type="checkbox" bind:checked={sanStripOpen} /> OpenAction</label>
                <label><input type="checkbox" bind:checked={sanStripThumbs} /> Thumbnails</label>
                <label><input type="checkbox" bind:checked={sanStripAnnots} /> Annotations</label>
                <button class="pt-btn-sm pt-go" onclick={doSanitise}><Shield size={12} /> {i18n.t.pdftools.apply}</button>
            {/if}
        </div>
    {/if}

    <!-- Main content area -->
    <div class="pt-body">
        {#if !filePath}
            <div class="pt-empty">
                <FileText size={48} strokeWidth={1} />
                <p>{i18n.t.pdftools.drop_hint}</p>
                <button class="pt-btn pt-open" onclick={openFile}>{i18n.t.pdftools.open}</button>
            </div>
        {:else if info}
            <div class="pt-split">
                <!-- Page list sidebar -->
                <div class="pt-page-list">
                    {#each info.pages as pg (pg.page_number)}
                        <button
                            class="pt-page-item"
                            class:selected={selected.has(pg.page_number - 1)}
                            onclick={() => togglePage(pg.page_number - 1)}
                        >
                            <span class="pt-page-num">{pg.page_number}</span>
                            <span class="pt-page-dim">{Math.round(pg.width_pt)}×{Math.round(pg.height_pt)}</span>
                            {#if pg.rotation !== 0}<span class="pt-page-rot">{pg.rotation}°</span>{/if}
                        </button>
                    {/each}
                </div>
                <!-- Viewer -->
                <div class="pt-viewer">
                    <DocumentViewer locationUri={filePath} filename={filePath.split(/[/\\]/).pop() ?? ''} />
                </div>
            </div>
        {/if}
    </div>
</div>

<style>
    .pdf-tools { display: flex; flex-direction: column; height: 100%; background: #09090b; }
    .pt-toolbar {
        display: flex; align-items: center; gap: 4px; padding: 6px 10px;
        background: #18181b; border-bottom: 1px solid #27272a; flex-wrap: wrap; flex-shrink: 0;
    }
    .pt-btn {
        display: inline-flex; align-items: center; gap: 4px; padding: 4px 10px;
        border: 1px solid #3f3f46; border-radius: 5px; background: #27272a; color: #d4d4d8;
        font-size: 0.78rem; cursor: pointer; transition: background 0.12s; white-space: nowrap;
    }
    .pt-btn:hover { background: #3f3f46; }
    .pt-btn.active { background: #1d4ed8; border-color: #2563eb; color: #fff; }
    .pt-btn.pt-open { background: #1d4ed8; border-color: #2563eb; color: #fff; }
    .pt-info-badge { font-size: 0.72rem; color: #a1a1aa; padding: 0 4px; }
    .pt-sep { width: 1px; height: 20px; background: #3f3f46; margin: 0 2px; }

    .pt-status { padding: 4px 12px; font-size: 0.78rem; flex-shrink: 0; }
    .pt-error { background: #450a0a; color: #fca5a5; }
    .pt-success { background: #052e16; color: #86efac; display: flex; align-items: center; gap: 6px; }
    .pt-loading { background: #18181b; color: #a1a1aa; display: flex; align-items: center; gap: 6px; }

    .pt-op-panel {
        display: flex; align-items: center; gap: 8px; padding: 6px 12px;
        background: #1c1917; border-bottom: 1px solid #3f3f46; flex-wrap: wrap; flex-shrink: 0;
    }
    .pt-op-panel label { font-size: 0.75rem; color: #a1a1aa; display: flex; align-items: center; gap: 4px; }
    .pt-op-panel select, .pt-input, .pt-input-sm {
        padding: 3px 6px; background: #18181b; border: 1px solid #3f3f46;
        border-radius: 4px; color: #d4d4d8; font-size: 0.75rem;
    }
    .pt-input { width: 140px; }
    .pt-input-sm { width: 50px; }
    .pt-op-hint { margin: 0; font-size: 0.75rem; color: #71717a; }
    .pt-btn-sm {
        padding: 3px 8px; border: 1px solid #3f3f46; border-radius: 4px;
        background: #27272a; color: #d4d4d8; font-size: 0.72rem; cursor: pointer;
        display: inline-flex; align-items: center; gap: 3px;
    }
    .pt-btn-sm:hover { background: #3f3f46; }
    .pt-btn-sm.pt-go { background: #1d4ed8; border-color: #2563eb; color: #fff; }
    .pt-btn-sm.pt-danger { background: #991b1b; border-color: #b91c1c; }
    .pt-merge-file {
        font-size: 0.72rem; color: #a1a1aa; background: #27272a; padding: 2px 6px;
        border-radius: 4px; display: inline-flex; align-items: center; gap: 4px;
    }
    .pt-merge-file button { background: none; border: none; color: #71717a; cursor: pointer; font-size: 0.8rem; }

    .pt-body { flex: 1; display: flex; min-height: 0; overflow: hidden; }
    .pt-empty {
        flex: 1; display: flex; flex-direction: column; align-items: center;
        justify-content: center; gap: 12px; color: #52525b;
    }
    .pt-empty p { font-size: 0.85rem; margin: 0; }

    .pt-split { display: flex; flex: 1; min-height: 0; }
    .pt-page-list {
        width: 100px; min-width: 80px; overflow-y: auto; background: #18181b;
        border-right: 1px solid #27272a; padding: 4px;
    }
    .pt-page-item {
        display: flex; flex-direction: column; align-items: center; gap: 2px;
        width: 100%; padding: 6px 4px; margin-bottom: 3px; border: 1px solid transparent;
        border-radius: 5px; background: #09090b; color: #a1a1aa; cursor: pointer;
        font-size: 0.68rem; transition: all 0.1s;
    }
    .pt-page-item:hover { background: #27272a; border-color: #3f3f46; }
    .pt-page-item.selected { background: #1e3a5f; border-color: #2563eb; color: #93c5fd; }
    .pt-page-num { font-weight: 600; font-size: 0.82rem; }
    .pt-page-dim { color: #52525b; }
    .pt-page-rot { color: #f59e0b; font-size: 0.62rem; }
    .pt-viewer { flex: 1; display: flex; min-width: 0; }

    :global(.spin) { animation: pt-spin 1s linear infinite; }
    @keyframes pt-spin { to { transform: rotate(360deg); } }
</style>
