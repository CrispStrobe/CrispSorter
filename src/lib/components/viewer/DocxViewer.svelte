<script lang="ts">
    import mammoth from 'mammoth';
    import { readFile } from '@tauri-apps/plugin-fs';
    import { i18n } from '$lib/i18n.svelte';

    let { path = '' }: { path: string } = $props();

    let html = $state('');
    let loading = $state(true);
    let error = $state('');

    $effect(() => {
        const p = path;
        if (!p) return;
        loading = true;
        error = '';
        html = '';
        let cancelled = false;

        (async () => {
            try {
                const bytes = await readFile(p);
                if (cancelled) return;
                const result = await mammoth.convertToHtml({
                    arrayBuffer: (bytes as Uint8Array).buffer,
                });
                if (!cancelled) {
                    html = result.value;
                    loading = false;
                }
            } catch (e: any) {
                if (!cancelled) {
                    error = e.message ?? String(e);
                    loading = false;
                }
            }
        })();

        return () => { cancelled = true; };
    });
</script>

<div class="docx-viewer">
    {#if loading}
        <p class="dv-msg">{i18n.t.viewer.loading}</p>
    {:else if error}
        <p class="dv-msg dv-error">{error}</p>
    {:else}
        <div class="dv-content">
            {@html html}
        </div>
    {/if}
</div>

<style>
    .docx-viewer {
        display: flex;
        flex-direction: column;
        flex: 1;
        overflow: auto;
        min-height: 0;
    }
    .dv-content {
        padding: 16px 20px;
        color: #e4e4e7;
        font-size: 0.85rem;
        line-height: 1.65;
    }
    /* Override mammoth's HTML output for dark theme */
    .dv-content :global(h1) { font-size: 1.4rem; font-weight: 700; margin: 1em 0 0.4em; color: #fafafa; }
    .dv-content :global(h2) { font-size: 1.15rem; font-weight: 600; margin: 0.9em 0 0.3em; color: #fafafa; }
    .dv-content :global(h3) { font-size: 1rem; font-weight: 600; margin: 0.8em 0 0.3em; color: #fafafa; }
    .dv-content :global(h4), .dv-content :global(h5), .dv-content :global(h6) {
        font-size: 0.9rem; font-weight: 600; margin: 0.7em 0 0.2em; color: #e4e4e7;
    }
    .dv-content :global(p) { margin: 0.5em 0; }
    .dv-content :global(ul), .dv-content :global(ol) { padding-left: 1.5em; margin: 0.5em 0; }
    .dv-content :global(li) { margin: 0.2em 0; }
    .dv-content :global(table) {
        border-collapse: collapse;
        margin: 0.8em 0;
        width: 100%;
    }
    .dv-content :global(th), .dv-content :global(td) {
        border: 1px solid #3f3f46;
        padding: 6px 10px;
        font-size: 0.82rem;
    }
    .dv-content :global(th) { background: #27272a; font-weight: 600; }
    .dv-content :global(td) { background: #18181b; }
    .dv-content :global(strong), .dv-content :global(b) { font-weight: 600; color: #fafafa; }
    .dv-content :global(em), .dv-content :global(i) { font-style: italic; }
    .dv-content :global(a) { color: #60a5fa; text-decoration: underline; }
    .dv-content :global(img) { max-width: 100%; height: auto; border-radius: 4px; margin: 0.5em 0; }
    .dv-content :global(blockquote) {
        border-left: 3px solid #3f3f46;
        padding-left: 12px;
        margin: 0.5em 0;
        color: #a1a1aa;
    }
    .dv-content :global(code) {
        font-family: 'SF Mono', 'Cascadia Code', monospace;
        font-size: 0.82em;
        background: #27272a;
        padding: 1px 4px;
        border-radius: 3px;
    }
    .dv-content :global(pre) {
        background: #18181b;
        border: 1px solid #3f3f46;
        border-radius: 6px;
        padding: 10px;
        overflow-x: auto;
    }

    .dv-msg { padding: 24px 16px; text-align: center; color: #71717a; font-size: 0.85rem; margin: 0; }
    .dv-error { color: #f87171; }
</style>
