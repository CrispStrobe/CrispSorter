<script lang="ts">
    import { Database, FolderOpen, Plus, Search, Pencil, Trash2 } from 'lucide-svelte';
    import { i18n } from '../i18n.svelte';

    interface IndexEntry {
        id: string;
        name: string;
        dataDir: string;
        mode: 'text' | 'vector' | 'hybrid';
        backend: 'local' | 'remote' | 'hybrid';
    }

    let {
        entries, activeId, selectedId, busy, ready, location, stats,
        onSelect, onOpen, onAdd, onRename, onRemove, onSearch,
    }: {
        entries: IndexEntry[];
        activeId: string | null;
        selectedId: string | null;
        busy: boolean;
        ready: boolean;
        location: string;
        stats: { doc_count: number; total_rows: number } | null;
        onSelect: (id: string) => void;
        onOpen: (id: string) => void;
        onAdd: (mode: 'existing' | 'new') => void;
        onRename: (id: string, name: string) => void;
        onRemove: (id: string) => void;
        onSearch: () => void;
    } = $props();

    let selected = $derived(entries.find(entry => entry.id === selectedId));
    let showingActive = $derived(ready && (!selected || selected.id === activeId));
    let renaming = $state(false);
    let draft = $state('');
    const copy = {
        en: {
            title: 'Index manager', indexes: 'Saved indexes', empty: 'No saved indexes',
            add: 'Add existing index', create: 'Create index', open: 'Open index',
            search: 'Search in Library', details: 'Index details', current: 'Current index',
            location: 'Index directory', appData: 'App data directory', remote: 'Remote server',
            status: 'Status', ready: 'Open', closed: 'Closed', opening: 'Opening…',
            mode: 'Search mode', documents: 'Documents', rows: 'Index rows',
            rename: 'Rename index', remove: 'Remove from list', cancel: 'Cancel', save: 'Save',
        },
        de: {
            title: 'Indexverwaltung', indexes: 'Gespeicherte Indizes', empty: 'Keine gespeicherten Indizes',
            add: 'Vorhandenen Index hinzufügen', create: 'Index erstellen', open: 'Index öffnen',
            search: 'In der Bibliothek suchen', details: 'Indexinformationen', current: 'Aktueller Index',
            location: 'Index-Datenverzeichnis', appData: 'App-Datenverzeichnis', remote: 'Remote-Server',
            status: 'Status', ready: 'Geöffnet', closed: 'Geschlossen', opening: 'Wird geöffnet…',
            mode: 'Suchmodus', documents: 'Dokumente', rows: 'Indexzeilen',
            rename: 'Index umbenennen', remove: 'Aus Liste entfernen', cancel: 'Abbrechen', save: 'Speichern',
        },
    };
    let t = $derived(copy[i18n.lang]);

    function select(id: string) {
        renaming = false;
        onSelect(id);
    }

    function rename(event: SubmitEvent) {
        event.preventDefault();
        if (selected && draft.trim()) onRename(selected.id, draft.trim());
        renaming = false;
    }
</script>

