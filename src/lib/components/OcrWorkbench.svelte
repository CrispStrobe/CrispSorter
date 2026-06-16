<script lang="ts">
    // OCR Workbench — interactive, user-steered OCR + proofreading.
    // Open a doc → page through → run OCR → see image + recognized text,
    // low-confidence regions highlighted → fix inline → save/export.
    import { invoke, convertFileSrc } from '@tauri-apps/api/core';
    import { open } from '@tauri-apps/plugin-dialog';
    import { i18n } from '$lib/i18n.svelte';

    interface Region { text: string; x: number; y: number; w: number; h: number; confidence: number; char_conf: number[]; orig: string; }
    interface PageOcr { width: number; height: number; regions: Region[]; ocred: boolean; proofread: boolean; cleanedPath?: string | null; }

    let locationUri = $state('');          // source doc path (sidecar/export/re-ingest target)
    let pages = $state<string[]>([]);       // per-page image paths
    let pageIdx = $state(0);
    let ocr = $state<Map<number, PageOcr>>(new Map());
    let showCleaned = $state(false);
    let threshold = $state(0.5);
    let selected = $state<number | null>(null);   // selected region index on the current page
    let zoom = $state(1);
    let busy = $state<string | null>(null);        // 'open' | 'ocr' | 'ocr-all' | 'clean' | 'save'
    let status = $state('');
    let error = $state('');
    // Save options
    let exportFormat = $state('pdf');              // pdf | hocr | alto | text
    let exportPdfa = $state(false);

    const cur = $derived(ocr.get(pageIdx));
    const curImg = $derived.by(() => {
        const p = pages[pageIdx];
        if (!p) return '';
        if (showCleaned && cur?.cleanedPath) return convertFileSrc(cur.cleanedPath);
        return convertFileSrc(p);
    });
    const lowConfCount = $derived(cur ? cur.regions.filter(r => r.confidence < threshold).length : 0);

    function setPageOcr(idx: number, patch: Partial<PageOcr>) {
        const next = new Map(ocr);
        const base: PageOcr = next.get(idx) ?? { width: 0, height: 0, regions: [], ocred: false, proofread: false };
        next.set(idx, { ...base, ...patch });
        ocr = next;
    }

    async function openDoc() {
        error = '';
        const picked = await open({
            multiple: false,
            filters: [{ name: 'Documents', extensions: ['pdf', 'png', 'jpg', 'jpeg', 'tif', 'tiff', 'bmp', 'webp'] }],
        });
        if (!picked || typeof picked !== 'string') return;
        busy = 'open'; status = i18n.t.ocrwb.opening;
        try {
            const res = await invoke<{ count: number; pages: string[] }>('ocr_doc_open', { locationUri: picked });
            locationUri = picked;
            pages = res.pages;
            pageIdx = 0;
            ocr = new Map();
            selected = null;
            zoom = 1;
            status = `${res.count} ${i18n.t.ocrwb.pages}`;
        } catch (e: any) {
            error = String(e?.message ?? e);
        } finally {
            busy = null;
        }
    }

    async function runOcr(idx: number) {
        const pagePath = pages[idx];
        if (!pagePath) return;
        busy = 'ocr'; status = i18n.t.ocrwb.running; error = '';
        try {
            const res = await invoke<{ width: number; height: number; regions: any[] }>('ocr_page_regions', { pagePath });
            const regions: Region[] = res.regions.map(r => ({ ...r, char_conf: r.char_conf ?? [], orig: r.text }));
            setPageOcr(idx, { width: res.width, height: res.height, regions, ocred: true });
            selected = null;
            status = `${regions.length} ${i18n.t.ocrwb.regions}`;
        } catch (e: any) {
            error = String(e?.message ?? e);
        } finally {
            busy = null;
        }
    }

    async function runOcrAll() {
        busy = 'ocr-all'; error = '';
        try {
            for (let i = 0; i < pages.length; i++) {
                status = `${i18n.t.ocrwb.running} ${i + 1}/${pages.length}`;
                const res = await invoke<{ width: number; height: number; regions: any[] }>('ocr_page_regions', { pagePath: pages[i] });
                const regions: Region[] = res.regions.map(r => ({ ...r, char_conf: r.char_conf ?? [], orig: r.text }));
                setPageOcr(i, { width: res.width, height: res.height, regions, ocred: true });
            }
            status = i18n.t.ocrwb.done;
        } catch (e: any) {
            error = String(e?.message ?? e);
        } finally {
            busy = null;
        }
    }

    async function loadCleaned() {
        const pagePath = pages[pageIdx];
        if (!pagePath || cur?.cleanedPath !== undefined) { showCleaned = !showCleaned; return; }
        busy = 'clean'; error = '';
        try {
            const path = await invoke<string>('ocr_page_cleaned', { pagePath });
            setPageOcr(pageIdx, { cleanedPath: path || null });
            showCleaned = !!path;
            if (!path) status = i18n.t.ocrwb.no_cleaned;
        } catch (e: any) {
            error = String(e?.message ?? e);
        } finally {
            busy = null;
        }
    }

    function editRegion(i: number, text: string) {
        if (!cur) return;
        const regions = cur.regions.map((r, j) => (j === i ? { ...r, text } : r));
        setPageOcr(pageIdx, { regions });
    }

    function toggleProofread() {
        if (cur) setPageOcr(pageIdx, { proofread: !cur.proofread });
    }

    function goto(i: number) {
        if (i < 0 || i >= pages.length) return;
        pageIdx = i;
        selected = null;
        zoom = 1;
        showCleaned = showCleaned && !!ocr.get(i)?.cleanedPath;
    }

    /** Pages that have OCR results, as the backend export payload. */
    function exportPayload() {
        const out: any[] = [];
        for (let i = 0; i < pages.length; i++) {
            const p = ocr.get(i);
            if (p?.ocred) {
                out.push({ image_path: pages[i], width: p.width, height: p.height, regions: p.regions });
            }
        }
        return out;
    }
    /** Joined corrected text across OCR'd pages (for sidecar / re-ingest). */
    function joinedText() {
        const parts: string[] = [];
        for (let i = 0; i < pages.length; i++) {
            const p = ocr.get(i);
            if (p?.ocred) parts.push(p.regions.map(r => r.text).join('\n'));
        }
        return parts.join('\n\n');
    }

    async function doExport() {
        const payload = exportPayload();
        if (!payload.length) { error = i18n.t.ocrwb.run_first; return; }
        busy = 'save'; error = '';
        try {
            const res = await invoke<{ saved_path: string; pages: number }>('ocr_workbench_export', {
                locationUri, format: exportFormat, pdfa: exportPdfa, pages: payload,
            });
            status = `${i18n.t.ocrwb.saved}: ${res.saved_path}`;
        } catch (e: any) { error = String(e?.message ?? e); } finally { busy = null; }
    }
    async function doSidecar() {
        busy = 'save'; error = '';
        try {
            const path = await invoke<string>('ocr_workbench_sidecar', { locationUri, text: joinedText() });
            status = `${i18n.t.ocrwb.saved}: ${path}`;
        } catch (e: any) { error = String(e?.message ?? e); } finally { busy = null; }
    }
    async function doReingest() {
        busy = 'save'; error = '';
        try {
            await invoke('ocr_workbench_reingest', { locationUri, text: joinedText() });
            status = i18n.t.ocrwb.reingested;
        } catch (e: any) { error = String(e?.message ?? e); } finally { busy = null; }
    }
