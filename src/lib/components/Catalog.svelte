<script lang="ts">
    /*
     * Catalog tab — drive cataloging via the Cathy/Catfish .caf format.
     *
     * Phase 3 of PLAN P6. The Rust backend (catalog/{caf,index,scan,dedup}.rs)
     * does the heavy lifting; this component is a thin shell over the four
     * `catalog_*` Tauri commands plus tauri-plugin-store for the
     * registered-list persistence.
     *
     * Storage model is option C from the design discussion:
     * `.caf` is the canonical persistent form; LanceDB integration
     * (Phase 4) will add an "Active in search" toggle that materialises
     * a catalog's rows into the existing search index. For now the
     * `active` flag is just persisted — wiring it to the search stack
     * is Phase 4b's job.
     */

    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
    import { getSetting, saveSetting } from '$lib/store';
    import { flog } from '$lib/log';
    import { i18n } from '$lib/i18n.svelte';
    import {
        FolderPlus, FilePlus, RefreshCw, Trash2, Eye, Loader2,
        HardDrive, Files, Database
    } from 'lucide-svelte';

    // ── Types ──────────────────────────────────────────────────────────────────

    interface CafMetadata {
        version: number;
        device: string;
        volume: string;
        alias: string;
        serial: number;
        comment: string;
        date: number;
        file_count: number;
        total_size: number;
        archive: number;
        freesize: number;
    }

    interface RegisteredCatalog {
        path: string;          // absolute path to the .caf file
        active: boolean;       // Phase 4b: include in unified search
        added_at: number;      // unix epoch ms
        volume_id?: string;    // diskutil UUID / blkid UUID / win serial (captured at add-time)
    }

    interface MountedVolume {
        id: string;
        mount_point: string;
        name: string;
    }

    interface FileEntry {
        path: string;
        size: number;
        mtime: number;
        hash: string | null;
    }

    interface FileIndex {
        root_path: string;
        is_windows_path: boolean;
        all_files: FileEntry[];
    }

    // ── State ──────────────────────────────────────────────────────────────────

    let registered = $state<RegisteredCatalog[]>([]);
    let metadataCache = $state<Record<string, CafMetadata | null>>({});
    let loading = $state(false);
    let error = $state('');

    // Mounted volumes — refreshed on mount and after catalog operations
    let mountedVolumes = $state<MountedVolume[]>([]);

    async function refreshMountedVolumes() {
        try {
            mountedVolumes = await invoke<MountedVolume[]>('index_list_mounted_volumes');
        } catch {
            mountedVolumes = [];
        }
    }

    /** Returns the current mount point for a catalog, or null if unmounted. */
    function mountPointFor(cat: RegisteredCatalog): string | null {
        if (!cat.volume_id) return null;
        return mountedVolumes.find(v => v.id === cat.volume_id)?.mount_point ?? null;
    }

    // Browse pane: selected catalog + loaded entries
    let browsing = $state<string | null>(null);
    let browsingEntries = $state<FileEntry[]>([]);
    let browsingFilter = $state('');
    let browsingLoading = $state(false);

    // ── Persistence (matches Settings.svelte pattern) ──────────────────────────

    async function loadRegistered() {
        const stored = (await getSetting('catalogList', null)) as RegisteredCatalog[] | null;
        registered = stored ?? [];
        // Refresh metadata for everyone we already know about; bad entries
        // get a null cache value, which the UI surfaces as "(unreadable)".
        for (const c of registered) {
            void refreshMetadata(c.path);
        }
    }

    async function persistRegistered() {
        await saveSetting('catalogList', registered);
    }

    async function refreshMetadata(path: string) {
        try {
            metadataCache[path] = await invoke<CafMetadata>('catalog_metadata', { path });
        } catch (e: any) {
            metadataCache[path] = null;
            flog('warn', `catalog_metadata(${path}): ${e}`);
        }
    }

    // ── Add / remove ───────────────────────────────────────────────────────────

    async function pickAndAddCaf() {
        const picked = await openDialog({
            multiple: false,
            filters: [{ name: 'Cathy Catalog', extensions: ['caf'] }],
        });
        if (typeof picked !== 'string') return;
        if (registered.some(c => c.path === picked)) {
            error = i18n.t.caf_catalog.err_already_registered;
            return;
        }
        // Capture the volume UUID from the catalog's device path so we can
        // show mount status and tag L1 rows with the drive's stable id.
        const meta = await invoke<CafMetadata | null>('catalog_metadata', { path: picked }).catch(() => null);
        const volumeId = meta?.device
            ? await invoke<string | null>('index_volume_id_for_path', { path: meta.device }).catch(() => null)
            : null;
        registered = [...registered, { path: picked, active: false, added_at: Date.now(), volume_id: volumeId ?? undefined }];
        await persistRegistered();
        await refreshMetadata(picked);
    }

    async function createFromFolder() {
        // Pick the folder to scan.
        const folder = await openDialog({ directory: true, multiple: false });
        if (typeof folder !== 'string') return;
        // Pick where to save the .caf. Default name = leaf folder + .caf.
        const leaf = folder.replace(/[\/\\]+$/, '').split(/[\/\\]/).pop() || 'catalog';
        const out = await saveDialog({
            defaultPath: `${leaf}.caf`,
            filters: [{ name: 'Cathy Catalog', extensions: ['caf'] }],
        });
        if (!out) return;
        loading = true;
        error = '';
        try {
            // 1. Scan the folder (no inline hashing — dedup will hash later if needed)
            const idx = await invoke<FileIndex>('catalog_scan_dir', {
                root: folder,
                hashAlgo: null,
                maxSizeBytes: null,
            });
            // 2. Save to .caf
            await invoke('catalog_save_caf', {
                path: out,
                index: idx,
                createdDate: Math.floor(Date.now() / 1000),
            });
            flog('info', `Created catalog: ${out} (${idx.all_files.length} files)`);
            // 3. Auto-register + capture volume UUID while the drive is still mounted
            const volumeId = await invoke<string | null>('index_volume_id_for_path', { path: folder }).catch(() => null);
            registered = [...registered, { path: out, active: false, added_at: Date.now(), volume_id: volumeId ?? undefined }];
            await persistRegistered();
            await refreshMetadata(out);
        } catch (e: any) {
            error = String(e);
        } finally {
            loading = false;
        }
    }

    async function refreshFromSource(path: string) {
        // Re-scan the catalog's original device path and overwrite the .caf.
        const meta = metadataCache[path];
        if (!meta || !meta.device) {
            error = i18n.t.caf_catalog.err_no_device_path;
            return;
        }
        loading = true;
        error = '';
        try {
            const idx = await invoke<FileIndex>('catalog_scan_dir', {
                root: meta.device,
                hashAlgo: null,
                maxSizeBytes: null,
            });
            await invoke('catalog_save_caf', {
                path,
                index: idx,
                createdDate: Math.floor(Date.now() / 1000),
            });
            await refreshMetadata(path);
            flog('info', `Refreshed catalog: ${path} (${idx.all_files.length} files)`);
        } catch (e: any) {
            error = String(e);
        } finally {
            loading = false;
        }
    }

    async function removeCatalog(path: string) {
        registered = registered.filter(c => c.path !== path);
        delete metadataCache[path];
        if (browsing === path) {
            browsing = null;
            browsingEntries = [];
        }
        await persistRegistered();
    }

    async function toggleActive(path: string) {
        const target = registered.find(c => c.path === path);
        if (!target) return;
        const willActivate = !target.active;
        // Optimistic UI update — flip the flag, kick off the backend
        // call, revert on failure.
        registered = registered.map(c =>
            c.path === path ? { ...c, active: willActivate } : c
        );
        await persistRegistered();
        loading = true;
        error = '';
        try {
            const dataDir = await invoke<string>('get_app_data_dir');
            const inserted = await invoke<number>('catalog_set_active', {
                catalogPath: path,
                active: willActivate,
                dataDir,
            });
            if (willActivate) {
                flog(
                    'info',
                    `Activated catalog: ${path} (${inserted} entries materialized)`
                );
            } else {
                flog('info', `Deactivated catalog: ${path}`);
            }
        } catch (e: any) {
            // Revert on failure so the UI doesn't lie about state.
            registered = registered.map(c =>
                c.path === path ? { ...c, active: !willActivate } : c
            );
            await persistRegistered();
            error = `set_active(${path}): ${e}`;
        } finally {
            loading = false;
        }
    }

    // ── Import to batch (PLAN P6 4d) ───────────────────────────────────────────
    //
    // Pull entries from the currently-browsed catalog into the sort
    // batch. Reuses the watcher's add_item path (already dedup-on-path)
    // so re-importing the same .caf is idempotent. Files don't auto-
    // process — the user still presses Start in BatchReview.
    let importing = $state(false);
    let importedCount = $state(0);

    async function importToBatch() {
        if (!browsing || browsingEntries.length === 0) return;
        importing = true;
        importedCount = 0;
        try {
            const { batchManager } = await import('$lib/batch/store.svelte');
            for (const e of browsingEntries) {
                const name = e.path.split(/[\\/]/).pop() || e.path;
                batchManager.addItem(e.path, name, e.size);
                importedCount++;
            }
            flog('info', `Imported ${importedCount} entries from catalog ${browsing}`);
        } catch (e: any) {
            error = `import_to_batch: ${e}`;
        } finally {
            importing = false;
        }
    }

    // ── Browse ─────────────────────────────────────────────────────────────────

    async function browse(path: string) {
        if (browsing === path) {
            browsing = null;
            browsingEntries = [];
            return;
        }
        browsing = path;
        browsingEntries = [];
        browsingLoading = true;
        try {
            const idx = await invoke<FileIndex>('catalog_load_caf', { path });
            browsingEntries = idx.all_files;
        } catch (e: any) {
            error = `load_caf(${path}): ${e}`;
        } finally {
            browsingLoading = false;
        }
    }

    const filteredEntries = $derived.by(() => {
        const q = browsingFilter.trim().toLowerCase();
        if (!q) return browsingEntries.slice(0, 500);
        return browsingEntries
            .filter(e => e.path.toLowerCase().includes(q))
            .slice(0, 500);
    });

    // ── Helpers ────────────────────────────────────────────────────────────────

    function formatSize(bytes: number): string {
        if (bytes < 1024) return `${bytes} B`;
        const units = ['KB', 'MB', 'GB', 'TB'];
        let n = bytes / 1024;
        let i = 0;
        while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
        return `${n.toFixed(n < 10 ? 1 : 0)} ${units[i]}`;
    }

    function formatDate(epochSeconds: number): string {
        if (!epochSeconds) return '—';
        return new Date(epochSeconds * 1000).toISOString().slice(0, 10);
    }

    onMount(() => {
        void loadRegistered();
        void refreshMountedVolumes();
    });
