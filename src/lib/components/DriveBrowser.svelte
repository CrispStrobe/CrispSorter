<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { openPath } from '@tauri-apps/plugin-opener';
    import { ChevronRight, Copy, Folder, FolderPlus, RefreshCw, Trash2 } from 'lucide-svelte';
    import {
        availableDriveActions,
        joinDrivePath,
        normalizeDrivePath,
        parentDrivePath,
        type DriveCapabilities,
    } from '$lib/drives/browser';
    import {
        loadDuplicateAudit,
        latestDuplicateDecision,
        saveDuplicateAudit,
        type DuplicateDecisionAudit,
    } from '$lib/drives/duplicateAudit';
    import { cloudDrivePanel, type ContextPanel, type DuplicateDecision } from '$lib/drives/panels';
    import { subscribeBrowserContext } from '$lib/drives/browserContext';

    type Drive = { id: string; label: string; kind: string };
    type Entry = { name: string; is_dir: boolean; size: number | null };
    type FileStat = { size: number; is_dir: boolean; mtime_unix: number | null };
    let drives = $state<Drive[]>([]);
    let driveId = $state('');
    let path = $state('/');
    let entries = $state<Entry[]>([]);
    let capabilities = $state<DriveCapabilities>({ create_dir: false, rename: false, move_path: false, copy: false, delete: false });
    let loading = $state(false);
    let error = $state('');
    let selected = $state<string | null>(null);
    let selectedStat = $state<FileStat | null>(null);
    let rightPanel = $state<ContextPanel | null>(null);
    let duplicateAudit = $state<DuplicateDecisionAudit[]>([]);
    const actions = $derived(availableDriveActions(capabilities, selected !== null));

    async function refresh() {
        if (!driveId) return;
        loading = true;
        error = '';
        try {
            entries = await invoke<Entry[]>('drive_list_dir', { driveId, path });
            capabilities = await invoke<DriveCapabilities>('drive_capabilities', { driveId });
            selected = null;
        } catch (e) {
            error = String(e);
            entries = [];
        } finally {
            loading = false;
        }
    }

    async function loadDrives() {
        try {
            drives = await invoke<Drive[]>('drive_list');
            if (!driveId && drives.length) driveId = drives[0].id;
            await refresh();
        } catch (e) { error = String(e); }
    }

    async function selectEntry(name: string) {
        selected = name;
        rightPanel = driveId ? cloudDrivePanel(driveId, joinDrivePath(path, name), name) : null;
        try {
            selectedStat = await invoke<FileStat>('drive_stat', { driveId, path: joinDrivePath(path, name) });
        } catch { selectedStat = null; }
    }

    function selectDrive(id: string) {
        driveId = id;
        path = '/';
        void refresh();
    }

    function open(entry: Entry) {
        if (entry.is_dir) {
            path = joinDrivePath(path, entry.name);
            void refresh();
        } else void selectEntry(entry.name);
    }

    async function createFolder() {
        const name = window.prompt('Folder name');
        if (!name?.trim() || !capabilities.create_dir) return;
        try { await invoke('drive_create_dir', { driveId, path: joinDrivePath(path, name.trim()) }); await refresh(); }
        catch (e) { error = String(e); }
    }

    async function mutate(kind: 'move' | 'copy') {
        if (!selected || (kind === 'move' ? !capabilities.move_path : !capabilities.copy)) return;
        const destination = window.prompt(`${kind === 'move' ? 'Move' : 'Copy'} to path`, joinDrivePath(path, selected));
        if (!destination?.trim()) return;
        try {
            await invoke(kind === 'move' ? 'drive_move_path' : 'drive_copy_path', {
                driveId, source: joinDrivePath(path, selected), destination: normalizeDrivePath(destination.trim())
            });
            await refresh();
        } catch (e) { error = String(e); }
    }

    async function renameSelected() {
        if (!selected || !capabilities.rename) return;
        const name = window.prompt('Rename to', selected);
        if (!name?.trim()) return;
        try {
            await invoke('drive_move_path', {
                driveId,
                source: joinDrivePath(path, selected),
                destination: joinDrivePath(path, name.trim()),
            });
            await refresh();
        } catch (e) { error = String(e); }
    }

    async function removeSelected() {
        if (!selected || !capabilities.delete || !window.confirm(`Delete ${selected}?`)) return;
        try { await invoke('drive_delete_path', { driveId, path: joinDrivePath(path, selected) }); await refresh(); }
        catch (e) { error = String(e); }
    }

    async function openDuplicateItem(itemPath: string) {
        try { await openPath(itemPath); }
        catch (e) { error = `Could not open duplicate: ${String(e)}`; }
    }

    async function copyDuplicatePath(itemPath: string) {
        try { await navigator.clipboard.writeText(itemPath); }
        catch (e) { error = `Could not copy duplicate path: ${String(e)}`; }
    }

    function setDuplicateDecision(decision: DuplicateDecision) {
        if (rightPanel?.source.kind !== 'DuplicateGroup') return;
        if (rightPanel.source.decision === decision) return;
        duplicateAudit = [...duplicateAudit, {
            groupId: rightPanel.source.groupId,
            previous: rightPanel.source.decision,
            next: decision,
            at: Date.now(),
        }];
        saveDuplicateAudit(duplicateAudit);
        rightPanel = { ...rightPanel, source: { ...rightPanel.source, decision } };
    }

    function undoDuplicateDecision() {
        const last = duplicateAudit.at(-1);
        if (!last || rightPanel?.source.kind !== 'DuplicateGroup' || rightPanel.source.groupId !== last.groupId) return;
        rightPanel = { ...rightPanel, source: { ...rightPanel.source, decision: last.previous } };
        duplicateAudit = duplicateAudit.slice(0, -1);
        saveDuplicateAudit(duplicateAudit);
    }

    function clearDuplicateAudit() {
        duplicateAudit = [];
        saveDuplicateAudit(duplicateAudit);
    }

    function decisionLabel(decision: DuplicateDecision): string {
        return decision.replace('_', ' ');
    }

    /// Label for a `PanelSource` variant that has no branch in the markup below.
    /// Every current variant is handled, so inside that `{:else}` the narrowed
    /// type is `never` and `never.kind` is a type error rather than a fallback.
    /// Taking the full union here keeps the defensive branch — which is the
    /// point of it, for whenever a seventh variant lands.
    function sourceKind(source: ContextPanel['source']): string {
        return source.kind;
    }

    onMount(() => {
        duplicateAudit = loadDuplicateAudit();
        const unsubscribe = subscribeBrowserContext((panel) => {
            if (panel.source.kind === 'DuplicateGroup') {
                const restored = latestDuplicateDecision(duplicateAudit, panel.source.groupId);
                rightPanel = restored
                    ? { ...panel, source: { ...panel.source, decision: restored } }
                    : panel;
            } else {
                rightPanel = panel;
            }
            if (panel.source.kind === 'CloudDrive') {
                driveId = panel.source.driveId;
                path = normalizeDrivePath(panel.source.path);
                void refresh();
            }
        });
        void loadDrives();
        return unsubscribe;
    });