</script>

<div class="ocrwb">
    <!-- Toolbar -->
    <div class="toolbar">
        <button class="btn" onclick={openDoc} disabled={busy !== null}>{i18n.t.ocrwb.open}</button>
        <span class="sep"></span>
        <button class="btn" onclick={() => runOcr(pageIdx)} disabled={busy !== null || !pages.length}>{i18n.t.ocrwb.run_page}</button>
        <button class="btn" onclick={runOcrAll} disabled={busy !== null || pages.length < 2}>{i18n.t.ocrwb.run_all}</button>
        <span class="sep"></span>
        <button class="btn" class:active={showCleaned} onclick={loadCleaned} disabled={busy !== null || !pages.length}>{i18n.t.ocrwb.cleaned}</button>
        <button class="btn" onclick={() => zoom = Math.min(zoom + 0.25, 6)} disabled={!pages.length}>＋</button>
        <button class="btn" onclick={() => zoom = Math.max(zoom - 0.25, 0.25)} disabled={!pages.length}>－</button>
        <button class="btn" onclick={() => zoom = 1} disabled={!pages.length}>1:1</button>
        <span class="sep"></span>
        <label class="thr">
            {i18n.t.ocrwb.threshold} ({threshold.toFixed(2)})
            <input type="range" min="0" max="1" step="0.05" bind:value={threshold} />
        </label>
        {#if cur?.ocred}<span class="muted">{lowConfCount} {i18n.t.ocrwb.lowconf}</span>{/if}
        <span class="grow"></span>
        <!-- Save controls -->
        <select bind:value={exportFormat} disabled={busy !== null} title={i18n.t.ocrwb.format}>
            <option value="pdf">Searchable PDF</option>
            <option value="hocr">hOCR</option>
            <option value="alto">ALTO</option>
            <option value="text">Text</option>
        </select>
        {#if exportFormat === 'pdf'}
            <label class="chk"><input type="checkbox" bind:checked={exportPdfa} /> PDF/A</label>
        {/if}
        <button class="btn" onclick={doExport} disabled={busy !== null || !pages.length}>{i18n.t.ocrwb.export}</button>
        <button class="btn" onclick={doSidecar} disabled={busy !== null || !pages.length} title={i18n.t.ocrwb.sidecar_hint}>{i18n.t.ocrwb.sidecar}</button>
        <button class="btn" onclick={doReingest} disabled={busy !== null || !pages.length} title={i18n.t.ocrwb.reingest_hint}>{i18n.t.ocrwb.reingest}</button>
    </div>

    {#if error}<div class="bar err">{error}</div>{/if}
    {#if status && !error}<div class="bar info">{status}</div>{/if}

    {#if !pages.length}
        <div class="empty">
            <p>{i18n.t.ocrwb.empty_title}</p>
            <p class="muted">{i18n.t.ocrwb.empty_hint}</p>
            <button class="btn big" onclick={openDoc}>{i18n.t.ocrwb.open}</button>
        </div>
    {:else}
        <div class="split">
            <!-- Image pane -->
            <div class="image-pane">
                <div class="canvas-scroll">
                    <div class="canvas" style="transform: scale({zoom}); transform-origin: top left;">
                        {#if curImg}
                            <img src={curImg} alt="page {pageIdx + 1}" draggable="false" />
                        {/if}
                        {#if cur?.ocred && cur.width > 0}
                            <svg viewBox="0 0 {cur.width} {cur.height}" preserveAspectRatio="none" class="overlay">
                                {#each cur.regions as r, i}
                                    <rect x={r.x} y={r.y} width={r.w} height={r.h}
                                        class="rgn"
                                        class:low={r.confidence < threshold}
                                        class:sel={selected === i}
                                        onclick={() => selected = i}
                                        onkeydown={(e) => { if (e.key === 'Enter') selected = i; }}
                                        role="button" tabindex="-1" />
                                {/each}
                            </svg>
                        {/if}
                    </div>
                </div>
            </div>

            <!-- Text pane -->
            <div class="text-pane">
                {#if !cur?.ocred}
                    <div class="empty small">
                        <p class="muted">{i18n.t.ocrwb.not_ocred}</p>
                        <button class="btn" onclick={() => runOcr(pageIdx)} disabled={busy !== null}>{i18n.t.ocrwb.run_page}</button>
                    </div>
                {:else}
                    {#each cur.regions as r, i}
                        <div class="region-row" class:low={r.confidence < threshold} class:sel={selected === i}>
                            <button class="conf" title={i18n.t.ocrwb.confidence} onclick={() => selected = i}>{(r.confidence * 100).toFixed(0)}</button>
                            <div class="region-body">
                                {#if r.char_conf.length}
                                    <!-- Per-character confidence: tint chars the recognizer was unsure
                                         about (aligned to the original OCR text, a "where to look" guide). -->
                                    <div class="charview" title={i18n.t.ocrwb.charconf_hint}>
                                        {#each [...r.orig] as ch, k}
                                            <span class:low={(r.char_conf[k] ?? 1) < threshold}>{ch === ' ' ? ' ' : ch}</span>
                                        {/each}
                                    </div>
                                {/if}
                                <textarea
                                    value={r.text}
                                    oninput={(e) => editRegion(i, (e.target as HTMLTextAreaElement).value)}
                                    onfocus={() => selected = i}
                                    rows="1"></textarea>
                            </div>
                        </div>
                    {/each}
                {/if}
            </div>
        </div>

        <!-- Page bar -->
        <div class="pagebar">
            <button class="btn" onclick={() => goto(pageIdx - 1)} disabled={pageIdx === 0}>‹ {i18n.t.ocrwb.prev}</button>
            <span class="pageno">{i18n.t.ocrwb.page} {pageIdx + 1} / {pages.length}</span>
            <button class="btn" onclick={() => goto(pageIdx + 1)} disabled={pageIdx >= pages.length - 1}>{i18n.t.ocrwb.next} ›</button>
            <span class="grow"></span>
            {#if cur?.ocred}
                <label class="chk"><input type="checkbox" checked={cur.proofread} onchange={toggleProofread} /> {i18n.t.ocrwb.proofread}</label>
            {/if}
        </div>
    {/if}
</div>

<style>
    .ocrwb { display: flex; flex-direction: column; height: 100%; min-height: 0; }
    .toolbar { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; padding: 8px; border-bottom: 1px solid #27272a; }
    .sep { width: 1px; height: 20px; background: #27272a; margin: 0 2px; }
    .grow { flex: 1; }
    .btn { background: #1f1f23; color: #e4e4e7; border: 1px solid #3f3f46; border-radius: 6px; padding: 5px 10px; font-size: 0.8125rem; cursor: pointer; }
    .btn:hover:not(:disabled) { background: #2a2a30; }
    .btn:disabled { opacity: 0.5; cursor: default; }
    .btn.active { background: #1d4ed8; border-color: #1d4ed8; }
    .btn.big { padding: 8px 18px; font-size: 0.9rem; margin-top: 10px; }
    .thr { font-size: 0.75rem; color: #a1a1aa; display: flex; align-items: center; gap: 6px; white-space: nowrap; }
    .chk { font-size: 0.75rem; color: #a1a1aa; display: flex; align-items: center; gap: 4px; }
    select { background: #1f1f23; color: #e4e4e7; border: 1px solid #3f3f46; border-radius: 6px; padding: 4px; font-size: 0.8125rem; }
    .muted { color: #71717a; font-size: 0.8125rem; }
    .bar { padding: 6px 10px; font-size: 0.8125rem; }
    .bar.err { background: #3f1d1d; color: #fca5a5; }
    .bar.info { background: #1c2333; color: #93c5fd; }
    .empty { display: flex; flex-direction: column; align-items: center; justify-content: center; flex: 1; gap: 4px; text-align: center; }
    .empty.small { padding: 20px; flex: none; }
    .split { display: flex; flex: 1; min-height: 0; }
    .image-pane { flex: 1; min-width: 0; border-right: 1px solid #27272a; background: #0a0a0a; overflow: hidden; }
    .canvas-scroll { width: 100%; height: 100%; overflow: auto; }
    .canvas { position: relative; display: inline-block; }
    .canvas img { display: block; max-width: none; user-select: none; }
    .overlay { position: absolute; top: 0; left: 0; width: 100%; height: 100%; }
    .rgn { fill: rgba(59,130,246,0.08); stroke: rgba(59,130,246,0.6); stroke-width: 1; cursor: pointer; vector-effect: non-scaling-stroke; }
    .rgn.low { fill: rgba(239,68,68,0.16); stroke: rgba(239,68,68,0.85); }
    .rgn.sel { fill: rgba(234,179,8,0.22); stroke: rgba(234,179,8,1); stroke-width: 2; }
    .text-pane { width: 42%; min-width: 280px; overflow: auto; padding: 8px; display: flex; flex-direction: column; gap: 4px; }
    .region-row { display: flex; gap: 6px; align-items: flex-start; border-radius: 6px; padding: 2px; }
    .region-row.low { background: rgba(239,68,68,0.08); }
    .region-row.sel { background: rgba(234,179,8,0.12); outline: 1px solid rgba(234,179,8,0.5); }
    .conf { flex: none; width: 30px; text-align: center; font-size: 0.7rem; color: #a1a1aa; background: #1f1f23; border: 1px solid #3f3f46; border-radius: 4px; cursor: pointer; padding: 2px 0; height: fit-content; }
    .region-row.low .conf { color: #fca5a5; border-color: #7f1d1d; }
    .region-body { flex: 1; display: flex; flex-direction: column; gap: 2px; min-width: 0; }
    .charview { font-family: ui-monospace, monospace; font-size: 0.8rem; color: #71717a; white-space: pre-wrap; word-break: break-word; padding: 1px 4px; }
    .charview span.low { background: rgba(239,68,68,0.35); color: #fecaca; border-radius: 2px; }
    .region-body textarea { resize: vertical; background: #18181b; color: #e4e4e7; border: 1px solid #27272a; border-radius: 4px; padding: 4px 6px; font-size: 0.85rem; font-family: inherit; min-height: 1.6em; width: 100%; box-sizing: border-box; }
    .pagebar { display: flex; align-items: center; gap: 8px; padding: 8px; border-top: 1px solid #27272a; }
    .pageno { font-size: 0.8125rem; color: #a1a1aa; }
</style>
