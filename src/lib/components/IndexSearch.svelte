<script lang="ts">
    import { invoke, convertFileSrc } from '@tauri-apps/api/core';
    import { openPath } from '@tauri-apps/plugin-opener';
    import { readTextFile } from '@tauri-apps/plugin-fs';
    import { onMount } from 'svelte';
    import { getSetting, saveSetting } from '$lib/store';
    import {
        Search, X, ChevronDown, ChevronRight,
        SlidersHorizontal, ExternalLink, Loader2,
        FileText, FolderOpen, HardDrive, Eye, Bookmark, BookmarkPlus, Trash2
    } from 'lucide-svelte';

    // Strip path → bare catalog filename for the badge label.
    function catalogName(path: string): string {
        return path.split(/[\\/]/).pop()?.replace(/\.caf$/i, '') ?? path;
    }

    // Convert a `crisp+local://user@machine/path` URI back to a plain
    // filesystem path (catalog hits already use plain paths). Returns
    // `null` for non-local URIs (vps / internxt) — those can't preview.
    function uriToPath(uri: string): string | null {
        if (uri.startsWith('crisp+local://')) {
            const rest = uri.slice('crisp+local://'.length);
            const slashIdx = rest.indexOf('/');
            if (slashIdx === -1) return null;
            return rest.slice(slashIdx);
        }
        // Catalog rows store plain paths in `location_uri`.
        if (uri.startsWith('/') || /^[A-Za-z]:[\\/]/.test(uri)) return uri;
        return null;
    }

    // ── Types ──────────────────────────────────────────────────────────────────

    interface SearchResult {
        doc_id:          string;
        chunk_index:     number;
        score:           number;
        title?:          string;
        author?:         string;
        year?:           number;
        filename?:       string;
        ext?:            string;
        language?:       string;
        location_uri:    string;
        snippet:         string;   // backend sends "snippet", not "full_text"
        // PLAN P6 4c / P7.1: when set, this hit came from the catalog
        // table (a substring filename match across active .caf-derived
        // catalogs) rather than the documents table. The path here is
        // the .caf this row was materialised from.
        catalog_source?: string;
    }

    // ── State ──────────────────────────────────────────────────────────────────

    let query       = $state('');
    let mode        = $state<'hybrid' | 'text' | 'vector'>('hybrid');
    let limit       = $state(20);
    let results     = $state<SearchResult[]>([]);
    let loading     = $state(false);
    let error       = $state('');
    let searched    = $state(false);
    let showFilters = $state(false);
    let expanded    = $state<Set<string>>(new Set());
    // PLAN P7.6 follow-up — when on (default), backend hides results
    // pinned to currently-unmounted volumes. Toggle off to show
    // everything regardless of mount state.
    let includeUnmounted = $state(false);

    // ── Preview pane (PLAN P7.3) ───────────────────────────────────────────────
    // Right-side slide-in pane that shows the matched document in place
    // so users can verify the hit without leaving the result list.
    // PDF / image: tauri.convertFileSrc into native <object>/<img>.
    // Text / markdown: readTextFile into a <pre>.
    // Anything else: "Open in app" fallback.
    let previewing      = $state<SearchResult | null>(null);
    let previewKind     = $state<'pdf' | 'image' | 'text' | 'unsupported'>('unsupported');
    let previewSrc      = $state('');           // file URL for pdf/image
    let previewText     = $state('');           // file contents for text
    let previewLoading  = $state(false);
    let previewError    = $state('');

    const TEXT_EXTS = new Set(['txt', 'md', 'markdown', 'rst', 'log',
        'csv', 'tsv', 'json', 'jsonl', 'yaml', 'yml', 'toml', 'xml', 'html',
        'rs', 'py', 'js', 'ts', 'svelte', 'go', 'java', 'c', 'cpp', 'h', 'hpp',
        'sh', 'bash', 'zsh']);
    const IMAGE_EXTS = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'avif',
        'bmp', 'svg', 'ico']);

    async function openPreview(r: SearchResult) {
        // Toggle off if clicking the same row.
        if (previewing && previewing.doc_id === r.doc_id) {
            closePreview();
            return;
        }
        const path = uriToPath(r.location_uri);
        if (!path) {
            previewing = r;
            previewKind = 'unsupported';
            previewError = 'No local path for this result (remote location)';
            return;
        }
        previewing = r;
        previewLoading = true;
        previewError = '';
        previewSrc = '';
        previewText = '';
        const ext = (r.ext ?? path.split('.').pop() ?? '').toLowerCase();
        if (ext === 'pdf') {
            previewKind = 'pdf';
            previewSrc = convertFileSrc(path);
        } else if (IMAGE_EXTS.has(ext)) {
            previewKind = 'image';
            previewSrc = convertFileSrc(path);
        } else if (TEXT_EXTS.has(ext)) {
            previewKind = 'text';
            try {
                // Cap at ~512 KB to avoid choking the DOM on huge logs.
                const raw = await readTextFile(path);
                previewText = raw.length > 512 * 1024
                    ? raw.slice(0, 512 * 1024) + '\n\n…(truncated; file is larger than 512 KB)'
                    : raw;
            } catch (e: any) {
                previewError = `read failed: ${e?.message ?? e}`;
            }
        } else {
            previewKind = 'unsupported';
        }
        previewLoading = false;
    }

    function closePreview() {
        previewing = null;
        previewSrc = '';
        previewText = '';
        previewError = '';
    }

    // ── Saved searches (PLAN P7.5) ────────────────────────────────────────────
    // Persist (name, query, mode, limit) tuples in tauri-plugin-store under
    // `savedSearches`. Click → load into the query bar + run. Lightweight
    // first cut — no per-saved-search filter persistence yet (filters reset
    // to defaults on load); that's the next iteration once the filter shape
    // settles.

    interface SavedSearch {
        name: string;
        query: string;
        mode: 'hybrid' | 'text' | 'vector';
        limit: number;
        savedAt: number; // unix ms
    }

    let savedSearches = $state<SavedSearch[]>([]);
    let showSavedDropdown = $state(false);

    onMount(async () => {
        const stored = (await getSetting('savedSearches', null)) as SavedSearch[] | null;
        savedSearches = stored ?? [];
    });

    async function persistSavedSearches() {
        await saveSetting('savedSearches', savedSearches);
    }

    async function saveCurrentSearch() {
        const q = query.trim();
        if (!q) return;
        // Default name = first 40 chars of the query, or user-edited later.
        const name = q.length > 40 ? q.slice(0, 37) + '…' : q;
        // Dedup on (name, query) — re-saving the same search updates timestamp.
        const idx = savedSearches.findIndex(s => s.name === name && s.query === q);
        const entry: SavedSearch = {
            name, query: q, mode, limit, savedAt: Date.now(),
        };
        if (idx >= 0) {
            savedSearches[idx] = entry;
        } else {
            savedSearches = [...savedSearches, entry];
        }
        await persistSavedSearches();
    }

    async function loadSavedSearch(s: SavedSearch) {
        query = s.query;
        mode = s.mode;
        limit = s.limit;
        showSavedDropdown = false;
        await runSearch();
    }

    async function deleteSavedSearch(idx: number) {
        savedSearches = savedSearches.filter((_, i) => i !== idx);
        await persistSavedSearches();
    }

    // Group results by doc_id
    const grouped = $derived.by(() => {
        const map = new Map<string, SearchResult[]>();
        for (const r of results) {
            if (!map.has(r.doc_id)) map.set(r.doc_id, []);
            map.get(r.doc_id)!.push(r);
        }
        return Array.from(map.entries())
            .map(([doc_id, chunks]) => ({
                doc_id,
                best:   chunks.sort((a, b) => b.score - a.score)[0],
                chunks: chunks.sort((a, b) => a.chunk_index - b.chunk_index),
            }))
            .sort((a, b) => b.best.score - a.best.score);
    });

    // ── Search ─────────────────────────────────────────────────────────────────

    async function runSearch() {
        if (!query.trim()) return;
        loading  = true;
        error    = '';
        searched = true;
        try {
            results = await invoke<SearchResult[]>('index_search', {
                query: query.trim(),
                mode,
                limit,
                ownerId: null,
                includeUnmounted,
            });
        } catch (e: any) {
            error   = String(e);
            results = [];
        } finally {
            loading = false;
        }
    }

    function onKeydown(e: KeyboardEvent) {
        if (e.key === 'Enter') runSearch();
    }

    function toggleExpand(doc_id: string) {
        const next = new Set(expanded);
        if (next.has(doc_id)) next.delete(doc_id);
        else next.add(doc_id);
        expanded = next;
    }

    function clearSearch() {
        query    = '';
        results  = [];
        searched = false;
        error    = '';
    }

    async function openFile(uri: string) {
        // uri may be: absolute path, crisp+local://... URI, or similar
        let path = uri;
        // Strip crisp+local:// scheme — everything after the scheme+authority is the path
        if (uri.startsWith('crisp+local://')) {
            // crisp+local://<authority><path>  — authority ends at second "/"
            const afterScheme = uri.slice('crisp+local://'.length);
            const slashIdx = afterScheme.indexOf('/');
            path = slashIdx >= 0 ? afterScheme.slice(slashIdx) : afterScheme;
        }
        try {
            await openPath(path);
        } catch (e) {
            console.error('[IndexSearch] openPath failed:', e, 'uri:', uri, 'path:', path);
            error = `Konnte Datei nicht öffnen: ${path}`;
        }
    }

    function highlightSnippet(text: string, q: string): string {
        if (!text) return '';
        const words = q.trim()
            .split(/\s+/)
            .filter(w => w.length > 2)
            .map(w => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
        if (!words.length) return escHtml(text);
        const re = new RegExp(`(${words.join('|')})`, 'gi');
        return escHtml(text).replace(re, '<mark>$1</mark>');
    }

    function escHtml(s: string): string {
        return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
    }

    function scoreBarPct(score: number): number {
        // RRF scores ~0.008–0.04; vector scores 0–1 (cosine similarity)
        // Normalise heuristically for display
        return Math.min(100, Math.round(score * 2500));
    }
</script>

<div class="search-root">
    <!-- ── Query bar ─────────────────────────────────────────────────────────── -->
    <div class="query-bar">
        <div class="query-input-wrap">
            <Search size={16} class="query-icon" />
            <!-- svelte-ignore a11y_autofocus -->
            <input
                class="query-input"
                type="text"
                bind:value={query}
                onkeydown={onKeydown}
                placeholder="Suche in indizierten Dokumenten …"
                title={'Operatoren: AND OR NOT, "Phrase", w/N (proximity), pre/N (ordered), foo* (wildcard), foo~2 (fuzzy), title:karl, headings:foo, body:foo, (Klammern). Großschreibung der Operatoren beliebig.'}
                autofocus
            />
            {#if query}
                <button class="clear-btn" onclick={clearSearch} title="Löschen"><X size={14} /></button>
            {/if}
        </div>
        <button class="search-btn" onclick={runSearch} disabled={loading || !query.trim()}>
            {#if loading}<Loader2 size={15} class="spin" />{:else}<Search size={15} />{/if}
            Suchen
        </button>
        <button
            class="filter-toggle"
            onclick={saveCurrentSearch}
            disabled={!query.trim()}
            title="Save this search"
        >
            <BookmarkPlus size={15} />
        </button>
        <div class="saved-wrap">
            <button
                class="filter-toggle"
                class:active={showSavedDropdown}
                onclick={() => showSavedDropdown = !showSavedDropdown}
                disabled={savedSearches.length === 0}
                title="Saved searches ({savedSearches.length})"
            >
                <Bookmark size={15} />
                {#if savedSearches.length > 0}
                    <span class="saved-count">{savedSearches.length}</span>
                {/if}
            </button>
            {#if showSavedDropdown && savedSearches.length > 0}
                <div class="saved-dropdown">
                    {#each savedSearches as s, i (s.name + s.savedAt)}
                        <div class="saved-item">
                            <button
                                class="saved-name"
                                onclick={() => loadSavedSearch(s)}
                                title={s.query}
                            >
                                {s.name}
                                <span class="saved-meta">
                                    {s.mode} · {s.limit}
                                </span>
                            </button>
                            <button
                                class="saved-del"
                                onclick={() => deleteSavedSearch(i)}
                                title="Delete saved search"
                            >
                                <Trash2 size={12} />
                            </button>
                        </div>
                    {/each}
                </div>
            {/if}
        </div>
        <button class="filter-toggle" class:active={showFilters}
            onclick={() => showFilters = !showFilters} title="Optionen">
            <SlidersHorizontal size={15} />
        </button>
    </div>

    <!-- ── Mode chips ─────────────────────────────────────────────────────────── -->
    <div class="mode-row">
        {#each (['hybrid', 'text', 'vector'] as const) as m}
            <button class="mode-chip" class:active={mode === m} onclick={() => mode = m}>
                { m === 'hybrid' ? 'Hybrid (RRF)' : m === 'text' ? 'Volltext (BM25)' : 'Vektor (ANN)' }
            </button>
        {/each}
    </div>

    <!-- ── Filters ─────────────────────────────────────────────────────────────── -->
    {#if showFilters}
        <div class="filter-row">
            <label class="filter-field">
                <span>Max. Ergebnisse</span>
                <select bind:value={limit}>
                    {#each [10, 20, 50, 100] as n}<option value={n}>{n}</option>{/each}
                </select>
            </label>
            <label class="filter-field" title="Include hits from drives that aren't currently mounted">
                <input type="checkbox" bind:checked={includeUnmounted} />
                <span>Inkl. nicht eingehängter Laufwerke</span>
            </label>
        </div>
    {/if}

    <!-- ── Results ────────────────────────────────────────────────────────────── -->
    <div class="results-and-preview">
    <div class="results-area" class:with-preview={previewing !== null}>
        {#if loading}
            <div class="state-msg"><Loader2 size={22} class="spin" /> Suche läuft …</div>

        {:else if error}
            <div class="state-msg error">
                {error}
                {#if error.includes('not initialised') || error.includes('disabled')}
                    <br /><small>Bitte den Index in Einstellungen → Search Index aktivieren und initialisieren.</small>
                {/if}
            </div>

        {:else if searched && grouped.length === 0}
            <div class="state-msg">Keine Ergebnisse für „{query}"</div>

        {:else if !searched}
            <div class="state-hint">
                <Search size={32} style="color:#3f3f46;" />
                <p>Suchbegriff eingeben und Enter drücken</p>
                <p class="hint-sub">Hybrid-Modus kombiniert Volltext (BM25) und Vektoren (ANN) über RRF</p>
            </div>

        {:else}
            <div class="result-count">{grouped.length} Dokument{grouped.length !== 1 ? 'e' : ''} · {results.length} Treffer</div>

            {#each grouped as group (group.doc_id)}
                {@const r = group.best}
                <div class="result-card">
                    <!-- Doc header -->
                    <div class="result-header"
                        role="button" tabindex="0"
                        onclick={() => toggleExpand(group.doc_id)}
                        onkeydown={e => e.key === 'Enter' && toggleExpand(group.doc_id)}>

                        <div class="ext-badge ext-{(r.ext ?? '').toLowerCase()}">{(r.ext ?? '?').toUpperCase()}</div>

                        <div class="result-meta">
                            <span class="result-title">
                                {r.title || r.filename || r.doc_id.slice(0, 20) + '…'}
                            </span>
                            {#if r.author || r.year}
                                <span class="result-byline">{r.author ?? ''}{r.author && r.year ? ' · ' : ''}{r.year ?? ''}</span>
                            {/if}
                            {#if r.catalog_source}
                                <span class="catalog-badge" title="Match from catalog: {r.catalog_source}">
                                    <HardDrive size={11} />
                                    catalog: {catalogName(r.catalog_source)}
                                </span>
                            {/if}
                            <span class="result-path" title={r.location_uri}>{r.filename ?? r.location_uri}</span>
                        </div>

                        <div class="result-right">
                            <div class="score-wrap" title="Score {r.score.toFixed(4)}">
                                <div class="score-bar"><div class="score-fill" style="width:{scoreBarPct(r.score)}%"></div></div>
                                <span class="score-label">{r.score.toFixed(3)}</span>
                            </div>
                            {#if group.chunks.length > 1}
                                <span class="chunk-count">{group.chunks.length} Chunks</span>
                            {/if}
                            <button class="open-btn"
                                class:active={previewing && previewing.doc_id === group.doc_id}
                                onclick={(e) => { e.stopPropagation(); openPreview(r); }}
                                title="Vorschau (Preview)">
                                <Eye size={13} />
                            </button>
                            <button class="open-btn"
                                onclick={(e) => { e.stopPropagation(); openFile(r.location_uri); }}
                                title="Datei öffnen">
                                <ExternalLink size={13} />
                            </button>
                            {#if group.chunks.length > 1}
                                {#if expanded.has(group.doc_id)}<ChevronDown size={14} />{:else}<ChevronRight size={14} />{/if}
                            {/if}
                        </div>
                    </div>

                    <!-- Best chunk snippet with highlighted terms -->
                    {#if r.snippet}
                        <div class="chunk-preview">
                            <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                            {@html highlightSnippet(r.snippet, query)}{#if r.snippet.length >= 400}…{/if}
                        </div>
                    {:else}
                        <div class="chunk-preview no-snippet">(Kein Textauszug verfügbar)</div>
                    {/if}

                    <!-- Expanded: all chunks -->
                    {#if expanded.has(group.doc_id) && group.chunks.length > 1}
                        <div class="chunk-list">
                            {#each group.chunks as chunk (chunk.chunk_index)}
                                <div class="chunk-item">
                                    <span class="chunk-idx">#{chunk.chunk_index + 1}</span>
                                    <div class="chunk-text">
                                        {#if chunk.snippet}
                                            <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                                            {@html highlightSnippet(chunk.snippet.slice(0, 280), query)}{#if chunk.snippet.length > 280}…{/if}
                                        {:else}
                                            <span style="color:#52525b">(leer)</span>
                                        {/if}
                                    </div>
                                    <span class="chunk-score">{chunk.score.toFixed(3)}</span>
                                </div>
                            {/each}
                        </div>
                    {/if}
                </div>
            {/each}
        {/if}
    </div>

    {#if previewing}
        <aside class="preview-pane">
            <header class="preview-header">
                <span class="preview-title" title={previewing.location_uri}>
                    {previewing.title || previewing.filename || (previewing.doc_id.slice(0, 24) + '…')}
                </span>
                <button class="preview-close" onclick={closePreview} title="Vorschau schließen">
                    <X size={14} />
                </button>
            </header>
            <div class="preview-body">
                {#if previewLoading}
                    <div class="state-msg"><Loader2 size={20} class="spin" /> Loading…</div>
                {:else if previewError}
                    <div class="state-msg error">{previewError}</div>
                {:else if previewKind === 'pdf'}
                    <object data={previewSrc} type="application/pdf" width="100%" height="100%" aria-label="PDF preview of {previewing.title || previewing.filename || 'document'}">
                        <p>PDF preview not supported by your webview.
                            <button class="open-btn" onclick={() => openFile(previewing!.location_uri)}>Open in app</button>
                        </p>
                    </object>
                {:else if previewKind === 'image'}
                    <img src={previewSrc} alt={previewing.filename ?? ''} class="preview-image" />
                {:else if previewKind === 'text'}
                    <pre class="preview-text">{previewText}</pre>
                {:else}
                    <div class="state-msg">
                        Preview not supported for this file type.
                        <br />
                        <button class="open-btn" onclick={() => openFile(previewing!.location_uri)}>
                            <ExternalLink size={13} /> Open in app
                        </button>
                    </div>
                {/if}
            </div>
        </aside>
    {/if}
    </div>
</div>

<style>
    .search-root {
        display: flex; flex-direction: column; height: 100%;
        background: #09090b; color: #fafafa; padding: 20px;
        box-sizing: border-box; gap: 12px; overflow: hidden;
    }

    /* PLAN P7.5 — saved-searches bookmark dropdown */
    .saved-wrap { position: relative; display: inline-flex; }
    .saved-count {
        margin-left: 4px;
        background: #3b82f6;
        color: white;
        font-size: 0.65rem;
        font-weight: 600;
        padding: 0 5px;
        border-radius: 8px;
        line-height: 1.5;
    }
    .saved-dropdown {
        position: absolute;
        top: calc(100% + 4px);
        right: 0;
        z-index: 20;
        min-width: 280px;
        max-width: 420px;
        max-height: 320px;
        overflow-y: auto;
        background: #18181b;
        border: 1px solid #3f3f46;
        border-radius: 6px;
        box-shadow: 0 8px 24px rgba(0,0,0,0.4);
        padding: 4px;
    }
    .saved-item {
        display: flex;
        align-items: center;
        gap: 4px;
        padding: 4px;
    }
    .saved-item:hover { background: #27272a; border-radius: 4px; }
    .saved-name {
        flex: 1;
        background: none;
        border: none;
        color: #fafafa;
        text-align: left;
        cursor: pointer;
        padding: 4px 6px;
        border-radius: 4px;
        font-size: 0.8rem;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }
    .saved-meta {
        display: block;
        font-size: 0.65rem;
        color: #71717a;
        text-transform: uppercase;
    }
    .saved-del {
        background: none;
        border: none;
        cursor: pointer;
        color: #71717a;
        padding: 4px;
    }
    .saved-del:hover { color: #ef4444; }

    /* PLAN P7.3 — live preview pane. Slides in from the right when a
       result row's eye-icon is clicked; results-area shrinks to share
       the viewport. PDF / image render natively via tauri.convertFileSrc;
       text reads the file via the fs plugin. */
    .results-and-preview {
        display: flex;
        flex: 1;
        gap: 12px;
        overflow: hidden;
        min-height: 0;
    }
    .results-area.with-preview { flex: 1; min-width: 0; }
    .preview-pane {
        flex: 1;
        max-width: 50%;
        min-width: 360px;
        display: flex;
        flex-direction: column;
        background: #18181b;
        border: 1px solid #3f3f46;
        border-radius: 8px;
        overflow: hidden;
    }
    .preview-header {
        display: flex; align-items: center; justify-content: space-between;
        padding: 8px 12px;
        background: #27272a;
        border-bottom: 1px solid #3f3f46;
        font-size: 0.85rem;
    }
    .preview-title {
        flex: 1;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: #fafafa;
        font-weight: 600;
        margin-right: 8px;
    }
    .preview-close {
        background: none; border: none; cursor: pointer;
        color: #a1a1aa; padding: 2px;
    }
    .preview-close:hover { color: #fafafa; }
    .preview-body {
        flex: 1;
        overflow: auto;
        background: #0a0a0c;
    }
    .preview-body object { display: block; width: 100%; height: 100%; border: 0; }
    .preview-image {
        max-width: 100%;
        max-height: 100%;
        display: block;
        margin: 0 auto;
    }
    .preview-text {
        margin: 0;
        padding: 12px;
        font-family: var(--mono, ui-monospace, monospace);
        font-size: 0.78rem;
        line-height: 1.4;
        color: #d4d4d8;
        white-space: pre-wrap;
        word-break: break-word;
    }

    /* PLAN P6 4c / P7.1 — catalog channel hits get a small inline pill
       sandwiched between byline and path so the source is obvious at a
       glance without crowding the row. */
    .catalog-badge {
        display: inline-flex;
        align-items: center;
        gap: 3px;
        padding: 1px 6px;
        margin: 0 6px;
        background: rgba(59, 130, 246, 0.15);
        color: #93c5fd;
        border: 1px solid rgba(59, 130, 246, 0.4);
        border-radius: 10px;
        font-size: 0.7rem;
        font-weight: 500;
    }

    .query-bar { display: flex; gap: 8px; align-items: center; }
    .query-input-wrap {
        flex: 1; display: flex; align-items: center; gap: 8px;
        background: #18181b; border: 1px solid #3f3f46; border-radius: 8px; padding: 0 10px;
        transition: border-color 0.2s;
    }
    .query-input-wrap:focus-within { border-color: #3b82f6; }
    :global(.query-icon) { color: #71717a; flex-shrink: 0; }
    .query-input {
        flex: 1; background: transparent; border: none; outline: none;
        color: #fafafa; font-size: 0.9375rem; padding: 10px 0;
    }
    .query-input::placeholder { color: #52525b; }
    .clear-btn { background: none; border: none; color: #71717a; cursor: pointer; padding: 4px; }
    .clear-btn:hover { color: white; }

    .search-btn {
        display: flex; align-items: center; gap: 6px; padding: 9px 16px;
        background: #3b82f6; color: white; border: none; border-radius: 8px;
        cursor: pointer; font-weight: 600; font-size: 0.875rem; white-space: nowrap;
    }
    .search-btn:hover:not(:disabled) { background: #2563eb; }
    .search-btn:disabled { opacity: 0.4; cursor: not-allowed; }

    .filter-toggle {
        padding: 9px; background: #18181b; border: 1px solid #3f3f46;
        border-radius: 8px; color: #71717a; cursor: pointer; transition: all 0.15s;
    }
    .filter-toggle:hover, .filter-toggle.active { background: #27272a; color: white; border-color: #3b82f6; }

    .mode-row { display: flex; gap: 6px; }
    .mode-chip {
        padding: 5px 12px; border-radius: 99px; border: 1px solid #3f3f46;
        background: transparent; color: #a1a1aa; cursor: pointer; font-size: 0.8rem; font-weight: 500;
    }
    .mode-chip:hover { border-color: #71717a; color: white; }
    .mode-chip.active { background: #3b82f622; border-color: #3b82f6; color: #93c5fd; }

    .filter-row {
        display: flex; gap: 12px; flex-wrap: wrap; background: #18181b;
        border: 1px solid #27272a; border-radius: 8px; padding: 10px 14px;
    }
    .filter-field { display: flex; flex-direction: column; gap: 4px; }
    .filter-field span { font-size: 0.72rem; color: #71717a; font-weight: 600; text-transform: uppercase; }
    .filter-field select {
        background: #09090b; border: 1px solid #27272a; border-radius: 5px;
        color: white; padding: 5px 8px; font-size: 0.8125rem;
    }

    .results-area { flex: 1; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }

    .state-msg { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; color: #71717a; text-align: center; }
    .state-msg.error { color: #f87171; }
    .state-hint { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; color: #52525b; }
    .hint-sub { font-size: 0.8rem; color: #3f3f46; }
    .result-count { font-size: 0.75rem; color: #71717a; }

    .result-card {
        background: #18181b; border: 1px solid #27272a; border-radius: 8px; overflow: hidden;
    }
    .result-card:hover { border-color: #3f3f46; }

    .result-header {
        display: flex; align-items: flex-start; gap: 10px;
        padding: 12px 14px; cursor: pointer; user-select: none;
    }
    .result-header:hover { background: #1c1c1f; }

    .ext-badge {
        font-size: 0.6rem; font-weight: 800; padding: 3px 5px; border-radius: 4px;
        background: #27272a; color: #a1a1aa; flex-shrink: 0; min-width: 32px;
        text-align: center; margin-top: 2px;
    }
    .ext-pdf  { background: #7f1d1d33; color: #fca5a5; }
    .ext-docx { background: #1e3a5f33; color: #93c5fd; }
    .ext-md   { background: #14532d33; color: #86efac; }
    .ext-txt  { background: #44403c33; color: #d6d3d1; }
    .ext-epub { background: #4c1d9533; color: #c4b5fd; }

    .result-meta { flex: 1; min-width: 0; }
    .result-title { display: block; font-size: 0.9rem; font-weight: 600; color: #e4e4e7; }
    .result-byline { display: block; font-size: 0.78rem; color: #a1a1aa; margin-top: 2px; }
    .result-path { display: block; font-size: 0.72rem; color: #52525b; margin-top: 2px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

    .result-right { display: flex; align-items: center; gap: 8px; flex-shrink: 0; }
    .score-wrap { display: flex; align-items: center; gap: 5px; }
    .score-bar { width: 40px; height: 4px; background: #27272a; border-radius: 2px; }
    .score-fill { height: 100%; background: #3b82f6; border-radius: 2px; }
    .score-label { font-size: 0.7rem; color: #71717a; font-variant-numeric: tabular-nums; }
    .chunk-count { font-size: 0.7rem; color: #52525b; white-space: nowrap; }

    .open-btn { background: none; border: none; color: #52525b; cursor: pointer; padding: 4px; border-radius: 4px; }
    .open-btn:hover { color: #3b82f6; background: #3b82f622; }

    .chunk-preview {
        padding: 8px 14px 12px 52px;
        font-size: 0.8rem; color: #a1a1aa; line-height: 1.55;
        border-top: 1px solid #1c1c1f;
    }
    .chunk-preview.no-snippet { color: #52525b; font-style: italic; }

    .chunk-list { border-top: 1px solid #27272a; }
    .chunk-item {
        display: flex; gap: 10px; padding: 8px 14px;
        border-bottom: 1px solid #1c1c1f; align-items: flex-start;
    }
    .chunk-item:last-child { border-bottom: none; }
    .chunk-idx { font-size: 0.7rem; color: #52525b; flex-shrink: 0; width: 24px; padding-top: 2px; }
    .chunk-text { flex: 1; font-size: 0.78rem; color: #a1a1aa; line-height: 1.45; }
    .chunk-score { font-size: 0.7rem; color: #52525b; flex-shrink: 0; }

    :global(mark) { background: #854d0e55; color: #fbbf24; border-radius: 2px; padding: 0 1px; }
    :global(.spin) { animation: spin 1s linear infinite; display: inline-flex; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
