<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { openPath } from '@tauri-apps/plugin-opener';
    import {
        Search, X, ChevronDown, ChevronRight,
        SlidersHorizontal, ExternalLink, Loader2,
        FileText, FolderOpen
    } from 'lucide-svelte';

    // ── Types ──────────────────────────────────────────────────────────────────

    interface SearchResult {
        doc_id:       string;
        chunk_index:  number;
        score:        number;
        title?:       string;
        author?:      string;
        year?:        number;
        filename?:    string;
        ext?:         string;
        language?:    string;
        location_uri: string;
        snippet:      string;   // backend sends "snippet", not "full_text"
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
        </div>
    {/if}

    <!-- ── Results ────────────────────────────────────────────────────────────── -->
    <div class="results-area">
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
</div>

<style>
    .search-root {
        display: flex; flex-direction: column; height: 100%;
        background: #09090b; color: #fafafa; padding: 20px;
        box-sizing: border-box; gap: 12px; overflow: hidden;
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
