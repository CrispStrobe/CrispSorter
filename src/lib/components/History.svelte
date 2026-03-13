<script lang="ts">
    import { onMount } from 'svelte';
    import { getSetting, saveSetting } from '../store';
    import { type BatchSession } from '../types';
    import { batchManager } from '../batch/store';
    import { Clock, Play, Trash2, Calendar, FileText } from 'lucide-svelte';

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
        onDone(); // Switch to batch view
    }

    async function handleDelete(id: string) {
        if (!confirm('Are you sure you want to delete this session?')) return;
        const saved = await getSetting('sessions', {}) as Record<string, BatchSession>;
        delete saved[id];
        await saveSetting('sessions', saved);
        await refreshHistory();
    }

    let { onResumeBatch } = $props<{ onResumeBatch: () => void }>();
</script>

<div class="history-container">
    <div class="header">
        <h1>Batch History</h1>
        <p>Resume previous sorting sessions or review results.</p>
    </div>

    <div class="sessions-list">
        {#each sessions as session}
            <div class="session-card">
                <div class="session-info">
                    <div class="session-main">
                        <Clock size={16} />
                        <strong>{new Date(session.startTime).toLocaleString()}</strong>
                        <span class="badge" class:completed={session.status === 'completed'}>
                            {session.status}
                        </span>
                    </div>
                    <div class="session-details">
                        <span><FileText size={14} /> {session.items.length} files</span>
                        <span><Calendar size={14} /> ID: {session.id.substring(0, 8)}</span>
                    </div>
                </div>
                <div class="session-actions">
                    <button class="resume-btn" onclick={() => handleResume(session.id, onResumeBatch)}>
                        <Play size={16} /> Resume
                    </button>
                    <button class="delete-btn" onclick={() => handleDelete(session.id)}>
                        <Trash2 size={16} />
                    </button>
                </div>
            </div>
        {:else}
            <div class="empty-state">
                <Clock size={48} />
                <p>No history found. Start a new batch to see it here.</p>
            </div>
        {/each}
    </div>
</div>

<style>
    .history-container {
        padding: 40px;
        height: 100%;
        overflow-y: auto;
        background: #f9fafb;
    }

    .header {
        margin-bottom: 32px;
    }

    h1 { font-size: 1.875rem; font-weight: 700; margin: 0 0 8px; }
    p { color: #6b7280; margin: 0; }

    .sessions-list {
        display: flex;
        flex-direction: column;
        gap: 16px;
        max-width: 800px;
    }

    .session-card {
        background: white;
        border: 1px solid #e5e7eb;
        border-radius: 12px;
        padding: 20px;
        display: flex;
        justify-content: space-between;
        align-items: center;
        transition: transform 0.2s, box-shadow 0.2s;
    }

    .session-card:hover {
        transform: translateY(-2px);
        box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1);
    }

    .session-info {
        display: flex;
        flex-direction: column;
        gap: 8px;
    }

    .session-main {
        display: flex;
        align-items: center;
        gap: 10px;
        font-size: 1rem;
    }

    .badge {
        font-size: 0.75rem;
        padding: 2px 8px;
        background: #fef3c7;
        color: #92400e;
        border-radius: 9999px;
        text-transform: uppercase;
        font-weight: 600;
    }

    .badge.completed { background: #dcfce7; color: #166534; }

    .session-details {
        display: flex;
        gap: 20px;
        font-size: 0.875rem;
        color: #6b7280;
    }

    .session-details span { display: flex; align-items: center; gap: 4px; }

    .session-actions {
        display: flex;
        gap: 12px;
    }

    .resume-btn {
        background: #3b82f6;
        color: white;
        border: none;
        padding: 8px 16px;
        border-radius: 8px;
        font-weight: 600;
        cursor: pointer;
        display: flex;
        align-items: center;
        gap: 8px;
    }

    .delete-btn {
        background: #fee2e2;
        color: #991b1b;
        border: none;
        padding: 8px;
        border-radius: 8px;
        cursor: pointer;
    }

    .empty-state {
        text-align: center;
        padding: 60px;
        color: #9ca3af;
    }

    @media (prefers-color-scheme: dark) {
        .history-container { background: #09090b; }
        .session-card { background: #18181b; border-color: #27272a; }
        .session-main { color: #f9fafb; }
        .session-details { color: #a1a1aa; }
        .delete-btn { background: #450a0a; color: #fecaca; }
    }
</style>
