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
    let filenamesOnly = $state<number | null>(null);
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
    <button onclick={() => { batchManager.resetFilters(); navigate('review'); }}>{review} {i18n.lang === 'de' ? 'Dateien benötigen Prüfung' : 'files need review'}</button>
    {#if failed}<button onclick={() => { batchManager.resetFilters('error'); navigate('batch'); }}>{failed} {i18n.lang === 'de' ? 'Verarbeitungsfehler' : 'processing failures'}</button>{/if}
    {#if filenamesOnly != null && filenamesOnly > 0}<button onclick={() => navigate('catalog')}>{filenamesOnly} {i18n.lang === 'de' ? 'Dateien nur mit Dateinamen indexiert' : 'files have filenames indexed only'}</button>{/if}
</div>
<style>
    .workflow-summary { display: flex; gap: 8px; padding: 8px 12px; flex-wrap: wrap; border-bottom: 1px solid #3f3f46; }
    button { background: #18181b; border: 1px solid #3f3f46; color: #d4d4d8; padding: 5px 9px; border-radius: 6px; cursor: pointer; font-size: 0.8rem; }
</style>
