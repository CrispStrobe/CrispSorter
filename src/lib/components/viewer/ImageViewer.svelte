<script lang="ts">
    import { convertFileSrc } from '@tauri-apps/api/core';
    import { ZoomIn, ZoomOut, Maximize2, Minimize2 } from 'lucide-svelte';
    import { i18n } from '$lib/i18n.svelte';

    let { path = '' }: { path: string } = $props();

    let zoom = $state(1);
    let fitMode = $state<'contain' | 'custom'>('contain');
    let error = $state(false);
    let scrollEl: HTMLDivElement;

    let src = $derived(path ? convertFileSrc(path) : '');

    function zoomIn() {
        fitMode = 'custom';
        zoom = Math.min(zoom * 1.25, 6);
    }
    function zoomOut() {
        fitMode = 'custom';
        zoom = Math.max(zoom / 1.25, 0.1);
    }
    function fitToggle() {
        if (fitMode === 'contain') {
            fitMode = 'custom';
            zoom = 1;
        } else {
            fitMode = 'contain';
            zoom = 1;
        }
    }
    function onWheel(e: WheelEvent) {
        if (!e.ctrlKey && !e.metaKey) return;
        e.preventDefault();
        if (e.deltaY < 0) zoomIn(); else zoomOut();
    }

    // Reset on path change
    $effect(() => {
        if (path) {
            zoom = 1;
            fitMode = 'contain';
            error = false;
        }
    });
</script>

<div class="image-viewer">
    <div class="iv-toolbar">
        <button onclick={zoomOut} title={i18n.t.viewer.zoom_out}><ZoomOut size={14} /></button>
        <span class="iv-zoom">{Math.round(zoom * 100)}%</span>
        <button onclick={zoomIn} title={i18n.t.viewer.zoom_in}><ZoomIn size={14} /></button>
        <button onclick={fitToggle} title={fitMode === 'contain' ? i18n.t.viewer.actual_size : i18n.t.viewer.fit_page}>
            {#if fitMode === 'contain'}<Maximize2 size={14} />{:else}<Minimize2 size={14} />{/if}
        </button>
    </div>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="iv-scroll" bind:this={scrollEl} onwheel={onWheel}>
        {#if error}
            <p class="iv-msg iv-error">{i18n.t.viewer.error}</p>
        {:else if src}
            {#if fitMode === 'contain'}
                <img {src} alt="" class="iv-img-contain" onerror={() => error = true} />
            {:else}
                <div class="iv-canvas" style="transform: scale({zoom}); transform-origin: top left;">
                    <img {src} alt="" class="iv-img-raw" onerror={() => error = true} />
                </div>
            {/if}
        {/if}
    </div>
</div>

<style>
    .image-viewer { display: flex; flex-direction: column; flex: 1; min-height: 0; }
    .iv-toolbar {
        display: flex;
        align-items: center;
        gap: 4px;
        padding: 4px 8px;
        background: #27272a;
        border-bottom: 1px solid #3f3f46;
        flex-shrink: 0;
    }
    .iv-toolbar button {
        display: inline-flex;
        align-items: center;
        padding: 3px 6px;
        border: 1px solid #3f3f46;
        border-radius: 4px;
        background: #18181b;
        color: #d4d4d8;
        cursor: pointer;
        transition: background 0.12s;
    }
    .iv-toolbar button:hover { background: #3f3f46; }
    .iv-zoom {
        font-size: 0.72rem;
        color: #a1a1aa;
        min-width: 36px;
        text-align: center;
        font-variant-numeric: tabular-nums;
    }
    .iv-scroll {
        flex: 1;
        overflow: auto;
        display: flex;
        align-items: center;
        justify-content: center;
        background: #0a0a0c;
        min-height: 0;
    }
    .iv-img-contain {
        max-width: 100%;
        max-height: 100%;
        object-fit: contain;
        display: block;
    }
    .iv-canvas { display: inline-block; }
    .iv-img-raw {
        display: block;
        max-width: none;
        user-select: none;
    }
    .iv-msg { padding: 24px; text-align: center; color: #71717a; font-size: 0.85rem; margin: 0; }
    .iv-error { color: #f87171; }
</style>
