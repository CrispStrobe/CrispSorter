<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';

    type Pair = {
        id: string;
        local_root: string;
        drive_id: string;
        remote_root: string;
        mode: string;
        enabled: boolean;
    };
    type Side = { size: number; mtime_unix: number | null } | null;
    type Row = {
        relative_path: string;
        local: Side;
        remote: Side;
        action: string;
    };
    type Policy = 'newest_wins' | 'local_wins' | 'remote_wins' | 'keep_both' | 'manual';

    let pairs = $state<Pair[]>([]);
    let selectedId = $state('');
    let policy = $state<Policy>('manual');
    let rows = $state<Row[]>([]);
    let loading = $state(false);
    let error = $state('');

    async function refreshPairs() {
        try {
            pairs = await invoke<Pair[]>('sync_pair_list');
            if (!selectedId && pairs.length > 0) selectedId = pairs[0].id;
        } catch (e) {
            error = `Could not load sync pairs: ${String(e)}`;
        }
    }

    async function compare() {
        if (!selectedId) return;
        loading = true;
        error = '';
        try {
            rows = await invoke<Row[]>('sync_pair_compare', { id: selectedId, policy });
        } catch (e) {
            error = `Could not compare sync pair: ${String(e)}`;
            rows = [];
        } finally {
            loading = false;
        }
    }

    function formatSide(side: Side): string {
        if (!side) return '—';
        return `${side.size.toLocaleString()} B${side.mtime_unix == null ? '' : ` · ${new Date(side.mtime_unix * 1000).toLocaleString()}`}`;
    }

    onMount(() => { void refreshPairs(); });
</script>

<div class="sync-pair-panel">
    <div class="panel-heading">
        <div>
            <strong>Sync-pair comparison</strong>
            <p class="hint">Read-only metadata comparison. No files are changed.</p>
        </div>
        <button type="button" onclick={refreshPairs}>Refresh</button>
    </div>
    {#if pairs.length === 0}
        <p class="hint">No sync pairs configured.</p>
    {:else}
        <div class="controls">
            <select bind:value={selectedId} aria-label="Sync pair">
                {#each pairs as pair (pair.id)}
                    <option value={pair.id}>{pair.id} · {pair.local_root} ↔ {pair.remote_root}</option>
                {/each}
            </select>
            <select bind:value={policy} aria-label="Comparison policy">
                <option value="manual">Manual review</option>
                <option value="newest_wins">Newest wins</option>
                <option value="local_wins">Local wins</option>
                <option value="remote_wins">Remote wins</option>
                <option value="keep_both">Keep both</option>
            </select>
            <button type="button" onclick={compare} disabled={loading}>{loading ? 'Comparing…' : 'Compare'}</button>
        </div>
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
    {#if rows.length > 0}
        <div class="comparison-table-wrap">
            <table>
                <thead><tr><th>Path</th><th>Local</th><th>Remote</th><th>Result</th></tr></thead>
                <tbody>
                    {#each rows as row (row.relative_path)}
                        <tr>
                            <td title={row.relative_path}>{row.relative_path}</td>
                            <td>{formatSide(row.local)}</td>
                            <td>{formatSide(row.remote)}</td>
                            <td><span class="action action-{row.action}">{row.action.replaceAll('_', ' ')}</span></td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        </div>
    {:else if selectedId && !loading}
        <p class="hint">Run Compare to inspect local and remote metadata.</p>
    {/if}
</div>

<style>
    .sync-pair-panel { margin: 14px 0; padding: 12px; border: 1px solid var(--color-border, #444); border-radius: 6px; }
    .panel-heading, .controls { display: flex; align-items: center; gap: 8px; }
    .panel-heading { justify-content: space-between; margin-bottom: 10px; }
    .hint { color: #a1a1aa; font-size: .8rem; margin: 4px 0; }
    .controls { flex-wrap: wrap; }
    .controls select { min-width: 150px; max-width: 100%; }
    .controls select:first-child { flex: 1 1 280px; }
    .comparison-table-wrap { max-height: 300px; overflow: auto; margin-top: 10px; }
    table { width: 100%; border-collapse: collapse; font-size: .78rem; }
    th, td { padding: 6px; border-top: 1px solid var(--color-border, #333); text-align: left; vertical-align: top; }
    td:first-child { max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .action { font-weight: 600; text-transform: capitalize; }
    .action-unchanged { color: #86efac; } .action-manual_review { color: #fbbf24; }
    .action-use_remote, .action-remote_only { color: #93c5fd; } .action-use_local, .action-local_only { color: #c4b5fd; }
    .error { color: #fca5a5; font-size: .82rem; }
</style>
