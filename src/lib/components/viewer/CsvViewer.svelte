<script lang="ts">
    import { readTextFile } from '@tauri-apps/plugin-fs';
    import { i18n } from '$lib/i18n.svelte';

    let { path = '' }: { path: string } = $props();

    const MAX_BYTES = 1024 * 1024; // 1 MB text limit
    const MAX_ROWS = 500;

    let rows = $state<string[][]>([]);
    let totalRows = $state(0);
    let truncated = $state(false);
    let loading = $state(true);
    let error = $state('');

    $effect(() => {
        const p = path;
        if (!p) return;
        loading = true;
        error = '';
        rows = [];
        totalRows = 0;
        truncated = false;
        let cancelled = false;

        (async () => {
            try {
                let text = await readTextFile(p);
                if (cancelled) return;
                if (text.length > MAX_BYTES) {
                    text = text.slice(0, MAX_BYTES);
                    truncated = true;
                }

                const lines = text.split(/\r?\n/).filter(l => l.trim().length > 0);
                totalRows = lines.length;

                // Auto-detect delimiter: tab-separated vs comma-separated
                const firstLine = lines[0] ?? '';
                const delim = firstLine.includes('\t') ? '\t' : ',';

                const parsed: string[][] = [];
                for (let i = 0; i < Math.min(lines.length, MAX_ROWS); i++) {
                    parsed.push(splitCsvLine(lines[i], delim));
                }
                if (lines.length > MAX_ROWS) truncated = true;

                rows = parsed;
                loading = false;
            } catch (e: any) {
                if (!cancelled) { error = e.message ?? String(e); loading = false; }
            }
        })();

        return () => { cancelled = true; };
    });

    /** Naive CSV split that respects double-quoted fields. */
    function splitCsvLine(line: string, delim: string): string[] {
        const cells: string[] = [];
        let i = 0;
        while (i <= line.length) {
            if (i >= line.length) { cells.push(''); break; }
            if (line[i] === '"') {
                let j = i + 1;
                let val = '';
                while (j < line.length) {
                    if (line[j] === '"') {
                        if (j + 1 < line.length && line[j + 1] === '"') {
                            val += '"'; j += 2;
                        } else {
                            j++; break;
                        }
                    } else {
                        val += line[j]; j++;
                    }
                }
                cells.push(val);
                i = j + 1; // skip delimiter
            } else {
                const next = line.indexOf(delim, i);
                if (next < 0) { cells.push(line.slice(i)); break; }
                cells.push(line.slice(i, next));
                i = next + 1;
            }
        }
        return cells;
    }
</script>

<div class="csv-viewer">
    {#if loading}
        <p class="cv-msg">{i18n.t.viewer.loading}</p>
    {:else if error}
        <p class="cv-msg cv-error">{error}</p>
    {:else if rows.length === 0}
        <p class="cv-msg">{i18n.t.viewer.unsupported}</p>
    {:else}
        <div class="cv-info">
            <span>{totalRows} {i18n.t.viewer.rows}{#if truncated} ({i18n.t.viewer.truncated}){/if}</span>
        </div>
        <div class="cv-scroll">
            <table class="cv-table">
                <thead>
                    <tr>
                        {#each rows[0] as cell, ci (ci)}
                            <th>{cell}</th>
                        {/each}
                    </tr>
                </thead>
                <tbody>
                    {#each rows.slice(1) as row, ri (ri)}
                        <tr>
                            {#each row as cell, ci (ci)}
                                <td>{cell}</td>
                            {/each}
                        </tr>
                    {/each}
                </tbody>
            </table>
        </div>
    {/if}
</div>

<style>
    .csv-viewer { display: flex; flex-direction: column; flex: 1; min-height: 0; }
    .cv-info {
        padding: 4px 10px;
        font-size: 0.72rem;
        color: #71717a;
        background: #27272a;
        border-bottom: 1px solid #3f3f46;
        flex-shrink: 0;
    }
    .cv-scroll { flex: 1; overflow: auto; background: #0a0a0c; }
    .cv-table {
        border-collapse: collapse;
        font-size: 0.78rem;
        font-family: 'SF Mono', 'Cascadia Code', monospace;
        min-width: 100%;
    }
    .cv-table th {
        position: sticky;
        top: 0;
        background: #27272a;
        color: #e4e4e7;
        font-weight: 600;
        padding: 5px 10px;
        border: 1px solid #3f3f46;
        text-align: left;
        white-space: nowrap;
    }
    .cv-table td {
        padding: 4px 10px;
        border: 1px solid #27272a;
        color: #d4d4d8;
        white-space: nowrap;
        max-width: 300px;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .cv-table tbody tr:hover td { background: #18181b; }
    .cv-msg { padding: 24px 16px; text-align: center; color: #71717a; font-size: 0.85rem; margin: 0; }
    .cv-error { color: #f87171; }
</style>
