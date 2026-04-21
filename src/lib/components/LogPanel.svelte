<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { listen, type UnlistenFn } from '@tauri-apps/api/event';

    interface LogEntry {
        ts: number;
        level: string;
        msg: string;
    }

    let logs = $state<LogEntry[]>([]);
    let autoscroll = $state(true);
    let levelFilter = $state('all');
    let searchFilter = $state('');
    let logContainer: HTMLElement | null = null;
    let unlisten: UnlistenFn | null = null;

    const MAX_UI_LOGS = 1000;

    let filteredLogs = $derived(logs.filter(l => {
        if (levelFilter !== 'all' && l.level !== levelFilter) return false;
        if (searchFilter && !l.msg.toLowerCase().includes(searchFilter.toLowerCase())) return false;
        return true;
    }));

    function formatTime(ts: number): string {
        const d = new Date(ts * 1000);
        return d.toLocaleTimeString('de-DE', { hour: '2-digit', minute: '2-digit', second: '2-digit' })
            + '.' + String(d.getMilliseconds()).padStart(3, '0');
    }

    function levelClass(level: string): string {
        if (level === 'error') return 'log-error';
        if (level === 'warn') return 'log-warn';
        return 'log-info';
    }

    function scrollToBottom() {
        if (autoscroll && logContainer) {
            requestAnimationFrame(() => {
                logContainer!.scrollTop = logContainer!.scrollHeight;
            });
        }
    }

    function clearLogs() {
        logs = [];
    }

    function copyLogs() {
        const text = filteredLogs
            .map(l => `[${formatTime(l.ts)}] [${l.level}] ${l.msg}`)
            .join('\n');
        navigator.clipboard.writeText(text);
    }

    onMount(async () => {
        // Load existing logs from ring buffer
        try {
            const existing = await invoke<LogEntry[]>('get_logs');
            logs = existing;
            scrollToBottom();
        } catch (e) {
            console.warn('[LogPanel] get_logs failed:', e);
        }

        // Listen for new log events
        unlisten = await listen<LogEntry>('app-log', (event) => {
            logs.push(event.payload);
            if (logs.length > MAX_UI_LOGS) {
                logs = logs.slice(-MAX_UI_LOGS);
            }
            // Also capture console-level frontend logs
            scrollToBottom();
        });
    });

    onDestroy(() => {
        unlisten?.();
    });
</script>

<div class="log-panel">
    <div class="log-toolbar">
        <span class="log-title">Logs</span>
        <select bind:value={levelFilter} class="log-select">
            <option value="all">All</option>
            <option value="info">Info</option>
            <option value="warn">Warn</option>
            <option value="error">Error</option>
        </select>
        <input type="text" bind:value={searchFilter} placeholder="Filter…" class="log-search" />
        <label class="log-autoscroll">
            <input type="checkbox" bind:checked={autoscroll} /> Auto-scroll
        </label>
        <button class="log-btn" onclick={copyLogs} title="Copy logs to clipboard">Copy</button>
        <button class="log-btn" onclick={clearLogs} title="Clear logs">Clear</button>
        <span class="log-count">{filteredLogs.length} entries</span>
    </div>
    <div class="log-entries" bind:this={logContainer}>
        {#each filteredLogs as entry (entry.ts + entry.msg)}
            <div class="log-line {levelClass(entry.level)}">
                <span class="log-ts">{formatTime(entry.ts)}</span>
                <span class="log-level">{entry.level.toUpperCase()}</span>
                <span class="log-msg">{entry.msg}</span>
            </div>
        {/each}
        {#if filteredLogs.length === 0}
            <div class="log-empty">No log entries{levelFilter !== 'all' ? ` matching "${levelFilter}"` : ''}.</div>
        {/if}
    </div>
</div>

<style>
    .log-panel {
        display: flex;
        flex-direction: column;
        height: 100%;
        font-family: 'SF Mono', 'Fira Code', 'Consolas', monospace;
        font-size: 12px;
        background: var(--bg-secondary, #1a1a2e);
        color: var(--text-primary, #e0e0e0);
    }

    .log-toolbar {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 6px 10px;
        border-bottom: 1px solid var(--border-color, #333);
        flex-shrink: 0;
        flex-wrap: wrap;
    }

    .log-title {
        font-weight: 600;
        font-size: 13px;
        margin-right: 4px;
    }

    .log-select {
        padding: 2px 6px;
        font-size: 11px;
        background: var(--bg-primary, #0f0f23);
        color: var(--text-primary, #e0e0e0);
        border: 1px solid var(--border-color, #444);
        border-radius: 4px;
    }

    .log-search {
        padding: 2px 8px;
        font-size: 11px;
        width: 140px;
        background: var(--bg-primary, #0f0f23);
        color: var(--text-primary, #e0e0e0);
        border: 1px solid var(--border-color, #444);
        border-radius: 4px;
    }

    .log-autoscroll {
        font-size: 11px;
        display: flex;
        align-items: center;
        gap: 3px;
        cursor: pointer;
    }

    .log-btn {
        padding: 2px 8px;
        font-size: 11px;
        background: var(--bg-primary, #0f0f23);
        color: var(--text-secondary, #aaa);
        border: 1px solid var(--border-color, #444);
        border-radius: 4px;
        cursor: pointer;
    }
    .log-btn:hover {
        background: var(--border-color, #444);
    }

    .log-count {
        font-size: 11px;
        color: var(--text-secondary, #888);
        margin-left: auto;
    }

    .log-entries {
        flex: 1;
        overflow-y: auto;
        padding: 4px 0;
        min-height: 0;
    }

    .log-line {
        display: flex;
        gap: 8px;
        padding: 1px 10px;
        line-height: 1.5;
        white-space: pre-wrap;
        word-break: break-word;
    }
    .log-line:hover {
        background: rgba(255, 255, 255, 0.04);
    }

    .log-ts {
        color: var(--text-secondary, #666);
        flex-shrink: 0;
        min-width: 85px;
    }

    .log-level {
        flex-shrink: 0;
        min-width: 42px;
        font-weight: 600;
    }

    .log-info .log-level { color: #6ec6ff; }
    .log-warn .log-level { color: #ffb74d; }
    .log-error .log-level { color: #ef5350; }

    .log-error { background: rgba(239, 83, 80, 0.08); }
    .log-warn { background: rgba(255, 183, 77, 0.06); }

    .log-msg {
        flex: 1;
    }

    .log-empty {
        padding: 20px;
        text-align: center;
        color: var(--text-secondary, #666);
    }
</style>
