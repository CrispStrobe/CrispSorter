<script lang="ts">
    import { readFile } from '@tauri-apps/plugin-fs';
    import { i18n } from '$lib/i18n.svelte';

    let { path = '' }: { path: string } = $props();

    let html = $state('');
    let loading = $state(true);
    let error = $state('');

    /** Decode HTML with charset sniffing (same logic as htmlExtractor.ts). */
    function decodeHtml(buf: ArrayBuffer): string {
        const sniff = new TextDecoder('utf-8', { fatal: false }).decode(buf.slice(0, 4096));
        const m = sniff.match(/<meta[^>]+charset\s*=\s*["']?([\w-]+)/i);
        const charset = (m?.[1] ?? 'utf-8').toLowerCase();
        if (charset === 'utf-8' || charset === 'utf8') return new TextDecoder('utf-8').decode(buf);
        try { return new TextDecoder(charset).decode(buf); } catch { return new TextDecoder('utf-8').decode(buf); }
    }

    /** Strip <script> and <noscript> tags for safety. */
    function sanitise(raw: string): string {
        return raw
            .replace(/<script[\s\S]*?<\/script>/gi, '')
            .replace(/<noscript[\s\S]*?<\/noscript>/gi, '');
    }

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
                const raw = decodeHtml((bytes as Uint8Array).buffer);
                html = sanitise(raw);
                loading = false;
            } catch (e: any) {
                if (!cancelled) { error = e.message ?? String(e); loading = false; }
            }
        })();

        return () => { cancelled = true; };
    });
</script>

<div class="html-viewer">
    {#if loading}
        <p class="hv-msg">{i18n.t.viewer.loading}</p>
    {:else if error}
        <p class="hv-msg hv-error">{error}</p>
    {:else}
        <div class="hv-content">
            {@html html}
        </div>
    {/if}
</div>

<style>
    .html-viewer {
        display: flex;
        flex-direction: column;
        flex: 1;
        overflow: auto;
        min-height: 0;
    }
    .hv-content {
        padding: 12px 16px;
        color: #e4e4e7;
        font-size: 0.85rem;
        line-height: 1.6;
        background: #0a0a0c;
    }
    /* Sensible dark-theme overrides for arbitrary HTML */
    .hv-content :global(body) { color: #e4e4e7; background: transparent; }
    .hv-content :global(h1), .hv-content :global(h2), .hv-content :global(h3) { color: #fafafa; }
    .hv-content :global(a) { color: #60a5fa; }
    .hv-content :global(img) { max-width: 100%; height: auto; }
    .hv-content :global(table) { border-collapse: collapse; }
    .hv-content :global(th), .hv-content :global(td) { border: 1px solid #3f3f46; padding: 4px 8px; }
    .hv-content :global(pre), .hv-content :global(code) { background: #18181b; color: #d4d4d8; }

    .hv-msg { padding: 24px 16px; text-align: center; color: #71717a; font-size: 0.85rem; margin: 0; }
    .hv-error { color: #f87171; }
</style>
