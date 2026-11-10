<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { batchManager } from '$lib/batch/store.svelte';
    import { inWorkflow } from '$lib/workflow';
    import { i18n } from '$lib/i18n.svelte';
    let { navigate }: { navigate: (tab: string) => void } = $props();
    const inbox = $derived(batchManager.items.filter(i => inWorkflow(i, 'inbox')).length);
    const review = $derived(batchManager.items.filter(i => inWorkflow(i, 'review') && !i.isAccepted).length);
    const failed = $derived(batchManager.items.filter(i => i.status === 'error').length);
    const reviewLabel = $derived(i18n.lang === 'de'
        ? `${review} ${review === 1 ? 'Datei benötigt Prüfung' : 'Dateien benötigen Prüfung'}`
        : `${review} ${review === 1 ? 'file needs review' : 'files need review'}`);
    const failureLabel = $derived(i18n.lang === 'de'
        ? `${failed} Verarbeitungsfehler`
        : `${failed} ${failed === 1 ? 'processing failure' : 'processing failures'}`);
    let filenamesOnly = $state<number | null>(null);
    const filenamesLabel = $derived(i18n.lang === 'de'
        ? `${filenamesOnly} ${filenamesOnly === 1 ? 'Datei nur mit Dateinamen indexiert' : 'Dateien nur mit Dateinamen indexiert'}`
        : `${filenamesOnly} ${filenamesOnly === 1 ? 'file has its filename indexed only' : 'files have filenames indexed only'}`);
    onMount(() => {
        let cancelled = false;
        const refresh = async () => {
            try { const n = await invoke<number>('index_filename_only_count'); if (!cancelled) filenamesOnly = n; }
            catch { if (!cancelled) filenamesOnly = null; }
        };
        refresh();
        const timer = setInterval(refresh, 30000);
        return () => { cancelled = true; clearInterval(timer); };
    });
</script>
<div class="workflow-summary" aria-label={i18n.lang === 'de' ? 'Offene Aufgaben' : 'Work remaining'}>
    <button onclick={() => { batchManager.resetFilters(); navigate('batch'); }}>{i18n.t.nav.batch} · {inbox}</button>
    <button onclick={() => { batchManager.resetFilters(); navigate('review'); }}>{reviewLabel}</button>
    {#if failed}<button onclick={() => { batchManager.resetFilters('error'); navigate('batch'); }}>{failureLabel}</button>{/if}
    {#if filenamesOnly != null && filenamesOnly > 0}<button onclick={() => navigate('catalog')}>{filenamesLabel}</button>{/if}
</div>
<style>
    .workflow-summary { display: flex; gap: 8px; padding: 8px 12px; flex-wrap: wrap; border-bottom: 1px solid #3f3f46; }
    button { background: #18181b; border: 1px solid #3f3f46; color: #d4d4d8; padding: 5px 9px; border-radius: 6px; cursor: pointer; font-size: 0.8rem; }
</style>
