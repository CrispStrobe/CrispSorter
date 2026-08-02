<script lang="ts">
    import { readTextFile, writeTextFile } from '@tauri-apps/plugin-fs';
    import { i18n } from '$lib/i18n.svelte';

    let { path = '' }: { path: string } = $props();

    const MAX_BYTES = 512 * 1024;

    let text = $state('');
    let originalText = $state('');
    let truncated = $state(false);
    let loading = $state(true);
    let error = $state('');
    let editing = $state(false);
    let saving = $state(false);

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
                originalText = text;
                editing = false;
                loading = false;
            })
            .catch((e: any) => {
                error = String(e.message ?? e);
                loading = false;
            });
    });

    let dirty = $derived(editing && text !== originalText);

    async function saveText(): Promise<void> {
        if (!path || truncated || !dirty || saving) return;
        saving = true;
        error = '';
        try {
            await writeTextFile(path, text);
            originalText = text;
            editing = false;
        } catch (e: any) {
            error = String(e?.message ?? e);
        } finally {
            saving = false;
        }
    }

    function cancelEdit(): void {
        text = originalText;
        editing = false;
    }
</script>

<div class="text-viewer">
    {#if loading}
        <p class="tv-msg">{i18n.t.viewer.loading}</p>
    {:else if error}
        <p class="tv-msg tv-error">{error}</p>
    {:else}
        <div class="tv-toolbar">
            {#if truncated}
                <span class="tv-trunc">Preview truncated; editing disabled.</span>
            {:else if editing}
                <button type="button" onclick={saveText} disabled={!dirty || saving}>
                    {saving ? 'Saving…' : 'Save'}
                </button>
                <button type="button" class="tv-cancel" onclick={cancelEdit} disabled={saving}>Cancel</button>
                {#if dirty}<span class="tv-dirty">Unsaved changes</span>{/if}
            {:else}
                <button type="button" onclick={() => editing = true}>Edit</button>
            {/if}
        </div>
        {#if editing}
            <textarea class="tv-text tv-editor" bind:value={text} spellcheck="false" aria-label="Text editor"></textarea>
        {:else}
            <pre class="tv-text">{text}{#if truncated}<span class="tv-trunc">{'\n\n'}{i18n.t.viewer.truncated}</span>{/if}</pre>
        {/if}
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
    .tv-toolbar {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 8px 12px;
        border-bottom: 1px solid #27272a;
        background: #111113;
    }
    .tv-toolbar button {
        border: 1px solid #3f3f46;
        border-radius: 4px;
        background: #27272a;
        color: #e4e4e7;
        padding: 3px 9px;
        cursor: pointer;
    }
    .tv-toolbar button:disabled { opacity: .45; cursor: default; }
    .tv-toolbar .tv-cancel { color: #a1a1aa; }
    .tv-dirty { color: #fbbf24; font-size: .75rem; }
    .tv-editor { flex: 1; width: 100%; box-sizing: border-box; border: 0; resize: none; outline: none; background: #0a0a0c; }
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
