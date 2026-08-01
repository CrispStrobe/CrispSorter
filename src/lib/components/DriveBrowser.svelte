<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { ChevronRight, Copy, Folder, FolderPlus, RefreshCw, Trash2 } from 'lucide-svelte';
    import {
        availableDriveActions,
        joinDrivePath,
        normalizeDrivePath,
        parentDrivePath,
        type DriveCapabilities,
    } from '$lib/drives/browser';

    type Drive = { id: string; label: string; kind: string };
    type Entry = { name: string; is_dir: boolean; size: number | null };
    let drives = $state<Drive[]>([]);
    let driveId = $state('');
    let path = $state('/');
    let entries = $state<Entry[]>([]);
    let capabilities = $state<DriveCapabilities>({ create_dir: false, rename: false, move_path: false, copy: false, delete: false });
    let loading = $state(false);
    let error = $state('');
    let selected = $state<string | null>(null);
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

    function selectDrive(id: string) {
        driveId = id;
        path = '/';
        void refresh();
    }

    function open(entry: Entry) {
        if (entry.is_dir) {
            path = joinDrivePath(path, entry.name);
            void refresh();
        } else selected = entry.name;
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

    onMount(() => { void loadDrives(); });
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
</section>

<style>
    .drive-browser { display: flex; flex-direction: column; gap: 12px; padding: 24px; height: 100%; box-sizing: border-box; }
    .browser-header, .browser-toolbar { display: flex; align-items: center; gap: 10px; }
    .browser-header { justify-content: space-between; }
    h2 { margin: 0; font-size: 1.25rem; } p { margin: 4px 0 0; }
    .muted, .entry-size { color: var(--text-muted, #8a8a96); font-size: .82rem; }
    .browser-controls { display: flex; gap: 8px; } select, button { border: 1px solid var(--border, #3a3a44); background: var(--surface, #202027); color: inherit; border-radius: 6px; padding: 7px 9px; }
    button { cursor: pointer; display: inline-flex; align-items: center; gap: 5px; } button:disabled { opacity: .45; cursor: default; } .icon-button { padding: 7px; }
    .browser-toolbar { flex-wrap: wrap; padding: 8px; background: var(--surface, #202027); border-radius: 8px; }
    .crumb { border: 0; background: transparent; padding: 3px 4px; } .crumb.current { color: var(--text-muted, #8a8a96); } .toolbar-spacer { flex: 1; }
    .entry-list { display: flex; flex-direction: column; border: 1px solid var(--border, #3a3a44); border-radius: 8px; overflow: hidden; }
    .entry { width: 100%; border: 0; border-bottom: 1px solid var(--border, #3a3a44); border-radius: 0; text-align: left; } .entry:last-child { border-bottom: 0; } .entry.selected { background: #263b58; }
    .entry-name { flex: 1; } .entry-size { margin-left: auto; } .file-dot { width: 10px; height: 12px; border: 1px solid #8993a4; border-radius: 2px; }
    .danger { color: #ff9a9a; } .browser-error { color: #ff9a9a; padding: 8px; background: #3b2024; border-radius: 6px; } .empty { color: var(--text-muted, #8a8a96); padding: 30px; text-align: center; }
</style>
