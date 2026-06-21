<script lang="ts">
    import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf';
    import { renderTextLayer } from 'pdfjs-dist/legacy/build/pdf';
    import { readFile } from '@tauri-apps/plugin-fs';
    import {
        ChevronLeft, ChevronRight, ZoomIn, ZoomOut,
        ChevronsLeft, ChevronsRight, Maximize2,
    } from 'lucide-svelte';
    import { i18n } from '$lib/i18n.svelte';

    // Ensure worker is configured (idempotent — same value as pdfExtractor.ts)
    pdfjsLib.GlobalWorkerOptions.workerSrc = '/pdf.worker.js';

    let { path = '' }: { path: string } = $props();

    let pdfDoc = $state<any>(null);
    let currentPage = $state(1);
    let totalPages = $state(0);
    let zoom = $state(1.0);
    let fitMode = $state<'width' | 'page' | 'custom'>('width');
    let loading = $state(true);
    let error = $state('');
    let rendering = $state(false);

    let canvasEl: HTMLCanvasElement;
    let textLayerEl: HTMLDivElement;
    let containerEl: HTMLDivElement;
    let pageInputEl: HTMLInputElement;

    // Load PDF when path changes
    $effect(() => {
        const p = path;
        if (!p) return;

        loading = true;
        error = '';
        currentPage = 1;
        totalPages = 0;
        zoom = 1.0;
        fitMode = 'width';

        let cancelled = false;
        let doc: any = null;

        (async () => {
            try {
                const bytes = await readFile(p);
                if (cancelled) return;
                const data = new Uint8Array(bytes);
                const loadingTask = pdfjsLib.getDocument({
                    data,
                    useSystemFonts: true,
                    disableFontFace: true,
                });
                doc = await loadingTask.promise;
                if (cancelled) { doc.destroy(); return; }
                pdfDoc = doc;
                totalPages = doc.numPages;
                loading = false;
            } catch (e: any) {
                if (!cancelled) {
                    error = e.message ?? String(e);
                    loading = false;
                }
            }
        })();

        return () => {
            cancelled = true;
            if (doc) { doc.destroy(); doc = null; }
            pdfDoc = null;
        };
    });

    // Render current page when page/zoom/fitMode changes
    $effect(() => {
        const doc = pdfDoc;
        const page = currentPage;
        const z = zoom;
        const fit = fitMode;
        if (!doc || !canvasEl || !containerEl) return;

        rendering = true;
        let cancelled = false;

        (async () => {
            try {
                const pg = await doc.getPage(page);
                if (cancelled) return;

                const baseViewport = pg.getViewport({ scale: 1 });
                let scale: number;
                if (fit === 'width') {
                    scale = (containerEl.clientWidth - 2) / baseViewport.width;
                } else if (fit === 'page') {
                    scale = Math.min(
                        (containerEl.clientWidth - 2) / baseViewport.width,
                        (containerEl.clientHeight - 2) / baseViewport.height,
                    );
                } else {
                    scale = z;
                }
                // Clamp
                scale = Math.max(0.1, Math.min(scale, 8));

                const viewport = pg.getViewport({ scale });
                const ctx = canvasEl.getContext('2d')!;
                canvasEl.width = viewport.width;
                canvasEl.height = viewport.height;

                const renderTask = pg.render({ canvasContext: ctx, viewport });
                await renderTask.promise;
                if (cancelled) return;

                // Text layer for selection
                if (textLayerEl) {
                    textLayerEl.innerHTML = '';
                    textLayerEl.style.width = `${viewport.width}px`;
                    textLayerEl.style.height = `${viewport.height}px`;
                    try {
                        const textContent = await pg.getTextContent();
                        if (!cancelled) {
                            const task = renderTextLayer({
                                textContentSource: textContent,
                                container: textLayerEl,
                                viewport,
                                textDivs: [],
                            });
                            await task.promise;
                        }
                    } catch {
                        // Text layer is best-effort
                    }
                }

                rendering = false;
            } catch (e: any) {
                if (!cancelled) {
                    error = e.message ?? String(e);
                    rendering = false;
                }
            }
        })();

        return () => { cancelled = true; };
    });

    function prevPage() { if (currentPage > 1) currentPage--; }
    function nextPage() { if (currentPage < totalPages) currentPage++; }
    function firstPage() { currentPage = 1; }
    function lastPage() { currentPage = totalPages; }

    function zoomIn() { fitMode = 'custom'; zoom = Math.min(zoom * 1.25, 8); }
    function zoomOut() { fitMode = 'custom'; zoom = Math.max(zoom / 1.25, 0.1); }
    function fitWidth() { fitMode = 'width'; }
    function fitPage() { fitMode = 'page'; }

    function onPageInput(e: Event) {
        const v = parseInt((e.target as HTMLInputElement).value);
        if (v >= 1 && v <= totalPages) currentPage = v;
    }
    function onPageKeydown(e: KeyboardEvent) {
        if (e.key === 'Enter') onPageInput(e);
    }

    function onWheel(e: WheelEvent) {
        if (!e.ctrlKey && !e.metaKey) return;
        e.preventDefault();
        if (e.deltaY < 0) zoomIn(); else zoomOut();
    }

    // Keyboard shortcuts
    function onKeydown(e: KeyboardEvent) {
        if (e.target instanceof HTMLInputElement) return;
        switch (e.key) {
            case 'ArrowLeft': case 'PageUp': prevPage(); e.preventDefault(); break;
            case 'ArrowRight': case 'PageDown': nextPage(); e.preventDefault(); break;
            case 'Home': firstPage(); e.preventDefault(); break;
            case 'End': lastPage(); e.preventDefault(); break;
            case '+': case '=': zoomIn(); e.preventDefault(); break;
            case '-': zoomOut(); e.preventDefault(); break;
        }
    }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="pdf-viewer" onkeydown={onKeydown} tabindex="-1">
    {#if loading}
        <p class="pv-msg">{i18n.t.viewer.loading}</p>
    {:else if error}
        <p class="pv-msg pv-error">{error}</p>
    {:else}
        <div class="pv-toolbar">
            <button onclick={firstPage} disabled={currentPage <= 1} title={i18n.t.viewer.first_page}>
                <ChevronsLeft size={14} />
            </button>
            <button onclick={prevPage} disabled={currentPage <= 1} title={i18n.t.viewer.prev}>
                <ChevronLeft size={14} />
            </button>
            <input
                bind:this={pageInputEl}
                type="number"
                class="pv-page-input"
                value={currentPage}
                min="1"
                max={totalPages}
                onchange={onPageInput}
                onkeydown={onPageKeydown}
            />
            <span class="pv-page-total">/ {totalPages}</span>
            <button onclick={nextPage} disabled={currentPage >= totalPages} title={i18n.t.viewer.next}>
                <ChevronRight size={14} />
            </button>
            <button onclick={lastPage} disabled={currentPage >= totalPages} title={i18n.t.viewer.last_page}>
                <ChevronsRight size={14} />
            </button>
            <span class="pv-sep"></span>
            <button onclick={zoomOut} title={i18n.t.viewer.zoom_out}><ZoomOut size={14} /></button>
            <span class="pv-zoom">{fitMode === 'custom' ? Math.round(zoom * 100) + '%' : fitMode === 'width' ? 'W' : 'P'}</span>
            <button onclick={zoomIn} title={i18n.t.viewer.zoom_in}><ZoomIn size={14} /></button>
            <button onclick={fitWidth} class:active={fitMode === 'width'} title={i18n.t.viewer.fit_width}>W</button>
            <button onclick={fitPage} class:active={fitMode === 'page'} title={i18n.t.viewer.fit_page}>
                <Maximize2 size={14} />
            </button>
        </div>
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="pv-scroll" bind:this={containerEl} onwheel={onWheel}>
            <div class="pv-canvas-wrap">
                <canvas bind:this={canvasEl}></canvas>
                <div bind:this={textLayerEl} class="pv-text-layer"></div>
            </div>
        </div>
    {/if}
</div>

<style>
    .pdf-viewer {
        display: flex;
        flex-direction: column;
        flex: 1;
        min-height: 0;
        outline: none;
    }
    .pv-toolbar {
        display: flex;
        align-items: center;
        gap: 2px;
        padding: 4px 8px;
        background: #27272a;
        border-bottom: 1px solid #3f3f46;
        flex-shrink: 0;
        flex-wrap: wrap;
    }
    .pv-toolbar button {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        min-width: 26px;
        height: 26px;
        padding: 0 4px;
        border: 1px solid #3f3f46;
        border-radius: 4px;
        background: #18181b;
        color: #d4d4d8;
        font-size: 0.72rem;
        cursor: pointer;
        transition: background 0.12s;
    }
    .pv-toolbar button:hover:not(:disabled) { background: #3f3f46; }
    .pv-toolbar button:disabled { opacity: 0.35; cursor: default; }
    .pv-toolbar button.active { background: #1d4ed8; border-color: #2563eb; color: #fff; }
    .pv-page-input {
        width: 40px;
        height: 24px;
        padding: 0 4px;
        border: 1px solid #3f3f46;
        border-radius: 4px;
        background: #18181b;
        color: #d4d4d8;
        font-size: 0.75rem;
        text-align: center;
        font-variant-numeric: tabular-nums;
        -moz-appearance: textfield;
    }
    .pv-page-input::-webkit-inner-spin-button,
    .pv-page-input::-webkit-outer-spin-button { -webkit-appearance: none; margin: 0; }
    .pv-page-total { font-size: 0.72rem; color: #a1a1aa; margin: 0 2px; }
    .pv-zoom {
        font-size: 0.72rem;
        color: #a1a1aa;
        min-width: 30px;
        text-align: center;
        font-variant-numeric: tabular-nums;
    }
    .pv-sep { width: 1px; height: 18px; background: #3f3f46; margin: 0 4px; }

    .pv-scroll {
        flex: 1;
        overflow: auto;
        background: #0a0a0c;
        display: flex;
        justify-content: center;
        padding: 8px;
        min-height: 0;
    }
    .pv-canvas-wrap {
        position: relative;
        display: inline-block;
        line-height: 0;
        box-shadow: 0 2px 8px rgba(0,0,0,0.5);
    }
    .pv-canvas-wrap canvas { display: block; }

    /* pdfjs text layer — positioned over canvas for text selection */
    .pv-text-layer {
        position: absolute;
        top: 0;
        left: 0;
        overflow: hidden;
        opacity: 0.3;
        line-height: 1;
    }
    .pv-text-layer :global(span) {
        position: absolute;
        white-space: pre;
        color: transparent;
        pointer-events: all;
    }
    .pv-text-layer :global(span::selection) {
        background: rgba(59, 130, 246, 0.4);
        color: transparent;
    }
    .pv-text-layer :global(span::-moz-selection) {
        background: rgba(59, 130, 246, 0.4);
        color: transparent;
    }

    .pv-msg {
        padding: 24px 16px;
        text-align: center;
        color: #71717a;
        font-size: 0.85rem;
        margin: 0;
    }
    .pv-error { color: #f87171; }
</style>
