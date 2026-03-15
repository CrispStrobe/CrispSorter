<script lang="ts">
    import { onMount } from 'svelte';
    import { getSetting, saveSetting } from '../store';
    import { type BatchSession } from '../types';
    import { batchManager } from '../batch/store.svelte';
    import { i18n } from '../i18n.svelte';
    import { Clock, Play, Trash2, Calendar, FileText, Upload, Download } from 'lucide-svelte';

    let sessions = $state<BatchSession[]>([]);

    onMount(async () => {
        await refreshHistory();
    });

    async function refreshHistory() {
        const saved = await getSetting('sessions', {}) as Record<string, BatchSession>;
        sessions = Object.values(saved).sort((a, b) => b.startTime - a.startTime);
    }

    async function handleResume(id: string, onDone: () => void) {
        await batchManager.loadSession(id);
        onDone();
    }

    async function handleDelete(id: string) {
        if (!confirm(i18n.t.history.delete_confirm)) return;
        const saved = await getSetting('sessions', {}) as Record<string, BatchSession>;
        delete saved[id];
        await saveSetting('sessions', saved);
        await refreshHistory();
    }

    async function handleImport() {
        await batchManager.importBatch();
        await refreshHistory();
    }

    let { onResumeBatch } = $props<{ onResumeBatch: () => void }>();
</script>

<div class="history-container">
    <div class="header">
        <div class="title-area">
            <h1>{i18n.t.history.title}</h1>
            <p>{i18n.t.history.subtitle}</p>
        </div>
        <div class="header-actions">
            <button class="action-btn" onclick={handleImport}>
                <Upload size={18} /> {i18n.t.history.import}
            </button>
            <button class="action-btn" onclick={() => batchManager.exportBatch()}>
                <Download size={18} /> {i18n.t.history.export}
            </button>
        </div>
    </div>

    <div class="sessions-list">
        {#each sessions as session}
            <div class="session-card">
                <div class="session-info">
                    <div class="session-main">
                        <Clock size={16} />
                        <strong>{new Date(session.startTime).toLocaleString()}</strong>
                        <span class="badge" class:completed={session.status === 'completed'} class:paused={session.status === 'paused'}>
                            {session.status === 'paused' ? i18n.t.history.paused : session.status}
                        </span>
                    </div>
                    <div class="session-details">
                        <span><FileText size={14} /> {session.items.length} files</span>
                        <span><Calendar size={14} /> ID: {session.id.substring(0, 8)}</span>
                    </div>
                </div>
                <div class="session-actions">
                    <button class="resume-btn" onclick={() => handleResume(session.id, onResumeBatch)}>
                        <Play size={16} /> {i18n.t.history.resume}
                    </button>
                    <button class="delete-btn" onclick={() => handleDelete(session.id)}>
                        <Trash2 size={16} />
                    </button>
                </div>
            </div>
        {:else}
            <div class="empty-state">
                <Clock size={48} />
                <p>{i18n.t.history.empty}</p>
            </div>
        {/each}
    </div>
</div>

<style>
    .history-container {
        padding: 40px;
        height: 100%;
        overflow-y: auto;
        background: #09090b;
        color: #fafafa;
    }

    .header { margin-bottom: 32px; display: flex; justify-content: space-between; align-items: flex-start; }
    h1 { font-size: 1.875rem; font-weight: 700; margin: 0 0 8px; }
    p { color: #a1a1aa; margin: 0; }

    .header-actions { display: flex; gap: 12px; }
    .action-btn { display: flex; align-items: center; gap: 8px; padding: 8px 16px; border: 1px solid #27272a; background: #18181b; border-radius: 8px; color: #d4d4d8; cursor: pointer; font-weight: 600; transition: background 0.2s; }
    .action-btn:hover { background: #27272a; }

    .sessions-list { display: flex; flex-direction: column; gap: 16px; max-width: 800px; }

    .session-card {
        background: #18181b;
        border: 1px solid #27272a;
        border-radius: 12px;
        padding: 20px;
        display: flex;
        justify-content: space-between;
        align-items: center;
        transition: transform 0.2s, box-shadow 0.2s;
    }

    .session-card:hover { transform: translateY(-2px); border-color: #3f3f46; }

    .session-info { display: flex; flex-direction: column; gap: 8px; }
    .session-main { display: flex; align-items: center; gap: 10px; font-size: 1rem; color: #f9fafb; }

    .badge { font-size: 0.75rem; padding: 2px 8px; background: #3f3f46; color: #d4d4d8; border-radius: 9999px; text-transform: uppercase; font-weight: 600; }
    .badge.completed { background: #166534; color: #dcfce7; }
    .badge.paused { background: #450a0a; color: #fca5a5; }

    .session-details { display: flex; gap: 20px; font-size: 0.875rem; color: #a1a1aa; }
    .session-details span { display: flex; align-items: center; gap: 4px; }

    .session-actions { display: flex; gap: 12px; }

    .resume-btn { background: #3b82f6; color: white; border: none; padding: 8px 16px; border-radius: 8px; font-weight: 600; cursor: pointer; display: flex; align-items: center; gap: 8px; }
    .delete-btn { background: #450a0a; color: #fecaca; border: none; padding: 8px; border-radius: 8px; cursor: pointer; }

    .empty-state { text-align: center; padding: 60px; color: #71717a; }
</style>
