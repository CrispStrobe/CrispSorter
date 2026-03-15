<script lang="ts">
    import { batchManager, type ProcessOverrides } from '../batch/store.svelte';
    import { i18n } from '../i18n.svelte';
    import { getSetting } from '../store';
    import { DEFAULT_PROVIDERS, type LLMProvider } from '../llm/client';
    import { open } from '@tauri-apps/plugin-dialog';
    import { listen } from '@tauri-apps/api/event';
    import { invoke } from '@tauri-apps/api/core';
    import { onMount } from 'svelte';
    import type { BatchItem } from '../types';
    import { stat } from '@tauri-apps/plugin-fs';
    import {
        Play, Trash2, Check, X, FileSearch, FolderOpen,
        Loader2, Eye, Edit, Rocket, CheckSquare, Copy,
        Square, Brain, Type, Search, Filter, ChevronDown, ChevronUp,
        Plus, Columns, Calendar, FileText, HardDrive, Hash,
        RefreshCw, AlertCircle, Code, Info, Scan
    } from 'lucide-svelte';

    let selectedItemId = $state<string | null>(null);
    let selectedItem = $derived(batchManager.items.find(i => i.id === selectedItemId));

    // Multi-selection state
    let selectedIds = $state<string[]>([]);
    let lastClickedId = $state<string | null>(null);
    let showFilters = $state(false);
    let showModeMenu = $state(false);
    let showColumnSelector = $state(false);

    // Re-analyze with options dropdown
    let showReanalyzeOpts = $state(false);
    let showReextractOpts = $state(false);
    let reanalProviders = $state<LLMProvider[]>([]);
    let reanalProviderId = $state('');
    let reanalModel = $state('');
    let reanalMaxChars = $state(5000);
    let reanalAuthorSort = $state(false);
    let reanalModels = $derived(reanalProviders.find(p => p.id === reanalProviderId)?.models ?? []);

    // Duplicates panel
    let showDuplicates = $state(false);
    let showSortOptions = $state(false);
    let duplicateGroups = $state<Array<{ size: number; items: BatchItem[] }>>([]);
    let checkingDuplicates = $state(false);
    let deepDupeCheck = $state(false);

    // Sorting state
    let sortColumn = $state<string>('file_name');
    let sortDirection = $state<'asc' | 'desc'>('asc');

    // Execution Report
    let lastExecutionStats = $state<any>(null);
    let showReportModal = $state(false);

    // Info Modal
    let showInfoModal = $state(false);
    let infoModalData = $state<any>(null);

    // Column visibility and width state
    let columns = $state([
        { id: 'status', label: i18n.t.batch.status, width: 90, visible: true, locked: true },
        { id: 'accepted', label: 'OK', width: 45, visible: true, locked: true },
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
        const handleKeyDown = (e: KeyboardEvent) => {
            if (e.key === 'Escape') {
                if (selectedIds.length > 0) selectedIds = [];
                showReportModal = false;
                showInfoModal = false;
            }
        };
        window.addEventListener('keydown', handleKeyDown);

        const unlisten = await listen('tauri://drag-drop', (event: any) => {
            const paths = event.payload.paths as string[];
            paths.forEach(path => {
                invoke<{ path: string; size: number }[]>('scan_folder', {
                    folderPath: path,
                    extensions: ['pdf', 'docx', 'txt', 'md', 'epub']
                }).then(entries => {
                    entries.forEach(e => batchManager.addItem(e.path, e.path.split(/[\\/]/).pop() || '', e.size));
                }).catch(e => console.error('[BatchReview] scan_folder error for dropped path:', path, e));
            });
        });

        // Load providers for re-analyze dropdown
        const savedProviders = await getSetting('providers');
        const merged = savedProviders
            ? DEFAULT_PROVIDERS.map(def => {
                const saved = (savedProviders as LLMProvider[]).find(p => p.id === def.id);
                return saved ? { ...def, ...saved } : def;
              })
            : DEFAULT_PROVIDERS;
        reanalProviders = merged;
        const activeId = await getSetting('activeProviderId', 'ollama') as string;
        reanalProviderId = activeId;
        const activeProv = merged.find(p => p.id === activeId) || merged[0];
        reanalModel = activeProv?.selectedModel || activeProv?.models?.[0] || '';
        reanalMaxChars = await getSetting('llmMaxChars', 5000) as number;
        reanalAuthorSort = await getSetting('authorSortEnabled', false) as boolean;

        return () => {
            unlisten();
            window.removeEventListener('keydown', handleKeyDown);
        };
    });

    async function handleAddFiles() {
        const selected = await open({
            multiple: true,
            filters: [{ name: 'Documents', extensions: ['pdf', 'docx', 'txt', 'md', 'epub'] }]
        });
        if (Array.isArray(selected)) {
            for (const path of selected) {
                const entries = await invoke<{ path: string; size: number }[]>('scan_folder', {
                    folderPath: path,
                    extensions: ['pdf', 'docx', 'txt', 'md', 'epub']
                }).catch(() => []);
                entries.forEach(e => batchManager.addItem(e.path, e.path.split(/[\\/]/).pop() || '', e.size));
            }
        }
    }

    async function handleAddFolder() {
        const selected = await open({ directory: true, multiple: false });
        if (typeof selected === 'string') {
            const entries = await invoke<{ path: string; size: number }[]>('scan_folder', {
                folderPath: selected,
                extensions: ['pdf', 'docx', 'txt', 'md', 'epub']
            });
            entries.forEach(e => batchManager.addItem(e.path, e.path.split(/[\\/]/).pop() || '', e.size));
        }
    }

    async function handleFindDuplicates() {
        checkingDuplicates = true;
        duplicateGroups = await batchManager.getDuplicateGroups(deepDupeCheck);
        checkingDuplicates = false;
        showDuplicates = true;
    }

    async function keepOnlyInGroup(groupIdx: number, keepId: string, deleteFromDisk: boolean) {
        const group = duplicateGroups[groupIdx];
        const toRemove = group.items.filter(i => i.id !== keepId);
        if (deleteFromDisk) {
            const paths = toRemove.map(i => i.originalPath);
            await invoke('delete_files', { paths }).catch(e => console.error('[Dupes] delete_files error:', e));
        }
        await batchManager.removeItems(toRemove.map(i => i.id));
        await handleFindDuplicates();
    }

    async function keepNewestInGroup(groupIdx: number, deleteFromDisk: boolean) {
        const group = duplicateGroups[groupIdx];
        const newest = group.items.reduce((a, b) => (a.modifiedAt > b.modifiedAt ? a : b));
        await keepOnlyInGroup(groupIdx, newest.id, deleteFromDisk);
    }

    async function removeAllInGroup(groupIdx: number, deleteFromDisk: boolean) {
        const group = duplicateGroups[groupIdx];
        if (deleteFromDisk) {
            const paths = group.items.map(i => i.originalPath);
            await invoke('delete_files', { paths }).catch(e => console.error('[Dupes] delete_files error:', e));
        }
        await batchManager.removeItems(group.items.map(i => i.id));
        await handleFindDuplicates();
    }

    function handleRowClick(e: MouseEvent | KeyboardEvent, id: string) {
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

    let allSelected = $derived(sortedItems.length > 0 && selectedIds.length === sortedItems.length);

    function selectAllVisible() {
        if (allSelected) {
            selectedIds = [];
        } else {
            selectedIds = sortedItems.map(i => i.id);
        }
    }

    function toggleId(id: string) {
        if (selectedIds.includes(id)) {
            selectedIds = selectedIds.filter(i => i !== id);
        } else {
            selectedIds = [...selectedIds, id];
        }
    }

    async function handleRedoSelected() {
        if (selectedIds.length === 0) return;
        await batchManager.reprocessItems(selectedIds);
    }

    async function handleRedoWithOptions() {
        if (selectedIds.length === 0) return;
        showReanalyzeOpts = false;
        const overrides: ProcessOverrides = {
            providerId: reanalProviderId || undefined,
            modelId: reanalModel || undefined,
            maxChars: reanalMaxChars || undefined,
            authorSort: reanalAuthorSort
        };
        await batchManager.reprocessItems(selectedIds, overrides);
    }

    function onReanalProviderChange() {
        const prov = reanalProviders.find(p => p.id === reanalProviderId);
        reanalModel = prov?.selectedModel || prov?.models?.[0] || '';
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

    async function handleBatchEnforceOcr() {
        if (selectedIds.length === 0) return;
        await batchManager.reprocessItems(selectedIds, { enforceOcr: true });
    }

    function handleBatchAccept(val: boolean) {
        if (selectedIds.length === 0) return;
        batchManager.setAcceptedItems(selectedIds, val);
    }

    async function startProcessing() {
        await batchManager.processAll();
    }

    function toggleSelectionAccepted(val: boolean) {
        const targetIds = batchManager.filteredItems.map(i => i.id);
        batchManager.setAcceptedItems(targetIds, val);
    }

    async function checkFileDetails(item: BatchItem) {
        try {
            console.log(`[BatchReview] checkFileDetails for ${item.originalName}`);
            const exists = await stat(item.originalPath).then(() => true).catch(() => false);
            const metadata = await stat(item.originalPath).catch(() => null);
            
            infoModalData = {
                id: item.id,
                exists,
                originalPath: item.originalPath,
                size: metadata ? formatSize(metadata.size) : '?',
                targetPath: item.targetPath || i18n.t.batch.path_hint,
                status: item.status,
                error: item.errorMessage
            };
            console.log(`[BatchReview] infoModalData set:`, infoModalData);
            showInfoModal = true;
        } catch (e: any) {
            console.error('[BatchReview] Error checking file details:', e);
        }
    }

    async function executeSorting(mode: 'move' | 'copy' | 'script_move' | 'script_copy' = 'move') {
        const accepted = batchManager.items.filter(i => i.isAccepted);
        if (accepted.length === 0) {
            console.log(`[BatchReview] No items accepted for sorting.`);
            return;
        }

        let confirmMsg = mode.includes('copy') 
            ? i18n.t.batch.confirm_copy.replace('{count}', accepted.length.toString())
            : i18n.t.batch.confirm_move.replace('{count}', accepted.length.toString());

        if (confirm(confirmMsg)) {
            console.log(`[BatchReview] Starting executeBatch(${mode}) for ${accepted.length} items`);
            const stats = await batchManager.executeBatch(mode);
            if (stats) {
                console.log(`[BatchReview] executeBatch finished. Showing report modal with stats:`, stats);
                lastExecutionStats = stats;
                showReportModal = true;
            } else {
                console.warn(`[BatchReview] executeBatch returned no stats.`);
            }
        }
    }

    async function handleReportChoice(choice: 'keep_problematic' | 'empty' | 'keep_all') {
        console.log(`[BatchReview] handleReportChoice: ${choice}`);
        showReportModal = false;
        if (choice === 'empty') {
            batchManager.items = [];
        } else if (choice === 'keep_problematic') {
            batchManager.items = batchManager.items.filter(i => i.status !== 'done');
        }
        await batchManager.saveCurrentSession();
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
    {#if selectedIds.length > 0}
        <div class="selection-toolbar">
            <span class="selection-info">
                <CheckSquare size={14} />
                {i18n.t.batch.selected_count.replace('{count}', selectedIds.length.toString())}
            </span>
            <div class="toolbar-divider"></div>
            <div class="dropdown-container">
                <div class="split-btn-group">
                    <button class="action-btn small" onclick={handleRedoSelected} title={i18n.t.batch.reanalyze}>
                        <RefreshCw size={14} /> {i18n.t.batch.reanalyze}
                    </button>
                    <button class="action-btn small split-caret" onclick={() => showReanalyzeOpts = !showReanalyzeOpts} title="Options">
                        <ChevronDown size={11} />
                    </button>
                </div>
                {#if showReanalyzeOpts}
                    <div class="dropdown-menu reanalyze-opts">
                        <div class="opts-row">
                            <label for="reanalProvider">{i18n.t.batch.reanalyze_provider}</label>
                            <select id="reanalProvider" bind:value={reanalProviderId} onchange={onReanalProviderChange}>
                                {#each reanalProviders as p}
                                    <option value={p.id}>{p.name}</option>
                                {/each}
                            </select>
                        </div>
                        <div class="opts-row">
                            <label for="reanalModel">{i18n.t.batch.reanalyze_model}</label>
                            <input id="reanalModel" type="text" bind:value={reanalModel} list="reanalModelList" placeholder="model id..." />
                            <datalist id="reanalModelList">
                                {#each reanalModels as m}<option value={m}></option>{/each}
                            </datalist>
                        </div>
                        <div class="opts-row">
                            <label for="reanalMaxChars">{i18n.t.batch.reanalyze_max_chars}</label>
                            <input id="reanalMaxChars" type="number" bind:value={reanalMaxChars} min="500" step="500" style="width: 80px;" />
                        </div>
                        <div class="opts-row">
                            <label for="reanalAuthorSort">{i18n.t.batch.reanalyze_author_step}</label>
                            <input id="reanalAuthorSort" type="checkbox" bind:checked={reanalAuthorSort} />
                        </div>
                        <button class="action-btn small primary opts-run" onclick={handleRedoWithOptions}>
                            <Play size={12} /> {i18n.t.batch.reanalyze_run} ({selectedIds.length})
                        </button>
                    </div>
                {/if}
            </div>
            
            <div class="dropdown-container">
                <div class="split-btn-group">
                    <button class="action-btn small" onclick={handleBatchReextract} title={i18n.t.batch.reextract}>
                        <FileSearch size={14} /> {i18n.t.batch.reextract}
                    </button>
                    <button class="action-btn small split-caret" onclick={() => showReextractOpts = !showReextractOpts} title="Options">
                        <ChevronDown size={11} />
                    </button>
                </div>
                {#if showReextractOpts}
                    <div class="dropdown-menu">
                        <button onclick={() => { handleBatchReextract(); showReextractOpts = false; }}>
                            <FileSearch size={14} /> {i18n.t.batch.reextract}
                        </button>
                        <button onclick={() => { handleBatchEnforceOcr(); showReextractOpts = false; }}>
                            <Scan size={14} /> {i18n.t.batch.reextract_ocr}
                        </button>
                    </div>
                {/if}
            </div>

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

    <div class="toolbar">
        <div class="left-actions">
            <div class="btn-group">
                <button class="icon-btn-main primary" onclick={handleAddFiles} title={i18n.t.batch.add_files}>
                    <Plus size={18} />
                </button>
                <button class="icon-btn-main primary" onclick={handleAddFolder} title={i18n.t.batch.add_folder}>
                    <FolderOpen size={18} />
                </button>
                <button class="icon-btn-main danger" onclick={() => batchManager.clear()} title={i18n.t.batch.clear_all}>
                    <Trash2 size={18} />
                </button>
            </div>

            <div class="dropdown-container">
                <button class="mode-select-btn" onclick={() => showModeMenu = !showModeMenu} aria-label="Extraction Mode">
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
                <input type="text" bind:value={batchManager.searchQuery} placeholder={i18n.t.batch.search_placeholder} aria-label="Search items" />
            </div>

            <button class="action-btn small" onclick={() => showFilters = !showFilters} class:active={showFilters} aria-label="Show Filters">
                <Filter size={16} />
            </button>

            <div class="dropdown-container">
                <button class="action-btn small" onclick={() => showColumnSelector = !showColumnSelector} class:active={showColumnSelector} aria-label="Select Columns">
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
            <button class="action-btn small" onclick={handleFindDuplicates} disabled={checkingDuplicates} title={i18n.t.batch.find_dupes}>
                <Copy size={14} />
                {#if checkingDuplicates}<span class="loader-anim"><Loader2 size={14} /></span>{/if}
            </button>

            {#if batchManager.isProcessing}
                <button class="action-btn danger small" onclick={() => batchManager.stopAll()}>
                    <Square size={16} /> {i18n.t.batch.stop}
                </button>
            {:else}
                <button class="action-btn success small" onclick={startProcessing}
                        disabled={batchManager.items.filter(i => i.status === 'queued' || i.status === 'error').length === 0}>
                    <Play size={16} /> {i18n.t.batch.start_batch}
                </button>
            {/if}

            <div class="btn-group">
                <button class="action-btn small" onclick={() => toggleSelectionAccepted(true)}>
                    <CheckSquare size={14} /> {i18n.t.batch.accept_all}
                </button>
                <button class="action-btn small" onclick={() => toggleSelectionAccepted(false)}>
                    <Square size={14} /> {i18n.t.batch.uncheck_all}
                </button>
            </div>
            
            <div class="dropdown-container">
                <div class="split-btn-group">
                    <button class="action-btn rocket-btn small" 
                            onclick={() => executeSorting('move')} 
                            disabled={batchManager.items.filter(i => i.isAccepted).length === 0}>
                        <Rocket size={16} /> {i18n.t.batch.execute}
                    </button>
                    <button class="action-btn rocket-btn small split-caret" 
                            onclick={() => showSortOptions = !showSortOptions}
                            disabled={batchManager.items.filter(i => i.isAccepted).length === 0}
                            aria-label="Sort Options">
                        <ChevronDown size={11} />
                    </button>
                </div>
                {#if showSortOptions}
                    <div class="dropdown-menu" style="right: 0; left: auto;">
                        <button onclick={() => { executeSorting('move'); showSortOptions = false; }}>
                            <Rocket size={14} /> {i18n.t.batch.execute_move}
                        </button>
                        <button onclick={() => { executeSorting('copy'); showSortOptions = false; }}>
                            <Copy size={14} /> {i18n.t.batch.execute_copy}
                        </button>
                        <button onclick={() => { executeSorting('script_move'); showSortOptions = false; }}>
                            <Code size={14} /> {i18n.t.batch.execute_script_move}
                        </button>
                        <button onclick={() => { executeSorting('script_copy'); showSortOptions = false; }}>
                            <Code size={14} /> {i18n.t.batch.execute_script_copy}
                        </button>
                    </div>
                {/if}
            </div>
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

    {#if showDuplicates}
        <div class="dupes-panel">
            <div class="dupes-header">
                <span><Copy size={14} /> {i18n.t.batch.find_dupes} — {duplicateGroups.length} {i18n.t.batch.dupe_groups}</span>
                <div style="display:flex;gap:8px;align-items:center;">
                    <label class="dupe-deep-label" for="deep-dupe-check">
                        <input type="checkbox" id="deep-dupe-check" bind:checked={deepDupeCheck} onchange={handleFindDuplicates} />
                        {i18n.t.batch.dupe_deep}
                    </label>
                    <button class="close-btn-minimal" onclick={() => showDuplicates = false}>×</button>
                </div>
            </div>
            {#if duplicateGroups.length === 0}
                <div class="dupes-empty">{i18n.t.batch.dupe_none}</div>
            {:else}
                <div class="dupes-list">
                    {#each duplicateGroups as group, gi}
                        <div class="dupe-group">
                            <div class="dupe-group-header">
                                <span class="dupe-size">{(group.size / 1024).toFixed(1)} KB — {group.items.length} files</span>
                                <div class="dupe-group-actions">
                                    <button class="action-btn small" onclick={() => keepNewestInGroup(gi, false)} title="Keep newest, remove others from list">
                                        Newest
                                    </button>
                                    <button class="action-btn small danger" onclick={() => keepNewestInGroup(gi, true)} title="Keep newest, delete others from disk">
                                        Newest + 🗑
                                    </button>
                                    <button class="action-btn small danger" onclick={() => { if (confirm('Delete ALL files in this group from disk?')) removeAllInGroup(gi, true); }} title="Delete all from disk">
                                        All 🗑
                                    </button>
                                </div>
                            </div>
                            {#each group.items as item}
                                <div class="dupe-row" class:selected={selectedIds.includes(item.id)}>
                                    <div class="dupe-info">
                                        <span class="dupe-name" title={item.originalPath}>{item.originalName}</span>
                                        <span class="dupe-meta">{item.originalPath.substring(0, item.originalPath.lastIndexOf('/') + 1)} · {new Date(item.modifiedAt).toLocaleDateString()}</span>
                                    </div>
                                    <div class="dupe-item-actions">
                                        <button class="action-btn small" onclick={() => keepOnlyInGroup(gi, item.id, false)} title="Keep only this one">Keep</button>
                                        <button class="action-btn small danger" onclick={() => keepOnlyInGroup(gi, item.id, true)} title="Keep this, delete others from disk">Keep+🗑</button>
                                        <button class="close-btn-minimal" onclick={() => batchManager.removeItems([item.id]).then(() => handleFindDuplicates())} title="Remove from list">×</button>
                                    </div>
                                </div>
                            {/each}
                        </div>
                    {/each}
                </div>
            {/if}
        </div>
    {/if}

    <div class="main-split">
        <div class="table-container">
            <table class="dense-table">
                <thead>
                    <tr>
                        <th style="width: 35px; min-width: 35px; text-align: center;">
                            <input type="checkbox" 
                                   checked={selectedIds.length === sortedItems.length && sortedItems.length > 0} 
                                   onchange={selectAllVisible} aria-label="Select all visible" />
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
                            <td onclick={e => { e.stopPropagation(); }} style="width: 35px; text-align: center;">
                                <input type="checkbox" 
                                       checked={selectedIds.includes(item.id)} 
                                       onchange={(e) => { e.stopPropagation(); toggleId(item.id); }}
                                       aria-label="Select item" />
                            </td>
                            {#each columns as col}
                                {#if col.visible}
                                    <td style="width: {col.width}px;">
                                        {#if col.id === 'status'}
                                            <span class="status-badge" class:status-active={['extracting', 'analyzing', 'moving'].includes(item.status)}>
                                                {item.status}
                                            </span>
                                        {:else if col.id === 'accepted'}
                                            <button class="ghost-toggle-btn" onclick={(e) => { e.stopPropagation(); item.isAccepted = !item.isAccepted; batchManager.saveCurrentSession(); }} title="Toggle Accept">
                                                {#if item.isAccepted}
                                                    <Check size={16} style="color: #10b981;" />
                                                {:else}
                                                    <X size={16} style="color: #ef4444; opacity: 0.5;" />
                                                {/if}
                                            </button>
                                        {:else if col.id === 'file_name'}
                                            <span class="file-name" title={item.originalPath}>{item.originalName}</span>
                                        {:else if col.id === 'title'}
                                            <input type="text" bind:value={item.suggestedTitle} onclick={e => e.stopPropagation()} class:fallback={item.suggestedTitle === 'Unknown Title'} aria-label="Suggested Title" />
                                        {:else if col.id === 'author'}
                                            <input type="text" bind:value={item.suggestedAuthor} onclick={e => e.stopPropagation()} class:fallback={item.suggestedAuthor === 'Unknown Author'} aria-label="Suggested Author" />
                                        {:else if col.id === 'year'}
                                            <input type="text" bind:value={item.suggestedYear} onclick={e => e.stopPropagation()} style="text-align: center;" aria-label="Suggested Year" />
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
                    <div style="display:flex; align-items:center; gap:8px;">
                        <h3>{i18n.t.batch.details}</h3>
                        <button class="icon-btn-minimal" onclick={() => checkFileDetails(selectedItem!)} title="Detailed Info">
                            <Info size={14} />
                        </button>
                    </div>
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
                        <h4>{i18n.t.batch.edit_metadata}</h4>
                        <div class="edit-fields">
                            <div class="field-row">
                                <span class="field-label">{i18n.t.batch.title}</span>
                                <input type="text" bind:value={selectedItem.suggestedTitle} aria-label="Edit Title" />
                            </div>
                            <div class="field-row">
                                <span class="field-label">{i18n.t.batch.author}</span>
                                <input type="text" bind:value={selectedItem.suggestedAuthor} aria-label="Edit Author" />
                            </div>
                            <div class="field-row">
                                <span class="field-label">{i18n.t.batch.year}</span>
                                <input type="text" bind:value={selectedItem.suggestedYear} aria-label="Edit Year" />
                            </div>
                        </div>
                    </div>

                    <div class="detail-section">
                        <h4>{i18n.t.batch.extracted_text}</h4>
                        <div class="text-preview">{selectedItem.extractedText || i18n.t.batch.extract_hint}</div>
                    </div>
                </div>
            </div>
        {/if}
    </div>

    <div class="status-footer">
        <div class="stats">
            <span class="stat-item">
                <FileText size={14} />
                {i18n.t.batch.stats_files.replace('{count}', batchManager.items.length.toString())}
            </span>
        </div>
    </div>
</div>

{#if showReportModal && lastExecutionStats}
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-overlay" onclick={() => showReportModal = false} role="button" tabindex="0" onkeydown={e => e.key === 'Escape' && (showReportModal = false)}>
        <div class="modal-content report-modal" onclick={e => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
            <div class="modal-header">
                <h2><Rocket size={18} /> {i18n.t.batch.report_title}</h2>
            </div>
            <div class="modal-body">
                <div class="report-stats">
                    {#if lastExecutionStats.mode.includes('copy')}
                        <div class="stat-line success"><Check size={16} /> {i18n.t.batch.report_copied.replace('{count}', lastExecutionStats.success)}</div>
                    {:else}
                        <div class="stat-line success"><Check size={16} /> {i18n.t.batch.report_moved.replace('{count}', lastExecutionStats.success)}</div>
                    {/if}
                    
                    {#if lastExecutionStats.notFound > 0}
                        <div class="stat-line warning"><AlertCircle size={16} /> {i18n.t.batch.report_not_found.replace('{count}', lastExecutionStats.notFound)}</div>
                    {/if}
                    
                    {#if lastExecutionStats.notWritable > 0}
                        <div class="stat-line danger"><X size={16} /> {i18n.t.batch.report_not_writable.replace('{count}', lastExecutionStats.notWritable)}</div>
                    {/if}
                </div>

                <div class="report-choices">
                    <button class="choice-btn" onclick={() => handleReportChoice('keep_problematic')}>
                        {i18n.t.batch.report_choice_keep_problematic}
                    </button>
                    <button class="choice-btn danger" onclick={() => handleReportChoice('empty')}>
                        {i18n.t.batch.report_choice_empty}
                    </button>
                    <button class="choice-btn secondary" onclick={() => handleReportChoice('keep_all')}>
                        {i18n.t.batch.report_choice_keep_all}
                    </button>
                </div>
            </div>
        </div>
    </div>
{/if}

{#if showInfoModal && infoModalData}
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-overlay" onclick={() => showInfoModal = false} role="button" tabindex="0" onkeydown={e => e.key === 'Escape' && (showInfoModal = false)}>
        <div class="modal-content info-modal" onclick={e => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
            <div class="modal-header">
                <h2><Info size={18} /> {i18n.t.batch.item_info.title}</h2>
            </div>
            <div class="modal-body info-body">
                <div class="info-grid">
                    <span class="info-label">{i18n.t.batch.item_info.exists}</span>
                    <span class="info-val">{infoModalData.exists ? '✅' : '❌'}</span>
                    
                    <span class="info-label">{i18n.t.batch.item_info.abs_path}</span>
                    <span class="info-val mono-path">{infoModalData.originalPath}</span>
                    
                    <span class="info-label">{i18n.t.batch.item_info.size}</span>
                    <span class="info-val">{infoModalData.size}</span>
                    
                    <span class="info-label">{i18n.t.batch.item_info.projected_path}</span>
                    <span class="info-val mono-path">{infoModalData.targetPath}</span>
                    
                    <span class="info-label">{i18n.t.batch.item_info.last_status}</span>
                    <span class="info-val">
                        <span class="status-badge" class:status-error={infoModalData.status === 'error'}>
                            {infoModalData.status}
                        </span>
                        {#if infoModalData.error}
                            <div class="error-msg">{infoModalData.error}</div>
                        {/if}
                    </span>
                </div>
                <button class="action-btn primary" style="margin-top:20px; width:100%; justify-content:center;" onclick={() => showInfoModal = false}>OK</button>
            </div>
        </div>
    </div>
{/if}

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

    .dupes-panel { background: #0f172a; border-bottom: 1px solid #1e293b; max-height: 320px; display: flex; flex-direction: column; }
    .dupes-header { display: flex; justify-content: space-between; align-items: center; padding: 8px 16px; border-bottom: 1px solid #1e293b; font-size: 0.8125rem; font-weight: 600; color: #94a3b8; flex-shrink: 0; }
    .dupes-empty { padding: 12px 16px; font-size: 0.8125rem; color: #52525b; }
    .dupes-list { overflow-y: auto; padding: 8px; display: flex; flex-direction: column; gap: 8px; flex: 1; }
    .dupe-group { background: #020617; border: 1px solid #1e293b; border-radius: 6px; overflow: hidden; }
    .dupe-group-header { display: flex; align-items: center; justify-content: space-between; padding: 4px 10px; background: #1e293b; gap: 8px; }
    .dupe-size { font-size: 0.65rem; font-weight: 700; color: #f59e0b; text-transform: uppercase; letter-spacing: 0.05em; }
    .dupe-group-actions { display: flex; gap: 4px; }
    .dupe-row { display: flex; align-items: center; justify-content: space-between; padding: 5px 10px; border-top: 1px solid #0f172a; gap: 8px; }
    .dupe-row.selected { background: #1e3a8a33; }
    .dupe-info { display: flex; flex-direction: column; flex: 1; overflow: hidden; }
    .dupe-name { font-size: 0.75rem; color: #cbd5e1; font-family: monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .dupe-meta { font-size: 0.65rem; color: #475569; font-family: monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .dupe-item-actions { display: flex; gap: 4px; align-items: center; flex-shrink: 0; }
    .dupe-deep-label { display: flex; align-items: center; gap: 6px; font-size: 0.7rem; color: #71717a; cursor: pointer; }

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

    .detail-pane { width: 400px; background: #0f172a; border-left: 1px solid #1e293b; display: flex; flex-direction: column; flex-shrink: 0; }
    .detail-header { padding: 10px 16px; border-bottom: 1px solid #1e293b; display: flex; justify-content: space-between; align-items: center; }
    .detail-content { padding: 12px; overflow-y: auto; display: flex; flex-direction: column; gap: 12px; }
    
    .detail-section h4 { font-size: 0.65rem; text-transform: uppercase; color: #71717a; margin: 0 0 6px 0; letter-spacing: 0.05em; }
    .path-preview { background: #020617; border: 1px solid #1e293b; padding: 8px; font-family: monospace; font-size: 0.7rem; border-radius: 6px; color: #94a3b8; word-break: break-all; }
    .text-preview { background: #020617; border: 1px solid #1e293b; padding: 10px; font-family: monospace; font-size: 0.75rem; border-radius: 6px; color: #94a3b8; min-height: 100px; max-height: 300px; overflow-y: auto; white-space: pre-wrap; }
    
    .edit-fields { display: flex; flex-direction: column; gap: 8px; }
    .field-row { display: flex; align-items: center; gap: 12px; }
    .field-label { width: 60px; font-size: 0.75rem; color: #a1a1aa; flex-shrink: 0; }
    .edit-fields input { flex: 1; padding: 4px 8px; border: 1px solid #334155; border-radius: 4px; background: #1e293b; color: white; font-size: 0.8125rem; }
    .edit-fields input:focus { border-color: #3b82f6; outline: none; }
    
    .loader-anim { display: inline-flex; animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

    .split-btn-group { display: flex; gap: 1px; }
    .split-btn-group .action-btn { border-radius: 0; }
    .split-btn-group .action-btn:first-child { border-radius: 6px 0 0 6px; }
    .split-caret { border-radius: 0 6px 6px 0 !important; padding: 4px 6px !important; border-left-color: #3f3f46 !important; }

    .reanalyze-opts { padding: 12px; min-width: 260px; display: flex; flex-direction: column; gap: 10px; }
    .opts-row { display: flex; align-items: center; gap: 8px; }
    .opts-row label { width: 80px; font-size: 0.7rem; font-weight: 600; color: #71717a; text-transform: uppercase; flex-shrink: 0; }
    .opts-row select, .opts-row input[type="text"], .opts-row input[type="number"] { flex: 1; background: #09090b; border: 1px solid #27272a; color: white; border-radius: 4px; padding: 4px 8px; font-size: 0.75rem; }
    .opts-row input[type="checkbox"] { width: 14px; height: 14px; cursor: pointer; }
    .opts-run { width: 100%; justify-content: center; margin-top: 4px; }
    .action-btn.primary { background: #3b82f6; color: white; border-color: #3b82f6; }

    .ghost-toggle-btn {
        background: transparent;
        border: none;
        cursor: pointer;
        display: flex;
        align-items: center;
        justify-content: center;
        width: 100%;
        height: 100%;
        padding: 0;
        transition: opacity 0.2s;
    }
    .ghost-toggle-btn:hover { opacity: 0.8; }

    .icon-btn-minimal { background: transparent; border: none; color: #71717a; cursor: pointer; display: flex; align-items: center; padding: 4px; border-radius: 4px; }
    .icon-btn-minimal:hover { color: #3b82f6; background: #1e3a8a33; }

    .modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.8); z-index: 1000; display: flex; align-items: center; justify-content: center; }
    .modal-content { background: #18181b; border: 1px solid #27272a; border-radius: 12px; width: 450px; box-shadow: 0 25px 50px -12px rgba(0,0,0,0.5); overflow: hidden; }
    .modal-header { padding: 16px 20px; border-bottom: 1px solid #27272a; }
    .modal-header h2 { font-size: 1.1rem; margin: 0; display: flex; align-items: center; gap: 10px; color: #f4f4f5; }
    .modal-body { padding: 20px; }
    
    .report-stats { display: flex; flex-direction: column; gap: 12px; margin-bottom: 24px; }
    .stat-line { display: flex; align-items: center; gap: 10px; font-weight: 600; font-size: 0.9rem; }
    .stat-line.success { color: #10b981; }
    .stat-line.warning { color: #f59e0b; }
    .stat-line.danger { color: #ef4444; }

    .report-choices { display: flex; flex-direction: column; gap: 10px; }
    .choice-btn { width: 100%; padding: 10px; border-radius: 8px; border: 1px solid #3b82f6; background: #1e3a8a33; color: #60a5fa; font-weight: 600; cursor: pointer; text-align: left; transition: all 0.2s; }
    .choice-btn:hover { background: #1e3a8a66; }
    .choice-btn.danger { border-color: #ef4444; background: #450a0a33; color: #f87171; }
    .choice-btn.danger:hover { background: #450a0a66; }
    .choice-btn.secondary { border-color: #27272a; background: #27272a; color: #a1a1aa; }
    .choice-btn.secondary:hover { background: #3f3f46; color: white; }

    .status-footer { padding: 6px 16px; background: #18181b; border-top: 1px solid #27272a; display: flex; align-items: center; font-size: 0.75rem; color: #71717a; }
    .stats { display: flex; align-items: center; gap: 12px; }
    .stat-item { display: flex; align-items: center; gap: 6px; }

    .info-modal { width: 500px; }
    .info-grid { display: grid; grid-template-columns: 140px 1fr; gap: 16px; }
    .info-label { font-size: 0.75rem; color: #71717a; text-transform: uppercase; font-weight: 600; padding-top: 2px; }
    .info-val { font-size: 0.875rem; color: #f4f4f5; line-height: 1.4; }
    .mono-path { font-family: monospace; font-size: 0.75rem; color: #94a3b8; word-break: break-all; background: #09090b; padding: 4px 8px; border-radius: 4px; border: 1px solid #27272a; }
    .error-msg { margin-top: 8px; padding: 8px; background: #450a0a33; border: 1px solid #ef444433; border-radius: 6px; color: #f87171; font-size: 0.8125rem; }
</style>
