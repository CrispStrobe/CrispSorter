<script lang="ts">
    import { initEpubFile } from '@lingo-reader/epub-parser';
    import { readFile } from '@tauri-apps/plugin-fs';
    import { ChevronLeft, ChevronRight, List } from 'lucide-svelte';
    import { i18n } from '$lib/i18n.svelte';

    let { path = '', filename = 'book.epub' }: { path: string; filename?: string } = $props();

    interface SpineItem { id: string; title?: string; }

    let spine = $state<SpineItem[]>([]);
    let chapterIdx = $state(0);
    let chapterHtml = $state('');
    let metadata = $state<any>(null);
    let loading = $state(true);
    let error = $state('');
    let tocOpen = $state(false);

    let epubRef: any = null;

    $effect(() => {
        const p = path;
        const fn = filename;
        if (!p) return;
        loading = true;
        error = '';
        spine = [];
        chapterIdx = 0;
        chapterHtml = '';
        metadata = null;
        tocOpen = false;
        let cancelled = false;

        (async () => {
            try {
                const bytes = await readFile(p);
                if (cancelled) return;
                const file = new File([bytes], fn, { type: 'application/epub+zip' });
                const epub = await initEpubFile(file);
                if (cancelled) { if ((epub as any).destroy) (epub as any).destroy(); return; }
                epubRef = epub;
                const sp = epub.getSpine();
                spine = sp.map((s: any) => ({ id: s.id, title: s.title || s.id }));
                metadata = epub.getMetadata();
                if (spine.length > 0) {
                    await loadChapter(0);
                }
                loading = false;
            } catch (e: any) {
                if (!cancelled) { error = e.message ?? String(e); loading = false; }
            }
        })();

        return () => {
            cancelled = true;
            if (epubRef && (epubRef as any).destroy) (epubRef as any).destroy();
            epubRef = null;
        };
    });

    async function loadChapter(idx: number) {
        if (!epubRef || idx < 0 || idx >= spine.length) return;
        chapterIdx = idx;
        try {
            const { html } = await epubRef.loadChapter(spine[idx].id);
            chapterHtml = html ?? '';
        } catch {
            chapterHtml = '<p style="color:#f87171">Failed to load chapter.</p>';
        }
        tocOpen = false;
    }

    function prevChapter() { if (chapterIdx > 0) loadChapter(chapterIdx - 1); }
    function nextChapter() { if (chapterIdx < spine.length - 1) loadChapter(chapterIdx + 1); }
</script>

<div class="epub-viewer">
    {#if loading}
        <p class="ev-msg">{i18n.t.viewer.loading}</p>
    {:else if error}
        <p class="ev-msg ev-error">{error}</p>
    {:else}
        <div class="ev-toolbar">
            <button onclick={() => tocOpen = !tocOpen} class:active={tocOpen} title={i18n.t.viewer.chapters}>
                <List size={14} />
            </button>
            <button onclick={prevChapter} disabled={chapterIdx <= 0} title={i18n.t.viewer.prev}>
                <ChevronLeft size={14} />
            </button>
            <span class="ev-pos">{chapterIdx + 1} / {spine.length}</span>
            <button onclick={nextChapter} disabled={chapterIdx >= spine.length - 1} title={i18n.t.viewer.next}>
                <ChevronRight size={14} />
            </button>
            {#if metadata?.title}
                <span class="ev-title">{metadata.title}</span>
            {/if}
        </div>
        <div class="ev-body">
            {#if tocOpen}
                <div class="ev-toc">
                    {#each spine as s, i (s.id)}
                        <button
                            class="ev-toc-item"
                            class:active={i === chapterIdx}
                            onclick={() => loadChapter(i)}
                        >
                            {s.title}
                        </button>
                    {/each}
                </div>
            {/if}
            <div class="ev-content">
                {@html chapterHtml}
            </div>
        </div>
    {/if}
</div>

<style>
    .epub-viewer { display: flex; flex-direction: column; flex: 1; min-height: 0; }
    .ev-toolbar {
        display: flex;
        align-items: center;
        gap: 4px;
        padding: 4px 8px;
        background: #27272a;
        border-bottom: 1px solid #3f3f46;
        flex-shrink: 0;
    }
    .ev-toolbar button {
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
    .ev-toolbar button:hover:not(:disabled) { background: #3f3f46; }
    .ev-toolbar button:disabled { opacity: 0.35; cursor: default; }
    .ev-toolbar button.active { background: #1d4ed8; border-color: #2563eb; color: #fff; }
    .ev-pos { font-size: 0.72rem; color: #a1a1aa; min-width: 40px; text-align: center; }
    .ev-title {
        font-size: 0.72rem;
        color: #71717a;
        margin-left: 8px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }

    .ev-body { display: flex; flex: 1; min-height: 0; overflow: hidden; }

    .ev-toc {
        width: 180px;
        min-width: 140px;
        overflow-y: auto;
        background: #18181b;
        border-right: 1px solid #3f3f46;
        padding: 4px 0;
        flex-shrink: 0;
    }
    .ev-toc-item {
        display: block;
        width: 100%;
        padding: 5px 10px;
        border: none;
        background: transparent;
        color: #a1a1aa;
        font-size: 0.72rem;
        text-align: left;
        cursor: pointer;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        transition: background 0.1s;
    }
    .ev-toc-item:hover { background: #27272a; color: #e4e4e7; }
    .ev-toc-item.active { background: #1d4ed8; color: #fff; }

    .ev-content {
        flex: 1;
        overflow: auto;
        padding: 16px 20px;
        color: #e4e4e7;
        font-size: 0.85rem;
        line-height: 1.7;
        background: #0a0a0c;
    }
    .ev-content :global(h1), .ev-content :global(h2), .ev-content :global(h3) { color: #fafafa; margin: 0.8em 0 0.3em; }
    .ev-content :global(h1) { font-size: 1.3rem; }
    .ev-content :global(h2) { font-size: 1.1rem; }
    .ev-content :global(p) { margin: 0.5em 0; }
    .ev-content :global(a) { color: #60a5fa; }
    .ev-content :global(img) { max-width: 100%; height: auto; border-radius: 4px; margin: 0.5em 0; }
    .ev-content :global(blockquote) { border-left: 3px solid #3f3f46; padding-left: 12px; color: #a1a1aa; margin: 0.5em 0; }
    .ev-content :global(ul), .ev-content :global(ol) { padding-left: 1.5em; }

    .ev-msg { padding: 24px 16px; text-align: center; color: #71717a; font-size: 0.85rem; margin: 0; }
    .ev-error { color: #f87171; }
</style>
