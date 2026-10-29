<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { save } from '@tauri-apps/plugin-dialog';
    import { writeTextFile } from '@tauri-apps/plugin-fs';
    import { approvedValues, conflictingApprovedLabels, fieldsCsv, type MetadataField } from '$lib/metadataFields';
    let { docId, onFilter, onSort }: {
        docId: string;
        onFilter?: (tag: string, value: string) => void;
        onSort?: (values: Record<string, string>) => Promise<void>;
    } = $props();
    let fields = $state<MetadataField[]>([]);
    let drafts = $state<Record<string, string>>({});
    let selected = $state<string[]>([]);
    function loaded(result: MetadataField[]) {
        fields = result;
        drafts = Object.fromEntries(result.map(field => [field.id, field.normalized_value]));
        selected = [];
    }
    let error = $state('');
    let busy = $state(false);
    let notice = $state('');
    const conflicts = $derived(conflictingApprovedLabels(fields));
    const approved = $derived(Object.fromEntries(Object.entries(approvedValues(fields.filter(field => drafts[field.id] === field.normalized_value)))
        .filter(([label]) => !conflicts.includes(label))));
    $effect(() => {
        const id = docId;
        let cancelled = false;
        loaded([]); error = ''; notice = '';
        invoke<MetadataField[]>('index_get_fields', { docId: id })
            .then(result => { if (!cancelled) loaded(result); })
            .catch(e => { if (!cancelled) error = String(e); });
        return () => { cancelled = true; };
    });
    async function review(field: MetadataField, status: MetadataField['review_status']) {
        const id = docId;
        busy = true; error = '';
        try {
            const result = await invoke<MetadataField[]>('index_review_field', {
                docId: id, fieldId: field.id, normalizedValue: drafts[field.id],
                reviewStatus: status, expectedRevision: field.revision,
            });
            if (id === docId) {
                const previousDrafts = drafts;
                fields = result;
                selected = selected.filter(key => key !== field.id);
                drafts = Object.fromEntries(result.map(value => [value.id,
                    value.id === field.id ? value.normalized_value : previousDrafts[value.id] ?? value.normalized_value]));
            }
        } catch (e) { if (id === docId) error = String(e); }
        finally { busy = false; }
    }
    async function reloadFields() {
        const id = docId;
        busy = true; error = '';
        try {
            const result = await invoke<MetadataField[]>('index_get_fields', { docId: id });
            if (id === docId) {
                const previous = drafts;
                const dirty = new Set(fields.filter(f => previous[f.id] !== f.normalized_value).map(f => f.id));
                fields = result;
                drafts = Object.fromEntries(result.map(f => [f.id, dirty.has(f.id) ? previous[f.id] : f.normalized_value]));
                selected = selected.filter(key => result.some(f => f.id === key && f.review_status === 'suggested'));
                notice = 'Latest saved fields loaded. Your unsaved edits are retained; review them before approving.';
            }
        } catch (e) { if (id === docId) error = String(e); }
        finally { busy = false; }
    }
    async function approveSelected() {
        const id = docId;
        const reviews = fields.filter(f => selected.includes(f.id) && f.review_status === 'suggested')
            .map(f => ({ fieldId: f.id, normalizedValue: drafts[f.id], expectedRevision: f.revision }));
        if (!reviews.length) return;
        busy = true; error = ''; notice = '';
        try {
            const result = await invoke<MetadataField[]>('index_approve_fields', { docId: id, reviews });
            if (id === docId) {
                const previous = drafts;
                const reviewed = new Set(reviews.map(r => r.fieldId));
                fields = result;
                drafts = Object.fromEntries(result.map(f => [f.id, reviewed.has(f.id) ? f.normalized_value : previous[f.id] ?? f.normalized_value]));
                selected = [];
                notice = `${reviews.length} fields approved. Review the target path before moving files.`;
            }
        } catch (e) { if (id === docId) error = String(e); }
        finally { busy = false; }
    }
    async function exportFields(format: 'json' | 'csv') {
        const id = docId;
        const content = format === 'json' ? JSON.stringify({ doc_id: id, fields }, null, 2) : fieldsCsv(fields);
        error = '';
        try {
            const path = await save({ defaultPath: `document-fields.${format}`, filters: [{ name: format.toUpperCase(), extensions: [format] }] });
            if (path) await writeTextFile(path, content);
        } catch (e) { if (id === docId) error = String(e); }
    }
    async function useForSorting() {
        if (!onSort) return;
        const id = docId;
        busy = true; error = '';
        try { await onSort(approved); if (id === docId) notice = 'Approved fields saved for sorting. Review and accept the file before applying moves.'; }
        catch (e) { if (id === docId) error = String(e); }
        finally { busy = false; }
    }
</script>

<details class="fields-panel">
    <summary>Extracted fields · {fields.length} · {fields.filter(f => f.review_status === 'suggested').length} need review</summary>
    {#if error}<p role="alert">{error}</p><button disabled={busy} onclick={reloadFields}>Reload latest fields</button>{/if}
    {#if notice}<p role="status">{notice}</p>{/if}
    {#if !fields.length && !error}<p>Enable named-entity extraction and reindex this document to retain structured fields and source evidence.</p>{/if}
    {#each fields as field (field.id)}
        <article>
            {#if field.review_status === 'suggested'}<label><input type="checkbox" value={field.id} bind:group={selected} disabled={busy} /> Select for bulk approval</label>{/if}
            <strong>{field.label}</strong> <span>{field.review_status}</span>
            <blockquote>{field.original_value}</blockquote>
            <label>Normalized value <input bind:value={drafts[field.id]} disabled={busy} /></label>
            {#if drafts[field.id] !== field.normalized_value}<small>Unsaved edit. Saved value: {field.normalized_value}. Approve the edit before using it for sorting. Exports retain the saved value.</small>{/if}
            <small>{field.model}{field.confidence == null ? '' : ` · model score ${field.confidence.toFixed(3)}`}
                · {field.page == null ? 'page unavailable' : `page ${field.page}`}
                {field.start_char == null ? '' : ` · characters ${field.start_char}–${field.end_char} (${field.anchor_method})`}</small>
            <div>
                <button disabled={busy} onclick={() => review(field, drafts[field.id] === field.original_value ? 'accepted' : 'corrected')}>Approve value</button>
                <button disabled={busy} onclick={() => review(field, 'rejected')}>Reject</button>
                {#if onFilter && field.review_status !== 'rejected'}<button onclick={() => onFilter?.(`field:${field.label}:${field.normalized_value}`, field.normalized_value)}>Find matching documents</button>{/if}
            </div>
        </article>
    {/each}
    {#if fields.length}
        <button disabled={busy || !fields.some(f => f.review_status === 'suggested')} onclick={() => { selected = fields.filter(f => f.review_status === 'suggested').slice(0, 500).map(f => f.id); }}>Select up to 500 suggested</button>
        <button disabled={busy || !selected.length} onclick={approveSelected}>Approve selected ({selected.length})</button>
        <button onclick={() => exportFields('json')}>Export JSON</button>
        <button onclick={() => exportFields('csv')}>Export CSV</button>
        {#if conflicts.length}<p role="status">Conflicting approved values for {conflicts.join(', ')} are excluded from sorting. Reject or correct the conflicting fields first.</p>{/if}
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
    input:not([type='checkbox']) { width: 100%; box-sizing: border-box; }
    button { margin: 4px 4px 0 0; }
    p[role='alert'] { color: #fca5a5; }
</style>