</script>

<section class="drive-browser">
    <header class="browser-header">
        <div>
            <h2>Cloud files</h2>
            <p class="muted">Browse registered drives and act on the selected context.</p>
        </div>
        <div class="browser-controls">
            <select value={driveId} onchange={(event) => selectDrive((event.currentTarget as HTMLSelectElement).value)} aria-label="Drive">
                <option value="" disabled>Select drive</option>
                {#each drives as drive}<option value={drive.id}>{drive.label} ({drive.kind})</option>{/each}
            </select>
            <button class="icon-button" onclick={refresh} title="Refresh" disabled={loading}><RefreshCw size={16} /></button>
        </div>
    </header>

    <div class="browser-toolbar">
        {#each path.split('/').filter(Boolean) as segment, index}
            <button class="crumb" onclick={() => { path = normalizeDrivePath('/' + path.split('/').filter(Boolean).slice(0, index + 1).join('/')); void refresh(); }}>{segment}</button>
            <ChevronRight size={14} />
        {/each}
        {#if path !== '/'}<button class="crumb" onclick={() => { path = parentDrivePath(path); void refresh(); }}>parent</button>{:else}<span class="crumb current">root</span>{/if}
        <span class="toolbar-spacer"></span>
        <button onclick={createFolder} disabled={!actions.create_dir}><FolderPlus size={15} /> New folder</button>
        <button onclick={renameSelected} disabled={!actions.rename}>Rename</button>
        <button onclick={() => mutate('move')} disabled={!actions.move}>Move</button>
        <button onclick={() => mutate('copy')} disabled={!actions.copy}><Copy size={15} /> Copy</button>
        <button class="danger" onclick={removeSelected} disabled={!actions.delete}><Trash2 size={15} /> Delete</button>
    </div>

    {#if error}<div class="browser-error">{error}</div>{/if}
    {#if !drives.length && !loading}<div class="empty">No registered drives yet.</div>
    {:else if loading}<div class="empty">Loading…</div>
    {:else}<div class="entry-list">
        {#each entries as entry (entry.name)}
            <button class:selected={selected === entry.name} class="entry" onclick={() => open(entry)}>
                {#if entry.is_dir}<Folder size={17} />{:else}<span class="file-dot"></span>{/if}
                <span class="entry-name">{entry.name}</span>
                {#if entry.size !== null}<span class="entry-size">{entry.size.toLocaleString()} B</span>{/if}
            </button>
        {:else}<div class="empty">This folder is empty.</div>{/each}
    </div>{/if}
    <aside class="context-pane">
        {#if rightPanel}
            <div class="context-kicker">Selected context</div>
            <h3>{rightPanel.title}</h3>
            {#if rightPanel.source.kind === 'CloudDrive'}
                <code>{rightPanel.source.path}</code>
            {:else if rightPanel.source.kind === 'LocalPath'}
                <code>{rightPanel.source.path}</code>
                <div class="context-provenance">Local filesystem</div>
            {:else if rightPanel.source.kind === 'SearchResults'}
                <div class="context-label">Search results</div>
                <code>{rightPanel.source.query}</code>
            {:else if rightPanel.source.kind === 'DuplicateGroup'}
                <!-- Capture the narrowed source: the `kind` check above does not
                     survive into the arrow callbacks below, because `rightPanel`
                     is mutable `$state` and TypeScript cannot prove it still
                     holds this variant by the time a closure runs. -->
                {@const dupSource = rightPanel.source}
                <code>group: {dupSource.groupId}</code>
                <div class="duplicate-decision">
                    <span class="context-label">Dry-run decision</span>
                    <select value={dupSource.decision} onchange={(event) => setDuplicateDecision((event.currentTarget as HTMLSelectElement).value as DuplicateDecision)}>
                        <option value="review">Review later</option>
                        <option value="keep_source">Keep source</option>
                        <option value="keep_destination">Keep destination</option>
                        <option value="keep_both">Keep both</option>
                    </select>
                    {#if duplicateAudit.some((entry) => entry.groupId === dupSource.groupId)}
                        <button class="duplicate-undo" onclick={undoDuplicateDecision}>Undo last decision</button>
                    {/if}
                </div>
                <ul class="duplicate-context-list">
                    {#each dupSource.items as item}
                        <li>
                            <span class="duplicate-role">{item.role}</span>
                            <span class="duplicate-path" title={item.path}>{item.path}</span>
                            <span class="entry-size">{item.size.toLocaleString()} B</span>
                            <span class="duplicate-mtime">{new Date(item.mtime * 1000).toLocaleString()}</span>
                            {#if item.hash}<code class="duplicate-hash" title={item.hash}>{item.hash}</code>{/if}
                            <span class="duplicate-actions">
                                <button onclick={() => openDuplicateItem(item.path)}>Open</button>
                                <button onclick={() => copyDuplicatePath(item.path)}>Copy path</button>
                            </span>
                        </li>
                    {/each}
                </ul>
                {#if duplicateAudit.length > 0}
                    <details class="duplicate-audit">
                        <summary>Decision audit ({duplicateAudit.length})</summary>
                        <div class="audit-list">
                            {#each duplicateAudit.filter((entry) => entry.groupId === dupSource.groupId).slice().reverse() as entry}
                                <div class="audit-entry">
                                    <span>{decisionLabel(entry.previous)} → {decisionLabel(entry.next)}</span>
                                    <time datetime={new Date(entry.at).toISOString()}>{new Date(entry.at).toLocaleString()}</time>
                                </div>
                            {/each}
                        </div>
                        <button class="duplicate-undo" onclick={clearDuplicateAudit}>Clear audit</button>
                    </details>
                {/if}
            {:else if rightPanel.source.kind === 'CatalogArchive'}
                <div class="context-label">Catalog archive</div>
                <code>{rightPanel.source.archivePath}</code>
            {:else if rightPanel.source.kind === 'RemoteSearchResults'}
                <div class="context-label">Remote search · {rightPanel.source.provider}</div>
                <code>{rightPanel.source.query}</code>
            {:else}
                <code>{sourceKind(rightPanel.source)}</code>
            {/if}
            {#if selectedStat}
                <dl>
                    <dt>Type</dt><dd>{selectedStat.is_dir ? 'Folder' : 'File'}</dd>
                    <dt>Size</dt><dd>{selectedStat.size.toLocaleString()} B</dd>
                    {#if selectedStat.mtime_unix}<dt>Modified</dt><dd>{new Date(selectedStat.mtime_unix * 1000).toLocaleString()}</dd>{/if}
                </dl>
            {/if}
        {:else}
            <div class="empty">Select a file to open its context.</div>
        {/if}
    </aside>
</section>

<style>
    .drive-browser { display: grid; grid-template-columns: minmax(0, 1fr) minmax(220px, .35fr); gap: 12px; padding: 24px; height: 100%; box-sizing: border-box; align-content: start; }
    .browser-header, .browser-toolbar, .browser-error, .empty { grid-column: 1 / -1; }
    .browser-header, .browser-toolbar { display: flex; align-items: center; gap: 10px; }
    .browser-header { justify-content: space-between; }
    h2 { margin: 0; font-size: 1.25rem; } p { margin: 4px 0 0; }
    .muted, .entry-size { color: var(--text-muted, #8a8a96); font-size: .82rem; }
    .browser-controls { display: flex; gap: 8px; } select, button { border: 1px solid var(--border, #3a3a44); background: var(--surface, #202027); color: inherit; border-radius: 6px; padding: 7px 9px; }
    button { cursor: pointer; display: inline-flex; align-items: center; gap: 5px; } button:disabled { opacity: .45; cursor: default; } .icon-button { padding: 7px; }
    .browser-toolbar { flex-wrap: wrap; padding: 8px; background: var(--surface, #202027); border-radius: 8px; }
    .crumb { border: 0; background: transparent; padding: 3px 4px; } .crumb.current { color: var(--text-muted, #8a8a96); } .toolbar-spacer { flex: 1; }
    .entry-list { grid-column: 1; grid-row: 4; display: flex; flex-direction: column; border: 1px solid var(--border, #3a3a44); border-radius: 8px; overflow: hidden; }
    .entry { width: 100%; border: 0; border-bottom: 1px solid var(--border, #3a3a44); border-radius: 0; text-align: left; } .entry:last-child { border-bottom: 0; } .entry.selected { background: #263b58; }
    .entry-name { flex: 1; } .entry-size { margin-left: auto; } .file-dot { width: 10px; height: 12px; border: 1px solid #8993a4; border-radius: 2px; }
    .context-pane { grid-column: 2; grid-row: 4; border: 1px solid var(--border, #3a3a44); border-radius: 8px; padding: 16px; min-height: 150px; }
    .context-kicker { color: var(--text-muted, #8a8a96); font-size: .75rem; text-transform: uppercase; letter-spacing: .06em; }
    .context-pane h3 { margin: 8px 0; overflow-wrap: anywhere; } .context-pane code { color: var(--text-muted, #8a8a96); overflow-wrap: anywhere; }
    .context-label, .context-provenance { color: var(--text-muted, #8a8a96); font-size: .75rem; margin: 10px 0 5px; } .duplicate-decision { display: grid; gap: 4px; margin-top: 12px; } .duplicate-decision select { width: 100%; } .duplicate-undo { justify-self: start; padding: 4px 6px; font-size: .7rem; } .duplicate-audit { margin-top: 14px; font-size: .72rem; } .duplicate-audit summary { cursor: pointer; color: var(--text-muted, #8a8a96); } .audit-list { display: grid; gap: 5px; margin: 8px 0; } .audit-entry { display: grid; gap: 2px; } .audit-entry time { color: var(--text-muted, #8a8a96); font-size: .65rem; }
    dl { display: grid; grid-template-columns: auto 1fr; gap: 8px; margin-top: 18px; font-size: .85rem; } dt { color: var(--text-muted, #8a8a96); } dd { margin: 0; text-align: right; }
    .duplicate-context-list { list-style: none; padding: 0; margin: 16px 0 0; display: grid; gap: 8px; font-size: .8rem; } .duplicate-context-list li { display: grid; grid-template-columns: auto 1fr; gap: 4px 8px; } .duplicate-role { color: var(--text-muted, #8a8a96); text-transform: uppercase; font-size: .68rem; } .duplicate-path { grid-column: 1 / -1; overflow-wrap: anywhere; } .duplicate-context-list .entry-size, .duplicate-mtime { grid-column: 1 / -1; text-align: left; } .duplicate-mtime { color: var(--text-muted, #8a8a96); font-size: .68rem; } .duplicate-hash { grid-column: 1 / -1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-muted, #8a8a96); font-size: .68rem; } .duplicate-actions { display: flex; gap: 6px; } .duplicate-actions button { padding: 3px 6px; font-size: .7rem; }
    @media (max-width: 720px) { .drive-browser { display: flex; } .context-pane { order: 5; } }
    .danger { color: #ff9a9a; } .browser-error { color: #ff9a9a; padding: 8px; background: #3b2024; border-radius: 6px; } .empty { color: var(--text-muted, #8a8a96); padding: 30px; text-align: center; }
</style>
