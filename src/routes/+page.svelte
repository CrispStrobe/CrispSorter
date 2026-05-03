<script lang="ts">
    import { onMount } from 'svelte';
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
    import { Settings as SettingsIcon, Database, ListChecks, MessageSquare, ChevronLeft, ChevronRight, Search, UploadCloud, Terminal, HardDrive, CopyCheck } from 'lucide-svelte';
    import IndexSearch from '$lib/components/IndexSearch.svelte';
    import IndexIngest from '$lib/components/IndexIngest.svelte';
    import LogPanel from '$lib/components/LogPanel.svelte';
    import Catalog from '$lib/components/Catalog.svelte';
    import Duplicates from '$lib/components/Duplicates.svelte';

    let activeTab = $state('batch'); // 'batch', 'history', 'chat', 'settings', 'index-search', 'index-ingest'
    let navCollapsed = $state(false);
    let showLogs = $state(false);

    const batchStats = $derived.by(() => {
        const items = batchManager.items;
        const counts: Record<string, number> = {};
        for (const item of items) {
            const ext = item.extension || 'other';
            counts[ext] = (counts[ext] || 0) + 1;
        }
        return { total: items.length, counts };
    });

    onMount(() => {
        let cleanup = () => {};
        (async () => {
            // Load saved language
            const savedLang = await getSetting('language', 'en') as Language;
            i18n.setLanguage(savedLang);

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
                for (const folder of folders) {
                    try {
                        await invoke('watch_start', { folder });
                        flog('info', `Watcher resumed: ${folder}`);
                    } catch (e) {
                        flog('warn', `Watcher resume failed for ${folder}: ${e}`);
                    }
                }
            } catch (e) {
                flog('warn', `Watcher resume read failed: ${e}`);
            }

            cleanup = () => {
                unlistenWatch();
                invoke('watch_stop_all').catch(() => {});
            };
        })();
        return () => cleanup();
    });

    function switchToBatch() {
        activeTab = 'batch';
    }
</script>

<div class="app-shell">
    <nav class="main-nav" class:collapsed={navCollapsed}>
        <div class="nav-top">
            <div class="logo-area">
                <div class="logo-icon">C</div>
                {#if !navCollapsed}<span class="logo-text">CrispSorter</span>{/if}
            </div>
            
            <button class="nav-item" class:active={activeTab === 'batch'} onclick={() => activeTab = 'batch'} title={i18n.t.nav.batch}>
                <ListChecks size={20} />
                {#if !navCollapsed}<span>{i18n.t.nav.batch}</span>{/if}
            </button>

            <button class="nav-item" class:active={activeTab === 'chat'} onclick={() => activeTab = 'chat'} title={i18n.t.nav.chat}>
                <MessageSquare size={20} />
                {#if !navCollapsed}<span>{i18n.t.nav.chat}</span>{/if}
            </button>
            
            <button class="nav-item" class:active={activeTab === 'history'} onclick={() => activeTab = 'history'} title={i18n.t.nav.history}>
                <Database size={20} />
                {#if !navCollapsed}<span>{i18n.t.nav.history}</span>{/if}
            </button>

            <div class="nav-separator"></div>

            <button class="nav-item" class:active={activeTab === 'index-search'} onclick={() => activeTab = 'index-search'} title="Index Suche">
                <Search size={20} />
                {#if !navCollapsed}<span>Index Suche</span>{/if}
            </button>

            <button class="nav-item" class:active={activeTab === 'index-ingest'} onclick={() => activeTab = 'index-ingest'} title="Index Ingest">
                <UploadCloud size={20} />
                {#if !navCollapsed}<span>Index Ingest</span>{/if}
            </button>

            <div class="nav-separator"></div>

            <button class="nav-item" class:active={activeTab === 'catalog'} onclick={() => activeTab = 'catalog'} title="Catalog (Cathy/Catfish .caf)">
                <HardDrive size={20} />
                {#if !navCollapsed}<span>Catalog</span>{/if}
            </button>

            <button class="nav-item" class:active={activeTab === 'dupes'} onclick={() => activeTab = 'dupes'} title="Find Duplicates">
                <CopyCheck size={20} />
                {#if !navCollapsed}<span>Duplicates</span>{/if}
            </button>
        </div>

        <div class="nav-bottom">
            {#if batchStats.total > 0}
                <div class="batch-stats" title="Files in current batch">
                    {#if !navCollapsed}
                        <div class="stats-total">{i18n.t.batch.stats_files.replace('{count}', batchStats.total.toString())}</div>
                        <div class="stats-breakdown">
                            {#each Object.entries(batchStats.counts).sort((a,b) => b[1]-a[1]) as [ext, count]}
                                <span class="stat-ext">{count} {ext}</span>
                            {/each}
                        </div>
                    {:else}
                        <span class="stats-badge-collapsed">{batchStats.total}</span>
                    {/if}
                </div>
            {/if}
            <button class="nav-item" class:active={showLogs} onclick={() => showLogs = !showLogs} title="Logs">
                <Terminal size={20} />
                {#if !navCollapsed}<span>Logs</span>{/if}
            </button>

            <button class="nav-item" class:active={activeTab === 'settings'} onclick={() => activeTab = 'settings'} title={i18n.t.nav.settings}>
                <SettingsIcon size={20} />
                {#if !navCollapsed}<span>{i18n.t.nav.settings}</span>{/if}
            </button>

            <button class="collapse-toggle" onclick={() => navCollapsed = !navCollapsed}>
                {#if navCollapsed}<ChevronRight size={16} />{:else}<ChevronLeft size={16} />{/if}
            </button>
        </div>
    </nav>

    <main class="main-content" class:with-logs={showLogs}>
        <div class="content-area">
            {#if activeTab === 'settings'}
                <Settings />
            {:else if activeTab === 'batch'}
                <BatchReview />
            {:else if activeTab === 'history'}
                <History onResumeBatch={switchToBatch} />
            {:else if activeTab === 'index-search'}
                <IndexSearch />
            {:else if activeTab === 'index-ingest'}
                <IndexIngest />
            {:else if activeTab === 'catalog'}
                <Catalog />
            {:else if activeTab === 'dupes'}
                <Duplicates />
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
</div>

<style>
    .app-shell {
        display: flex;
        width: 100vw;
        height: 100vh;
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

    .nav-separator { height: 1px; background: #27272a; margin: 8px 16px; }
    .batch-stats { padding: 8px 16px; margin-bottom: 8px; border-top: 1px solid #27272a; }
    .stats-total { font-size: 0.75rem; font-weight: 700; color: #a1a1aa; white-space: nowrap; }
    .stats-breakdown { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 4px; }
    .stat-ext { font-size: 0.65rem; background: #27272a; border-radius: 3px; padding: 1px 5px; color: #71717a; text-transform: uppercase; font-weight: 600; }
    .stats-badge-collapsed { display: flex; align-items: center; justify-content: center; width: 28px; height: 20px; background: #3b82f633; border-radius: 4px; font-size: 0.7rem; font-weight: 700; color: #60a5fa; margin: 0 auto; }
</style>
