<script lang="ts">
    /*
     * Find Duplicates tab — source vs N destinations against either
     * folders or registered .caf catalogs.
     *
     * Phase 3 of PLAN P6. Wraps the `catalog_find_duplicates` and
     * `catalog_generate_deletion_script` Tauri commands. The backend's
     * size-bucket fast path means name-and-size strategy is essentially
     * free even on million-entry catalogs; hash strategies pay only
     * for the size-collision candidates.
     */

    import { invoke } from '@tauri-apps/api/core';
    import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
    import { writeTextFile } from '@tauri-apps/plugin-fs';
    import {
        FolderOpen, FilePlus, X, Search, Loader2, Download,
        AlertTriangle
    } from 'lucide-svelte';
    import { i18n } from '$lib/i18n.svelte';

    // ── Types ──────────────────────────────────────────────────────────────────

    interface FileEntry {
        path: string;
        size: number;
        mtime: number;
        hash: string | null;
    }

    interface DuplicateMatch {
        source: FileEntry;
        destinations: FileEntry[];
    }

    type Strategy = 'name-and-size' | 'hash:md5' | 'hash:sha1' | 'hash:sha256';
    type ScriptFormat = 'bash' | 'batch' | 'powershell';
    type DeletionTarget = 'destinations' | 'source';

    // ── State ──────────────────────────────────────────────────────────────────

    let source = $state<string>('');
    let destinations = $state<string[]>([]);
    let strategy = $state<Strategy>('name-and-size');
    let scanning = $state(false);
    let error = $state('');
    let matches = $state<DuplicateMatch[]>([]);
    let selected = $state<Set<number>>(new Set());

    // Script generation
    let scriptFormat = $state<ScriptFormat>('bash');
    let scriptTarget = $state<DeletionTarget>('destinations');
    let generatedScript = $state('');

    // ── Pickers ────────────────────────────────────────────────────────────────

    async function pickPath(forSource: boolean, idx?: number) {
        // Allow either a folder OR a .caf file. The backend auto-detects
        // by extension + file/dir test, so the user picks what they have.
        const folder = await openDialog({
            directory: true,
            multiple: false,
            title: forSource ? i18n.t.duplicates.picker_source_folder : i18n.t.duplicates.picker_destination_folder,
        });
        if (typeof folder === 'string') {
            applyPick(folder, forSource, idx);
            return;
        }
    }

    async function pickCaf(forSource: boolean, idx?: number) {
        const f = await openDialog({
            multiple: false,
            filters: [{ name: 'Cathy Catalog', extensions: ['caf'] }],
            title: forSource ? i18n.t.duplicates.picker_source_caf : i18n.t.duplicates.picker_destination_caf,
        });
        if (typeof f === 'string') applyPick(f, forSource, idx);
    }

    function applyPick(path: string, forSource: boolean, idx?: number) {
        if (forSource) source = path;
        else if (idx !== undefined) destinations[idx] = path;
        else destinations = [...destinations, path];
    }

    function addDestination() {
        destinations = [...destinations, ''];
    }
    function removeDestination(idx: number) {
        destinations = destinations.filter((_, i) => i !== idx);
    }

    // ── Run dedup ──────────────────────────────────────────────────────────────

    async function runDedup() {
        if (!source.trim()) {
            error = 'Pick a source folder or .caf';
            return;
        }
        const dests = destinations.filter(d => d.trim());
        if (dests.length === 0) {
            error = 'Add at least one destination';
            return;
        }
        scanning = true;
        error = '';
        matches = [];
        selected = new Set();
        generatedScript = '';
        try {
            matches = await invoke<DuplicateMatch[]>('catalog_find_duplicates', {
                source: source.trim(),
                destinations: dests,
                strategy,
            });
            // Default-select all destinations for the script (user can
            // unselect individual rows).
            selected = new Set(matches.flatMap((_, i) => [i]));
        } catch (e: any) {
            error = String(e);
        } finally {
            scanning = false;
        }
    }

    // ── Script generation ──────────────────────────────────────────────────────

    async function generateScript() {
        const selectedMatches = matches.filter((_, i) => selected.has(i));
        if (selectedMatches.length === 0) {
            error = 'Select at least one match row';
            return;
        }
        try {
            generatedScript = await invoke<string>('catalog_generate_deletion_script', {
                matches: selectedMatches,
                format: scriptFormat,
                target: scriptTarget,
            });
        } catch (e: any) {
            error = String(e);
        }
    }

    async function saveScript() {
        if (!generatedScript) return;
        const ext = scriptFormat === 'bash' ? 'sh'
                  : scriptFormat === 'powershell' ? 'ps1'
                  : 'bat';
        const out = await saveDialog({
            defaultPath: `crispsorter-cleanup-${Date.now()}.${ext}`,
            filters: [{ name: 'Script', extensions: [ext] }],
        });
        if (!out) return;
        await writeTextFile(out, generatedScript);
    }

    function toggleSelect(idx: number) {
        const next = new Set(selected);
        if (next.has(idx)) next.delete(idx);
        else next.add(idx);
        selected = next;
    }

    // ── Helpers ────────────────────────────────────────────────────────────────

    function formatSize(bytes: number): string {
        if (bytes < 1024) return `${bytes} B`;
        const units = ['KB', 'MB', 'GB', 'TB'];
        let n = bytes / 1024;
        let i = 0;
        while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
        return `${n.toFixed(n < 10 ? 1 : 0)} ${units[i]}`;
    }

    const totalToDelete = $derived.by(() => {
        let bytes = 0;
        for (let i = 0; i < matches.length; i++) {
            if (!selected.has(i)) continue;
            if (scriptTarget === 'source') bytes += matches[i].source.size;
            else for (const d of matches[i].destinations) bytes += d.size;
        }
        return bytes;
    });
