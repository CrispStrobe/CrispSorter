<script lang="ts">
    import { readTextFile } from '@tauri-apps/plugin-fs';
    import { i18n } from '$lib/i18n.svelte';

    let { path = '' }: { path: string } = $props();

    const MAX_BYTES = 512 * 1024;

    let text = $state('');
    let truncated = $state(false);
    let loading = $state(true);
    let error = $state('');

    $effect(() => {
        if (!path) return;
        loading = true;
        error = '';
        text = '';
        truncated = false;

        readTextFile(path)
            .then((content) => {
                if (content.length > MAX_BYTES) {
                    text = content.slice(0, MAX_BYTES);
                    truncated = true;
                } else {
                    text = content;
                }
                loading = false;
            })
            .catch((e: any) => {
                error = String(e.message ?? e);
                loading = false;
            });
    });
</script>

<div class="text-viewer">
    {#if loading}
        <p class="tv-msg">{i18n.t.viewer.loading}</p>
    {:else if error}
        <p class="tv-msg tv-error">{error}</p>
    {:else}
        <pre class="tv-text">{text}{#if truncated}<span class="tv-trunc">{'\n\n'}{i18n.t.viewer.truncated}</span>{/if}</pre>
    {/if}
</div>

<style>
    .text-viewer {
        display: flex;
        flex-direction: column;
        flex: 1;
        overflow: auto;
        min-height: 0;
    }
    .tv-text {
        margin: 0;
        padding: 12px;
        font-family: 'SF Mono', 'Cascadia Code', 'Fira Code', monospace;
        font-size: 0.78rem;
        line-height: 1.5;
        white-space: pre-wrap;
        word-break: break-word;
        color: #d4d4d8;
    }
    .tv-trunc { color: #71717a; font-style: italic; }
    .tv-msg {
        padding: 24px 16px;
        text-align: center;
        color: #71717a;
        font-size: 0.85rem;
        margin: 0;
    }
    .tv-error { color: #f87171; }
</style>
