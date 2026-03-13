<script lang="ts">
    import { onMount } from 'svelte';
    import Settings from '$lib/components/Settings.svelte';
    import BatchReview from '$lib/components/BatchReview.svelte';
    import History from '$lib/components/History.svelte';
    import { batchManager } from '$lib/batch/store.svelte';
    import { i18n, type Language } from '$lib/i18n.svelte';
    import { getSetting } from '$lib/store';
    import { Settings as SettingsIcon, Database, ListChecks } from 'lucide-svelte';

    let activeTab = $state('batch'); // 'batch', 'settings', 'history'

    onMount(async () => {
        // Load saved language
        const savedLang = await getSetting('language', 'en') as Language;
        i18n.setLanguage(savedLang);

        try {
            await batchManager.resumeLastSession();
        } catch (e) {
            console.error("Session resume failed:", e);
        }
    });

    function switchToBatch() {
        activeTab = 'batch';
    }
</script>

<div class="app-shell">
    <nav class="main-nav">
        <div class="nav-top">
            <div class="logo-area">
                <div class="logo-icon">C</div>
                <span class="logo-text">CrispSorter</span>
            </div>
            
            <button class="nav-item" class:active={activeTab === 'batch'} onclick={() => activeTab = 'batch'}>
                <ListChecks size={20} />
                <span>{i18n.t.nav.batch}</span>
            </button>
            
            <button class="nav-item" class:active={activeTab === 'history'} onclick={() => activeTab = 'history'}>
                <Database size={20} />
                <span>{i18n.t.nav.history}</span>
            </button>
        </div>

        <div class="nav-bottom">
            <button class="nav-item" class:active={activeTab === 'settings'} onclick={() => activeTab = 'settings'}>
                <SettingsIcon size={20} />
                <span>{i18n.t.nav.settings}</span>
            </button>
        </div>
    </nav>

    <main class="main-content">
        {#if activeTab === 'settings'}
            <Settings />
        {:else if activeTab === 'batch'}
            <BatchReview />
        {:else if activeTab === 'history'}
            <History onResumeBatch={switchToBatch} />
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
        width: fit-content; /* Fit to labels as requested */
        min-width: 180px;
        background: #18181b;
        color: #a1a1aa;
        display: flex;
        flex-direction: column;
        justify-content: space-between;
        padding: 20px 0;
        border-right: 1px solid #27272a;
        flex-shrink: 0;
    }

    .logo-area {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 0 20px 30px;
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
        padding: 12px 20px;
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

    .main-content {
        flex: 1;
        overflow: hidden;
        position: relative;
    }
</style>
