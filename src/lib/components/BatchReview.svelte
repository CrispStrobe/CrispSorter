<script lang="ts">
    import { inWorkflow, processingOverrides, availableDocumentTools, documentToolRequest, type WorkflowStage, type ProcessingPreset } from '$lib/workflow';
    import { caps } from '$lib/capabilities';
    let { workflow = 'inbox' }: { workflow?: WorkflowStage } = $props();
    let processingPreset = $state<ProcessingPreset>('balanced');
    $effect(() => {
        if (batchManager.filterStatus !== 'all' && !inWorkflow({ status: batchManager.filterStatus as BatchStatus }, workflow)) {
            batchManager.filterStatus = 'all';
        }
    });
    const workflowItems = $derived(batchManager.filteredItems.filter(item => inWorkflow(item, workflow)));
    import {
        probeFolder,
        requestFolderAccess,
        declineFolder,
        pathContains,
    } from '../folderAccess';
    import IntendedPurposeGate from './IntendedPurposeGate.svelte';
    import AiGeneratedBadge from './AiGeneratedBadge.svelte';
    import { batchManager, isUnknownSentinel, type ProcessOverrides } from '../batch/store.svelte';
    import { createSortPlan, textSidecarPath, type SortMode, type SortPlan } from '../batch/sortPlan';
    import { isProviderUri, pickProviderDocuments } from '../mobileProvider';
    import { upsertItem, upsertItemsBulk, deleteItems, clearBatch } from '../batchStore';
    import { i18n } from '../i18n.svelte';
    import { getSetting } from '../store';
    import { AUDIO_EXTENSIONS, MULTIMODAL_EXTENSIONS } from '../extractors';
    import { DEFAULT_PROVIDERS, type LLMProvider, llmClient } from '../llm/client';
    import { open, save, ask } from '@tauri-apps/plugin-dialog';
    import { listen } from '@tauri-apps/api/event';
    import { invoke } from '@tauri-apps/api/core';
    import { onMount } from 'svelte';
    import type { BatchItem, BatchStatus } from '../types';
    import { stat, writeTextFile } from '@tauri-apps/plugin-fs';
    import {
        Play, Trash2, Check, X, FileSearch, FolderOpen,
        Loader2, Eye, Edit, Rocket, CheckSquare, Copy,
        Square, Brain, Type, Search, Filter, ChevronDown, ChevronUp,
        Plus, Columns, Calendar, FileText, HardDrive, Hash,
        RefreshCw, AlertCircle, Code, Info, Scan, UploadCloud, FileDown
    } from 'lucide-svelte';
    import { buildBibFile } from '../export/bibtex';
    import { logInfo, logWarn } from '../log';

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
    let reanalModelsFetched = $state<string[]>([]);
    let reanalModelsLoading = $state(false);

    // Detail pane local edit state (buffered — not immediately written to item)
    let detailTitle  = $state('');
    let detailAuthor = $state('');
    let detailYear   = $state('');
    let detailDirty  = $derived(
        selectedItem != null && (
            detailTitle  !== (selectedItem.suggestedTitle  ?? '') ||
            detailAuthor !== (selectedItem.suggestedAuthor ?? '') ||
            detailYear   !== (selectedItem.suggestedYear   ?? '')
        )
    );

    // Sync local state when the selected item changes
    $effect(() => {
        if (selectedItem) {
            detailTitle  = selectedItem.suggestedTitle  ?? '';
            detailAuthor = selectedItem.suggestedAuthor ?? '';
            detailYear   = selectedItem.suggestedYear   ?? '';
        }
    });

    /** Audio/video extensions warrant a "Transcribing" badge instead
     *  of the generic "Extracting" — the user knows they dropped an
     *  mp3 and is waiting on whisper, not on a docx parser. */
    const AUDIO_EXTS_SET = new Set<string>(AUDIO_EXTENSIONS);

    function statusLabel(status: BatchStatus, extension?: string): string {
        const t = i18n.t.batch;
        if (status === 'extracting' && extension && AUDIO_EXTS_SET.has(extension.toLowerCase())) {
            return t.status_transcribing;
        }
        const map: Record<BatchStatus, string> = {
            queued: t.status_queued,
            extracting: t.status_extracting,
            analyzing: t.status_analyzing,
            review: t.status_review,
            ready: t.status_ready,
            unfinished: t.status_unfinished,
            moving: t.status_moving,
            done: t.status_done,
            error: t.status_error,
        };
        return map[status] ?? status;
    }

    async function saveDetailChanges() {
        if (!selectedItem) return;
        selectedItem.suggestedTitle  = detailTitle;
        selectedItem.suggestedAuthor = detailAuthor;
        selectedItem.suggestedYear   = detailYear;
        await batchManager.recalculateTargetPath(selectedItem.id);
    }

    /** Enter on any of the three metadata inputs commits, Escape reverts. */
    function onMetaKey(e: KeyboardEvent) {
        if (e.key === 'Enter') {
            e.preventDefault();
            if (detailDirty) saveDetailChanges();
        } else if (e.key === 'Escape' && selectedItem && detailDirty) {
            e.preventDefault();
            detailTitle  = selectedItem.suggestedTitle  ?? '';
            detailAuthor = selectedItem.suggestedAuthor ?? '';
            detailYear   = selectedItem.suggestedYear   ?? '';
        }
    }

    // Duplicates panel
    let showDuplicates = $state(false);
    let showSortOptions = $state(false);
    let duplicateGroups = $state<Array<{ size: number; items: BatchItem[] }>>([]);
    let checkingDuplicates = $state(false);
    let deepDupeCheck = $state(false);

    // Sorting state
    let sortPlan = $state.raw<SortPlan | null>(null);
    let planLabels = $derived(i18n.lang === 'de' ? {
        title: 'Sortierplan prüfen', files: 'Datei(en)', move: 'Verschieben', copy: 'Kopieren',
        scriptMove: 'Verschiebe-Skript erstellen', scriptCopy: 'Kopier-Skript erstellen',
        excluded: 'akzeptierte Datei(en) ausgeschlossen: kein Ziel, ungeeigneter Status oder übersprungenes Duplikat.',
        sidecars: 'Extrahierter Text wird, falls vorhanden, ebenfalls gespeichert.',
        from: 'Von:', to: 'Nach:', text: 'Text:', cancel: 'Abbrechen', confirm: 'Bestätigen und ausführen', preparing: 'Vorbereitung…',
    } : {
        title: 'Review sort plan', files: 'file(s)', move: 'Move', copy: 'Copy',
        scriptMove: 'Generate move script', scriptCopy: 'Generate copy script',
        excluded: 'accepted file(s) excluded because they have no destination, have an ineligible status, or are skipped duplicates.',
        sidecars: 'Extracted text will also be saved where available.',
        from: 'From:', to: 'To:', text: 'Text:', cancel: 'Cancel', confirm: 'Confirm and execute', preparing: 'Preparing…',
    });
    let preparingSort = $state(false);
    let confirmingSort = $state(false);
    let planIndexAfterSort = false;

    let sortColumn = $state<string>('file_name');
    let sortDirection = $state<'asc' | 'desc'>('asc');

    // Execution Report
    let lastExecutionStats = $state<any>(null);
    let showReportModal = $state(false);

    // Sort options (P15)
    let skipDuplicatesOnSort = $state(false);
    // Index integration
    let indexAfterSort     = $state(false);
    let indexingSelected   = $state(false);
    let toastMessage = $state<string | null>(null);
    let toastTimeout: ReturnType<typeof setTimeout> | null = null;

    function showToast(msg: string) {
        toastMessage = msg;
        if (toastTimeout) clearTimeout(toastTimeout);
        toastTimeout = setTimeout(() => toastMessage = null, 5000);
    }

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
        // P13.6 Step 2: whisper-detected source language for audio/video
        // items.  Default-hidden (most batches are text-only); toggle via
        // column visibility menu.  Future: auto-show when at least one
        // audio item is in the batch.
        { id: 'language', label: i18n.t.batch.language ?? 'Lang', width: 60, visible: false },
        // P13.6 Step 3b: audio L2 duration from symphonia probe.  Renders
        // as "MM:SS" or "H:MM:SS" — same format conventions audio players
        // use.  Default-hidden; toggle via column visibility menu.
        { id: 'duration', label: i18n.t.batch.duration ?? 'Dur', width: 70, visible: false },
        { id: 'path', label: i18n.t.batch.path, width: 400, visible: false },
    ]);

    // Resizing logic
    let resizingColIdx = $state<number | null>(null);
    let startX = 0;
    let startWidth = 0;
    let tableContainerEl = $state<HTMLElement | null>(null);

    // Total table width (sum of visible columns + checkbox column)
    let tableWidth = $derived(
        35 + columns.filter(c => c.visible).reduce((sum, c) => sum + c.width, 0)
    );

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
            columns[resizingColIdx].width = Math.max(30, startWidth + diff);
        }
    }

    function stopResizing() {
        resizingColIdx = null;
        window.removeEventListener('mousemove', handleMouseMove);
        window.removeEventListener('mouseup', stopResizing);
    }

    function scaleColumnsToContainer() {
        if (!tableContainerEl) return;
        const available = tableContainerEl.clientWidth - 35 - 2; // minus checkbox col and borders
        const visibleCols = columns.filter(c => c.visible);
        const totalDefault = visibleCols.reduce((sum, c) => sum + c.width, 0);
        if (available > totalDefault) {
            const scale = available / totalDefault;
            visibleCols.forEach(c => { c.width = Math.floor(c.width * scale); });
        }
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
        const items = [...workflowItems];
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
                case 'language': valA = a.detectedLanguage ?? ''; valB = b.detectedLanguage ?? ''; break;
                case 'duration': valA = a.audioDurationSeconds ?? -1; valB = b.audioDurationSeconds ?? -1; break;
                case 'path': valA = a.originalPath; valB = b.originalPath; break;
            }

            if (valA < valB) return sortDirection === 'asc' ? -1 : 1;
            if (valA > valB) return sortDirection === 'asc' ? 1 : -1;
            return 0;
        });
    });

    onMount(() => {
        let cleanup = () => {};

        (async () => {
            // Scale columns to fill the available container width
            await new Promise(r => setTimeout(r, 0));
            scaleColumnsToContainer();

            const handleKeyDown = async (e: KeyboardEvent) => {
                // Skip when the user is typing into an input/textarea/contenteditable.
                const target = e.target as HTMLElement | null;
                const isEditable = !!target && (
                    target.tagName === 'INPUT' ||
                    target.tagName === 'TEXTAREA' ||
                    target.tagName === 'SELECT' ||
                    target.isContentEditable
                );

                if (e.key === 'Escape') {
                    if (selectedIds.length > 0) selectedIds = [];
                    showReportModal = false;
                    if (!confirmingSort) sortPlan = null;
                    showInfoModal = false;
                    return;
                }

                // Delete / Backspace removes the selected entries (Stapel view).
                if (!isEditable && (e.key === 'Delete' || e.key === 'Backspace')) {
                    if (selectedIds.length === 0) return;
                    e.preventDefault();
                    if (confirm(i18n.t.history.delete_confirm)) {
                        await batchManager.removeItems(selectedIds);
                        selectedIds = [];
                    }
                }
            };
            window.addEventListener('keydown', handleKeyDown);

            const unlisten = await listen('tauri://drag-drop', (event: any) => {
                const paths = event.payload.paths as string[];
                paths.forEach(path => {
                    invoke<{ path: string; size: number }[]>('scan_folder', {
                        folderPath: path,
                        // Multimodal: scan_folder picks up audio/video too,
                        // and extractText routes them through the Rust audio
                        // dispatch when the batch item is processed.
                        extensions: [...MULTIMODAL_EXTENSIONS]
                    }).then(entries => {
                        batchManager.addItemsBulk(entries.map(e => ({ path: e.path, name: e.path.split(/[\\/]/).pop() || '', size: e.size })));
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
            llmClient.llamacppPort = await getSetting('llamacppPort', 8080) as number;
            llmClient.mlxPort = await getSetting('mlxPort', 8000) as number;
            // Pre-fetch models for the active provider
            onReanalProviderChange();

            cleanup = () => {
                unlisten();
                window.removeEventListener('keydown', handleKeyDown);
            };
        })();

        return () => cleanup();
    });

    async function handleAddFiles() {
        if ($caps.platform === 'android') {
            await addProviderDocuments(false);
            return;
        }
        const selected = await open({
            multiple: true,
            // Same multimodal whitelist as the drag-drop path so picker
            // and drop accept the same files (documents + images +
            // audio/video).
            filters: [{ name: 'Documents, Audio & Video', extensions: [...MULTIMODAL_EXTENSIONS] }]
        });
        if (Array.isArray(selected)) {
            for (const path of selected) {
                const entries = await invoke<{ path: string; size: number }[]>('scan_folder', {
                    folderPath: path,
                    extensions: [...MULTIMODAL_EXTENSIONS]
                }).catch(() => []);
                batchManager.addItemsBulk(entries.map(e => ({ path: e.path, name: e.path.split(/[\\/]/).pop() || '', size: e.size })));
            }
        }
    }

    async function handleAddFolder() {
        if ($caps.platform === 'android') {
            await addProviderDocuments(true);
            return;
        }
        const selected = await open({ directory: true, multiple: false });
        if (typeof selected === 'string') {
            const entries = await invoke<{ path: string; size: number }[]>('scan_folder', {
                folderPath: selected,
                // Multimodal: pick up audio/video alongside docs+images.
                extensions: [...MULTIMODAL_EXTENSIONS]
            });
            batchManager.addItemsBulk(entries.map(e => ({ path: e.path, name: e.path.split(/[\\/]/).pop() || '', size: e.size })));
        }
    }

    async function addProviderDocuments(folder: boolean) {
        try {
            const result = await pickProviderDocuments(folder);
            if (!result) return; // User cancelled the system picker.
            await batchManager.addItemsBulk(result.accepted.map(file => ({
                path: file.uri, name: file.name, size: Math.max(0, file.size),
            })));
            const skipped = result.unsupported + result.oversized;
            showToast(i18n.lang === 'de'
                ? `${result.accepted.length} Dokument(e) ausgewählt; ${skipped} nicht unterstützt oder über 16 MiB. Nur Lesen.`
                : `${result.accepted.length} document(s) selected; ${skipped} unsupported or over 16 MiB. Read-only.`);
        } catch (e: any) {
            showToast(i18n.lang === 'de'
                ? `Dateiauswahl fehlgeschlagen: ${e?.message ?? e}`
                : `Could not access the selection: ${e?.message ?? e}`);
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
        if (deleteFromDisk && toRemove.some(i => isProviderUri(i.originalPath))) {
            showToast('Provider documents are read-only and cannot be deleted here.');
            return;
        }
        if (deleteFromDisk) {
            const paths = toRemove.map(i => i.originalPath).filter(path => !isProviderUri(path));
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
        if (deleteFromDisk && group.items.some(i => isProviderUri(i.originalPath))) {
            showToast('Provider documents are read-only and cannot be deleted here.');
            return;
        }
        if (deleteFromDisk) {
            const paths = group.items.map(i => i.originalPath).filter(path => !isProviderUri(path));
            await invoke('delete_files', { paths }).catch(e => console.error('[Dupes] delete_files error:', e));
        }
        await batchManager.removeItems(group.items.map(i => i.id));
        await handleFindDuplicates();
    }

    function handleRowClick(e: MouseEvent | KeyboardEvent, id: string) {
        // Reported: clicking anywhere on a row was popping up the
        // bulk-actions bar (because plain-click set `selectedIds`).
        // Split the concepts:
        //   * `selectedItemId` — single-focus highlight, drives the
        //     preview/detail panel.  Plain click sets this only.
        //   * `selectedIds`    — bulk selection, drives the actions
        //     bar.  Toggled exclusively via the left-edge checkbox
        //     (`toggleId`), shift-click for range, or cmd/ctrl-click
        //     for multi-toggle.  Plain click DOES NOT touch it.
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
            // Plain click: focus only.  Do NOT collapse bulk selection
            // to this one row — that was the bug; users expected the
            // detail-panel selection to be independent of bulk
            // selection.
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

    async function onReanalProviderChange() {
        const prov = reanalProviders.find(p => p.id === reanalProviderId);
        reanalModel = prov?.selectedModel || prov?.models?.[0] || '';
        reanalModelsFetched = prov?.models ?? [];

        if (['mistralrs', 'webllm', 'ort'].includes(reanalProviderId)) return;

        reanalModelsLoading = true;
        try {
            const fetched = await llmClient.fetchModels(reanalProviderId, prov?.apiKey, prov?.baseUrl);
            reanalModelsFetched = fetched;
            if (fetched.length > 0 && !reanalModel) reanalModel = fetched[0];
        } catch {
            reanalModelsFetched = prov?.models ?? [];
        } finally {
            reanalModelsLoading = false;
        }
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

    async function addSelectedToIndex() {
        if (selectedIds.length === 0) return;
        const cfg = await invoke<{ enabled: boolean }>('index_get_config').catch(() => ({ enabled: false }));
        if (!cfg.enabled) {
            showToast('Search index is not enabled. Enable it in Settings → Search Index.');
            return;
        }
        indexingSelected = true;
        const items = batchManager.items.filter(i => selectedIds.includes(i.id));
        const BATCH_SIZE = 16;
        let ok = 0, fail = 0;

        type IngestInput = {
            fullText: string; fullTextMd: string; headings: string[];
            title: string | null; author: string | null; year: number | null;
            filename: string; ext: string; language: string;
            locationUri: string; ownerId: string; sourceHash: string; tags: string[];
        };
        const inputs: IngestInput[] = [];
        for (const item of items) {
            if (isProviderUri(item.originalPath) || !item.extractedText || item.extractedText.length < 20) { fail++; continue; }
            inputs.push({
                fullText:    item.extractedText,
                fullTextMd:  '',
                headings:    [],
                title:       item.suggestedTitle  || null,
                author:      item.suggestedAuthor || null,
                year:        item.suggestedYear   ? parseInt(item.suggestedYear) : null,
                filename:    item.originalName,
                ext:         item.extension || '',
                language:    'de',
                locationUri: `crisp+local://local/${item.originalPath}`,
                ownerId:     'local',
                sourceHash:  item.id,
                tags:        [],
            });
        }

        for (let off = 0; off < inputs.length; off += BATCH_SIZE) {
            const batch = inputs.slice(off, off + BATCH_SIZE);
            try {
                await invoke('index_ingest_batch', { inputs: batch });
                ok += batch.length;
            } catch (e) {
                console.error('[Index] batch ingest failed:', e);
                fail += batch.length;
            }
        }

        indexingSelected = false;
        showToast(`Index: ${ok} erfolgreich, ${fail} fehlgeschlagen`);
    }

    async function startProcessing() {
        await batchManager.processAll(processingOverrides(processingPreset));
    }

    function toggleSelectionAccepted(val: boolean) {
        const targetIds = workflowItems.map(i => i.id);
        batchManager.setAcceptedItems(targetIds, val);
    }

    async function checkFileDetails(item: BatchItem) {
        try {
            console.log(`[BatchReview] checkFileDetails for ${item.originalName}`);
            const provider = isProviderUri(item.originalPath);
            const exists = provider ? null : await stat(item.originalPath).then(() => true).catch(() => false);
            const metadata = provider ? null : await stat(item.originalPath).catch(() => null);
            
            infoModalData = {
                id: item.id,
                exists,
                originalPath: item.originalPath,
                size: metadata ? formatSize(metadata.size) : provider && item.size > 0 ? formatSize(item.size) : '?',
                targetPath: provider ? 'Read-only provider document' : item.targetPath || i18n.t.batch.path_hint,
                status: item.status,
                statusDetail: item.statusDetail,
                error: item.errorMessage
            };
            console.log(`[BatchReview] infoModalData set:`, $state.snapshot(infoModalData));
            showInfoModal = true;
        } catch (e: any) {
            console.error('[BatchReview] Error checking file details:', e);
        }
    }

    /**
     * Make sure every destination folder can actually be written to,
     * asking the user to grant access where macOS has not.
     *
     * Returns `false` only when a needed folder was left ungranted *and*
     * nothing can be sorted without it — never for a folder the user
     * declined, since "stop asking" has to mean the sort still runs and
     * reports what it could not do.
     *
     * Distinct roots only: one grant covers a whole subtree, so a
     * thousand items under one destination is one question.
     */
    async function ensureDestinationsWritable(items: Array<{ targetPath?: string }>): Promise<boolean> {
        const parents = new Set<string>();
        for (const it of items) {
            if (!it.targetPath) continue;
            const p = it.targetPath.replace(/[/\\][^/\\]*$/, '');
            if (p) parents.add(p);
        }
        if (parents.size === 0) return true;

        // Probe the shallowest paths first: granting a parent often makes
        // the deeper probes moot, so this asks the fewest questions.
        const ordered = [...parents].sort((a, b) => a.length - b.length);
        const granted: string[] = [];
        let blocked = 0;

        for (const dir of ordered) {
            if (granted.some(g => pathContains(g, dir))) continue;
            let status;
            try {
                status = await probeFolder(dir);
            } catch (e) {
                logWarn(`Could not probe ${dir}: ${e}`);
                continue;
            }
            if (status.writability.state === 'writable') continue;

            if (!status.canRequest) {
                // A real filesystem problem — picking the folder cannot
                // fix a read-only volume, so do not pretend otherwise.
                logWarn(`${dir} is not writable: ${'reason' in status.writability ? status.writability.reason : ''}`);
                blocked++;
                continue;
            }
            if (!status.shouldAsk) {
                logInfo(`Not asking about ${dir} (already answered).`);
                blocked++;
                continue;
            }

            const wants = await ask(
                i18n.t.batch.grant_folder_prompt.replace('{folder}', dir),
                { title: 'CrispSorter', kind: 'warning' }
            ).catch(() => false);

            if (!wants) {
                // "No" here means this run; it does not silently become
                // "never ask again" — that has to be chosen explicitly.
                const never = await ask(
                    i18n.t.batch.grant_folder_never_ask.replace('{folder}', dir),
                    { title: 'CrispSorter', kind: 'info' }
                ).catch(() => false);
                if (never) await declineFolder(dir).catch(e => logWarn(`decline failed: ${e}`));
                blocked++;
                continue;
            }

            const outcome = await requestFolderAccess(dir);
            if (outcome.ok) {
                granted.push(outcome.granted);
                logInfo(`Folder access granted and saved: ${outcome.granted}`);
            } else {
                logWarn(`Folder access not granted for ${dir}: ${outcome.reason} ${outcome.message ?? ''}`);
                if (outcome.reason === 'wrong-folder' && outcome.message) {
                    showToast(outcome.message);
                }
                blocked++;
            }
        }

        // Proceed as long as something can still be written. A partial
        // sort that reports the rest beats refusing to do any of it.
        if (blocked > 0 && granted.length === 0 && blocked === ordered.length) {
            showToast(i18n.t.batch.grant_folder_none);
            return false;
        }
        return true;
    }

    async function executeSorting(mode: SortMode = 'move') {
        if ($caps.platform === 'ios') {
            showToast('Sorting iOS imports is unavailable until their original provider identity can be retained.');
            return;
        }
        if (batchManager.isExecuting || preparingSort || confirmingSort || sortPlan) {
            showToast(i18n.t.batch.executing);
            return;
        }
        preparingSort = true;
        try {
            const skipDuplicates = skipDuplicatesOnSort;
            // Recover missing targets before the user sees and approves the plan.
            const missing = batchManager.items.filter(i => i.isAccepted && !i.targetPath
                && !(skipDuplicates && i.isDuplicatePrimary === false));
            for (const item of missing) await batchManager.recalculateTargetPath(item.id, false);
            const saveTxt = await getSetting('saveTxt', true) as boolean;
            const plan = createSortPlan(batchManager.items, mode, skipDuplicates, saveTxt);
            if (!plan.items.length) {
                const missingTargets = plan.candidates.filter(i => !i.targetPath).length;
                const excluded = plan.candidates.length - missingTargets;
                const detail = plan.candidates.length ? ` (${missingTargets} without a destination, ${excluded} ineligible or skipped)` : '';
                showToast(`${i18n.t.batch.no_items_ready}${detail}`);
                return;
            }
            planIndexAfterSort = indexAfterSort;
            sortPlan = plan;
        } catch (e: any) {
            logWarn(`Could not prepare sort preview: ${e?.message ?? e}`);
            showToast(`Could not prepare sort preview: ${e?.message ?? e}`);
        } finally {
            preparingSort = false;
        }
    }

    async function confirmSorting() {
        const plan = sortPlan;
        if (!plan || confirmingSort) return;
        confirmingSort = true;
        try {
            if (!(await ensureDestinationsWritable(plan.items)) || sortPlan !== plan) return;
            const stats = await batchManager.executeBatch(plan);
            sortPlan = null;
            lastExecutionStats = stats;
            showReportModal = true;
            if (planIndexAfterSort && !plan.mode.startsWith('script')) {
                selectedIds = plan.items.map(i => i.id);
                await addSelectedToIndex();
                selectedIds = [];
            }
        } catch (e: any) {
            logWarn(`Sort aborted: ${e?.message ?? e}`);
            sortPlan = null;
            showToast(`Sort aborted: ${e?.message ?? e}`);
        } finally {
            confirmingSort = false;
        }
    }

    // Local alias for the shared sentinel-recogniser. Used to apply the
    // .fallback CSS class on the Title/Author/Year inputs when the LLM
    // came back with a placeholder. The single source of truth is in
    // batch/store.svelte.ts so the auto-accept rule and the UI class
    // can never disagree.
    function isUnknown(v: string | undefined): boolean {
        return isUnknownSentinel(v);
    }

    /** Per-row 3-pip progress indicator: M | T | L
     *  (Metadata read / Text extracted / LLM analysed).
     *
     *  Each pip is one of:
     *    'gray'    — pending / not yet attempted
     *    'pulse'   — actively running (this stage is happening now)
     *    'ok'      — succeeded
     *    'partial' — got something but it's a placeholder (e.g. LLM
     *                returned "Unknown Author")
     *    'fail'    — explicit failure
     *    'na'      — N/A for this filetype (semi-transparent)
     *
     *  Computed per-item; cheap (3 string comparisons + a sentinel
     *  check) so a 1000-row Stapel re-renders without trouble. */
    type PipState = 'gray' | 'pulse' | 'ok' | 'partial' | 'fail' | 'na';
    function progressPips(item: BatchItem): { m: PipState; t: PipState; l: PipState } {
        // ── M (metadata) ───────────────────────────────────────────
        let m: PipState;
        switch (item.metadataReadStatus) {
            case 'ok':     m = 'ok';     break;
            case 'failed': m = 'fail';   break;
            case 'na':     m = 'na';     break;
            default:       m = item.status === 'extracting' ? 'pulse' : 'gray';
        }

        // ── T (text) ───────────────────────────────────────────────
        let t: PipState;
        const textLen = item.extractedText?.length ?? 0;
        if (textLen >= 100)                   t = 'ok';        // healthy extraction
        else if (textLen > 0)                 t = 'partial';   // got something but suspiciously short
        else if (item.status === 'extracting') t = 'pulse';
        else if (item.status === 'error' && /extract/i.test(item.errorMessage ?? '')) t = 'fail';
        else                                   t = 'gray';

        // ── L (LLM) ────────────────────────────────────────────────
        let l: PipState;
        const author = item.suggestedAuthor;
        const llmRan = !!(author || item.suggestedTitle || item.suggestedYear);
        if (llmRan && author && !isUnknownSentinel(author))            l = 'ok';
        else if (llmRan && author && isUnknownSentinel(author))        l = 'partial';
        else if (item.status === 'analyzing')                          l = 'pulse';
        else if (item.status === 'error' && !/extract/i.test(item.errorMessage ?? '')) l = 'fail';
        else                                                            l = 'gray';

        return { m, t, l };
    }

    async function handleReportChoice(choice: 'keep_problematic' | 'remove_done' | 'empty' | 'keep_all') {
        showReportModal = false;
        if (choice === 'empty') {
            batchManager.items = [];
            await clearBatch();
        } else if (choice === 'keep_problematic') {
            const removedIds = batchManager.items.filter(i => i.status === 'done').map(i => i.id);
            batchManager.items = batchManager.items.filter(i => i.status !== 'done');
            if (removedIds.length > 0) await deleteItems(removedIds);
        } else if (choice === 'remove_done') {
            // Remove successfully completed items; keep pending, errors, and copy-fallback that need attention
            const removedIds = batchManager.items.filter(i => i.status === 'done' && i.errorMessage !== 'COPY_FALLBACK').map(i => i.id);
            batchManager.items = batchManager.items.filter(i => !(i.status === 'done' && i.errorMessage !== 'COPY_FALLBACK'));
            if (removedIds.length > 0) await deleteItems(removedIds);
        }
        // 'keep_all': statuses already persisted by executeBatch
    }

    /// PLAN P6 4d — dump the current batch (items the user has loaded
    /// for sorting, regardless of LanceDB ingest state) to a fresh .caf
    /// file. Bit-perfect Cathy-compatible; can be re-loaded later via
    /// the Catalog tab. For exporting the *full* indexed corpus instead,
    /// use the `catalog_export_sorted` backend command (no UI yet —
    /// CLI-only in P8.2).
    async function exportCaf() {
        const items = batchManager.items.filter(it => !it.isIgnored);
        if (items.length === 0) return;
        const defaultName = `crispsorter-batch-${new Date().toISOString().slice(0, 10)}.caf`;
        const savePath = await save({
            defaultPath: defaultName,
            filters: [{ name: 'Cathy Catalog', extensions: ['caf'] }],
        });
        if (!savePath) return;
        try {
            // Build the FileIndex JSON the backend expects. Stat each file
            // for current size + mtime; fall back to whatever the batch
            // stored (it had a `size` field at add time but no mtime).
            const all_files = [];
            for (const it of items) {
                const path = it.targetPath || it.originalPath;
                let size = it.size ?? 0;
                let mtime = 0;
                try {
                    const info = await stat(path);
                    size = Number((info as any).size ?? size);
                    const mt = (info as any).mtime;
                    if (mt instanceof Date) mtime = Math.floor(mt.getTime() / 1000);
                    else if (typeof mt === 'number') mtime = Math.floor(mt / 1000);
                } catch { /* file moved/missing — keep zeros */ }
                all_files.push({ path, size, mtime, hash: null });
            }
            await invoke('catalog_save_caf', {
                path: savePath,
                index: { root_path: '/', is_windows_path: false, all_files },
                createdDate: Math.floor(Date.now() / 1000),
            });
        } catch (e: any) {
            await ask(`Failed to write .caf file: ${e?.message ?? e}`, {
                title: 'Export failed',
                kind: 'error',
            });
        }
    }

    async function exportBibtex() {
        const items = batchManager.items;
        if (items.length === 0) return;

        const sources = items
            // Skip ignored items — user explicitly opted them out of the
            // sorted output, so they shouldn't end up in the citation file.
            .filter(it => !it.isIgnored)
            .map(it => ({
                title: it.suggestedTitle,
                author: it.suggestedAuthor,
                year: it.suggestedYear,
                filename: it.originalName,
                path: it.targetPath || it.originalPath,
            }));

        if (sources.length === 0) return;

        const defaultName = `crispsorter-${new Date().toISOString().slice(0, 10)}.bib`;
        const savePath = await save({
            defaultPath: defaultName,
            filters: [{ name: 'BibTeX', extensions: ['bib'] }],
        });
        if (!savePath) return;

        const content = buildBibFile(sources);
        try {
            await writeTextFile(savePath, content);
        } catch (e: any) {
            await ask(`Failed to write .bib file: ${e?.message ?? e}`, {
                title: 'Export failed',
                kind: 'error',
            });
        }
    }

    async function checkAndUpdateSources() {
        console.log('[BatchReview] checkAndUpdateSources called, total items:', batchManager.items.length);
        let missingCount = 0;
        let providerCount = 0;
        let missingIds: string[] = [];
        const changedItems: BatchItem[] = [];
        for (const item of batchManager.items) {
            if (isProviderUri(item.originalPath)) { providerCount++; continue; }
            console.log('[BatchReview] Checking:', item.originalName, '→', item.originalPath, '(status:', item.status, 'err:', item.errorMessage, ')');
            const exists = await stat(item.originalPath)
                .then(() => true)
                .catch((e: any) => { console.log('[BatchReview] stat failed for', item.originalName, ':', e?.message || String(e)); return false; });
            console.log('[BatchReview]', item.originalName, '→', exists ? 'EXISTS ✓' : 'MISSING ✗');
            if (!exists) {
                missingCount++;
                // For 'done' items (successfully moved), source being gone is expected — just update the badge
                if (item.status === 'done' && item.errorMessage !== 'COPY_FALLBACK') {
                    item.statusDetail = '✓ moved (src gone)';
                } else {
                    // Not yet processed or COPY_FALLBACK — source should still exist → flag as error
                    item.status = 'error';
                    item.errorMessage = 'SOURCE_NOT_FOUND';
                    item.statusDetail = '❌ source deleted';
                    missingIds.push(item.id);
                }
                changedItems.push(item);
            }
        }
        if (changedItems.length > 0) await upsertItemsBulk(changedItems);
        if (missingCount === 0) {
            showToast(providerCount
                ? `${i18n.t.batch.update_sources_ok} ${providerCount} provider document(s) were not checked.`
                : i18n.t.batch.update_sources_ok);
        } else {
            const msg = i18n.t.batch.update_sources_missing.replace('{count}', missingCount.toString());
            if (missingIds.length > 0 && await ask(`${msg}\n\nRemove unprocessed missing files from the batch?`, { title: 'CrispSorter' })) {
                await batchManager.removeItems(missingIds);
            } else if (missingIds.length === 0) {
                showToast(msg + ' (all were already sorted — expected)');
            }
        }
    }

    async function copyTextToClipboard() {
        if (!selectedItem?.extractedText) return;
        await navigator.clipboard.writeText(selectedItem.extractedText);
        showToast('Copied to clipboard.');
    }

    async function exportTextFile() {
        if (!selectedItem?.extractedText) return;
        const defaultPath = selectedItem.originalName.replace(/\.[^.]+$/, '.txt');
        const savePath = await save({
            defaultPath,
            filters: [{ name: 'Text Files', extensions: ['txt'] }]
        });
        if (savePath) {
            await writeTextFile(savePath, selectedItem.extractedText);
        }
    }

    function formatErrorCode(code: string | undefined): string {
        if (!code) return '';
        const de = i18n.lang === 'de';
        switch (code) {
            case 'COPY_FALLBACK': return de
                ? 'Kopiert, aber Original nicht gelöscht — Datei war in anderer App geöffnet. Original kann manuell entfernt werden.'
                : 'Copied but original not deleted — file was in use by another app. You can safely delete the original manually.';
            case 'SOURCE_NOT_FOUND': return de
                ? 'Quelldatei nicht gefunden — wurde gelöscht oder verschoben.'
                : 'Source file not found — it was deleted or moved.';
            case 'LOCKED': return de
                ? 'Datei gesperrt (in anderer App geöffnet) — App schließen und erneut versuchen.'
                : 'File is locked by another app — close it and retry.';
            default: return code;
        }
    }

    function formatSize(bytes: number) {
        if (bytes === 0) return '0 B';
        const k = 1024;
        const sizes = ['B', 'KB', 'MB', 'GB'];
        const i = Math.floor(Math.log(bytes) / Math.log(k));
        return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
    }

    /** Format seconds → "MM:SS" or "H:MM:SS" — same shape audio
     *  players use.  Negative / non-finite returns "—" so callers
     *  don't have to guard. */
    function formatDuration(seconds: number): string {
        if (!Number.isFinite(seconds) || seconds < 0) return '—';
        const total = Math.round(seconds);
        const h = Math.floor(total / 3600);
        const m = Math.floor((total % 3600) / 60);
        const s = total % 60;
        const pad = (n: number) => String(n).padStart(2, '0');
        return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
    }

    /** Compact audio-row tooltip — `codec stereo @ 192 kbps` etc.
     *  Used as the `title` attribute on the duration cell so a
     *  hover surfaces the full L2 set without a separate column
     *  per field. */
    function audioRowTooltip(item: BatchItem): string {
        const parts: string[] = [];
        if (item.audioCodec) parts.push(item.audioCodec);
        if (typeof item.audioChannels === 'number') {
            parts.push(item.audioChannels === 1 ? 'mono' : item.audioChannels === 2 ? 'stereo' : `${item.audioChannels}ch`);
        }
        if (typeof item.audioSampleRateHz === 'number') parts.push(`${(item.audioSampleRateHz / 1000).toFixed(item.audioSampleRateHz % 1000 === 0 ? 0 : 1)} kHz`);
        if (typeof item.audioBitrateKbps === 'number') parts.push(`@ ${item.audioBitrateKbps} kbps`);
        return parts.join(' ');
    }
</script>

<div class="batch-container" ondragover={e => e.preventDefault()} role="region" aria-label="File drop zone">
    <!-- Selection-toolbar moved below the main toolbar to keep it
         from overlaying the Start/Stop / Force-reset bar — see the
         `{#if selectedIds.length > 0}` block AFTER the next `</div>`. -->

    <div class="workflow-actions">
        <strong>{workflow === 'inbox' ? 'Inbox' : 'Review'}</strong>
        <label for="batch-processing-preset">Processing</label> <select id="batch-processing-preset" aria-label="Processing preset" bind:value={processingPreset} disabled={batchManager.isProcessing}>
            <option value="fast">Fast</option><option value="balanced">Balanced</option>
            <option value="quality">Best quality</option><option value="advanced">Advanced settings</option>
        </select>
        {#if selectedItem && !isProviderUri(selectedItem.originalPath)}
            {#each availableDocumentTools(selectedItem.originalName, $caps) as tool}
                <button onclick={() => documentToolRequest.set({ tool, locationUri: selectedItem.originalPath, filename: selectedItem.originalName })}>{tool === 'pdf' ? 'Edit PDF' : tool === 'ocr' ? 'OCR selected document' : 'Translate selected document'}</button>
            {/each}
        {/if}
    </div>
    <div class="toolbar">
        <div class="left-actions">
            <div class="btn-group">
                <button class="icon-btn-main primary" onclick={handleAddFiles} disabled={$caps.platform === 'ios'} title={$caps.platform === 'ios' ? 'iOS provider import is not yet available' : i18n.t.batch.add_files}>
                    <Plus size={18} />
                </button>
                <button class="icon-btn-main primary" onclick={handleAddFolder} disabled={$caps.platform === 'ios'} title={$caps.platform === 'ios' ? 'iOS provider import is not yet available' : i18n.t.batch.add_folder}>
                    <FolderOpen size={18} />
                </button>
                <button class="icon-btn-main danger" onclick={() => batchManager.clear()} title={i18n.t.batch.clear_all}>
                    <Trash2 size={18} />
                </button>
            </div>

            <button class="action-btn small" onclick={checkAndUpdateSources} title={i18n.t.batch.update_sources}
                    disabled={batchManager.items.length === 0 || batchManager.isProcessing}>
                <RefreshCw size={14} />
            </button>

            <button class="action-btn small" onclick={exportBibtex} title={i18n.t.batch.export_bibtex}
                    disabled={batchManager.items.length === 0}>
                <FileDown size={14} />
            </button>

            <button class="action-btn small" onclick={exportCaf}
                    title="Export current batch as Cathy/Catfish .caf catalog"
                    disabled={batchManager.items.length === 0}>
                <HardDrive size={14} />
            </button>

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

            {#if batchManager.isProcessing || batchManager.items.some(i => i.status === 'extracting' || i.status === 'analyzing' || i.status === 'unfinished')}
                <button class="action-btn small danger"
                        onclick={() => batchManager.forceReset()}
                        title={i18n.t.batch.force_reset ?? 'Nuclear reset: abandon in-flight workers, lift stuck items back to queued'}>
                    <RefreshCw size={14} /> {i18n.t.batch.force_reset ?? 'Force reset'}
                </button>
            {/if}
            {#if !batchManager.isProcessing && batchManager.items.some(i => i.status === 'review' || i.status === 'ready')}
                <button class="action-btn small" title="Alle bereits verarbeiteten Dateien zurücksetzen und neu analysieren"
                        onclick={async () => { await batchManager.resetToQueued(); await batchManager.processAll(); }}>
                    <RefreshCw size={14} /> Neu analysieren
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
                    <!-- Sortieren button stays clickable even when zero
                         items are accepted — `executeSorting` itself
                         logs "Sortieren clicked …", computes the per-
                         status histogram and surfaces a diagnostic
                         toast ("Keine Elemente bereit (X ohne Zielpfad,
                         Y mit ungeeignetem Status)") so the user
                         sees WHY nothing got picked up.  Pre-fix the
                         button was simply disabled with no hint at all
                         when LLM auto-accept didn't fire (e.g. every
                         response came back "Unknown Author").  Only
                         the in-flight sort lock (`isExecuting`) still
                         disables it. -->
                    <!-- Button is NEVER disabled at the HTML level — that
                         was the cause of "Sortieren does nothing, no
                         log entries" reports: a disabled button swallows
                         the click before any handler runs, so neither
                         the diagnostic log nor a toast ever fires.  All
                         gates (0 accepted, sort already in flight) are
                         handled inside `executeSorting`, which always
                         logs the click and shows an actionable toast. -->
                    <button class="action-btn rocket-btn small"
                            onclick={() => executeSorting('move')}
                            title={(() => {
                                const accepted = batchManager.items.filter(i => i.isAccepted).length;
                                if (batchManager.isExecuting) return i18n.t.batch.executing;
                                if (accepted === 0) return i18n.t.batch.no_items_ready;
                                return `${accepted} / ${batchManager.items.length}`;
                            })()}>
                        {#if batchManager.isExecuting}
                            <span class="loader-anim"><Loader2 size={16} /></span> {i18n.t.batch.executing}
                        {:else}
                            <Rocket size={16} /> {i18n.t.batch.execute}
                        {/if}
                    </button>
                    <button class="action-btn rocket-btn small split-caret"
                            onclick={() => showSortOptions = !showSortOptions}
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
                        <div class="dropdown-separator"></div>
                        <label class="dropdown-toggle-row" title="Inhaltsidentische Duplikate beim Sortieren überspringen (nicht-primäre Kopien bleiben in der Liste)">
                            <Copy size={13} /> Duplikate überspringen
                            <input type="checkbox" bind:checked={skipDuplicatesOnSort} />
                        </label>
                        <label class="dropdown-toggle-row" title="Dokumente nach dem Sortieren automatisch in den Suchindex aufnehmen">
                            <UploadCloud size={13} /> Index nach Sort
                            <input type="checkbox" bind:checked={indexAfterSort} />
                        </label>
                    </div>
                {/if}
            </div>
        </div>
    </div>
    {#if $caps.platform === 'android'}
        <p class="provider-import-note">{i18n.lang === 'de'
            ? 'Dateien aus Android-Anbietern werden nur gelesen (PDF, DOCX, EPUB, TXT, Markdown, HTML; maximal 16 MiB). Verschieben und Kopieren werden noch nicht unterstützt.'
            : 'Android provider documents are read-only (PDF, DOCX, EPUB, TXT, Markdown, HTML; up to 16 MiB). Moving and copying are not yet supported.'}</p>
    {:else if $caps.platform === 'ios'}
        <p class="provider-import-note">{i18n.lang === 'de'
            ? 'Der Import aus iOS-Dateianbietern ist noch nicht verfügbar.'
            : 'Import from iOS file providers is not yet available.'}</p>
    {/if}

    {#if selectedIds.length > 0}
        <!-- Bulk-selection toolbar — only appears when items are ticked
             via the left-edge checkbox / shift-click / cmd-click.  Plain
             row click only sets `selectedItemId` (detail-panel focus)
             and does NOT touch `selectedIds`.  Sits below the main
             toolbar so it never overlays the Start/Stop bar. -->
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
                            {#if reanalModelsLoading}
                                <span style="display:flex;align-items:center;gap:6px;font-size:0.75rem;color:#a1a1aa;">
                                    <Loader2 size={12} class="loader-spin" /> Loading…
                                </span>
                            {:else if reanalModelsFetched.length > 0}
                                <select id="reanalModel" bind:value={reanalModel} class="styled-select" style="flex:1;min-width:0;">
                                    {#each reanalModelsFetched as m}<option value={m}>{m}</option>{/each}
                                </select>
                            {:else}
                                <input id="reanalModel" type="text" bind:value={reanalModel} placeholder="model id…" />
                            {/if}
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
            <div class="toolbar-divider"></div>
            <button class="action-btn small" onclick={addSelectedToIndex}
                disabled={indexingSelected}
                title="Ausgewählte zum Suchindex hinzufügen">
                {#if indexingSelected}
                    <Loader2 size={14} class="loader-spin" /> Indexiere …
                {:else}
                    <UploadCloud size={14} /> Zum Index
                {/if}
            </button>
            <button class="close-btn-minimal" onclick={() => selectedIds = []}>×</button>
        </div>
    {/if}

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
                    <option value="queued">{i18n.t.batch.status_queued}</option>
                    <option value="unfinished">{i18n.t.batch.status_unfinished}</option>
                    <option value="extracting">{i18n.t.batch.status_extracting}</option>
                    <option value="analyzing">{i18n.t.batch.status_analyzing}</option>
                    <option value="review">{i18n.t.batch.status_review}</option>
                    <option value="done">{i18n.t.batch.status_done}</option>
                    <option value="error">{i18n.t.batch.status_error}</option>
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
                                    <button class="action-btn small" onclick={() => keepNewestInGroup(gi, false)} title={i18n.t.batch.dupe_keep_newest_remove}>
                                        Newest
                                    </button>
                                    <button class="action-btn small danger" onclick={() => keepNewestInGroup(gi, true)} disabled={group.items.some(item => isProviderUri(item.originalPath))} title={i18n.t.batch.dupe_keep_newest_delete}>
                                        Newest + 🗑
                                    </button>
                                    <button class="action-btn small danger" onclick={() => { if (confirm(i18n.t.batch.dupe_confirm_delete_all)) removeAllInGroup(gi, true); }} disabled={group.items.some(item => isProviderUri(item.originalPath))} title={i18n.t.batch.dupe_delete_all}>
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
                                        <button class="action-btn small" onclick={() => keepOnlyInGroup(gi, item.id, false)} title={i18n.t.batch.dupe_keep_only}>Keep</button>
                                        <button class="action-btn small danger" onclick={() => keepOnlyInGroup(gi, item.id, true)} disabled={group.items.some(other => other.id !== item.id && isProviderUri(other.originalPath))} title={i18n.t.batch.dupe_keep_only_delete}>Keep+🗑</button>
                                        <button class="close-btn-minimal" onclick={() => batchManager.removeItems([item.id]).then(() => handleFindDuplicates())} title={i18n.t.settings.action_remove_from_list}>×</button>
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
        <div class="table-container" bind:this={tableContainerEl}>
            <table class="dense-table" style="width: {tableWidth}px;">
                <thead>
                    <tr>
                        <th style="width: 35px; min-width: 35px; text-align: center;">
                            <input type="checkbox" 
                                   checked={selectedIds.length === sortedItems.length && sortedItems.length > 0} 
                                   onchange={selectAllVisible} aria-label="Select all visible" />
                        </th>
                        {#each columns as col, i}
                            {#if col.visible}
                                <th style="width: {col.width}px;">
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
                            class:is-dupe-nonprimary={item.isDuplicatePrimary === false}
                            class:is-chapter-member={!!item.chapterGroupId && !item.isChapterRepresentative}
                            class:is-chapter-rep={!!item.chapterGroupId && item.isChapterRepresentative}
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
                                            {@const pips = progressPips(item)}
                                            <span class="progress-pips" title={`Metadaten: ${pips.m} · Text: ${pips.t} · LLM: ${pips.l}`}>
                                                <span class="pip pip-{pips.m}" aria-label={`Metadata ${pips.m}`}></span>
                                                <span class="pip pip-{pips.t}" aria-label={`Text ${pips.t}`}></span>
                                                <span class="pip pip-{pips.l}" aria-label={`LLM ${pips.l}`}></span>
                                            </span>
                                            <span class="status-badge"
                                                class:status-active={['extracting', 'analyzing', 'moving'].includes(item.status)}
                                                class:status-unfinished={item.status === 'unfinished'}
                                                class:status-error={item.status === 'error'}>
                                                {statusLabel(item.status, item.extension)}
                                            </span>
                                            {#if item.statusDetail}
                                                {#if /^\d+\/\d+/.test(item.statusDetail)}
                                                    <span class="status-detail-badge">{item.statusDetail}</span>
                                                {:else}
                                                    <button
                                                        class="status-icon-btn"
                                                        title={item.statusDetail}
                                                        onclick={(e) => { e.stopPropagation(); checkFileDetails(item); }}
                                                    >
                                                        {#if item.statusDetail === 'moved' || item.statusDetail === 'copied'}
                                                            <Check size={10} style="color:#10b981" />
                                                        {:else if item.statusDetail === 'copied (orig. kept)' || item.statusDetail.includes('poor')}
                                                            <AlertCircle size={10} style="color:#f59e0b" />
                                                        {:else if item.statusDetail.startsWith('❌') || item.statusDetail.includes('deleted')}
                                                            <AlertCircle size={10} style="color:#ef4444" />
                                                        {:else}
                                                            <Info size={10} style="color:#60a5fa" />
                                                        {/if}
                                                    </button>
                                                {/if}
                                            {/if}
                                        {:else if col.id === 'accepted'}
                                            <button class="ghost-toggle-btn" onclick={(e) => { e.stopPropagation(); item.isAccepted = !item.isAccepted; upsertItem(item); }} title={i18n.t.batch.dupe_toggle_accept}>
                                                {#if item.isAccepted}
                                                    <Check size={16} style="color: #10b981;" />
                                                {:else}
                                                    <X size={16} style="color: #ef4444; opacity: 0.5;" />
                                                {/if}
                                            </button>
                                        {:else if col.id === 'file_name'}
                                            <span class="file-name" title={item.originalPath}>{item.originalName}</span>
                                            {#if item.isDuplicatePrimary === false}
                                                <span class="inline-badge dupe-badge"
                                                      title="Inhaltsidentisches Duplikat (Gruppe {item.duplicateGroupId}). Wird beim Sortieren übersprungen wenn 'Duplikate überspringen' aktiv ist."
                                                      onclick={(e) => { e.stopPropagation(); showDuplicates = true; }}>= dup</span>
                                            {:else if item.isDuplicatePrimary === true && item.duplicateGroupId}
                                                {@const nDupes = batchManager.items.filter(i => i.duplicateGroupId === item.duplicateGroupId && i.isDuplicatePrimary === false).length}
                                                <span class="inline-badge dupe-primary-badge"
                                                      title="{nDupes} identische Kopie(n) in der Liste (Gruppe {item.duplicateGroupId})"
                                                      onclick={(e) => { e.stopPropagation(); showDuplicates = true; }}>+{nDupes} dup</span>
                                            {/if}
                                            {#if item.chapterGroupId}
                                                {@const n = item.chapterGroupSize ?? 0}
                                                {@const isEV = !!item.chapterIsEditedVolume}
                                                <span class="inline-badge chapter-badge"
                                                      class:chapter-edited-vol={isEV}
                                                      title="{isEV ? 'Sammelband (Autor pro Kapitel)' : 'Monographie (Autor vererbt)'} — {item.chapterGroupId} ({n} Kap.){item.isChapterRepresentative ? ' · Repräsentant' : ` · ${item.chapterSuffix ?? ''}`}. Klick: Sammelband-Modus umschalten"
                                                      onclick={(e) => { e.stopPropagation(); batchManager.toggleChapterEditedVolume(item.chapterGroupId!); }}>
                                                    {isEV ? '📖' : '📚'}{item.isChapterRepresentative ? ` ${n}` : ` ${item.chapterSuffix ?? ''}`}
                                                </span>
                                            {/if}
                                            {#if item.historyMatch}
                                                <span class="inline-badge history-badge"
                                                      title="Already sorted on {new Date(item.historyMatch.processedAt).toLocaleDateString()} — click to re-process from scratch"
                                                      onclick={(e) => { e.stopPropagation(); item.historyMatch = undefined; item.skipHistoryCheck = true; item.status = 'queued'; item.suggestedTitle = undefined; item.suggestedAuthor = undefined; item.suggestedYear = undefined; item.targetPath = undefined; upsertItem(item); }}>
                                                    ✓ sorted
                                                </span>
                                            {/if}
                                        {:else if col.id === 'title'}
                                            <input type="text" bind:value={item.suggestedTitle} onclick={(e) => { e.stopPropagation(); selectedItemId = item.id; }} onchange={() => batchManager.recalculateTargetPath(item.id)} class:fallback={isUnknown(item.suggestedTitle)} aria-label="Suggested Title" />
                                        {:else if col.id === 'author'}
                                            <input type="text" bind:value={item.suggestedAuthor} onclick={(e) => { e.stopPropagation(); selectedItemId = item.id; }} onchange={() => batchManager.recalculateTargetPath(item.id)} class:fallback={isUnknown(item.suggestedAuthor)} aria-label="Suggested Author" />
                                        {:else if col.id === 'year'}
                                            <input type="text" bind:value={item.suggestedYear} onclick={(e) => { e.stopPropagation(); selectedItemId = item.id; }} onchange={() => batchManager.recalculateTargetPath(item.id)} style="text-align: center;" class:fallback={isUnknown(item.suggestedYear)} aria-label="Suggested Year" />
                                        {:else if col.id === 'size'}
                                            <span class="mono">{formatSize(item.size)}</span>
                                        {:else if col.id === 'date'}
                                            <span class="mono">{new Date(item.modifiedAt).toLocaleDateString()}</span>
                                        {:else if col.id === 'extension'}
                                            <span class="ext-badge">{item.extension}</span>
                                        {:else if col.id === 'language'}
                                            {#if item.detectedLanguage}
                                                <span class="ext-badge" title="Detected source language ({item.detectedLanguage})">{item.detectedLanguage.toUpperCase()}</span>
                                            {:else}
                                                <span class="mono" style="color: #71717a">–</span>
                                            {/if}
                                        {:else if col.id === 'duration'}
                                            {#if typeof item.audioDurationSeconds === 'number'}
                                                <span class="mono" title={audioRowTooltip(item)}>{formatDuration(item.audioDurationSeconds)}</span>
                                            {:else}
                                                <span class="mono" style="color: #71717a">–</span>
                                            {/if}
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
                <!-- Non-blocking here: reviewing suggestions is fine, it is
                     applying them (execute_batch) that Rust refuses. The notice
                     appears where the user will act, with the accept path. -->
                <IntendedPurposeGate blocking={false} />
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
                        <div class="error-box">{formatErrorCode(selectedItem.errorMessage)}</div>
                    {/if}
                    <div class="detail-section">
                        <h4>{i18n.t.batch.target_path}</h4>
                        <div class="path-preview full-path">{selectedItem.targetPath || i18n.t.batch.path_hint}</div>
                    </div>
                    
                    <div class="detail-section">
                        <div class="detail-section-header">
                            <h4>{i18n.t.batch.edit_metadata}</h4>
                            <!-- Title / author / year are inferred by a local
                                 model, and the target path is derived from them.
                                 Section-level rather than per-field: the whole
                                 group is machine-derived, and the user edits it
                                 before anything moves. See docs/ai-act.md. -->
                            <AiGeneratedBadge compact />
                            {#if detailDirty}
                                <div class="detail-actions">
                                    <button class="action-btn small danger" onclick={() => {
                                        detailTitle  = selectedItem!.suggestedTitle  ?? '';
                                        detailAuthor = selectedItem!.suggestedAuthor ?? '';
                                        detailYear   = selectedItem!.suggestedYear   ?? '';
                                    }}>
                                        <X size={12} /> Revert
                                    </button>
                                    <button class="action-btn small success" onclick={saveDetailChanges}>
                                        <Check size={12} /> Save
                                    </button>
                                </div>
                            {/if}
                        </div>
                        <div class="edit-fields" role="group" aria-label="Edit metadata">
                            <div class="field-row">
                                <span class="field-label">{i18n.t.batch.title}</span>
                                <input type="text" bind:value={detailTitle} onkeydown={onMetaKey} aria-label="Edit Title" />
                            </div>
                            <div class="field-row">
                                <span class="field-label">{i18n.t.batch.author}</span>
                                <input type="text" bind:value={detailAuthor} onkeydown={onMetaKey} aria-label="Edit Author" />
                            </div>
                            <div class="field-row">
                                <span class="field-label">{i18n.t.batch.year}</span>
                                <input type="text" bind:value={detailYear} onkeydown={onMetaKey} aria-label="Edit Year" />
                            </div>
                        </div>
                    </div>

                    <div class="detail-section">
                        <div class="detail-section-header">
                            <h4>{i18n.t.batch.extracted_text}</h4>
                            {#if selectedItem.extractedText}
                                <div class="detail-actions">
                                    <button class="action-btn small" onclick={copyTextToClipboard} title={i18n.t.batch.copy_text}>
                                        <Copy size={12} /> {i18n.t.batch.copy_text}
                                    </button>
                                    <button class="action-btn small" onclick={exportTextFile} title={i18n.t.batch.export_text}>
                                        <FileText size={12} /> {i18n.t.batch.export_text}
                                    </button>
                                </div>
                            {/if}
                        </div>
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
            {#if batchManager.isProcessing}
                {@const processed = batchManager.items.filter(i => i.status === 'review' || i.status === 'done').length}
                {@const extracting = batchManager.items.filter(i => i.status === 'extracting').length}
                {@const analyzing = batchManager.items.filter(i => i.status === 'analyzing').length}
                <span class="stat-divider">·</span>
                <span class="stat-item stat-processing">
                    <Loader2 size={13} class="loader-spin" />
                    {i18n.t.batch.processing_stats.replace('{done}', processed.toString()).replace('{total}', batchManager.items.length.toString())}
                    {#if extracting > 0}<span class="stat-sub">· {i18n.t.batch.status_extracting} {extracting}</span>{/if}
                    {#if analyzing > 0}<span class="stat-sub">· {i18n.t.batch.status_analyzing} {analyzing}</span>{/if}
                </span>
            {:else}
                {@const done = batchManager.items.filter(i => i.status === 'review' || i.status === 'done').length}
                {@const unfinished = batchManager.items.filter(i => i.status === 'unfinished').length}
                {@const errors = batchManager.items.filter(i => i.status === 'error').length}
                {#if done > 0}
                    <span class="stat-divider">·</span>
                    <span class="stat-item stat-ready">{done} {i18n.t.batch.status_review}</span>
                {/if}
                {#if unfinished > 0}
                    <span class="stat-divider">·</span>
                    <span class="stat-item stat-unfinished">{unfinished} {i18n.t.batch.status_unfinished}</span>
                {/if}
                {#if errors > 0}
                    <span class="stat-divider">·</span>
                    <span class="stat-item stat-err">{errors} {i18n.t.batch.status_error}</span>
                {/if}
            {/if}
        </div>
    </div>
</div>

{#if toastMessage}
    <div class="toast-message" role="alert">{toastMessage}</div>
{/if}

{#if sortPlan}
    <div class="modal-overlay" role="presentation">
        <div class="modal-content sort-preview" role="dialog" aria-modal="true" aria-labelledby="sort-preview-title" tabindex="-1">
            <div class="modal-header">
                <h2 id="sort-preview-title">{planLabels.title}</h2>
            </div>
            <div class="modal-body sort-preview-body">
                <p>{sortPlan.items.length} {planLabels.files} · {sortPlan.mode === 'move' ? planLabels.move : sortPlan.mode === 'copy' ? planLabels.copy : sortPlan.mode === 'script_move' ? planLabels.scriptMove : planLabels.scriptCopy}</p>
                {#if sortPlan.candidates.length > sortPlan.items.length}
                    <p>{sortPlan.candidates.length - sortPlan.items.length} {planLabels.excluded}</p>
                {/if}
                {#if sortPlan.saveTxt && !sortPlan.mode.startsWith('script')}
                    <p>{planLabels.sidecars}</p>
                {/if}
                <ol class="sort-plan-list">
                    {#each sortPlan.items as item (item.id)}
                        <li>
                            <div><strong>{planLabels.from}</strong> <span>{item.originalPath}</span></div>
                            <div><strong>{planLabels.to}</strong> <span>{item.targetPath}</span></div>
                            {#if sortPlan.saveTxt && !sortPlan.mode.startsWith('script') && textSidecarPath(item)}
                                <div><strong>{planLabels.text}</strong> <span>{textSidecarPath(item)}</span></div>
                            {/if}
                        </li>
                    {/each}
                </ol>
                <div class="sort-preview-actions">
                    <button class="action-btn" disabled={confirmingSort} onclick={() => sortPlan = null}>{planLabels.cancel}</button>
                    <button class="action-btn primary" disabled={confirmingSort} onclick={confirmSorting}>
                        {confirmingSort ? planLabels.preparing : planLabels.confirm}
                    </button>
                </div>
            </div>
        </div>
    </div>
{/if}

{#if showReportModal && lastExecutionStats}
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="modal-overlay" onclick={() => showReportModal = false} role="button" tabindex="0" onkeydown={e => e.key === 'Escape' && (showReportModal = false)}>
        <div class="modal-content report-modal" onclick={e => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
            <div class="modal-header">
                <h2><Rocket size={18} /> {i18n.t.batch.report_title}</h2>
            </div>
            <div class="modal-body">
                <div class="report-stats">
                    {#if lastExecutionStats.success > 0}
                        {#if lastExecutionStats.mode.includes('copy')}
                            <div class="stat-line success"><Check size={16} /> {i18n.t.batch.report_copied.replace('{count}', lastExecutionStats.success)}</div>
                        {:else}
                            <div class="stat-line success"><Check size={16} /> {i18n.t.batch.report_moved.replace('{count}', lastExecutionStats.success)}</div>
                        {/if}
                    {/if}

                    {#if (lastExecutionStats.copiedFallback ?? 0) > 0}
                        <div class="stat-line warning"><AlertCircle size={16} /> {i18n.t.batch.report_copy_fallback.replace('{count}', lastExecutionStats.copiedFallback)}</div>
                    {/if}

                    {#if (lastExecutionStats.locked ?? 0) > 0}
                        <div class="stat-line danger"><X size={16} /> {i18n.t.batch.report_locked.replace('{count}', lastExecutionStats.locked)}</div>
                    {/if}

                    {#if (lastExecutionStats.notFound ?? 0) > 0}
                        <div class="stat-line warning"><AlertCircle size={16} /> {i18n.t.batch.report_not_found.replace('{count}', lastExecutionStats.notFound)}</div>
                    {/if}

                    {#if (lastExecutionStats.notWritable ?? 0) > 0}
                        <div class="stat-line danger"><X size={16} /> {i18n.t.batch.report_not_writable.replace('{count}', lastExecutionStats.notWritable)}</div>
                    {/if}

                    {#if (lastExecutionStats.notPermitted ?? 0) > 0}
                        <div class="stat-line danger"><X size={16} /> {i18n.t.batch.report_not_permitted.replace('{count}', lastExecutionStats.notPermitted)}</div>
                    {/if}
                </div>

                {#if lastExecutionStats}
                    {@const problemCount = (lastExecutionStats.notFound ?? 0)
                                           + (lastExecutionStats.notWritable ?? 0)
                                           + (lastExecutionStats.notPermitted ?? 0)
                                           + (lastExecutionStats.locked ?? 0)
                                           + (lastExecutionStats.copiedFallback ?? 0)}
                    {@const allSucceeded = problemCount === 0 && lastExecutionStats.success > 0}
                    <div class="report-choices">
                        {#if lastExecutionStats.success > 0}
                            <button class="choice-btn success" onclick={() => handleReportChoice('remove_done')}>
                                {i18n.t.batch.report_choice_remove_done}
                            </button>
                        {/if}
                        {#if problemCount > 0}
                            <button class="choice-btn" onclick={() => handleReportChoice('keep_problematic')}>
                                {i18n.t.batch.report_choice_keep_problematic}
                            </button>
                        {/if}
                        <button class="choice-btn danger" onclick={() => handleReportChoice('empty')}>
                            {i18n.t.batch.report_choice_empty}
                        </button>
                        <!-- Hide "keep entire batch" when every file
                             succeeded: there's nothing left to act on. -->
                        {#if !allSucceeded}
                            <button class="choice-btn secondary" onclick={() => handleReportChoice('keep_all')}>
                                {i18n.t.batch.report_choice_keep_all}
                            </button>
                        {/if}
                    </div>
                {/if}
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
                    <span class="info-val">{infoModalData.exists === null ? '—' : infoModalData.exists ? '✅' : '❌'}</span>
                    
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
                        {#if infoModalData.statusDetail}
                            <span style="font-size:0.75rem; color:#a1a1aa; margin-left:6px;">{infoModalData.statusDetail}</span>
                        {/if}
                        {#if infoModalData.error}
                            <div class="error-msg" class:warn-msg={infoModalData.error === 'COPY_FALLBACK'}>
                                {formatErrorCode(infoModalData.error)}
                            </div>
                        {/if}
                    </span>
                </div>
                <button class="action-btn primary" style="margin-top:20px; width:100%; justify-content:center;" onclick={() => showInfoModal = false}>OK</button>
            </div>
        </div>
    </div>
{/if}

<style>
    .provider-import-note { margin: 0.5rem 0; color: #a1a1aa; font-size: 0.8rem; }
    .workflow-actions { display: flex; align-items: center; gap: 10px; padding: 8px 12px; flex-wrap: wrap; border-bottom: 1px solid #3f3f46; }
    .workflow-actions select, .workflow-actions button { background: #27272a; color: #eee; border: 1px solid #52525b; border-radius: 5px; padding: 5px; }

    .batch-container { display: flex; flex-direction: column; height: 100%; background: #09090b; overflow: hidden; }
    /* Responsive top toolbar.  Reported: "if width is not very large,
       Sortieren button has not enough room."  Pre-fix, the toolbar was
       `display: flex` without `flex-wrap`, so on narrow widths the
       button row overflowed the viewport and the rightmost actions
       (Sortieren, dropdowns) got clipped.  Now both the outer toolbar
       and the inner action groups wrap onto a second row when needed,
       and the Sortieren/execute button refuses to shrink so its
       label+chevron stay readable. */
    .toolbar {
        padding: 8px 16px;
        background: #18181b;
        border-bottom: 1px solid #27272a;
        display: flex;
        justify-content: space-between;
        align-items: center;
        gap: 12px;
        flex-wrap: wrap;
        row-gap: 8px;
    }
    .left-actions, .right-actions {
        display: flex;
        align-items: center;
        gap: 8px;
        flex-wrap: wrap;
        row-gap: 8px;
    }
    /* Sortieren split-button (Rocket) is the user's primary action on
       this screen — it must never shrink below its natural width.  The
       parent `.right-actions` lets less-critical buttons (Find dupes,
       Stop, Force-reset, Neu analysieren, Accept/Uncheck-all) wrap to
       a second row first. */
    .action-btn.rocket-btn { flex-shrink: 0; }
    .dropdown-container { flex-shrink: 0; }
    
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

    .dropdown-separator { height: 1px; background: #27272a; margin: 4px 0; }
    .dropdown-toggle-row { display: flex; align-items: center; gap: 8px; padding: 8px 12px; color: #d4d4d8; font-size: 0.8125rem; cursor: pointer; width: 100%; }
    .dropdown-toggle-row:hover { background: #27272a; color: white; }
    .dropdown-toggle-row input[type="checkbox"] { margin-left: auto; }

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

    .dense-table { min-width: 100%; border-collapse: collapse; font-size: 0.8125rem; table-layout: fixed; }
    .dense-table th { position: sticky; top: 0; z-index: 10; background: #18181b; padding: 0; text-align: left; border-bottom: 2px solid #27272a; color: #71717a; font-weight: 600; text-transform: uppercase; font-size: 0.7rem; height: 32px; overflow: hidden; }
    .th-content { display: flex; align-items: center; gap: 6px; padding: 0 12px; height: 100%; cursor: pointer; }
    .th-content:hover { background: #27272a; color: white; }
    .th-label { flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    
    .resizer { position: absolute; right: 0; top: 0; width: 4px; height: 100%; cursor: col-resize; transition: background 0.2s; z-index: 20; }
    .resizer:hover { background: #3b82f6; }

    .dense-table td { padding: 4px 12px; border-bottom: 1px solid #1e1e1e; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: #e2e8f0; height: 32px; vertical-align: middle; max-width: 0; }
    .dense-table tr:hover { background: #1e293b; }
    .dense-table tr.selected { background: #1e3a8a; }
    .dense-table tr.active-row { border-left: 3px solid #3b82f6; }
    .dense-table tr.status-done { background: #064e3b33; }
    /* P15a — duplicate rows */
    .dense-table tr.is-dupe-nonprimary { background: #451a0322; border-left: 3px solid #f59e0b88; }
    /* P15b — chapter rows */
    .dense-table tr.is-chapter-member { background: #0c1a3022; border-left: 3px solid #3b82f644; }
    .dense-table tr.is-chapter-rep    { background: #0c1a3044; border-left: 3px solid #3b82f6; }
    /* P15 — inline badges in file-name cell */
    .inline-badge {
        display: inline-block; font-size: 0.6rem; font-weight: 800;
        padding: 1px 4px; border-radius: 3px; margin-left: 4px;
        vertical-align: middle; cursor: pointer; flex-shrink: 0;
    }
    .dupe-badge        { background: #451a0388; color: #fbbf24; }
    .dupe-primary-badge { background: #78350f55; color: #fde68a; }
    .chapter-badge              { background: #1e3a5f55; color: #93c5fd; }
    .chapter-badge.chapter-edited-vol { background: #2e1a5255; color: #c084fc; }
    .history-badge { background: #064e3b88; color: #34d399; }
    
    .dense-table input[type="text"] { width: 100%; border: 1px solid transparent; background: transparent; padding: 2px 6px; border-radius: 4px; font-size: 0.8125rem; color: #f8fafc; }
    .dense-table tr:hover input[type="text"] { background: #0f172a; border-color: #334155; }
    .dense-table input.fallback { color: #fbbf24; font-style: italic; }
    
    .status-badge { padding: 2px 6px; border-radius: 4px; background: #27272a; font-size: 0.7rem; font-weight: 600; color: #a1a1aa; text-transform: capitalize; }

    /* Per-row progress indicator: 3 pips for Metadata / Text / LLM.
       Sized to fit alongside the status badge inside the existing
       status column without widening it. Hover shows the per-stage
       state in the tooltip. */
    .progress-pips {
        display: inline-flex; gap: 2px; align-items: center;
        margin-right: 4px; vertical-align: middle;
    }
    .pip {
        width: 7px; height: 7px; border-radius: 50%;
        background: #3f3f46;             /* gray = pending */
        flex-shrink: 0;
    }
    .pip-ok      { background: #22c55e; }
    .pip-partial { background: #eab308; }
    .pip-fail    { background: #ef4444; }
    .pip-na      { background: #3f3f46; opacity: 0.18; }
    .pip-pulse   { background: #71717a; animation: pip-pulse 1.4s ease-in-out infinite; }
    @keyframes pip-pulse {
        0%, 100% { opacity: 1; }
        50%      { opacity: 0.3; }
    }
    .status-active { color: #3b82f6; background: #1e3a8a33; }
    .status-unfinished { color: #f59e0b; background: #78350f33; }
    .status-error { color: #ef4444; background: #450a0a33; }
    .stat-unfinished { color: #f59e0b; }
    .status-detail-badge { font-size: 0.6rem; color: #60a5fa; background: #1e3a8a33; padding: 1px 4px; border-radius: 3px; margin-left: 4px; white-space: nowrap; }
    .status-icon-btn { background: transparent; border: none; cursor: pointer; display: inline-flex; align-items: center; padding: 2px 3px; border-radius: 3px; margin-left: 3px; vertical-align: middle; }
    .status-icon-btn:hover { background: #27272a; }

    .mono { font-family: monospace; font-size: 0.75rem; color: #a1a1aa; }
    .ext-badge { font-size: 0.65rem; background: #27272a; padding: 1px 4px; border-radius: 3px; color: #71717a; text-transform: uppercase; font-weight: 700; }
    .path-text { font-family: monospace; font-size: 0.7rem; color: #71717a; }

    .detail-pane { width: 400px; background: #0f172a; border-left: 1px solid #1e293b; display: flex; flex-direction: column; flex-shrink: 0; }
    .detail-header { padding: 10px 16px; border-bottom: 1px solid #1e293b; display: flex; justify-content: space-between; align-items: center; }
    .detail-content { padding: 12px; overflow-y: auto; display: flex; flex-direction: column; gap: 12px; }
    
    .detail-section h4 { font-size: 0.65rem; text-transform: uppercase; color: #71717a; margin: 0; letter-spacing: 0.05em; }
    .detail-section-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 6px; }
    .detail-actions { display: flex; gap: 6px; }
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
    
    .sort-preview { width: min(900px, 95vw); }
    .sort-preview-body { display: flex; flex-direction: column; gap: 12px; }
    .sort-preview-body p { margin: 0; color: #cbd5e1; }
    .sort-plan-list { margin: 0; padding: 0 0 0 2em; max-height: 55vh; overflow: auto; }
    .sort-plan-list li { padding: 8px; border-bottom: 1px solid #27272a; }
    .sort-plan-list div { display: flex; gap: 8px; }
    .sort-plan-list strong { flex: 0 0 3em; }
    .sort-plan-list span { overflow-wrap: anywhere; font-family: monospace; user-select: text; }
    .sort-preview-actions { display: flex; justify-content: flex-end; gap: 8px; }

    .report-stats { display: flex; flex-direction: column; gap: 12px; margin-bottom: 24px; }
    .stat-line { display: flex; align-items: center; gap: 10px; font-weight: 600; font-size: 0.9rem; }
    .stat-line.success { color: #10b981; }
    .stat-line.warning { color: #f59e0b; }
    .stat-line.danger { color: #ef4444; }

    .report-choices { display: flex; flex-direction: column; gap: 10px; }
    .choice-btn { width: 100%; padding: 10px; border-radius: 8px; border: 1px solid #3b82f6; background: #1e3a8a33; color: #60a5fa; font-weight: 600; cursor: pointer; text-align: left; transition: all 0.2s; }
    .choice-btn:hover { background: #1e3a8a66; }
    .choice-btn.success { border-color: #10b981; background: #064e3b33; color: #34d399; }
    .choice-btn.success:hover { background: #064e3b66; }
    .choice-btn.danger { border-color: #ef4444; background: #450a0a33; color: #f87171; }
    .choice-btn.danger:hover { background: #450a0a66; }
    .choice-btn.secondary { border-color: #27272a; background: #27272a; color: #a1a1aa; }
    .choice-btn.secondary:hover { background: #3f3f46; color: white; }

    .status-footer { padding: 6px 16px; background: #18181b; border-top: 1px solid #27272a; display: flex; align-items: center; font-size: 0.75rem; color: #71717a; }
    .stats { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
    .stat-item { display: flex; align-items: center; gap: 6px; }
    .stat-divider { color: #3f3f46; }
    .stat-processing { color: #60a5fa; font-weight: 600; }
    .stat-sub { color: #4b5563; font-weight: 400; }
    .stat-ready { color: #34d399; font-weight: 600; }
    .stat-err { color: #f87171; font-weight: 600; }

    .info-modal { width: 500px; }
    .info-grid { display: grid; grid-template-columns: 140px 1fr; gap: 16px; }
    .info-label { font-size: 0.75rem; color: #71717a; text-transform: uppercase; font-weight: 600; padding-top: 2px; }
    .info-val { font-size: 0.875rem; color: #f4f4f5; line-height: 1.4; }
    .mono-path { font-family: monospace; font-size: 0.75rem; color: #94a3b8; word-break: break-all; background: #09090b; padding: 4px 8px; border-radius: 4px; border: 1px solid #27272a; }
    .error-box { padding: 8px 10px; background: #450a0a33; border: 1px solid #ef444433; border-radius: 6px; color: #f87171; font-size: 0.8125rem; margin-bottom: 4px; }
    .error-msg { margin-top: 8px; padding: 8px; background: #450a0a33; border: 1px solid #ef444433; border-radius: 6px; color: #f87171; font-size: 0.8125rem; }
    .warn-msg { background: #78350f22 !important; border-color: #f59e0b33 !important; color: #fbbf24 !important; }

    .full-path { user-select: all; cursor: text; }

    .toast-message {
        position: fixed;
        bottom: 24px;
        left: 50%;
        transform: translateX(-50%);
        background: #3b82f6;
        color: white;
        padding: 10px 20px;
        border-radius: 8px;
        font-size: 0.875rem;
        font-weight: 600;
        z-index: 2000;
        box-shadow: 0 8px 24px rgba(0,0,0,0.4);
        animation: fadeInUp 0.2s ease-out;
    }
    @keyframes fadeInUp { from { opacity: 0; transform: translateX(-50%) translateY(8px); } to { opacity: 1; transform: translateX(-50%) translateY(0); } }

    /* ── Responsive: phone (<768px) ────────────────────────────────── */
    @media (max-width: 767px) {
        .toolbar { padding: 6px 8px; gap: 6px; }
        .search-box { min-width: 0; flex-basis: 100%; order: -1; }
        .main-split { flex-direction: column; }
        .detail-pane { width: 100%; max-height: 50vh; border-left: none; border-top: 1px solid #1e293b; }
        .dense-table th, .dense-table td { padding: 4px 6px; font-size: 0.75rem; }
        /* Hide less-important columns on small screens */
        .dense-table th:nth-child(n+5), .dense-table td:nth-child(n+5) { display: none; }
        .filter-bar { flex-wrap: wrap; gap: 8px; padding: 6px 8px; }
        .modal-content, .info-modal { width: 95vw; }
        .status-footer { padding: 4px 8px; font-size: 0.65rem; }
        .action-btn { padding: 6px 8px; font-size: 0.75rem; }
    }

    /* ── Responsive: tablet (768-1024px) ───────────────────────────── */
    @media (min-width: 768px) and (max-width: 1024px) {
        .detail-pane { width: 280px; }
        .search-box { min-width: 160px; }
    }
</style>
