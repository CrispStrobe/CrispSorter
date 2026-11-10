<script lang="ts">
    import { loadExperimentalCrispEmbed } from '$lib/experimentalCrispEmbed';
    import { documentToolRequest, supportsOcrWorkbench } from '$lib/workflow';
    import { uriToPath } from '$lib/components/viewer/types';
    import WorkflowSummary from '$lib/components/WorkflowSummary.svelte';
    import { onMount, tick } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';
    import { stat } from '@tauri-apps/plugin-fs';
    import Settings from '$lib/components/Settings.svelte';
    import BatchReview from '$lib/components/BatchReview.svelte';
    import History from '$lib/components/History.svelte';
    import Chat from '$lib/components/Chat.svelte';
    import { batchManager } from '$lib/batch/store.svelte';
    import { i18n, type Language } from '$lib/i18n.svelte';
    import { getSetting } from '$lib/store';
    import { flog } from '$lib/log';
    import { Settings as SettingsIcon, Database, Library, ListChecks, MessageSquare, ChevronLeft, ChevronRight, UploadCloud, Terminal, Languages, ScanText, FileText, Search } from 'lucide-svelte';
    import { CORE_TABS, AITOOLKIT_TABS, MOBILE_TABS, visibleTabs } from '$lib/tabs';
    import { aitoolkitCaps } from '$lib/aitoolkit';
    import { caps, buildFlags, loadCapabilities } from '$lib/capabilities';
    import AIToolkitView from '$lib/components/AIToolkitView.svelte';
    import AIToolkitCapability from '$lib/components/AIToolkitCapability.svelte';
    import ThirdPartyAiConsent from '$lib/components/ThirdPartyAiConsent.svelte';
    import IndexIngest from '$lib/components/IndexIngest.svelte';
    import LogPanel from '$lib/components/LogPanel.svelte';
    import Translate from '$lib/components/Translate.svelte';
    import OcrWorkbench from '$lib/components/OcrWorkbench.svelte';
    import PdfWorkspace from '$lib/components/PdfWorkspace.svelte';
    import TransferDrawer from '$lib/components/TransferDrawer.svelte';
    import DriveBrowser from '$lib/components/DriveBrowser.svelte';
    import { subscribeBrowserContext } from '$lib/drives/browserContext';
    import { getBackendStatus, retryBackend, type BackendStatus } from '$lib/secretBackend';
    import QuickNavigation from '$lib/components/QuickNavigation.svelte';
    import { isNavigationShortcut, navigationCopy, navigationDestinations } from '$lib/navigation';

    $effect(() => { if ($documentToolRequest && ($documentToolRequest.tool !== 'ocr' || supportsOcrWorkbench($caps))) activeTab = $documentToolRequest.tool; });
    let activeTab = $state('batch'); // 'batch', 'drives', 'history', 'chat', 'settings', 'catalog', 'translate'
    let ocrInitialized = $state(false);

    // Preserve the queue, review history, and autosave revision across tab changes.
    // Opening another tab must not start a second workspace for the same project.
    $effect(() => {
        if (activeTab === 'ocr') ocrInitialized = true;
    });

    /**
     * Set when the credential store has refused to hand over a secret, or
     * could not be opened at all.
     *
     * This is the one case where the choice of store has to be put in front
     * of the user rather than waiting in Settings: on macOS a refusal means
     * the login keychain is asking for a password the user may not have,
     * and every provider key is unreachable until they pick somewhere else.
     * Deliberately NOT shown merely because the user has never chosen —
     * that would nag everyone whose keychain works fine.
     */
    let secretStoreAlert: BackendStatus | null = $state(null);
    let secretStoreRetrying = $state(false);
    /** Reopen the chosen store, e.g. after its volume was remounted. */
    async function retrySecretStore() {
        secretStoreRetrying = true;
        try {
            const st = await retryBackend();
            secretStoreAlert = st.denied || st.error ? st : null;
        } catch (e) {
            flog('warn', `secret store retry failed: ${e}`);
        } finally {
            secretStoreRetrying = false;
        }
    }
    let secretStoreAlertDismissed = $state(false);
    let navCollapsed = $state(false);
    let showMobileTools = $state(false);
    let showLogs = $state(false);
    let showQuickNavigation = $state(false);
    let mobileToolsTrigger = $state<HTMLButtonElement>();
    let mobileToolsMenu = $state<HTMLDivElement>();
    const navCopy = $derived(navigationCopy[i18n.lang]);

    function navigate(tab: string) {
        showMobileTools = false;
        if (tab === 'logs') showLogs = true;
        else activeTab = tab;
    }

    function openQuickNavigation() {
        // A mobile menu action is about to disappear; restore its persistent
        // trigger first so the dialog has somewhere useful to return focus.
        if (showMobileTools) mobileToolsTrigger?.focus();
        showMobileTools = false;
        showQuickNavigation = true;
    }

    async function toggleMobileTools() {
        showMobileTools = !showMobileTools;
        if (showMobileTools) {
            await tick();
            mobileToolsMenu?.querySelector<HTMLButtonElement>('button')?.focus();
        }
    }

    function closeMobileTools(restoreFocus = false) {
        showMobileTools = false;
        if (restoreFocus) mobileToolsTrigger?.focus();
    }

    function onWindowKey(event: KeyboardEvent) {
        if (isNavigationShortcut(event)) {
            // Respect another modal's keyboard scope and content editors' link
            // shortcut. Ordinary text inputs can still open quick navigation.
            if (!showQuickNavigation && (document.querySelector('dialog[open], [aria-modal="true"]')
                || (event.target instanceof HTMLElement && event.target.isContentEditable))) return;
            event.preventDefault();
            if (showQuickNavigation) showQuickNavigation = false;
            else openQuickNavigation();
        } else if (showMobileTools && event.key === 'Escape' && !event.isComposing) {
            event.preventDefault();
            closeMobileTools(true);
        }
    }

    function onWindowClick(event: MouseEvent) {
        if (showMobileTools && event.target instanceof Node
            && !mobileToolsMenu?.contains(event.target) && !mobileToolsTrigger?.contains(event.target)) closeMobileTools();
    }

    $effect(() => subscribeBrowserContext(() => { activeTab = 'drives'; }));

    const batchStats = $derived.by(() => {
        const items = batchManager.items;
        const counts: Record<string, number> = {};
        for (const item of items) {
            const ext = item.extension || 'other';
            counts[ext] = (counts[ext] || 0) + 1;
        }
        return { total: items.length, counts };
    });

    // Catalog (DB) row count -- total rows currently indexed across all
    // levels. Refreshed on a 4 s timer so it reflects ingest progress
    // without hammering Rust. Cheap query (LanceDB count_rows).
    let dbDocCount = $state(0);
    let statsExpanded = $state(false);

    // Sync outbox status (P11 SyncManager).
    type SyncStatus = { pending_count: number; last_push_ts: number | null; remote_online: boolean };
    let syncStatus = $state<SyncStatus | null>(null);
    async function refreshSyncStatus() {
        try {
            syncStatus = await invoke<SyncStatus>('sync_status');
        } catch { syncStatus = null; }
    }
    // Poll sync status every 30 s when in Hybrid mode.
    $effect(() => {
        const interval = setInterval(refreshSyncStatus, 30_000);
        refreshSyncStatus();
        return () => clearInterval(interval);
    });
    // Writer queue depth: jobs submitted to IngestPipeline's background
    // writer task but not yet completed. Polled every 2s while processing.
    let writeQueueDepth = $state(0);
    async function refreshQueueDepth() {
        try {
            writeQueueDepth = await invoke<number>('index_queue_depth');
        } catch { writeQueueDepth = 0; }
    }
    function shortNumber(n: number): string {
        if (n < 1000) return n.toLocaleString();
        if (n < 1_000_000) return (n / 1000).toFixed(n < 10_000 ? 1 : 0).replace(/\.0$/, '') + 'k';
        return (n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0).replace(/\.0$/, '') + 'M';
    }
    async function refreshDbStats() {
        try {
            const stats = await invoke<{ doc_count: number }>('index_stats');
            dbDocCount = stats?.doc_count ?? 0;
        } catch { /* index disabled or not yet initialised */ }
    }

    // Live worker counters from the Stapel pipeline. The numbers below
    // come straight from $state on batchManager so they update on
    // every increment without needing an interval.
    const workerStats = $derived.by(() => {
        const elapsed = batchManager.runStartTs > 0
            ? (Date.now() - batchManager.runStartTs) / 1000
            : 0;
        const totalDone = batchManager.extractionDone + batchManager.llmDone;
        const docsPerMin = elapsed >= 1
            ? Math.round((totalDone / elapsed) * 60)
            : 0;
        return {
            extractionActive: batchManager.extractionActive,
            llmActive: batchManager.llmActive,
            extractionTarget: batchManager.extractionTargetWorkers,
            llmTarget: batchManager.llmTargetWorkers,
            extractionDone: batchManager.extractionDone,
            llmDone: batchManager.llmDone,
            docsPerMin,
            elapsed,
            isProcessing: batchManager.isProcessing,
            writeQueueDepth,
        };
    });

    // Start/stop the queue-depth poll based on active processing.
    // Using a module-level reference so startQueuePoll / stopQueuePoll (defined
    // inside onMount) can be called from the $effect.  We store the fns here
    // and set them once the onMount async block has finished.
    let _startQueuePoll: (() => void) | null = null;
    let _stopQueuePoll:  (() => void) | null = null;
    $effect(() => {
        const active = batchManager.isProcessing || writeQueueDepth > 0;
        if (active) _startQueuePoll?.();
        else         _stopQueuePoll?.();
    });

    // P36.5 — one capability set for every `requires:` gate: `build:*`
    // keys from what this binary compiled in, `service:*` keys from a
    // connected AIToolkit backend. Merged here rather than in `tabs.ts` so
    // the registry stays a plain data file with no store imports.
    let allCaps = $derived(new Set([...$buildFlags, ...$aitoolkitCaps]));
    const navigationItems = $derived(navigationDestinations(
        [...CORE_TABS, ...AITOOLKIT_TABS], allCaps, i18n.t.nav as Record<string, string>, i18n.lang,
    ));

    onMount(() => {
        let cleanup = () => {};
        (async () => {
            // Probe the build before anything renders conditionally. Its
            // own failure path leaves every gated surface hidden, so this
            // deliberately does not guard the rest of startup.
            await loadCapabilities();
            await loadExperimentalCrispEmbed();

            // Polled, not read once: the store refuses on the first *read*
            // of a secret, which happens long after mount — when a provider
            // key is resolved for a query. The call itself only reads state
            // the vault layer already holds; it never touches the keychain,
            // which is the whole point.
            const checkSecretStore = async () => {
                if (secretStoreAlertDismissed) return;
                try {
                    const st = await getBackendStatus();
                    secretStoreAlert = st.denied || st.error ? st : null;
                } catch (e) {
                    flog('warn', `secret store status unavailable: ${e}`);
                }
            };
            await checkSecretStore();
            const secretStoreTimer = setInterval(checkSecretStore, 15_000);

            // Load saved language
            const savedLang = await getSetting('language', 'en') as Language;
            i18n.setLanguage(savedLang);

            // Restore the in-process log verbosity threshold so the
            // Logs panel stays as quiet (or chatty) as the user left it.
            try {
                const v = await getSetting('logVerbosity', 'info') as any;
                const { setLogVerbosity } = await import('$lib/log');
                setLogVerbosity(v);
            } catch (e) { /* log module not loaded yet -- non-fatal */ }

            try {
                await batchManager.resumeLastSession();
            } catch (e) {
                console.error("Session resume failed:", e);
            }

            // ── Folder watcher ──────────────────────────────────────────
            // Single global listener for folder-watch:added events. The
            // Rust watcher emits one event per new file (debounced 2s);
            // we stat for size and append to the batch. addItem dedupes
            // on path, so re-emitted events don't create duplicate rows.
            const unlistenWatch = await listen<{ path: string }>(
                'folder-watch:added',
                async (event) => {
                    const path = event.payload?.path;
                    if (!path) return;
                    const name = path.split(/[\\/]/).pop() || path;
                    let size = 0;
                    try {
                        const info = await stat(path);
                        size = Number((info as any).size ?? 0);
                    } catch {
                        /* file may have been moved between detection and
                           stat — ignore and import with size=0 */
                    }
                    batchManager.addItem(path, name, size);
                    flog('info', `Watcher added: ${name}`);
                }
            );

            type SyncPair = {
                id: string;
                local_root: string;
                mode: 'to_cloud' | 'to_local' | 'two_way';
                enabled: boolean;
            };
            type SyncPairCandidate = { path: string; watched_folder: string };
            const syncPairPushes = new Set<string>();
            const unlistenSyncPair = await listen<SyncPairCandidate>(
                'folder-watch:sync-pair-candidate',
                async (event) => {
                    const candidate = event.payload;
                    if (!candidate?.path) return;
                    try {
                        const pairs = await invoke<SyncPair[]>('sync_pair_list');
                        for (const pair of pairs) {
                            if (!pair.enabled || pair.mode === 'to_local' || syncPairPushes.has(pair.id)) continue;
                            const root = pair.local_root.replace(/[\\/]+$/, '');
                            const changed = candidate.path.replaceAll('\\', '/');
                            const normalizedRoot = root.replaceAll('\\', '/');
                            if (changed !== normalizedRoot && !changed.startsWith(`${normalizedRoot}/`)) continue;
                            syncPairPushes.add(pair.id);
                            try {
                                await invoke('sync_pair_push', {
                                    id: pair.id,
                                    dryRun: false,
                                    conflictPolicy: 'local_wins',
                                });
                                flog('info', `[sync-pair] watcher push completed: ${pair.id}`);
                            } catch (error) {
                                flog('warn', `[sync-pair] watcher push failed for ${pair.id}: ${String(error)}`);
                            } finally {
                                syncPairPushes.delete(pair.id);
                            }
                        }
                    } catch (error) {
                        flog('warn', `[sync-pair] watcher dispatch failed: ${String(error)}`);
                    }
                },
            );

            // Resume any watchers configured in a previous session.
            // Migrate the v0.1.32 single-folder shape (watchEnabled +
            // watchFolder) to the v0.1.34 list shape (watchFolders) on
            // first read so existing users don't lose their setup.
            try {
                let folders = (await getSetting('watchFolders', null)) as string[] | null;
                if (folders == null) {
                    const legacyEnabled = (await getSetting('watchEnabled', false)) as boolean;
                    const legacyFolder = (await getSetting('watchFolder', '')) as string;
                    folders = legacyEnabled && legacyFolder ? [legacyFolder] : [];
                }
                const modes = (await getSetting('watchModes', {})) as Record<string, string>;
                for (const folder of folders) {
                    try {
                        const mode = modes[folder] || 'off';
                        await invoke('watch_start', { folder, mode, initialScan: false });
                        flog('info', `Watcher resumed: ${folder} (mode: ${mode})`);
                    } catch (e) {
                        flog('warn', `Watcher resume failed for ${folder}: ${e}`);
                    }
                }
            } catch (e) {
                flog('warn', `Watcher resume read failed: ${e}`);
            }

            // ── Restore the search-index config on every app launch ─────
            // The Rust IndexState boots with `IndexConfig::default()`
            // (enabled=false, BgeM3, ONNX, ...). The user's persisted
            // choices live in the JS-side `tauri-plugin-store`. Without
            // this push, every restart silently reverts the model /
            // backend / catalog dir to the defaults until the user
            // opens Settings and clicks Apply -- which is exactly what
            // the user was complaining about.
            //
            // We also auto-`index_init` so the L1 rows that already
            // exist in the LanceDB table on disk show up in Übersicht
            // immediately. `withEmbedder=false` keeps init cheap when
            // the user isn't doing vector search yet -- IndexIngest
            // upgrades to a full init on demand.
            try {
                // Mirror of Settings.svelte's indexEmbedderToRust /
                // indexModeToRust / indexDeviceToRust / indexBackendToRust
                // / rerankerToRust. Kept inline (not a $lib helper)
                // because it's the only boot-time translation; coupling
                // a settings-save format to a Rust serde format with a
                // shared module would be over-engineering for two
                // dictionaries of ~20 entries each.
                const modeToRust = (m: string) =>
                    ({ text: 'text_only', vector: 'vector_only', hybrid: 'hybrid' } as Record<string,string>)[m] ?? 'hybrid';
                const backendToRust = (b: string) =>
                    ({ remote: 'remote', hybrid: 'hybrid' } as Record<string,string>)[b] ?? 'local';
                const deviceToRust = (d: string) =>
                    ({ auto: 'auto', cpu: 'cpu', metal: 'metal', cuda: 'cuda' } as Record<string,string>)[d] ?? 'auto';
                const embedderToRust = (m: string) => ({
                    bge_m3: 'bge-m3',
                    pixie: 'pixie-rune-v1', pixie_q: 'pixie-rune-v1-q',
                    pixie_int4: 'pixie-rune-v1-int4', pixie_int4_full: 'pixie-rune-v1-int4-full',
                    octen: 'octen-06b-int8-local',
                    snowflake_l: 'snowflake-arctic-lv2', snowflake_l_fp16: 'snowflake-arctic-lv2-fp16',
                    snowflake_l_int8: 'snowflake-arctic-lv2-int8', snowflake_l_q4: 'snowflake-arctic-lv2-q4',
                    snowflake_l_q4f16: 'snowflake-arctic-lv2-q4-f16',
                    snowflake_l_o4: 'snowflake-arctic-lv2-o4', snowflake_l_fp32: 'snowflake-arctic-lv2-fp32',
                    jina_nano: 'jina-v5-nano', multilingual_mini_lm: 'multilingual-mini-lm',
                    multilingual_e5_small: 'multilingual-e5-small',
                    multilingual_e5_base:  'multilingual-e5-base',
                    multilingual_e5_large: 'multilingual-e5-large',
                    bge_small_en_v15: 'bge-small-en-v15',
                    bge_base_en_v15:  'bge-base-en-v15',
                    bge_large_en_v15: 'bge-large-en-v15',
                    nomic_embed_v15:  'nomic-embed-text-v15',
                    mxbai_large_v1:   'mxbai-embed-large-v1',
                    minilm_l6_v2:     'all-mini-lm-l6-v2',
                    embedding_gemma_300m: 'embedding-gemma300-m',
                    gte_base_en_v15:  'gte-base-en-v15',
                    gte_large_en_v15: 'gte-large-en-v15',
                    // HEAD-side aliases retained for backward compat
                    mxbai_embed_large_v1: 'mxbai-embed-large-v1',
                    nomic_embed_text_v15: 'nomic-embed-text-v15',
                    all_mini_lm_l6_v2:    'all-mini-lm-l6-v2',
                } as Record<string,string>)[m] ?? 'bge-m3';
                const rerankerToRust = (m: string): string | null => {
                    if (!m) return null;
                    return ({
                        bge_v2_m3: 'bge-reranker-v2-m3',
                        bge_base: 'bge-reranker-base',
                        jina_v2_multi: 'jina-reranker-v2-base-multilingual',
                    } as Record<string,string>)[m] ?? null;
                };

                const cfg = {
                    enabled:          (await getSetting('indexEnabled', false)) as boolean,
                    mode:             modeToRust(await getSetting('indexSearchMode', 'hybrid')),
                    backend_type:     backendToRust(await getSetting('indexBackendType', 'local')),
                    remote_url:       (await getSetting('indexRemoteUrl', '')) || null,
                    remote_api_key:   (await getSetting('indexRemoteApiKey', '')) || null,
                    embedder_model:   embedderToRust(await getSetting('indexEmbedderModel', 'bge_m3')),
                    embedder_device:  deviceToRust(await getSetting('indexDevice', 'auto')),
                    embedder_backend: await getSetting('indexEmbedderBackend', 'onnx'),
                    use_vector:           (await getSetting('indexUseVector', true)) as boolean,
                    embedder_location:    await getSetting('indexEmbedderLocation', 'client'),
                    reranker_model:   rerankerToRust(await getSetting('indexRerankerModel', '')),
                    rerank_top_n:     (await getSetting('indexRerankerTopN', 50)) as number,
                    model_cache_dir:  (await getSetting('indexModelCacheDir', '')) || null,
                    matryoshka_dim:   (await getSetting('indexMatryoshkaDim', 0)) || null,
                };
                await invoke('index_set_config', { config: cfg });
                flog('info', '[boot] Pushed persisted index config to Rust.');

                if (cfg.enabled) {
                    const dataDir = await invoke<string>('get_app_data_dir').catch(() => '');
                    await invoke('index_init', { dataDir, withEmbedder: false });
                    flog('info', `[boot] Index auto-initialised at ${dataDir} (use_vector=${cfg.use_vector}).`);
                }
            } catch (e: any) {
                flog('warn', `[boot] Index restore failed: ${e?.message ?? e}`);
            }

            // Initial DB stats + 4 s refresh tick. Cheap (single LanceDB
            // count_rows) so even unconditional polling is OK; we'd switch
            // to event-driven (push from Rust on every ingest) once the
            // ingest pipeline gains a `dbcount-changed` Tauri event.
            await refreshDbStats();
            const dbStatsTimer = setInterval(refreshDbStats, 4000);

            // Writer queue depth: only poll while an ingest is active.
            // The depth is always 0 between runs, so a 24/7 poll just wastes IPC.
            let queueDepthTimer: ReturnType<typeof setInterval> | null = null;
            const startQueuePoll = () => {
                if (!queueDepthTimer) queueDepthTimer = setInterval(refreshQueueDepth, 2000);
            };
            const stopQueuePoll = () => {
                if (queueDepthTimer) { clearInterval(queueDepthTimer); queueDepthTimer = null; writeQueueDepth = 0; }
            };
            // Expose to the $effect above so it can react to batchManager.isProcessing changes.
            _startQueuePoll = startQueuePoll;
            _stopQueuePoll  = stopQueuePoll;

            cleanup = () => {
                clearInterval(dbStatsTimer);
                clearInterval(secretStoreTimer);
                stopQueuePoll();
                unlistenWatch();
                unlistenSyncPair();
                invoke('watch_stop_all').catch(() => {});
            };
        })();
        return () => cleanup();
    });

    function switchToBatch() {
        navigate('batch');
    }
