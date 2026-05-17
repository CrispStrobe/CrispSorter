<script lang="ts">
    import { onMount, onDestroy } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { listen, type UnlistenFn } from '@tauri-apps/api/event';
    import { frontendLogs, type FrontendLogEntry, getLogVerbosity, setLogVerbosity, type LogLevel } from '../log';
    import { saveSetting } from '../store';
    import { i18n } from '../i18n.svelte';

    interface LogEntry {
        ts: number;
        level: string;
        msg: string;
        src?: 'be' | 'fe';
    }

    let backendLogs = $state<LogEntry[]>([]);
    let feLogs = $state<FrontendLogEntry[]>([]);
    let autoscroll = $state(true);
    let levelFilter = $state('all');
    let searchFilter = $state('');
    /** Global log VERBOSITY (what gets PUSHED into the log store).
     *  Distinct from `levelFilter` above which only controls what's
     *  SHOWN — verbosity actually decides whether `info` / `debug`
     *  entries are emitted at all.  Mirrors the Settings → General
     *  → Log-Verbosity dropdown but reachable without leaving the
     *  current screen. */
    let verbosity = $state<LogLevel>(getLogVerbosity());
    let logContainer: HTMLElement | null = null;
    let unlisten: UnlistenFn | null = null;
    let unsubFe: (() => void) | null = null;

    const MAX_UI_LOGS = 1000;

    // Merge backend + frontend logs, sorted by timestamp
    let allLogs = $derived(
        [...backendLogs.map(l => ({ ...l, src: 'be' as const })),
         ...feLogs.map(l => ({ ...l, src: 'fe' as const }))]
            .sort((a, b) => a.ts - b.ts)
            .slice(-MAX_UI_LOGS)
    );

    let filteredLogs = $derived(allLogs.filter(l => {
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
        backendLogs = [];
        feLogs = [];
    }

    function copyLogs() {
        const text = filteredLogs
            .map(l => `[${formatTime(l.ts)}] [${l.src ?? '??'}] [${l.level}] ${l.msg}`)
            .join('\n');
        navigator.clipboard.writeText(text);
    }

    onMount(async () => {
        try {
            const existing = await invoke<LogEntry[]>('get_logs');
            backendLogs = existing;
            scrollToBottom();
        } catch (e) {
            console.warn('[LogPanel] get_logs failed:', e);
        }

        unlisten = await listen<LogEntry>('app-log', (event) => {
            backendLogs.push(event.payload);
            if (backendLogs.length > MAX_UI_LOGS) {
                backendLogs = backendLogs.slice(-MAX_UI_LOGS);
            }
            scrollToBottom();
        });

        // Subscribe to frontend log store
        unsubFe = frontendLogs.subscribe(v => {
            feLogs = v;
            scrollToBottom();
        });
    });

    onDestroy(() => {
        unlisten?.();
        unsubFe?.();
    });
</script>

<div class="log-panel">
    <div class="log-toolbar">
        <span class="log-title">{i18n.t.logs.title}</span>
        <select bind:value={levelFilter} class="log-select" title={i18n.t.logs.filter_placeholder}>
            <option value="all">{i18n.t.logs.level_all}</option>
            <option value="info">{i18n.t.logs.level_info}</option>
            <option value="warn">{i18n.t.logs.level_warn}</option>
            <option value="error">{i18n.t.logs.level_error}</option>
        </select>
        <!-- Verbosity selector — controls what GETS LOGGED, not what's
             shown.  Persists via tauri-plugin-store so a relaunch
             keeps the user's choice.  No keyboard listener wired —
             change is observed via `onchange` and propagates
             immediately. -->
        <select bind:value={verbosity} class="log-select"
                onchange={async () => {
                    setLogVerbosity(verbosity);
                    try { await saveSetting('logVerbosity', verbosity); } catch {}
                }}
                title="Verbosity (silent / error / info / debug)">
            <option value="silent">silent</option>
            <option value="error">error</option>
            <option value="info">info</option>
            <option value="debug">debug</option>
        </select>
        <input type="text" bind:value={searchFilter} placeholder={i18n.t.logs.filter_placeholder} class="log-search" />
        <label class="log-autoscroll">
            <input type="checkbox" bind:checked={autoscroll} /> {i18n.t.logs.autoscroll}
        </label>
        <button class="log-btn" onclick={copyLogs} title={i18n.t.logs.copy_title}>{i18n.t.logs.copy}</button>
        <button class="log-btn" onclick={clearLogs} title={i18n.t.logs.clear_title}>{i18n.t.logs.clear}</button>
        <span class="log-count">{i18n.t.logs.entries.replace('{count}', String(filteredLogs.length))}</span>
    </div>
    <div class="log-entries" bind:this={logContainer}>
        {#each filteredLogs as entry (entry.ts + entry.msg)}
            <div class="log-line {levelClass(entry.level)}">
                <span class="log-ts">{formatTime(entry.ts)}</span>
                <span class="log-src" class:log-src-fe={entry.src === 'fe'}>{entry.src ?? 'be'}</span>
                <span class="log-level">{entry.level.toUpperCase()}</span>
                <span class="log-msg">{entry.msg}</span>
            </div>
        {/each}
        {#if filteredLogs.length === 0}
            <div class="log-empty">{levelFilter !== 'all' ? i18n.t.logs.empty_filtered.replace('{filter}', levelFilter) : i18n.t.logs.empty}</div>
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

    .log-src {
        flex-shrink: 0;
        min-width: 22px;
        font-size: 10px;
        color: #555;
        font-weight: 600;
        text-transform: uppercase;
    }
    .log-src-fe { color: #7c6a2a; }

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