<section class="manager" aria-labelledby="index-manager-heading">
    <header>
        <h2 id="index-manager-heading"><Database size={19} /> {t.title}</h2>
        <div class="actions">
            <button type="button" class="primary" disabled={busy} onclick={() => onAdd('existing')}>
                <FolderOpen size={16} /> {t.add}
            </button>
            <button type="button" disabled={busy} onclick={() => onAdd('new')}>
                <Plus size={16} /> {t.create}
            </button>
        </div>
    </header>
    <div class="panes">
        <div class="entries">
            <h3>{t.indexes}</h3>
            {#if entries.length === 0}
                <p>{t.empty}</p>
            {:else}
                <ul>
                    {#each entries as entry (entry.id)}
                        <li>
                            <button type="button" aria-pressed={entry.id === selectedId} disabled={busy}
                                onclick={() => select(entry.id)}
                                ondblclick={() => onOpen(entry.id)}>
                                <span>{entry.name}</span>
                                {#if entry.id === activeId && ready}<small>{t.ready}</small>{/if}
                            </button>
                        </li>
                    {/each}
                </ul>
            {/if}
        </div>
        <div class="details">
            <h3>{selected?.name ?? t.current}</h3>
            <dl aria-label={t.details}>
                <dt>{t.location}</dt>
                <dd>{selected ? (selected.backend === 'remote' ? t.remote : selected.dataDir || t.appData) : location || t.appData}</dd>
                <dt>{t.status}</dt>
                <dd role="status">{busy ? t.opening : showingActive ? t.ready : t.closed}</dd>
                {#if selected}
                    <dt>{t.mode}</dt>
                    <dd>{i18n.t.settings.index[`mode_${selected.mode}`]}</dd>
                {/if}
                {#if showingActive && stats}
                    <dt>{t.documents}</dt><dd>{stats.doc_count.toLocaleString(i18n.lang)}</dd>
                    <dt>{t.rows}</dt><dd>{stats.total_rows.toLocaleString(i18n.lang)}</dd>
                {/if}
            </dl>
            {#if renaming && selected}
                <form onsubmit={rename}>
                    <input aria-label={t.rename} bind:value={draft} required />
                    <button type="submit">{t.save}</button>
                    <button type="button" onclick={() => renaming = false}>{t.cancel}</button>
                </form>
            {:else}
                <div class="actions">
                    {#if showingActive}
                        <button type="button" class="primary" disabled={busy} onclick={onSearch}><Search size={15} /> {t.search}</button>
                    {:else if selected}
                        <button type="button" class="primary" disabled={busy} onclick={() => selected && onOpen(selected.id)}><FolderOpen size={15} /> {t.open}</button>
                    {/if}
                    {#if selected}
                        <button type="button" disabled={busy} onclick={() => { draft = selected?.name ?? ''; renaming = true; }}>
                            <Pencil size={14} /> {t.rename}
                        </button>
                        <button type="button" disabled={busy} onclick={() => selected && onRemove(selected.id)}>
                            <Trash2 size={14} /> {t.remove}
                        </button>
                    {/if}
                </div>
            {/if}
        </div>
    </div>
</section>

<style>
    .manager { border: 1px solid #3f3f46; background: #18181b; border-radius: 10px; overflow: hidden; margin-bottom: 1rem; }
    header, .actions, h2, form { display: flex; gap: .5rem; align-items: center; flex-wrap: wrap; }
    header { justify-content: space-between; padding: .85rem 1rem; border-bottom: 1px solid #3f3f46; }
    h2, h3 { margin: 0; color: #fafafa; font-weight: 650; }
    h2 { font-size: 1rem; }
    h3 { font-size: .88rem; margin-bottom: .75rem; overflow-wrap: anywhere; }
    .panes { display: grid; grid-template-columns: minmax(10rem, 1fr) minmax(0, 2fr); }
    .entries, .details { padding: .85rem 1rem; min-width: 0; }
    .entries { border-inline-end: 1px solid #3f3f46; }
    .entries p { font-size: .82rem; color: #a1a1aa; margin: 0; }
    ul { list-style: none; padding: 0; margin: 0; max-height: 15rem; overflow: auto; }
    li + li { margin-top: .25rem; }
    button { display: inline-flex; gap: .35rem; align-items: center; border: 1px solid #52525b; border-radius: 6px; padding: .43rem .6rem; font: inherit; font-size: .78rem; background: #27272a; color: #f4f4f5; cursor: pointer; }
    button:hover:not(:disabled) { background: #3f3f46; }
    button:disabled { opacity: .55; cursor: default; }
    button.primary { background: #244462; border-color: #5678a2; }
    li button { width: 100%; text-align: start; justify-content: space-between; }
    li button[aria-pressed="true"] { background: #243754; border-color: #6585b4; }
    li span { min-width: 0; overflow-wrap: anywhere; }
    small { color: #9fddbd; }
    dl { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: .45rem .75rem; margin: 0 0 .85rem; font-size: .81rem; }
    dt { color: #a1a1aa; }
    dd { margin: 0; color: #e4e4e7; overflow-wrap: anywhere; }
    input { max-width: 100%; padding: .35rem; background: #09090b; color: #fafafa; border: 1px solid #52525b; border-radius: 5px; }
    @media (max-width: 680px) {
        .panes { grid-template-columns: minmax(0, 1fr); }
        .entries { border-inline-end: 0; border-bottom: 1px solid #3f3f46; }
    }
</style>
