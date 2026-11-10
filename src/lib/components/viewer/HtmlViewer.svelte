<script lang="ts">
    import { readFile, stat } from '@tauri-apps/plugin-fs';
    import { i18n } from '$lib/i18n.svelte';
    import { htmlPreviewDocument, MAX_HTML_PREVIEW_BYTES } from './htmlPreview';

    let { path = '' }: { path: string } = $props();

    let html = $state('');
    let loading = $state(true);
    let error = $state('');

    /** Decode HTML with charset sniffing (same logic as htmlExtractor.ts). */
    function decodeHtml(buf: Uint8Array): string {
        const sniff = new TextDecoder('utf-8', { fatal: false }).decode(buf.slice(0, 4096));
        const m = sniff.match(/<meta[^>]+charset\s*=\s*["']?([\w-]+)/i);
        const charset = (m?.[1] ?? 'utf-8').toLowerCase();
        if (charset === 'utf-8' || charset === 'utf8') return new TextDecoder('utf-8').decode(buf);
        try { return new TextDecoder(charset).decode(buf); } catch { return new TextDecoder('utf-8').decode(buf); }
    }

    $effect(() => {
        const p = path;
        loading = Boolean(p);
        error = '';
        html = '';
        if (!p) return;
        let cancelled = false;

        (async () => {
            try {
                const info = await stat(p);
                if (cancelled) return;
                if (info.size > MAX_HTML_PREVIEW_BYTES) throw new Error('This HTML document exceeds the 8 MiB preview limit.');
                const bytes = await readFile(p);
                if (cancelled) return;
                if (bytes.byteLength > MAX_HTML_PREVIEW_BYTES) throw new Error('This HTML document exceeds the 8 MiB preview limit.');
                html = htmlPreviewDocument(decodeHtml(bytes));
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
        <iframe title="HTML document preview" sandbox="" referrerpolicy="no-referrer" srcdoc={html}></iframe>
    {/if}
</div>

<style>
    .html-viewer {
        display: flex;
        flex-direction: column;
        flex: 1;
        overflow: hidden;
        min-height: 0;
    }
    iframe { flex: 1; width: 100%; min-height: 0; border: 0; background: #0a0a0c; }

    .hv-msg { padding: 24px 16px; text-align: center; color: #71717a; font-size: 0.85rem; margin: 0; }
    .hv-error { color: #f87171; }
</style>