</script>

<div class="dupes-tab">
    <header>
        <h1>{i18n.t.duplicates.title}</h1>
        <p class="subtitle">{i18n.t.duplicates.subtitle}</p>
    </header>

    <section class="config">
        <div class="row">
            <span class="row-label">{i18n.t.duplicates.source}</span>
            <input
                type="text"
                bind:value={source}
                placeholder=".caf / folder"
                class="path-input"
            />
            <button class="btn small" onclick={() => pickPath(true)}>
                <FolderOpen size={14} /> {i18n.t.indexIngest.tab_sources}
            </button>
            <button class="btn small" onclick={() => pickCaf(true)}>
                <FilePlus size={14} /> .caf
            </button>
        </div>

        {#each destinations as dest, i}
            <div class="row">
                <span class="row-label">{i === 0 ? i18n.t.duplicates.destinations : ''}</span>
                <input
                    type="text"
                    bind:value={destinations[i]}
                    placeholder=".caf / folder"
                    class="path-input"
                />
                <button class="btn small" onclick={() => pickPath(false, i)}>
                    <FolderOpen size={14} /> {i18n.t.indexIngest.tab_sources}
                </button>
                <button class="btn small" onclick={() => pickCaf(false, i)}>
                    <FilePlus size={14} /> .caf
                </button>
                <button class="icon-btn" onclick={() => removeDestination(i)} title={i18n.t.duplicates.remove}>
                    <X size={14} />
                </button>
            </div>
        {/each}

        <div class="row">
            <span class="row-label"></span>
            <button class="btn small" onclick={addDestination}>+ {i18n.t.duplicates.destinations}</button>
        </div>

        <div class="row">
            <span class="row-label">{i18n.t.duplicates.match_mode}</span>
            <select bind:value={strategy} class="strategy-select">
                <option value="name-and-size">{i18n.t.duplicates.match_size_name}</option>
                <option value="hash:md5">MD5 — {i18n.t.duplicates.match_size_hash}</option>
                <option value="hash:sha1">SHA-1 — {i18n.t.duplicates.match_size_hash}</option>
                <option value="hash:sha256">SHA-256 — {i18n.t.duplicates.match_size_hash}</option>
            </select>
        </div>

        <div class="row">
            <span class="row-label"></span>
            <button class="btn primary" onclick={runDedup} disabled={scanning}>
                {#if scanning}
                    <Loader2 size={14} class="spin" />
                    {i18n.t.duplicates.running}
                {:else}
                    <Search size={14} />
                    {i18n.t.duplicates.find}
                {/if}
            </button>
        </div>
    </section>

    {#if error}
        <div class="error"><AlertTriangle size={14} /> {i18n.t.duplicates.error.replace('{message}', error)}</div>
    {/if}

    {#if matches.length > 0}
        <section class="results">
            <header>
                <h2>{i18n.t.duplicates.matches_count.replace('{count}', matches.length.toLocaleString())}</h2>
                <span class="muted">{i18n.t.duplicates.selected_count.replace('{count}', selected.size.toString())}</span>
            </header>

            <table class="match-table">
                <thead>
                    <tr>
                        <th><input
                            type="checkbox"
                            checked={selected.size === matches.length}
                            onchange={() => {
                                selected = selected.size === matches.length
                                    ? new Set()
                                    : new Set(matches.map((_, i) => i));
                            }}
                        /></th>
                        <th>{i18n.t.duplicates.col_source}</th>
                        <th>{i18n.t.duplicates.col_size}</th>
                        <th>{i18n.t.duplicates.col_destinations}</th>
                    </tr>
                </thead>
                <tbody>
                    {#each matches as m, i (m.source.path)}
                        <tr>
                            <td>
                                <input
                                    type="checkbox"
                                    checked={selected.has(i)}
                                    onchange={() => toggleSelect(i)}
                                />
                            </td>
                            <td class="path-cell" title={m.source.path}>{m.source.path}</td>
                            <td>{formatSize(m.source.size)}</td>
                            <td>
                                <ul class="dest-list">
                                    {#each m.destinations as d}
                                        <li title={d.path}>{d.path}</li>
                                    {/each}
                                </ul>
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>

            <section class="script-builder">
                <h3>{i18n.t.duplicates.generate_script}</h3>
                <div class="row">
                    <span class="row-label">{i18n.t.duplicates.script_format}</span>
                    <select bind:value={scriptFormat}>
                        <option value="bash">{i18n.t.duplicates.script_format_bash}</option>
                        <option value="batch">{i18n.t.duplicates.script_format_batch}</option>
                        <option value="powershell">{i18n.t.duplicates.script_format_powershell}</option>
                    </select>
                    <span class="row-label" style="margin-left: 12px;">{i18n.t.duplicates.script_delete}</span>
                    <select bind:value={scriptTarget}>
                        <option value="destinations">{i18n.t.duplicates.script_target_destinations}</option>
                        <option value="source">{i18n.t.duplicates.script_target_source}</option>
                    </select>
                    <button class="btn small primary" onclick={generateScript}>
                        {i18n.t.duplicates.script_generate_btn}
                    </button>
                </div>
                <p class="muted" style="margin: 4px 0;">
                    {@html i18n.t.duplicates.space_freed.replace('{size}', `<strong>${formatSize(totalToDelete)}</strong>`)}
                </p>
                {#if generatedScript}
                    <textarea readonly class="script-output">{generatedScript}</textarea>
                    <button class="btn small" onclick={saveScript}>
                        <Download size={14} /> {i18n.t.duplicates.save_script}
                    </button>
                {/if}
            </section>
        </section>
    {:else if scanning === false && !error}
        <div class="empty">
            <Search size={32} />
            <p>{i18n.t.duplicates.empty_pick_run}</p>
        </div>
    {/if}
</div>

<style>
    .dupes-tab {
        padding: 16px 24px;
        max-width: 1200px;
    }
    h1 { margin: 0 0 4px; font-size: 1.4rem; }
    .subtitle { color: var(--text-muted, #666); margin: 0 0 16px; font-size: 0.85rem; }

    .config {
        background: var(--surface, #fafafa);
        padding: 12px;
        border-radius: 6px;
        margin-bottom: 16px;
        border: 1px solid var(--border, #eee);
    }
    .row {
        display: flex;
        align-items: center;
        gap: 8px;
        margin-bottom: 8px;
    }
    .row-label {
        min-width: 90px;
        font-weight: 600;
        font-size: 0.8rem;
        color: var(--text-muted, #666);
    }
    .path-input {
        flex: 1;
        padding: 4px 8px;
        border: 1px solid var(--border, #ddd);
        border-radius: 4px;
        font-size: 0.85rem;
        font-family: var(--mono, monospace);
    }
    .strategy-select { padding: 4px 8px; border: 1px solid var(--border, #ddd); border-radius: 4px; font-size: 0.85rem; }

    .btn {
        display: inline-flex;
        align-items: center;
        gap: 6px;
        padding: 6px 12px;
        background: var(--surface, #f5f5f5);
        border: 1px solid var(--border, #ddd);
        border-radius: 4px;
        cursor: pointer;
        font-size: 0.85rem;
    }
    .btn.small { padding: 4px 8px; font-size: 0.8rem; }
    .btn:hover { background: var(--surface-hover, #ebebeb); }
    .btn.primary { background: #0066cc; color: white; border-color: #0055aa; }
    .btn.primary:hover { background: #0055aa; }
    .btn:disabled { opacity: 0.5; cursor: not-allowed; }
    :global(.spin) { animation: spin 1s linear infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }

    .icon-btn {
        background: none; border: none; cursor: pointer;
        padding: 4px; opacity: 0.6;
    }
    .icon-btn:hover { opacity: 1; }

    .error {
        display: flex; align-items: center; gap: 6px;
        padding: 8px 12px;
        background: #ffe5e5; color: #cc0000;
        border: 1px solid #ffcccc; border-radius: 4px;
        margin-bottom: 12px; font-size: 0.85rem;
    }

    .empty {
        padding: 48px 24px;
        text-align: center;
        color: var(--text-muted, #666);
    }
    .empty p { margin: 4px 0; }

    .results header {
        display: flex; align-items: center; gap: 12px; margin: 12px 0 8px;
    }
    .results h2 { margin: 0; font-size: 1rem; }
    .muted { color: var(--text-muted, #666); font-size: 0.85rem; }

    .match-table {
        width: 100%; border-collapse: collapse; font-size: 0.85rem;
    }
    .match-table th, .match-table td {
        padding: 6px 8px; text-align: left;
        border-bottom: 1px solid var(--border, #eee);
    }
    .match-table th {
        background: var(--surface, #fafafa);
        font-weight: 600; font-size: 0.75rem;
        text-transform: uppercase; color: var(--text-muted, #666);
    }
    .path-cell {
        max-width: 320px; overflow: hidden;
        text-overflow: ellipsis; white-space: nowrap;
    }
    .dest-list { margin: 0; padding-left: 16px; }
    .dest-list li {
        max-width: 480px; overflow: hidden;
        text-overflow: ellipsis; white-space: nowrap;
        font-family: var(--mono, monospace); font-size: 0.78rem;
    }

    .script-builder {
        margin-top: 24px;
        padding-top: 12px;
        border-top: 1px solid var(--border, #ddd);
    }
    .script-builder h3 { margin: 0 0 8px; font-size: 0.95rem; }
    .script-output {
        width: 100%;
        min-height: 180px;
        font-family: var(--mono, monospace);
        font-size: 0.78rem;
        border: 1px solid var(--border, #ddd);
        border-radius: 4px;
        padding: 8px;
        background: var(--surface, #fafafa);
        margin: 8px 0;
        white-space: pre;
    }
</style>
