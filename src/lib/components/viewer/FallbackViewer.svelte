<script lang="ts">
    import { ExternalLink } from 'lucide-svelte';
    import { i18n } from '$lib/i18n.svelte';

    let { path = '', filename = '' }: { path: string; filename?: string } = $props();

    async function openExternal() {
        try {
            const { openPath } = await import('@tauri-apps/plugin-opener');
            await openPath(path);
        } catch (e: any) {
            console.warn('[FallbackViewer] openPath failed:', e);
        }
    }
</script>

<div class="fallback">
    <p class="fb-msg">{i18n.t.viewer.unsupported}</p>
    {#if path}
        <button class="fb-btn" onclick={openExternal}>
            <ExternalLink size={14} />
            {i18n.t.viewer.open_external}
        </button>
    {/if}
    {#if filename}
        <p class="fb-name">{filename}</p>
    {/if}
</div>

<style>
    .fallback {
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: 12px;
        padding: 32px 16px;
        min-height: 120px;
    }
    .fb-msg { color: #71717a; font-size: 0.85rem; margin: 0; }
    .fb-name { color: #52525b; font-size: 0.75rem; margin: 0; word-break: break-all; }
    .fb-btn {
        display: inline-flex;
        align-items: center;
        gap: 6px;
        padding: 6px 14px;
        border: 1px solid #3f3f46;
        border-radius: 6px;
        background: #27272a;
        color: #e4e4e7;
        font-size: 0.82rem;
        cursor: pointer;
        transition: background 0.12s;
    }
    .fb-btn:hover { background: #3f3f46; }
</style>