</script>

<svelte:window onkeydown={onWindowKey} onclick={onWindowClick} />

<div class="app-shell">
    <nav class="main-nav" class:collapsed={navCollapsed} aria-label={navCopy.primary}>
        <div class="nav-top">
            <div class="logo-area">
                <div class="logo-icon">C</div>
                {#if !navCollapsed}<span class="logo-text">CrispSorter</span>{/if}
            </div>
            <button class="nav-item quick-navigation-trigger" onclick={openQuickNavigation}
                aria-label={navCopy.open} aria-keyshortcuts="Control+k Meta+k" title={`${navCopy.open} (Ctrl/⌘ K)`}>
                <Search size={20} aria-hidden="true" />
                {#if !navCollapsed}<span>{navCopy.open}</span><kbd>⌘/Ctrl K</kbd>{/if}
            </button>
            <div class="nav-separator"></div>
            {#each visibleTabs([...CORE_TABS, ...AITOOLKIT_TABS], allCaps) as tab (tab.id)}
                {#if tab.separatorBefore}<div class="nav-separator"></div>{/if}
                {@const Icon = tab.icon}
                {@const label = (i18n.t.nav as Record<string, string>)[tab.id] ?? tab.label ?? tab.id}
                <button class="nav-item" class:active={activeTab === tab.id} onclick={() => navigate(tab.id)} title={label}
                    aria-label={label} aria-current={activeTab === tab.id ? 'page' : undefined}>
                    <Icon size={20} aria-hidden="true" />
                    {#if !navCollapsed}<span>{((i18n.t.nav as Record<string, string>)[tab.id] ?? tab.label ?? tab.id)}</span>{/if}
                </button>
            {/each}
        </div>

        <div class="nav-bottom">
            {#if batchStats.total > 0 || dbDocCount > 0}
                <button class="batch-stats stats-toggle"
                    onclick={() => statsExpanded = !statsExpanded}
                    title={statsExpanded ? 'Click to collapse' : 'Click for breakdown'}>
                    {#if !navCollapsed}
                        <div class="stats-summary">
                            {#if batchStats.total > 0}
                                <span class="stats-pair"><span class="stats-key">Stapel:</span> <span class="stats-val">{shortNumber(batchStats.total)}</span></span>
                            {/if}
                            {#if dbDocCount > 0}
                                <span class="stats-sep" aria-hidden="true">·</span>
                                <span class="stats-pair"><span class="stats-key">DB:</span> <span class="stats-val">{shortNumber(dbDocCount)}</span></span>
                            {/if}
                        </div>
                        {#if statsExpanded && batchStats.total > 0}
                            <div class="stats-breakdown">
                                {#each Object.entries(batchStats.counts).sort((a,b) => b[1]-a[1]) as [ext, count]}
                                    <span class="stat-ext">{count} {ext}</span>
                                {/each}
                            </div>
                        {/if}
                    {:else}
                        <span class="stats-badge-collapsed">{shortNumber(batchStats.total + dbDocCount)}</span>
                    {/if}
                </button>
            {/if}

            <!-- Live worker / throughput chip. Only shown while a
                 processAll run is in flight; it's a separate visual
                 group from the static Stapel/DB totals above. -->
            {#if workerStats.isProcessing || workerStats.extractionActive > 0 || workerStats.llmActive > 0 || workerStats.writeQueueDepth > 0}
                <button class="batch-stats stats-toggle worker-stats"
                    onclick={() => statsExpanded = !statsExpanded}
                    title="Workers (click for details)">
                    {#if !navCollapsed}
                        <div class="stats-summary">
                            <span class="worker-dot" class:active={workerStats.extractionActive > 0} aria-hidden="true">●</span>
                            <span class="stats-pair">
                                <span class="stats-key">Ex:</span>
                                <span class="stats-val">{workerStats.extractionActive}/{workerStats.extractionTarget}</span>
                            </span>
                            <span class="stats-sep" aria-hidden="true">·</span>
                            <span class="worker-dot" class:active={workerStats.llmActive > 0} aria-hidden="true">●</span>
                            <span class="stats-pair">
                                <span class="stats-key">LLM:</span>
                                <span class="stats-val">{workerStats.llmActive}/{workerStats.llmTarget}</span>
                            </span>
                            {#if workerStats.writeQueueDepth > 0}
                                <span class="stats-sep" aria-hidden="true">·</span>
                                <span class="worker-dot active" aria-hidden="true">●</span>
                                <span class="stats-pair">
                                    <span class="stats-key">W:</span>
                                    <span class="stats-val">{workerStats.writeQueueDepth}</span>
                                </span>
                            {/if}
                            {#if workerStats.docsPerMin > 0}
                                <span class="stats-sep" aria-hidden="true">·</span>
                                <span class="stats-val">{workerStats.docsPerMin}/min</span>
                            {/if}
                        </div>
                        {#if statsExpanded}
                            <div class="stats-breakdown">
                                <span class="stat-ext">{workerStats.extractionDone} extracted</span>
                                <span class="stat-ext">{workerStats.llmDone} analyzed</span>
                                {#if workerStats.writeQueueDepth > 0}
                                    <span class="stat-ext">{workerStats.writeQueueDepth} write queue</span>
                                {/if}
                                <span class="stat-ext">{Math.round(workerStats.elapsed)}s elapsed</span>
                            </div>
                        {/if}
                    {:else}
                        <span class="stats-badge-collapsed" style="background:#16a34a33; color:#86efac;">{workerStats.extractionActive + workerStats.llmActive + workerStats.writeQueueDepth}</span>
                    {/if}
                </button>
            {/if}
            {#if syncStatus && (syncStatus.pending_count > 0 || syncStatus.remote_online)}
                <button class="batch-stats sync-chip"
                        onclick={() => invoke('sync_push').catch(() => {})}
                        title="{syncStatus.pending_count} ausstehend · {syncStatus.remote_online ? 'Server online' : 'Server offline'} · Klick zum Senden">
                    {#if !navCollapsed}
                        <span class="sync-dot" class:online={syncStatus.remote_online} aria-hidden="true">⇅</span>
                        <span class="stats-val">{syncStatus.pending_count}</span>
                    {:else}
                        <span class="stats-badge-collapsed" style="background:{syncStatus.remote_online ? '#16a34a33' : '#45222233'}; color:{syncStatus.remote_online ? '#86efac' : '#f87171'};">{syncStatus.pending_count}</span>
                    {/if}
                </button>
            {/if}
            <button class="nav-item" class:active={showLogs} onclick={() => showLogs = !showLogs} title={i18n.t.nav.logs}
                aria-label={i18n.t.nav.logs} aria-expanded={showLogs}>
                <Terminal size={20} aria-hidden="true" />
                {#if !navCollapsed}<span>{i18n.t.nav.logs}</span>{/if}
            </button>

            <button class="nav-item" class:active={activeTab === 'settings'} onclick={() => navigate('settings')} title={i18n.t.nav.settings}
                aria-label={i18n.t.nav.settings} aria-current={activeTab === 'settings' ? 'page' : undefined}>
                <SettingsIcon size={20} aria-hidden="true" />
                {#if !navCollapsed}<span>{i18n.t.nav.settings}</span>{/if}
            </button>

            <button class="collapse-toggle" onclick={() => navCollapsed = !navCollapsed}
                aria-label={navCollapsed ? navCopy.expand : navCopy.collapse} title={navCollapsed ? navCopy.expand : navCopy.collapse} aria-expanded={!navCollapsed}>
                {#if navCollapsed}<ChevronRight size={16} />{:else}<ChevronLeft size={16} />{/if}
            </button>
        </div>
    </nav>

    <main class="main-content" class:with-logs={showLogs}>
        {#if secretStoreAlert && !secretStoreAlertDismissed}
            <div class="secret-store-banner" role="alert">
                <span>
                    {#if secretStoreAlert.denied}
                        Your system credential store refused to release CrispSorter's
                        saved keys. On macOS this is the login keychain asking for a
                        password — CrispSorter cannot supply it, and will stop asking
                        for the rest of this session.
                    {:else}
                        CrispSorter could not open the store your secrets are kept in, and is
                        running without it: saved keys are unavailable and new ones are not
                        saved until it opens. ({secretStoreAlert.error})
                    {/if}
                </span>
                {#if secretStoreAlert.error}
                    <button class="secret-store-banner-action" disabled={secretStoreRetrying} onclick={retrySecretStore}>
                        {secretStoreRetrying ? 'Retrying…' : 'Retry'}
                    </button>
                {/if}
                <button class="secret-store-banner-action" onclick={() => { activeTab = 'settings'; secretStoreAlertDismissed = true; }}>
                    Choose where secrets are stored
                </button>
                <button class="secret-store-banner-close" aria-label="Dismiss" onclick={() => secretStoreAlertDismissed = true}>×</button>
            </div>
        {/if}
        {#if ['batch', 'review', 'catalog'].includes(activeTab)}
            <WorkflowSummary {navigate} />
        {/if}
        <div class="content-area">
            {#if activeTab === 'settings'}
                <Settings />
            {:else if activeTab === 'batch'}
                <BatchReview workflow="inbox" />
            {:else if activeTab === 'review'}
                <BatchReview workflow="review" />
            {:else if activeTab === 'drives'}
                <DriveBrowser />
            {:else if activeTab === 'history'}
                <History onResumeBatch={switchToBatch} />
            {:else if activeTab === 'catalog'}
                <IndexIngest />
            {:else if activeTab === 'translate'}
                <Translate initialPath={$documentToolRequest?.tool === 'translate' ? uriToPath($documentToolRequest.locationUri) || '' : ''} />
            {:else if activeTab === 'pdf'}
                <PdfWorkspace initialPath={$documentToolRequest?.tool === 'pdf' ? uriToPath($documentToolRequest.locationUri) || '' : ''} />
            <!-- PLAN P36.16 — the AIToolkit panels only exist in builds that
                 asked for them. The nav already hides these tabs, so this
                 second check is for the state the nav cannot reach: an
                 `activeTab` that was set before the probe resolved, or by a
                 future deep link. Gating the *mount* rather than trusting the
                 nav means the client never connects to a backend this build
                 was not supposed to talk to. -->
            {:else if activeTab === 'aitoolkit' && $caps.aitoolkit}
                <AIToolkitView />
            {:else if activeTab.startsWith('ai:') && $caps.aitoolkit}
                <AIToolkitCapability capability={activeTab.slice(3)} />
            {/if}
            {#if ocrInitialized || activeTab === 'ocr'}
                <div class="persistent-ocr" hidden={activeTab !== 'ocr'} inert={activeTab !== 'ocr'}>
                    <!-- One persistent workspace: a document sent from the library is
                         imported into it rather than mounting a second instance. -->
                    <OcrWorkbench active={activeTab === 'ocr'}
                        initialLocationUri={$documentToolRequest?.tool === 'ocr' ? uriToPath($documentToolRequest.locationUri) || '' : ''} />
                </div>
            {/if}
            <div class="persistent-chat" style:display={activeTab === 'chat' ? 'block' : 'none'}>
                <Chat />
            </div>
        </div>
        {#if showLogs}
            <div class="log-drawer">
                <LogPanel />
            </div>
        {/if}
    </main>

    <TransferDrawer />

    <!-- PLAN P36.13 / App Review 5.1.2(i). Mounted once at the shell level:
         it registers itself as the prompter for the LLM egress gate, so
         every path that could send document text to a third party is
         covered without any call site knowing it exists. -->
    <ThirdPartyAiConsent />

    {#if showQuickNavigation}
        <QuickNavigation destinations={navigationItems} current={activeTab} language={i18n.lang}
            onNavigate={navigate} onClose={() => showQuickNavigation = false} />
    {/if}

    <!-- Mobile bottom tab bar — visible only on small screens -->
    <nav class="mobile-nav" aria-label={navCopy.mobile}>
        {#each visibleTabs(MOBILE_TABS, allCaps).slice(0, 4) as tab (tab.id)}
            {@const Icon = tab.icon}
            <button class="mobile-tab" class:active={activeTab === tab.id} onclick={() => navigate(tab.id)} aria-current={activeTab === tab.id ? 'page' : undefined}>
                <Icon size={20} aria-hidden="true" /><span>{(i18n.t.nav as Record<string, string>)[tab.id] ?? tab.label ?? tab.id}</span>
            </button>
        {/each}
        <button class="mobile-tab" bind:this={mobileToolsTrigger} class:active={!['batch', 'review', 'catalog', 'chat', 'settings'].includes(activeTab)}
            aria-expanded={showMobileTools} aria-controls="mobile-tools-menu" onclick={toggleMobileTools}>
            <span aria-hidden="true">⋯</span><span>{navCopy.tools}</span>
        </button>
        {#if showMobileTools}
            <div id="mobile-tools-menu" class="mobile-tools-menu" bind:this={mobileToolsMenu} role="group" aria-label={navCopy.tools}
                onfocusout={(event) => {
                    if (event.relatedTarget instanceof Node && !mobileToolsMenu?.contains(event.relatedTarget)
                        && !mobileToolsTrigger?.contains(event.relatedTarget)) closeMobileTools();
                }}>
                <button class="mobile-quick-navigation" onclick={openQuickNavigation}><Search size={16} aria-hidden="true" />{navCopy.open}</button>
                {#each visibleTabs(MOBILE_TABS, allCaps).slice(4) as tab}
                    <button onclick={() => navigate(tab.id)} aria-current={activeTab === tab.id ? 'page' : undefined}>{(i18n.t.nav as Record<string, string>)[tab.id] ?? tab.label ?? tab.id}</button>
                {/each}
                {#each ['pdf', 'drives', 'history'] as tab}
                    <button onclick={() => navigate(tab)} aria-current={activeTab === tab ? 'page' : undefined}>{(i18n.t.nav as Record<string, string>)[tab]}</button>
                {/each}
                <button onclick={() => navigate('logs')}>{i18n.t.nav.logs}</button>
            </div>
        {/if}
        <button class="mobile-tab" class:active={activeTab === 'settings'} onclick={() => navigate('settings')} aria-current={activeTab === 'settings' ? 'page' : undefined}>
            <SettingsIcon size={20} aria-hidden="true" /><span>{i18n.t.nav.settings}</span>
        </button>
    </nav>
</div>

<style>
    .mobile-tools-menu {
        position: absolute;
        /* The containing navigation already sits inside all four safe edges.
           Its actual height can grow with text; anchor above that height. */
        bottom: calc(100% + 8px);
        right: 12px;
        max-width: calc(100% - 24px);
        max-height: calc(100vh - env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px) - 100% - 16px);
        max-height: calc(100dvh - env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px) - 100% - 16px);
        overflow-y: auto;
        background: #18181b;
        border: 1px solid #52525b;
        border-radius: 8px;
        padding: 8px;
        z-index: 100;
        display: flex;
        flex-direction: column;
    }
    .mobile-tools-menu button {
        background: transparent;
        border: 0;
        color: #eee;
        padding: 10px 20px;
        min-height: 44px;
        font-size: 16px;
        text-align: left;
        cursor: pointer;
    }
    .mobile-tools-menu button:hover, .mobile-tools-menu button[aria-current="page"] { background: #27272a; }
    .mobile-tools-menu .mobile-quick-navigation { display: flex; align-items: center; gap: 8px; border-bottom: 1px solid #3f3f46; margin-bottom: 4px; }
    .main-nav button:focus-visible, .mobile-nav button:focus-visible { outline: 2px solid #93c5fd; outline-offset: -3px; }

    .app-shell {
        display: flex;
        width: 100vw;
        height: 100vh;
        height: 100dvh;
        /* app.html opts out of WebKit's automatic content inset. Keep the
           entire flex shell inside the safe area exactly once, including
           landscape cutouts and the home indicator below its navigation. */
        box-sizing: border-box;
        padding: env(safe-area-inset-top, 0px) env(safe-area-inset-right, 0px)
                 env(safe-area-inset-bottom, 0px) env(safe-area-inset-left, 0px);
        overflow: hidden;
        background: #09090b;
        color: #fafafa;
    }

    .main-nav {
        width: 200px;
        background: #18181b;
        color: #a1a1aa;
        display: flex;
        flex-direction: column;
        justify-content: space-between;
        padding: 20px 0;
        border-right: 1px solid #27272a;
        flex-shrink: 0;
        transition: width 0.3s cubic-bezier(0.4, 0, 0.2, 1);
        overflow: hidden;
    }

    .main-nav.collapsed {
        width: 64px;
    }

    .nav-top { flex: 1; min-height: 0; overflow-y: auto; overflow-x: hidden; }
    .nav-bottom { flex-shrink: 0; }
    .nav-item.quick-navigation-trigger { display: grid; grid-template-columns: 20px minmax(0, 1fr); gap: 3px 10px; padding: 10px 18px; font-size: 0.8rem; }
    .quick-navigation-trigger :global(svg) { grid-row: 1 / 3; }
    .quick-navigation-trigger span { min-width: 0; white-space: normal; overflow-wrap: anywhere; line-height: 1.3; }
    .quick-navigation-trigger kbd { font: inherit; font-size: 0.65rem; color: #a1a1aa; grid-column: 2; }
    .collapsed .quick-navigation-trigger { display: flex; padding: 12px 22px; }

    .logo-area {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 0 16px 30px;
    }

    .logo-icon {
        width: 32px;
        height: 32px;
        background: #3b82f6;
        color: white;
        border-radius: 8px;
        display: flex;
        align-items: center;
        justify-content: center;
        font-weight: 800;
        font-size: 1.2rem;
        flex-shrink: 0;
    }

    .logo-text {
        font-weight: 700;
        font-size: 1.1rem;
        color: white;
        white-space: nowrap;
    }

    .nav-item {
        width: 100%;
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 12px 22px;
        border: none;
        background: transparent;
        color: #a1a1aa;
        cursor: pointer;
        font-size: 0.9375rem;
        font-weight: 500;
        transition: all 0.2s;
        text-align: left;
        white-space: nowrap;
    }

    .nav-item:hover {
        background: #27272a;
        color: white;
    }

    .nav-item.active {
        background: #27272a;
        color: white;
        border-right: 3px solid #3b82f6;
    }

    .collapse-toggle {
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 12px;
        margin-top: 10px;
        border: none;
        background: transparent;
        color: #71717a;
        cursor: pointer;
        transition: color 0.2s;
    }
    .collapse-toggle:hover { color: white; }

    .main-content {
        flex: 1;
        min-width: 0;
        overflow: hidden;
        position: relative;
        display: flex;
        flex-direction: column;
    }

    .content-area {
        flex: 1;
        overflow: hidden;
        position: relative;
        min-height: 0;
    }

    /* Shown only when the credential store has actually refused or failed
       to open — at that point every saved API key is unreachable, so the
       choice of store stops being a Settings detail. */
    .secret-store-banner {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 10px 14px;
        background: #45222233;
        border-bottom: 1px solid #7f1d1d;
        color: #fecaca;
        font-size: 0.85rem;
        flex: 0 0 auto;
    }
    .secret-store-banner span { flex: 1; }
    .secret-store-banner-action {
        flex: 0 0 auto;
        background: #7f1d1d;
        color: #fee2e2;
        border: none;
        border-radius: 6px;
        padding: 6px 12px;
        cursor: pointer;
        font: inherit;
    }
    .secret-store-banner-action:hover { background: #991b1b; }
    .secret-store-banner-close {
        flex: 0 0 auto;
        background: none;
        border: none;
        color: #fecaca;
        cursor: pointer;
        font-size: 1.2rem;
        line-height: 1;
        padding: 0 4px;
    }

    .log-drawer {
        height: 220px;
        border-top: 1px solid #27272a;
        flex-shrink: 0;
        overflow: hidden;
    }

    .persistent-chat {
        position: absolute;
        inset: 0;
        z-index: 5;
    }

    .persistent-ocr {
        height: 100%;
        min-width: 0;
        min-height: 0;
    }

    .persistent-ocr[hidden] { display: none; }

    .nav-separator { height: 1px; background: #27272a; margin: 8px 16px; }
    .batch-stats { padding: 8px 16px; margin-bottom: 8px; border-top: 1px solid #27272a; }
    .stats-toggle {
        width: 100%; background: transparent; border: none; color: inherit;
        text-align: left; cursor: pointer; padding: 8px 16px; margin-bottom: 8px;
        border-top: 1px solid #27272a;
    }
    .stats-toggle:hover { background: #1f1f23; }
    .stats-summary {
        display: flex; flex-wrap: wrap; gap: 6px; align-items: baseline;
        font-size: 0.75rem; color: #a1a1aa; white-space: nowrap;
    }
    .stats-pair { display: inline-flex; gap: 4px; align-items: baseline; }
    .stats-key { font-weight: 600; color: #71717a; }
    .stats-val { font-weight: 700; color: #e4e4e7; }
    .stats-sep { color: #3f3f46; }
    .stats-breakdown { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 6px; }
    .stat-ext { font-size: 0.65rem; background: #27272a; border-radius: 3px; padding: 1px 5px; color: #71717a; text-transform: uppercase; font-weight: 600; }
    .stats-badge-collapsed { display: flex; align-items: center; justify-content: center; width: 28px; height: 20px; background: #3b82f633; border-radius: 4px; font-size: 0.7rem; font-weight: 700; color: #60a5fa; margin: 0 auto; }
    .worker-stats { border-top: 1px dashed #27272a; }
    .sync-chip { border-top: 1px dashed #27272a; }
    .sync-dot { font-size: 0.9rem; margin-right: 4px; color: #3f3f46; }
    .sync-dot.online { color: #22c55e; }
    .worker-dot { color: #3f3f46; font-size: 0.7rem; }
    .worker-dot.active { color: #22c55e; animation: pulse 1.4s ease-in-out infinite; }
    @keyframes pulse {
        0%, 100% { opacity: 1; }
        50% { opacity: 0.35; }
    }

    /* ── Mobile bottom tab bar ─────────────────────────────────────── */
    .mobile-nav {
        position: relative;
        display: none;
    }

    .mobile-tab {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 2px;
        padding: 6px 0;
        border: none;
        background: transparent;
        color: #71717a;
        cursor: pointer;
        font-size: 0.625rem;
        font-weight: 500;
        flex: 1;
        min-width: 0;
        min-height: 44px;
    }
    .mobile-tab.active {
        color: #3b82f6;
    }

    /* ── Responsive: phone (<768px) ────────────────────────────────── */
    @media (max-width: 767px) {
        .app-shell {
            flex-direction: column;
        }
        .main-nav {
            display: none;
        }
        .mobile-nav {
            display: flex;
            justify-content: space-around;
            align-items: center;
            background: #18181b;
            border-top: 1px solid #27272a;
            padding: 4px 0;
            flex-shrink: 0;
        }
        .main-content {
            flex: 1;
            min-height: 0;
        }
        .log-drawer {
            height: 160px;
        }
    }

    /* ── Responsive: tablet (768-1024px) ───────────────────────────── */
    @media (min-width: 768px) and (max-width: 1024px) {
        .main-nav {
            width: 64px;
        }
        .main-nav .logo-text,
        .main-nav .nav-item span,
        .main-nav .quick-navigation-trigger kbd,
        .main-nav .collapse-toggle {
            display: none;
        }
        .main-nav .quick-navigation-trigger { display: flex; padding: 12px 22px; }
    }

    @media (prefers-reduced-motion: reduce) {
        .main-nav, .nav-item, .collapse-toggle { transition: none; }
        .worker-dot.active { animation: none; }
    }
</style>
