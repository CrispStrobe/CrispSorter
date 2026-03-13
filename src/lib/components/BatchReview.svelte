<script lang="ts">
    import { batchManager } from '../batch/store.svelte';
    import { i18n } from '../i18n.svelte';
    import { open } from '@tauri-apps/plugin-dialog';
    import { listen } from '@tauri-apps/api/event';
    import { onMount } from 'svelte';
    import { 
        Play, Trash2, Check, X, FileSearch, 
        Loader2, Eye, Edit, Rocket, CheckSquare, 
        Square, Brain, Type, Search, Filter, ChevronDown, ChevronUp, 
        Plus
    } from 'lucide-svelte';

    let selectedItemId = $state<string | null>(null);
    let selectedItem = $derived(batchManager.items.find(i => i.id === selectedItemId));
    
    // Multi-selection state
    let selection = $state<Set<string>>(new Set());
    let lastClickedId = $state<string | null>(null);
    let showFilters = $state(false);
    let showModeMenu = $state(false);

    onMount(async () => {
        // Tauri drag-drop listener
        const unlisten = await listen('tauri://drag-drop', (event: any) => {
            const paths = event.payload.paths as string[];
            paths.forEach(path => {
                const name = path.split(/[\\/]/).pop() || '';
                batchManager.addItem(path, name);
            });
        });
        return () => unlisten();
    });

    async function handleAddFiles() {
        const selected = await open({
            multiple: true,
            filters: [{ name: 'Documents', extensions: ['pdf', 'docx', 'txt', 'md'] }]
        });
        if (Array.isArray(selected)) {
            selected.forEach(path => {
                const name = path.split(/[\\/]/).pop() || '';
                batchManager.addItem(path, name);
            });
        }
    }

    function handleRowClick(e: MouseEvent, id: string) {
        if (e.shiftKey && lastClickedId) {
            const items = batchManager.filteredItems;
            const start = items.findIndex(i => i.id === lastClickedId);
            const end = items.findIndex(i => i.id === id);
            const range = items.slice(Math.min(start, end), Math.max(start, end) + 1);
            range.forEach(i => selection.add(i.id));
        } else if (e.metaKey || e.ctrlKey) {
            if (selection.has(id)) selection.delete(id);
            else selection.add(id);
        } else {
            selection.clear();
            selection.add(id);
            selectedItemId = id;
        }
        lastClickedId = id;
    }

    async function startProcessing() {
        await batchManager.processAll();
    }

    async function executeSorting() {
        const count = batchManager.items.filter(i => i.isAccepted).length;
        if (confirm(i18n.t.batch.confirm_move.replace('{count}', count.toString()))) {
            await batchManager.executeBatch();
        }
    }

    function toggleSelectionAccepted(val: boolean) {
        const targetIds = selection.size > 0 ? Array.from(selection) : batchManager.filteredItems.map(i => i.id);
        batchManager.items.forEach(i => {
            if (targetIds.includes(i.id) && (i.status === 'review' || i.status === 'done' || i.status === 'queued')) {
                i.isAccepted = val;
            }
        });
    }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="batch-container" ondragover={e => e.preventDefault()} role="region" aria-label="File drop zone">
    <div class="toolbar">
        <div class="left-actions">
            <div class="btn-group">
                <button class="icon-btn-main primary" onclick={handleAddFiles} title={i18n.t.batch.add_files}>
                    <Plus size={18} />
                </button>
                <button class="icon-btn-main danger" onclick={() => batchManager.clear()} title={i18n.t.batch.clear_all}>
                    <Trash2 size={18} />
                </button>
            </div>

            <div class="dropdown-container">
                <button class="mode-select-btn" onclick={() => showModeMenu = !showModeMenu}>
                    {#if batchManager.isMetadataExtractionEnabled}<Brain size={16} />{:else}<Type size={16} />{/if}
                    <ChevronDown size={14} />
                </button>
                {#if showModeMenu}
                    <div class="dropdown-menu">
                        <button onclick={() => { batchManager.isMetadataExtractionEnabled = false; showModeMenu = false; }}>
                            <Type size={14} /> {i18n.t.batch.text_only}
                        </button>
                        <button onclick={() => { batchManager.isMetadataExtractionEnabled = true; showModeMenu = false; }}>
                            <Brain size={14} /> {i18n.t.batch.ai_sort}
                        </button>
                    </div>
                {/if}
            </div>

            <div class="search-box">
                <Search size={14} style="color: #71717a; margin-right: 8px;" />
                <input type="text" bind:value={batchManager.searchQuery} placeholder={i18n.t.batch.search_placeholder} />
            </div>

            <button class="action-btn small" onclick={() => showFilters = !showFilters} class:active={showFilters}>
                <Filter size={16} />
            </button>
        </div>

        <div class="right-actions">
            <button class="action-btn success small" onclick={startProcessing} disabled={batchManager.isProcessing}>
                {#if batchManager.isProcessing}<Loader2 class="loader-spin" size={16} />{:else}<Play size={16} />{/if}
                {i18n.t.batch.start_batch}
            </button>

            <div class="btn-group">
                <button class="action-btn small" onclick={() => toggleSelectionAccepted(true)}>
                    <CheckSquare size={14} /> {i18n.t.batch.accept_all}
                </button>
                <button class="action-btn small" onclick={() => toggleSelectionAccepted(false)}>
                    <Square size={14} /> {i18n.t.batch.uncheck_all}
                </button>
            </div>
            
            <button class="action-btn rocket-btn small" 
                    onclick={executeSorting} 
                    disabled={batchManager.items.filter(i => i.isAccepted).length === 0}>
                <Rocket size={16} /> {i18n.t.batch.execute}
            </button>
        </div>
    </div>

    {#if showFilters}
        <div class="filter-bar">
            <div class="filter-group">
                <label for="ext-filter">{i18n.t.batch.filter_type}</label>
                <select id="ext-filter" bind:value={batchManager.filterExtension}>
                    <option value="all">All</option>
                    <option value="pdf">PDF</option>
                    <option value="docx">DOCX</option>
                    <option value="txt">TXT</option>
                </select>
            </div>
            <div class="filter-group">
                <label for="status-filter">{i18n.t.batch.filter_status}</label>
                <select id="status-filter" bind:value={batchManager.filterStatus}>
                    <option value="all">All</option>
                    <option value="queued">Queued</option>
                    <option value="review">Review</option>
                    <option value="done">Done</option>
                    <option value="error">Error</option>
                </select>
            </div>
            <div class="filter-group">
                <label for="size-filter">{i18n.t.batch.filter_size}</label>
                <input id="size-filter" type="number" bind:value={batchManager.filterMinSize} min="0" />
            </div>
        </div>
    {/if}

    <div class="main-split">
        <div class="table-container">
            <table class="dense-table">
                <thead>
                    <tr>
                        <th width="30"></th>
                        <th width="90">{i18n.t.batch.status}</th>
                        <th>{i18n.t.batch.file_name}</th>
                        <th>{i18n.t.batch.title}</th>
                        <th>{i18n.t.batch.author}</th>
                    </tr>
                </thead>
                <tbody>
                    {#each batchManager.filteredItems as item (item.id)}
                        <tr 
                            class:selected={selection.has(item.id)}
                            class:active-row={selectedItemId === item.id}
                            onclick={(e) => handleRowClick(e, item.id)}
                            class:status-error={item.status === 'error'}
                            class:status-done={item.status === 'done'}
                        >
                            <td onclick={e => e.stopPropagation()}>
                                <input type="checkbox" bind:checked={item.isAccepted} />
                            </td>
                            <td>
                                <span class="status-badge" class:status-active={['extracting', 'analyzing', 'moving'].includes(item.status)}>
                                    {item.status}
                                </span>
                            </td>
                            <td class="file-name" title={item.originalPath}>{item.originalName}</td>
                            <td><input type="text" bind:value={item.suggestedTitle} onclick={e => e.stopPropagation()} /></td>
                            <td><input type="text" bind:value={item.suggestedAuthor} onclick={e => e.stopPropagation()} /></td>
                        </tr>
                    {/each}
                    {#if batchManager.filteredItems.length === 0}
                        <tr>
                            <td colspan="5" class="empty-row">{i18n.t.batch.empty}</td>
                        </tr>
                    {/if}
                </tbody>
            </table>
        </div>

        {#if selectedItem}
            <div class="detail-pane">
                <div class="detail-header">
                    <h3>{i18n.t.batch.details}</h3>
                    <button class="close-btn" onclick={() => selectedItemId = null}>×</button>
                </div>
                <div class="detail-content">
                    {#if selectedItem.errorMessage}
                        <div class="error-box">{selectedItem.errorMessage}</div>
                    {/if}
                    <div class="detail-section">
                        <h4>{i18n.t.batch.target_path}</h4>
                        <div class="path-preview">{selectedItem.targetPath || i18n.t.batch.path_hint}</div>
                    </div>
                    <div class="detail-section">
                        <h4>{i18n.t.batch.extracted_text}</h4>
                        <div class="text-preview">{selectedItem.extractedText || i18n.t.batch.extract_hint}</div>
                    </div>
                    <div class="detail-section">
                        <h4>{i18n.t.batch.edit_metadata}</h4>
                        <div class="edit-fields">
                            <label>
                                {i18n.t.batch.title} 
                                <input type="text" bind:value={selectedItem.suggestedTitle} />
                            </label>
                            <label>
                                {i18n.t.batch.author} 
                                <input type="text" bind:value={selectedItem.suggestedAuthor} />
                            </label>
                            <label>
                                {i18n.t.batch.year} 
                                <input type="text" bind:value={selectedItem.suggestedYear} />
                            </label>
                        </div>
                    </div>
                </div>
            </div>
        {/if}
    </div>
</div>

<style>
    .batch-container { display: flex; flex-direction: column; height: 100%; background: #09090b; overflow: hidden; }
    .toolbar { padding: 8px 16px; background: #18181b; border-bottom: 1px solid #27272a; display: flex; justify-content: space-between; align-items: center; gap: 12px; }
    .left-actions, .right-actions { display: flex; align-items: center; gap: 8px; }
    
    .btn-group { display: flex; gap: 1px; background: #27272a; border-radius: 6px; overflow: hidden; border: 1px solid #27272a; }
    
    .icon-btn-main { background: #18181b; border: none; color: #d4d4d8; padding: 4px 10px; cursor: pointer; display: flex; align-items: center; justify-content: center; }
    .icon-btn-main:hover { background: #27272a; }
    .icon-btn-main.primary { color: #3b82f6; }
    .icon-btn-main.danger { color: #ef4444; }

    .dropdown-container { position: relative; }
    .mode-select-btn { display: flex; align-items: center; gap: 6px; padding: 4px 10px; background: #18181b; border: 1px solid #27272a; border-radius: 6px; color: #d4d4d8; cursor: pointer; font-size: 0.8125rem; }
    .dropdown-menu { position: absolute; top: 100%; left: 0; background: #18181b; border: 1px solid #27272a; border-radius: 6px; box-shadow: 0 10px 15px rgba(0,0,0,0.5); z-index: 100; margin-top: 4px; min-width: 150px; }
    .dropdown-menu button { width: 100%; text-align: left; padding: 8px 12px; background: transparent; border: none; color: #d4d4d8; cursor: pointer; font-size: 0.8125rem; display: flex; align-items: center; gap: 8px; }
    .dropdown-menu button:hover { background: #27272a; color: white; }

    .search-box { display: flex; align-items: center; background: #09090b; border: 1px solid #27272a; border-radius: 6px; padding: 0 10px; flex: 1; min-width: 250px; }
    .search-box input { border: none; background: transparent; color: white; padding: 4px 0; font-size: 0.8125rem; width: 100%; }
    .search-box input:focus { outline: none; }

    .filter-bar { display: flex; gap: 20px; background: #18181b; border-bottom: 1px solid #27272a; padding: 8px 16px; align-items: center; }
    .filter-group { display: flex; align-items: center; gap: 8px; }
    .filter-group label { font-size: 0.7rem; font-weight: 600; color: #71717a; text-transform: uppercase; }
    .filter-group select, .filter-group input { background: #09090b; border: 1px solid #27272a; color: white; border-radius: 4px; padding: 2px 6px; font-size: 0.75rem; }

    .action-btn { display: flex; align-items: center; gap: 6px; padding: 4px 10px; border: 1px solid #27272a; background: #18181b; border-radius: 6px; cursor: pointer; font-size: 0.8125rem; font-weight: 600; color: #d4d4d8; }
    .action-btn:hover:not(:disabled) { background: #27272a; }
    .action-btn.active { background: #3b82f6; color: white; border-color: #3b82f6; }
    .action-btn.success { background: #10b981; color: white; border-color: #10b981; }
    .action-btn.rocket-btn { background: #8b5cf6; color: white; border-color: #8b5cf6; }
    .small { padding: 4px 8px; font-size: 0.75rem; }

    .main-split { display: flex; flex: 1; overflow: hidden; }
    .table-container { flex: 1; overflow-y: auto; }
    .dense-table { width: 100%; border-collapse: collapse; font-size: 0.8125rem; table-layout: fixed; }
    .dense-table th { position: sticky; top: 0; z-index: 10; background: #18181b; padding: 8px 12px; text-align: left; border-bottom: 2px solid #27272a; color: #71717a; font-weight: 600; text-transform: uppercase; font-size: 0.7rem; }
    .dense-table td { padding: 4px 12px; border-bottom: 1px solid #1e1e1e; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: #e2e8f0; }
    .dense-table tr:hover { background: #1e293b; }
    .dense-table tr.selected { background: #1e3a8a; }
    .dense-table tr.active-row { border-left: 3px solid #3b82f6; }
    .dense-table tr.status-done { background: #064e3b33; }
    .dense-table input[type="text"] { width: 100%; border: 1px solid transparent; background: transparent; padding: 2px 6px; border-radius: 4px; font-size: 0.8125rem; color: #f8fafc; }
    .dense-table tr:hover input[type="text"] { background: #0f172a; border-color: #334155; }
    
    .status-badge { padding: 2px 6px; border-radius: 4px; background: #27272a; font-size: 0.7rem; font-weight: 600; color: #a1a1aa; }
    .status-active { color: #3b82f6; background: #1e3a8a33; }
    .status-error { color: #ef4444; background: #450a0a33; }

    .detail-pane { width: 400px; background: #0f172a; border-left: 1px solid #1e293b; display: flex; flex-direction: column; }
    .detail-header { padding: 12px 16px; border-bottom: 1px solid #1e293b; display: flex; justify-content: space-between; align-items: center; }
    .detail-content { padding: 16px; overflow-y: auto; display: flex; flex-direction: column; gap: 16px; }
    .path-preview, .text-preview { background: #020617; border: 1px solid #1e293b; padding: 10px; font-family: monospace; font-size: 0.75rem; border-radius: 6px; color: #94a3b8; }
    .edit-fields label { display: block; font-size: 0.75rem; margin-bottom: 12px; color: #a1a1aa; }
    .edit-fields input { width: 100%; margin-top: 4px; padding: 6px 10px; border: 1px solid #334155; border-radius: 6px; background: #1e293b; color: white; font-size: 0.8125rem; }
    
    .loader-spin { animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
