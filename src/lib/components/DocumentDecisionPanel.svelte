<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { experimentalCrispEmbed } from '$lib/experimentalCrispEmbed';
    import AiGeneratedBadge from './AiGeneratedBadge.svelte';
    let { document, onUse }: { document: object; onUse?: (values: Record<string, string>) => Promise<void> } = $props();
    let task = $state('category');
    let choicesInput = $state('Invoice, Correspondence, Article, Book, Other');
    let busy = $state(false);
    let error = $state('');
    let result = $state<{ value: string; confidence: number; model: string; input_excerpt: string } | null>(null);
    let resultTask = $state('');
    $effect(() => { void document; result = null; error = ''; });
    async function classify() {
        const source = document;
        busy = true; error = ''; result = null;
        try {
            const response = await invoke<NonNullable<typeof result>>('crispembed_classify_document', {
                endpoint: $experimentalCrispEmbed.endpoint, document: source, task,
                choices: task === 'needs_review' ? [] : choicesInput.split(',').map(v => v.trim()).filter(Boolean),
            });
            if (source === document) { result = response; resultTask = task; }
        } catch (e) { if (source === document) error = String(e); }
        finally { busy = false; }
    }
    async function useSuggestion() {
        if (!result || !onUse) return;
        try { await onUse({ [resultTask === 'category' ? 'doctype' : resultTask]: result.value }); }
        catch (e) { error = String(e); }
    }
</script>

{#if $experimentalCrispEmbed.decisions && $experimentalCrispEmbed.endpoint}
    <details class="decision-panel">
        <summary>Classification suggestion · experimental</summary>
        <AiGeneratedBadge />
        <label>Question <select bind:value={task} disabled={busy}>
            <option value="category">Document type</option>
            <option value="collection">Collection</option>
            <option value="needs_review">Needs review?</option>
        </select></label>
        {#if task !== 'needs_review'}<label>Choices, separated by commas <input bind:value={choicesInput} disabled={busy} /></label>{/if}
        <button onclick={classify} disabled={busy}>{busy ? 'Classifying…' : 'Suggest from document excerpt'}</button>
        {#if error}<p role="alert">{error}</p>{/if}
        {#if result}
            <p><strong>{result.value}</strong> · model score {result.confidence.toFixed(3)} · {result.model}</p>
            <p>Based on the excerpt below; the server profile clips state to 512 model tokens. Independent accuracy is not assumed.</p>
            <blockquote>{result.input_excerpt}</blockquote>
            {#if onUse && resultTask !== 'needs_review'}<button onclick={useSuggestion}>Approve as a sorting field</button>{/if}
        {/if}
    </details>
{/if}

<style>
    .decision-panel { padding: 10px; border-bottom: 1px solid #52525b; font-size: 0.8rem; max-height: 40vh; overflow: auto; }
    summary { cursor: pointer; }
    label { display: block; margin: 8px 0; }
    input, select, button { background: #27272a; color: #eee; border: 1px solid #52525b; padding: 5px; border-radius: 4px; }
    input { display: block; width: 95%; }
    blockquote { white-space: pre-wrap; max-height: 140px; overflow: auto; margin: 8px 0; }
    p[role='alert'] { color: #fca5a5; }
</style>
