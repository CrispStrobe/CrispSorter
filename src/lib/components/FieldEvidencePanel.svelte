<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { save } from '@tauri-apps/plugin-dialog';
    import { writeTextFile } from '@tauri-apps/plugin-fs';
    import { approvedValues, fieldsCsv, type MetadataField } from '$lib/metadataFields';
    let { docId, onFilter, onSort }: {
        docId: string;
        onFilter?: (tag: string, value: string) => void;
        onSort?: (values: Record<string, string>) => Promise<void>;
    } = $props();
    let fields = $state<MetadataField[]>([]);
    let error = $state('');
    let busy = $state(false);
    let notice = $state('');
    const approved = $derived(approvedValues(fields));
    $effect(() => {
        const id = docId;
        let cancelled = false;
        fields = []; error = ''; notice = '';
        invoke<MetadataField[]>('index_get_fields', { docId: id })
            .then(result => { if (!cancelled) fields = result; })
            .catch(e => { if (!cancelled) error = String(e); });
        return () => { cancelled = true; };
    });
    async function review(field: MetadataField, status: MetadataField['review_status']) {
        const id = docId;
        busy = true; error = '';
        try {
            const result = await invoke<MetadataField[]>('index_review_field', {
                docId: id, fieldId: field.id, normalizedValue: field.normalized_value,
                reviewStatus: status, expectedRevision: field.revision,
            });
            if (id === docId) fields = result;
        } catch (e) { if (id === docId) error = String(e); }
        finally { busy = false; }
    }
    async function exportFields(format: 'json' | 'csv') {
        error = '';
        try {
            const path = await save({ defaultPath: `document-fields.${format}`, filters: [{ name: format.toUpperCase(), extensions: [format] }] });
            if (path) await writeTextFile(path, format === 'json' ? JSON.stringify({ doc_id: docId, fields }, null, 2) : fieldsCsv(fields));
        } catch (e) { error = String(e); }
    }
    async function useForSorting() {
        if (!onSort) return;
        busy = true; error = '';
        try { await onSort(approved); notice = 'Approved fields saved for sorting. Review the file in Inbox before applying moves.'; }
        catch (e) { error = String(e); }
        finally { busy = false; }
    }
</script>

<details class="fields-panel">
    <summary>Extracted fields · {fields.length} · {fields.filter(f => f.review_status === 'suggested').length} need review</summary>
    {#if error}<p role="alert">{error}</p>{/if}
    {#if notice}<p role="status">{notice}</p>{/if}
    {#if !fields.length && !error}<p>Enable named-entity extraction and reindex this document to retain structured fields and source evidence.</p>{/if}
    {#each fields as field (field.id)}
        <article>
            <strong>{field.label}</strong> <span>{field.review_status}</span>
            <blockquote>{field.original_value}</blockquote>
            <label>Normalized value <input bind:value={field.normalized_value} disabled={busy} /></label>
            <small>{field.model}{field.confidence == null ? '' : ` · model score ${field.confidence.toFixed(3)}`}
                · {field.page == null ? 'page unavailable' : `page ${field.page}`}
                {field.start_char == null ? '' : ` · characters ${field.start_char}–${field.end_char} (${field.anchor_method})`}</small>
            <div>
                <button disabled={busy} onclick={() => review(field, field.normalized_value === field.original_value ? 'accepted' : 'corrected')}>Approve value</button>
                <button disabled={busy} onclick={() => review(field, 'rejected')}>Reject</button>
                {#if onFilter && field.review_status !== 'rejected'}<button onclick={() => onFilter?.(`field:${field.label}:${field.normalized_value}`, field.normalized_value)}>Find matching documents</button>{/if}
            </div>
        </article>
    {/each}
    {#if fields.length}
        <button onclick={() => exportFields('json')}>Export JSON</button>
        <button onclick={() => exportFields('csv')}>Export CSV</button>
        {#if onSort}<button disabled={busy || !Object.keys(approved).length} onclick={useForSorting}>Use approved fields for sorting</button>{/if}
        <p>Use <code>{'{Field:vendor}'}</code> or another approved field in the output path template.</p>
    {/if}
</details>

<style>
    .fields-panel { border-bottom: 1px solid #3f3f46; padding: 10px; max-height: 45vh; overflow: auto; font-size: 0.8rem; }
    summary, button { cursor: pointer; }
    article { border-top: 1px solid #3f3f46; padding: 10px 0; }
    article span, small { color: #a1a1aa; }
    blockquote { margin: 8px 0; white-space: pre-wrap; padding-left: 10px; border-left: 2px solid #60a5fa; }
    label, small { display: block; margin-bottom: 6px; }
    input, button { background: #27272a; color: #eee; border: 1px solid #52525b; border-radius: 4px; padding: 5px; }
    input { width: 100%; box-sizing: border-box; }
    button { margin: 4px 4px 0 0; }
    p[role='alert'] { color: #fca5a5; }
</style>
