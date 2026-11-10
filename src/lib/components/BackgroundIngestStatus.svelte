<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';
    import { onMount } from 'svelte';
    import { AlertCircle, Loader2, Pause, Play, RefreshCw, Trash2, X } from 'lucide-svelte';

    interface Snapshot {
        status: 'idle' | 'running' | 'paused' | 'stopping';
        pending: number;
        active: number;
        current: string | null;
        done: number;
        errored: number;
        last_error: string | null;
    }

    let { ensureReady }: { ensureReady: () => Promise<boolean> } = $props();
    let snapshot = $state<Snapshot | null>(null);
    let error = $state('');
    let busy = $state(false);
    let revision = 0;

    async function refresh(clearError = false) {
        const request = ++revision;
        try {
            const result = await invoke<Snapshot | null>('bg_ingest_status');
            if (request !== revision) return;
            snapshot = result;
            if (clearError) error = '';
        } catch (cause) {
            if (request === revision) error = `Gespeicherte Hintergrundaufträge konnten nicht geladen werden: ${String(cause)}`;
        }
    }

    async function control(action: 'pause' | 'resume' | 'cancel' | 'clear') {
        if (busy) return;
        busy = true;
        error = '';
        ++revision;
        try {
            if (action === 'resume' && !(await ensureReady())) {
                error = 'Der Suchindex konnte nicht gestartet werden. Die gespeicherten Aufträge bleiben erhalten.';
                return;
            }
            await invoke(`bg_ingest_${action}`);
            await refresh(true);
        } catch (cause) {
            error = `Hintergrundauftrag konnte nicht geändert werden: ${String(cause)}`;
        } finally { busy = false; }
    }

    onMount(() => {
        let mounted = true;
        let unlisten: (() => void) | undefined;
        void refresh();
        const interval = setInterval(() => { if (!busy) void refresh(); }, 4000);
        void listen<Snapshot>('bg-ingest:status', event => {
            ++revision;
            snapshot = event.payload;
        }).then(cleanup => {
            if (mounted) unlisten = cleanup;
            else cleanup();
        }).catch(cause => {
            // Polling still provides state; report why live updates are absent.
            error = `Live-Fortschritt ist nicht verfügbar: ${String(cause)}`;
        });
        return () => { mounted = false; ++revision; clearInterval(interval); unlisten?.(); };
    });
</script>

{#if error || (snapshot && (snapshot.pending > 0 || snapshot.active > 0 || snapshot.done > 0 || snapshot.errored > 0 || snapshot.last_error))}
    <section class="background-ingest-status" aria-label="Hintergrund-Indexierung">
        <div class="heading">
            <strong>Hintergrund-Indexierung</strong>
            {#if snapshot}
                <span>{snapshot.pending} ausstehend · {snapshot.done} gespeichert · {snapshot.errored} Fehler</span>
            {/if}
            <button aria-label="Hintergrundstatus aktualisieren" onclick={() => refresh(true)} disabled={busy}><RefreshCw size={14} /></button>
        </div>
        {#if snapshot?.current}<p class="current" title={snapshot.current}>{snapshot.current}</p>{/if}
        {#if snapshot?.last_error}<p class="failure"><AlertCircle size={14} /> {snapshot.last_error}</p>{/if}
        {#if error}<p class="failure" role="alert"><AlertCircle size={14} /> {error}</p>{/if}
        {#if snapshot}
            <div class="controls">
                {#if snapshot.status === 'running'}
                    <button onclick={() => control('pause')} disabled={busy}><Pause size={14} /> Hintergrund pausieren</button>
                {:else if snapshot.status !== 'stopping' && (snapshot.pending > 0 || snapshot.errored > 0)}
                    <button onclick={() => control('resume')} disabled={busy}><Play size={14} /> Hintergrund fortsetzen{#if snapshot.errored > 0} und Fehler erneut versuchen{/if}</button>
                {/if}
                {#if snapshot.status === 'running' || (snapshot.active ?? 0) > 0}
                    <button onclick={() => control('cancel')} disabled={busy || snapshot.status === 'stopping'}>
                        {#if snapshot.status === 'stopping'}<Loader2 size={14} class="spin" /> Wird abgeschlossen …{:else}<X size={14} /> Hintergrund stoppen{/if}
                    </button>
                {/if}
                <button onclick={() => control('clear')} disabled={busy || (snapshot.active ?? 0) > 0 || snapshot.status === 'running' || snapshot.status === 'stopping'}>
                    <Trash2 size={14} /> Hintergrundliste leeren
                </button>
                {#if snapshot.status === 'paused'}<span>Gespeichert und pausiert. Fertige Dateien werden beim Fortsetzen übersprungen.</span>{/if}
            </div>
        {/if}
    </section>
{/if}

<style>
    .background-ingest-status { margin: 12px 16px; padding: 12px; border: 1px solid #334155; border-radius: 8px; background: #111827; color: #e2e8f0; font-size: 0.8rem; }
    .heading, .controls { display: flex; align-items: center; flex-wrap: wrap; gap: 9px; }
    .heading strong { margin-right: 4px; }
    .heading > button { margin-left: auto; }
    .heading span, .controls > span { color: #94a3b8; }
    .controls { margin-top: 10px; }
    button { display: inline-flex; align-items: center; gap: 5px; padding: 6px 8px; border: 1px solid #475569; border-radius: 5px; background: #1e293b; color: #e2e8f0; cursor: pointer; font-size: 0.75rem; }
    button:disabled { opacity: 0.5; cursor: default; }
    .current { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: #cbd5e1; }
    .failure { display: flex; align-items: flex-start; gap: 7px; overflow-wrap: anywhere; color: #fecaca; margin: 8px 0; }
    .background-ingest-status :global(.spin) { animation: spin 1s linear infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }
</style>