</script>

<div class=”catalog-tab”>
    <header class=”catalog-header”>
        <h1>{i18n.t.caf_catalog.title}</h1>
        <p class=”subtitle”>{i18n.t.caf_catalog.subtitle}</p>
    </header>

    <div class=”actions”>
        <button class={'btn primary'} onclick={createFromFolder} disabled={loading}>
            <FolderPlus size={16} />
            <span>{i18n.t.caf_catalog.create_from_folder}</span>
        </button>
        <button class=”btn” onclick={pickAndAddCaf} disabled={loading}>
            <FilePlus size={16} />
            <span>{i18n.t.caf_catalog.add_existing}</span>
        </button>
        {#if loading}
            <span class=”loading-inline”>
                <Loader2 size={16} class=”spin” />
                <span>{i18n.t.caf_catalog.working}</span>
            </span>
        {/if}
    </div>

    {#if error}
        <div class=”error”>{error}</div>
    {/if}

    {#if registered.length === 0}
        <div class=”empty”>
            <Database size={32} />
            <p>{i18n.t.caf_catalog.empty_state}</p>
            <p class=”hint”>{i18n.t.caf_catalog.empty_hint}</p>
        </div>
    {:else}
        <table class=”catalog-table”>
            <thead>
                <tr>
                    <th>{i18n.t.caf_catalog.col_active}</th>
                    <th>{i18n.t.caf_catalog.col_file}</th>
                    <th>{i18n.t.caf_catalog.col_device}</th>
                    <th>{i18n.t.caf_catalog.col_files}</th>
                    <th>{i18n.t.caf_catalog.col_size}</th>
                    <th>{i18n.t.caf_catalog.col_created}</th>
                    <th>{i18n.t.caf_catalog.col_actions}</th>
                </tr>
            </thead>
            <tbody>
                {#each registered as cat (cat.path)}
                    {@const meta = metadataCache[cat.path]}
                    <tr class:active={cat.active}>
                        <td>
                            <input
                                type=”checkbox”
                                checked={cat.active}
                                onchange={() => toggleActive(cat.path)}
                                title={i18n.t.caf_catalog.active_tooltip}
                            />
                        </td>
                        <td class=”path-cell” title={cat.path}>
                            {cat.path.split(/[\\/]/).pop()}
                        </td>
                        <td>
                            {#if meta}
                                {@const mountAt = mountPointFor(cat)}
                                {@const mountTitle = cat.volume_id
                                    ? mountAt
                                        ? i18n.t.caf_catalog.drive_mounted.replace('{path}', mountAt)
                                        : i18n.t.caf_catalog.drive_unmounted
                                    : i18n.t.caf_catalog.drive_unknown}
                                {@const tooltipParts = [
                                    mountTitle,
                                    meta.volume && `${i18n.t.caf_catalog.volume}: ${meta.volume}`,
                                    meta.alias && meta.alias !== meta.volume && `${i18n.t.caf_catalog.alias}: ${meta.alias}`,
                                    meta.serial && `${i18n.t.caf_catalog.serial}: 0x${meta.serial.toString(16).toUpperCase().padStart(8, '0')}`,
                                    meta.freesize > 0 && `${i18n.t.caf_catalog.free_at_scan}: ${formatSize(meta.freesize)}`,
                                    meta.archive ? i18n.t.caf_catalog.archive_flag : null,
                                    meta.comment && `${i18n.t.caf_catalog.comment}: ${meta.comment}`,
                                    `.caf v${meta.version}`,
                                ].filter(Boolean).join('\n')}
                                <span
                                    class=”mount-dot”
                                    class:mounted={!!mountAt}
                                    class:unmounted={cat.volume_id && !mountAt}
                                    title={mountTitle}
                                ></span>
                                <HardDrive size={12} />
                                <span class=”muted” title={tooltipParts}>
                                    {mountAt ? mountAt.split(/[\\/]/).pop() || mountAt : (meta.volume || meta.device || '—')}
                                </span>
                            {:else if meta === null}
                                <span class=”warn”>{i18n.t.caf_catalog.metadata_unreadable}</span>
                            {:else}
                                <span class=”muted”>…</span>
                            {/if}
                        </td>
                        <td>{meta ? meta.file_count.toLocaleString() : '—'}</td>
                        <td>{meta ? formatSize(meta.total_size) : '—'}</td>
                        <td>{meta ? formatDate(meta.date) : '—'}</td>
                        <td class=”actions-cell”>
                            <button
                                class=”icon-btn”
                                onclick={() => browse(cat.path)}
                                title={i18n.t.caf_catalog.btn_view_title}
                            >
                                <Eye size={14} />
                            </button>
                            <button
                                class=”icon-btn”
                                onclick={() => refreshFromSource(cat.path)}
                                disabled={!meta || !meta.device}
                                title={i18n.t.caf_catalog.btn_refresh_title}
                            >
                                <RefreshCw size={14} />
                            </button>
                            <button
                                class={'icon-btn danger'}
                                onclick={() => removeCatalog(cat.path)}
                                title={i18n.t.caf_catalog.btn_remove_title}
                            >
                                <Trash2 size={14} />
                            </button>
                        </td>
                    </tr>
                {/each}
            </tbody>
        </table>

        {#if browsing}
            <section class=”browse-pane”>
                <header>
                    <h2>
                        <Files size={16} />
                        {i18n.t.caf_catalog.browsing_header.replace('{name}', browsing.split(/[\\/]/).pop() ?? '')}
                    </h2>
                    <input
                        type=”text”
                        placeholder={i18n.t.caf_catalog.entries_filter}
                        bind:value={browsingFilter}
                    />
                    <span class=”muted”>
                        {i18n.t.caf_catalog.entries_count.replace('{count}', browsingEntries.length.toLocaleString())}
                        {#if browsingFilter}
                            {i18n.t.caf_catalog.entries_count_filtered.replace('{matched}', String(filteredEntries.length))}
                        {:else}
                            {i18n.t.caf_catalog.entries_count_showing}
                        {/if}
                    </span>
                    <button
                        class={'btn small'}
                        onclick={importToBatch}
                        disabled={importing || browsingEntries.length === 0}
                        title={i18n.t.caf_catalog.import_btn_title}
                    >
                        {#if importing}
                            <Loader2 size={12} class=”spin” />
                            {i18n.t.caf_catalog.importing_btn.replace('{count}', String(importedCount))}
                        {:else}
                            <FilePlus size={12} /> {i18n.t.caf_catalog.import_btn}
                        {/if}
                    </button>
                </header>
                {#if browsingLoading}
                    <div class=”loading”><Loader2 size={20} class=”spin” /> {i18n.t.caf_catalog.entries_loading}</div>
                {:else}
                    <table class=”entries-table”>
                        <thead>
                            <tr>
                                <th>{i18n.t.caf_catalog.col_path}</th>
                                <th>{i18n.t.caf_catalog.col_size}</th>
                                <th>{i18n.t.caf_catalog.col_modified}</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each filteredEntries as e (e.path)}
                                <tr>
                                    <td class=”path-cell” title={e.path}>{e.path}</td>
                                    <td>{formatSize(e.size)}</td>
                                    <td>{formatDate(e.mtime)}</td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                {/if}
            </section>
        {/if}
    {/if}
</div>

<style>
    .catalog-tab {
        padding: 16px 24px;
        max-width: 1200px;
    }
    .catalog-header h1 {
        margin: 0 0 4px;
        font-size: 1.4rem;
    }
    .subtitle {
        color: var(--text-muted, #666);
        margin: 0 0 16px;
        font-size: 0.85rem;
    }
    .actions {
        display: flex;
        gap: 8px;
        align-items: center;
        margin-bottom: 12px;
    }
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
    .btn:hover { background: var(--surface-hover, #ebebeb); }
    .btn.primary { background: #0066cc; color: white; border-color: #0055aa; }
    .btn.primary:hover { background: #0055aa; }
    .btn:disabled { opacity: 0.5; cursor: not-allowed; }

    .loading-inline {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        color: var(--text-muted, #666);
        font-size: 0.8rem;
    }
    :global(.spin) { animation: spin 1s linear infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }

    .error {
        padding: 8px 12px;
        background: #ffe5e5;
        color: #cc0000;
        border: 1px solid #ffcccc;
        border-radius: 4px;
        margin-bottom: 12px;
        font-size: 0.85rem;
    }

    .empty {
        padding: 48px 24px;
        text-align: center;
        color: var(--text-muted, #666);
    }
    .empty p { margin: 4px 0; }
    .empty .hint { font-size: 0.85rem; max-width: 480px; margin: 8px auto 0; }

    .catalog-table {
        width: 100%;
        border-collapse: collapse;
        font-size: 0.85rem;
    }
    .catalog-table th, .catalog-table td {
        padding: 6px 8px;
        text-align: left;
        border-bottom: 1px solid var(--border, #eee);
    }
    .catalog-table th {
        background: var(--surface, #fafafa);
        font-weight: 600;
        font-size: 0.75rem;
        text-transform: uppercase;
        color: var(--text-muted, #666);
    }
    .catalog-table tr.active {
        background: rgba(0, 102, 204, 0.05);
    }
    .path-cell {
        max-width: 320px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }
    .actions-cell {
        white-space: nowrap;
    }
    .icon-btn {
        background: none;
        border: none;
        cursor: pointer;
        padding: 4px;
        margin: 0 2px;
        color: var(--text, #333);
        opacity: 0.7;
    }
    .icon-btn:hover { opacity: 1; }
    .icon-btn.danger:hover { color: #cc0000; }
    .icon-btn:disabled { opacity: 0.3; cursor: not-allowed; }

    .muted { color: var(--text-muted, #666); }
    .warn { color: #cc6600; font-style: italic; }

    .mount-dot {
        display: inline-block;
        width: 7px;
        height: 7px;
        border-radius: 50%;
        background: var(--text-muted, #aaa);
        margin-right: 4px;
        vertical-align: middle;
        flex-shrink: 0;
    }
    .mount-dot.mounted   { background: #22aa44; }
    .mount-dot.unmounted { background: #cc4444; }

    .browse-pane {
        margin-top: 24px;
        border-top: 1px solid var(--border, #ddd);
        padding-top: 16px;
    }
    .browse-pane header {
        display: flex;
        align-items: center;
        gap: 12px;
        margin-bottom: 8px;
    }
    .browse-pane h2 {
        margin: 0;
        font-size: 1rem;
        display: inline-flex;
        align-items: center;
        gap: 6px;
    }
    .browse-pane input[type="text"] {
        flex: 1;
        max-width: 320px;
        padding: 4px 8px;
        border: 1px solid var(--border, #ddd);
        border-radius: 4px;
        font-size: 0.85rem;
    }
    .entries-table {
        width: 100%;
        border-collapse: collapse;
        font-size: 0.8rem;
    }
    .entries-table th, .entries-table td {
        padding: 4px 6px;
        text-align: left;
        border-bottom: 1px solid var(--border, #f0f0f0);
    }
    .entries-table th {
        background: var(--surface, #fafafa);
        font-weight: 600;
        font-size: 0.7rem;
        text-transform: uppercase;
    }
    .loading {
        padding: 24px;
        text-align: center;
        color: var(--text-muted, #666);
        display: flex;
        justify-content: center;
        align-items: center;
        gap: 6px;
    }
</style>
