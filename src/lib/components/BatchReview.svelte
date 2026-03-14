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
        Plus, Columns, Calendar, FileText, HardDrive, Hash,
        RefreshCw, AlertCircle
    } from 'lucide-svelte';

    let selectedItemId = $state<string | null>(null);
    let selectedItem = $derived(batchManager.items.find(i => i.id === selectedItemId));
    
    // Multi-selection state
    let selectedIds = $state<string[]>([]);
    let lastClickedId = $state<string | null>(null);
    let showFilters = $state(false);
    let showModeMenu = $state(false);
    let showColumnSelector = $state(false);

    // Sorting state
    let sortColumn = $state<string>('file_name');
    let sortDirection = $state<'asc' | 'desc'>('asc');

    // Column visibility and width state
    let columns = $state([
        { id: 'status', label: i18n.t.batch.status, width: 90, visible: true, locked: true },
        { id: 'file_name', label: i18n.t.batch.file_name, width: 250, visible: true },
        { id: 'title', label: i18n.t.batch.title, width: 250, visible: true },
        { id: 'author', label: i18n.t.batch.author, width: 180, visible: true },
        { id: 'year', label: i18n.t.batch.year, width: 70, visible: true },
        { id: 'size', label: i18n.t.batch.size, width: 80, visible: false },
        { id: 'date', label: i18n.t.batch.date, width: 140, visible: false },
        { id: 'extension', label: i18n.t.batch.extension, width: 60, visible: false },
        { id: 'path', label: i18n.t.batch.path, width: 400, visible: false },
    ]);

    // Resizing logic
    let resizingColIdx = $state<number | null>(null);
    let startX = 0;
    let startWidth = 0;

    function startResizing(e: MouseEvent, index: number) {
        e.preventDefault();
        e.stopPropagation();
        resizingColIdx = index;
        startX = e.pageX;
        startWidth = columns[index].width;
        window.addEventListener('mousemove', handleMouseMove);
        window.addEventListener('mouseup', stopResizing);
    }

    function handleMouseMove(e: MouseEvent) {
        if (resizingColIdx !== null) {
            const diff = e.pageX - startX;
            columns[resizingColIdx].width = Math.max(50, startWidth + diff);
        }
    }

    function stopResizing() {
        resizingColIdx = null;
        window.removeEventListener('mousemove', handleMouseMove);
        window.removeEventListener('mouseup', stopResizing);
    }

    // Sorting logic
    function toggleSort(colId: string) {
        if (sortColumn === colId) {
            sortDirection = sortDirection === 'asc' ? 'desc' : 'asc';
        } else {
            sortColumn = colId;
            sortDirection = 'asc';
        }
    }

    let sortedItems = $derived.by(() => {
        const items = [...batchManager.filteredItems];
        return items.sort((a, b) => {
            let valA: any = '';
            let valB: any = '';

            switch (sortColumn) {
                case 'status': valA = a.status; valB = b.status; break;
                case 'file_name': valA = a.originalName; valB = b.originalName; break;
                case 'title': valA = a.suggestedTitle || ''; valB = b.suggestedTitle || ''; break;
                case 'author': valA = a.suggestedAuthor || ''; valB = b.suggestedAuthor || ''; break;
                case 'year': valA = a.suggestedYear || ''; valB = b.suggestedYear || ''; break;
                case 'size': valA = a.size; valB = b.size; break;
                case 'date': valA = a.modifiedAt; valB = b.modifiedAt; break;
                case 'extension': valA = a.extension; valB = b.extension; break;
                case 'path': valA = a.originalPath; valB = b.originalPath; break;
            }

            if (valA < valB) return sortDirection === 'asc' ? -1 : 1;
            if (valA > valB) return sortDirection === 'asc' ? 1 : -1;
            return 0;
        });
    });

    onMount(async () => {
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

    function handleRowClick(e: MouseEvent | KeyboardEvent, id: string) {
        console.log(`[BatchReview] Row Click: ${id}, Shift: ${e.shiftKey}, Meta: ${e.metaKey || e.ctrlKey}`);
        
        if (e.shiftKey && lastClickedId) {
            const items = sortedItems;
            const start = items.findIndex(i => i.id === lastClickedId);
            const end = items.findIndex(i => i.id === id);
            if (start !== -1 && end !== -1) {
                const range = items.slice(Math.min(start, end), Math.max(start, end) + 1);
                const ids = range.map(i => i.id);
                selectedIds = Array.from(new Set([...selectedIds, ...ids]));
            }
        } else if (e.metaKey || e.ctrlKey) {
            if (selectedIds.includes(id)) {
                selectedIds = selectedIds.filter(i => i !== id);
            } else {
                selectedIds = [...selectedIds, id];
            }
        } else {
            selectedIds = [id];
            selectedItemId = id;
        }
        lastClickedId = id;
    }

    function selectAllVisible() {
        if (selectedIds.length === sortedItems.length && sortedItems.length > 0) {
            selectedIds = [];
        } else {
            selectedIds = sortedItems.map(i => i.id);
        }
    }

    async function handleRedoSelected() {
        if (selectedIds.length === 0) return;
        await batchManager.reprocessItems(selectedIds);
    }

    async function handleBatchReextract() {
        if (selectedIds.length === 0) return;
        await batchManager.reextractItems(selectedIds);
    }

    async function handleBatchRemove() {
        if (selectedIds.length === 0) return;
        if (confirm(i18n.t.history.delete_confirm)) {
            await batchManager.removeItems(selectedIds);
            selectedIds = [];
        }
    }

    function handleBatchAccept(val: boolean) {
        if (selectedIds.length === 0) return;
        batchManager.setAcceptedItems(selectedIds, val);
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
        const targetIds = selectedIds.length > 0 ? selectedIds : batchManager.filteredItems.map(i => i.id);
        batchManager.setAcceptedItems(targetIds, val);
    }

    function formatSize(bytes: number) {
        if (bytes === 0) return '0 B';
        const k = 1024;
        const sizes = ['B', 'KB', 'MB', 'GB'];
        const i = Math.floor(Math.log(bytes) / Math.log(k));
        return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
    }
</script>

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

            <div class="dropdown-container">
                <button class="action-btn small" onclick={() => showColumnSelector = !showColumnSelector} class:active={showColumnSelector}>
                    <Columns size={16} />
                </button>
                {#if showColumnSelector}
                    <div class="dropdown-menu col-selector">
                        {#each columns as col}
                            {#if !col.locked}
                                <label class="col-opt">
                                    <input type="checkbox" bind:checked={col.visible} />
                                    <span>{col.label}</span>
                                </label>
                            {/if}
                        {/each}
                    </div>
                {/if}
            </div>
        </div>

        <div class="right-actions">
            <button class="action-btn success small" onclick={startProcessing} disabled={batchManager.isProcessing}>
                <span class={batchManager.isProcessing ? "loader-anim" : ""}>
                    {#if batchManager.isProcessing}<Loader2 size={16} />{:else}<Play size={16} />{/if}
                </span>
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
            {#if selectedIds.length > 0}
                <div class="selection-toolbar">
                    <span class="selection-info">
                        <CheckSquare size={14} />
                        {i18n.t.batch.selected_count.replace('{count}', selectedIds.length.toString())}
                    </span>
                    <div class="toolbar-divider"></div>
                    <button class="action-btn small" onclick={handleRedoSelected} title={i18n.t.batch.reanalyze}>
                        <RefreshCw size={14} /> {i18n.t.batch.reanalyze}
                    </button>
                    <button class="action-btn small" onclick={handleBatchReextract} title={i18n.t.batch.reextract}>
                        <FileSearch size={14} /> {i18n.t.batch.reextract}
                    </button>
                    <button class="action-btn small" onclick={() => handleBatchAccept(true)} title={i18n.t.batch.confirm}>
                        <Check size={14} /> {i18n.t.batch.confirm}
                    </button>
                    <button class="action-btn small" onclick={() => handleBatchAccept(false)} title={i18n.t.batch.ignore}>
                        <X size={14} /> {i18n.t.batch.ignore}
                    </button>
                    <button class="action-btn small danger" onclick={handleBatchRemove} title={i18n.t.batch.remove}>
                        <Trash2 size={14} /> {i18n.t.batch.remove}
                    </button>
                    <button class="close-btn-minimal" onclick={() => selectedIds = []}>×</button>
                </div>
            {/if}

            <table class="dense-table">
                <thead>
                    <tr>
                        <th style="width: 35px; min-width: 35px; text-align: center;">
                            <input type="checkbox" 
                                   checked={selectedIds.length === sortedItems.length && sortedItems.length > 0} 
                                   onchange={selectAllVisible} />
                        </th>
                        {#each columns as col, i}
                            {#if col.visible}
                                <th style="width: {col.width}px; min-width: {col.width}px;">
                                    <div 
                                        class="th-content" 
                                        onclick={() => toggleSort(col.id)}
                                        onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && toggleSort(col.id)}
                                        role="button"
                                        tabindex="0"
                                    >
                                        <span class="th-label">{col.label}</span>
                                        {#if sortColumn === col.id}
                                            {#if sortDirection === 'asc'}<ChevronUp size={12} />{:else}<ChevronDown size={12} />{/if}
                                        {/if}
                                    </div>
                                    <div 
                                        class="resizer" 
                                        onmousedown={(e) => startResizing(e, i)}
                                        role="button"
                                        tabindex="-1"
                                        aria-label="Resize column"
                                    ></div>
                                </th>
                            {/if}
                        {/each}
                    </tr>
                </thead>
                <tbody>
                    {#each sortedItems as item (item.id)}
                        <tr 
                            class:selected={selectedIds.includes(item.id)}
                            class:active-row={selectedItemId === item.id}
                            onclick={(e) => handleRowClick(e, item.id)}
                            onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && handleRowClick(e, item.id)}
                            class:status-error={item.status === 'error'}
                            class:status-done={item.status === 'done'}
                            tabindex="0"
                        >
                            <td onclick={e => { e.stopPropagation(); console.log('TD Clicked'); }} style="width: 35px; text-align: center;">
                                <input type="checkbox" 
                                       checked={selectedIds.includes(item.id)} 
                                       onchange={(e) => { e.stopPropagation(); console.log('Check Change'); if(selectedIds.includes(item.id)) selectedIds = selectedIds.filter(i => i !== item.id); else selectedIds = [...selectedIds, item.id]; }}
                                       aria-label="Select item" />
                            </td>
                            {#each columns as col}
                                {#if col.visible}
                                    <td style="width: {col.width}px;">
                                        {#if col.id === 'status'}
                                            <span class="status-badge" class:status-active={['extracting', 'analyzing', 'moving'].includes(item.status)}>
                                                {item.status}
                                            </span>
                                        {:else if col.id === 'file_name'}
                                            <span class="file-name" title={item.originalPath}>{item.originalName}</span>
                                        {:else if col.id === 'title'}
                                            <input type="text" bind:value={item.suggestedTitle} onclick={e => e.stopPropagation()} class:fallback={item.suggestedTitle === 'Unknown Title'} />
                                        {:else if col.id === 'author'}
                                            <input type="text" bind:value={item.suggestedAuthor} onclick={e => e.stopPropagation()} class:fallback={item.suggestedAuthor === 'Unknown Author'} />
                                        {:else if col.id === 'year'}
                                            <input type="text" bind:value={item.suggestedYear} onclick={e => e.stopPropagation()} style="text-align: center;" />
                                        {:else if col.id === 'size'}
                                            <span class="mono">{formatSize(item.size)}</span>
                                        {:else if col.id === 'date'}
                                            <span class="mono">{new Date(item.modifiedAt).toLocaleDateString()}</span>
                                        {:else if col.id === 'extension'}
                                            <span class="ext-badge">{item.extension}</span>
                                        {:else if col.id === 'path'}
                                            <span class="path-text" title={item.originalPath}>{item.originalPath}</span>
                                        {/if}
                                    </td>
                                {/if}
                            {/each}
                        </tr>
                    {/each}
                    {#if sortedItems.length === 0}
                        <tr>
                            <td colspan={columns.filter(c => c.visible).length + 1} class="empty-row">{i18n.t.batch.empty}</td>
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

    .col-selector { padding: 8px; min-width: 180px; }
    .col-opt { display: flex; align-items: center; gap: 8px; padding: 6px 10px; cursor: pointer; border-radius: 4px; font-size: 0.8125rem; color: #a1a1aa; }
    .col-opt:hover { background: #27272a; color: white; }
    .col-opt input { cursor: pointer; }

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
    .action-btn.danger:hover { background: #ef4444; color: white; border-color: #ef4444; }
    .small { padding: 4px 8px; font-size: 0.75rem; }

    .main-split { display: flex; flex: 1; overflow: hidden; }
    .table-container { flex: 1; overflow: auto; position: relative; display: flex; flex-direction: column; }
    
    .selection-toolbar {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 8px 16px;
        background: #1e3a8a;
        border-bottom: 1px solid #1e40af;
        position: sticky;
        top: 0;
        z-index: 30;
        animation: slideDown 0.2s ease-out;
    }
    @keyframes slideDown { from { transform: translateY(-100%); } to { transform: translateY(0); } }

    .selection-info { font-size: 0.8125rem; font-weight: 700; color: white; display: flex; align-items: center; gap: 8px; }
    .toolbar-divider { width: 1px; height: 20px; background: #3b82f666; margin: 0 4px; }
    .close-btn-minimal { background: transparent; border: none; color: #bfdbfe; cursor: pointer; font-size: 1.25rem; margin-left: auto; padding: 0 4px; }
    .close-btn-minimal:hover { color: white; }

    .dense-table { width: max-content; min-width: 100%; border-collapse: collapse; font-size: 0.8125rem; table-layout: fixed; }
    .dense-table th { position: sticky; top: 0; z-index: 10; background: #18181b; padding: 0; text-align: left; border-bottom: 2px solid #27272a; color: #71717a; font-weight: 600; text-transform: uppercase; font-size: 0.7rem; height: 32px; }
    .th-content { display: flex; align-items: center; gap: 6px; padding: 0 12px; height: 100%; cursor: pointer; }
    .th-content:hover { background: #27272a; color: white; }
    .th-label { flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    
    .resizer { position: absolute; right: 0; top: 0; width: 4px; height: 100%; cursor: col-resize; transition: background 0.2s; z-index: 20; }
    .resizer:hover { background: #3b82f6; }

    .dense-table td { padding: 4px 12px; border-bottom: 1px solid #1e1e1e; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: #e2e8f0; height: 32px; vertical-align: middle; }
    .dense-table tr:hover { background: #1e293b; }
    .dense-table tr.selected { background: #1e3a8a; }
    .dense-table tr.active-row { border-left: 3px solid #3b82f6; }
    .dense-table tr.status-done { background: #064e3b33; }
    
    .dense-table input[type="text"] { width: 100%; border: 1px solid transparent; background: transparent; padding: 2px 6px; border-radius: 4px; font-size: 0.8125rem; color: #f8fafc; }
    .dense-table tr:hover input[type="text"] { background: #0f172a; border-color: #334155; }
    .dense-table input.fallback { color: #fbbf24; font-style: italic; }
    
    .status-badge { padding: 2px 6px; border-radius: 4px; background: #27272a; font-size: 0.7rem; font-weight: 600; color: #a1a1aa; text-transform: capitalize; }
    .status-active { color: #3b82f6; background: #1e3a8a33; }
    .status-error { color: #ef4444; background: #450a0a33; }

    .mono { font-family: monospace; font-size: 0.75rem; color: #a1a1aa; }
    .ext-badge { font-size: 0.65rem; background: #27272a; padding: 1px 4px; border-radius: 3px; color: #71717a; text-transform: uppercase; font-weight: 700; }
    .path-text { font-family: monospace; font-size: 0.7rem; color: #71717a; }

    .detail-pane { width: 400px; background: #0f172a; border-left: 1px solid #1e293b; display: flex; flex-direction: column; }
    .detail-header { padding: 12px 16px; border-bottom: 1px solid #1e293b; display: flex; justify-content: space-between; align-items: center; }
    .detail-content { padding: 16px; overflow-y: auto; display: flex; flex-direction: column; gap: 16px; }
    .path-preview, .text-preview { background: #020617; border: 1px solid #1e293b; padding: 10px; font-family: monospace; font-size: 0.75rem; border-radius: 6px; color: #94a3b8; }
    .edit-fields label { display: block; font-size: 0.75rem; margin-bottom: 12px; color: #a1a1aa; }
    .edit-fields input { width: 100%; margin-top: 4px; padding: 6px 10px; border: 1px solid #334155; border-radius: 6px; background: #1e293b; color: white; font-size: 0.8125rem; }
    
    .loader-anim { display: inline-flex; animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
