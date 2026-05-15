<script lang="ts">
    import { invoke, convertFileSrc } from '@tauri-apps/api/core';
    import { openPath } from '@tauri-apps/plugin-opener';
    import { readTextFile } from '@tauri-apps/plugin-fs';
    import { save } from '@tauri-apps/plugin-dialog';
    import { onMount } from 'svelte';
    import { getSetting, saveSetting } from '$lib/store';
    import { AUDIO_EXTENSIONS } from '$lib/extractors/index';
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
        // P13.5 batch translation — backend populates these when the
        // doc was ingested with `IndexConfig.translate_to` set.
        // Frontend uses them as the no-network-round-trip cache for
        // the "Translate to …" button: if the target matches, just
        // show the existing column without re-invoking m2m100.
        text_translated?:      string;
        text_translated_lang?: string;
    }

    // P13.5 on-demand translation surface — per-result state tracking
    // the lifecycle of a "Translate to en" click.  Keyed by
    // `${doc_id}:${chunk_index}` so each chunk in an expanded result
    // group gets its own state.
    interface TranslateState {
        loading:         boolean;
        error?:          string;
        translated_text?: string;
        source_lang?:    string;
        target_lang?:    string;
        backend?:        string;
        cached?:         boolean;
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

    // P13.5 on-demand translation — per-result Map keyed by
    // `${doc_id}:${chunk_index}`.  Kept in Map state rather than
    // per-row signals so a search-state reset (new query) clears
    // every translation in one assignment.
    let translateTargetLang = $state<string>('en');
    let translations        = $state<Map<string, TranslateState>>(new Map());

    // P13.7 Stage M — remote search via /api/v2/index/search.
    // Held separately from `results` so the local + remote panes
    // stay independently visible; clicking "Search remote" doesn't
    // wipe the local pane, and the user can flip between them
    // without re-running.
    interface RemoteHit {
        doc_id:         string;
        sha256:         string;
        path?:          string | null;
        filename?:      string | null;
        title?:         string | null;
        author?:        string | null;
        year?:          number | null;
        ext?:           string | null;
        language?:      string | null;
        full_text?:     string | null;
        indexed_at:     number;
        score:          number;
        score_text?:    number | null;
        score_vector?:  number | null;
        collection_id?: string | null;
    }
    let remoteResults  = $state<RemoteHit[]>([]);
    let remoteLoading  = $state(false);
    let remoteError    = $state('');
    let remoteSearched = $state(false);
    let remoteUsedText   = $state(false);
    let remoteUsedVector = $state(false);
    let remoteShards     = $state(0);
    // Per-row download state (sha → state).
    interface RemoteDownloadState {
        downloading: boolean;
        bytes?: number;
        dest?: string;
        error?: string;
    }
    let remoteDownloads = $state<Map<string, RemoteDownloadState>>(new Map());

    // Stage S — federated search state.
    interface FederatedHit {
        id: string;
        source: 'local' | 'cloud_backup' | 'crisplens' | string;
        score: number;
        rrf_rank: number;
        filename?: string | null;
        path?: string | null;
        ext?: string | null;
        title?: string | null;
        author?: string | null;
        year?: number | null;
        language?: string | null;
        sha256?: string | null;
        size_bytes?: number | null;
        snippet?: string | null;
        location_uri?: string | null;
    }
    let fedBackends     = $state<string[]>(['local', 'cloud_backup', 'crisplens']);
    let fedResults      = $state<FederatedHit[]>([]);
    let fedLoading      = $state(false);
    let fedSearched     = $state(false);
    let fedErrors       = $state<Record<string, string>>({});

    const SOURCE_ICON: Record<string, string> = {
        local: '💾',
        cloud_backup: '☁',
        crisplens: '👁',
    };

    async function runFederatedSearch() {
        const q = query.trim();
        if (!q) return;
        fedLoading  = true;
        fedSearched = true;
        fedResults  = [];
        fedErrors   = {};
        try {
            const r = await invoke<{ hits: FederatedHit[]; errors: Record<string, string> }>(
                'sync_federated_search',
                {
                    q,
                    limit,
                    backends: fedBackends.join(','),
                },
            );
            fedResults = r.hits ?? [];
            fedErrors  = r.errors ?? {};
        } catch (e: any) {
            fedErrors = { global: String(e?.message ?? e) };
        } finally {
            fedLoading = false;
        }
    }

    // P13.6 Step 8 — "Transcribe" surface for audio rows ingested at
    // L1 (no transcript) or L2 (probe-only).  Same Map-state pattern
    // as translations; keyed by doc_id (no chunk granularity since
    // promote operates at the doc level).
    interface TranscribeState {
        loading: boolean;
        done?: boolean;
        chunks?: number;
        error?: string;
    }
    let transcribes = $state<Map<string, TranscribeState>>(new Map());
    /** Set of audio/video extensions for O(1) lookup in template. */
    const AUDIO_EXTS_SET_SEARCH = new Set<string>(AUDIO_EXTENSIONS);
    /** Set of image extensions — kept in lockstep with
     *  `extractors::OCR_IMAGE_EXTS`.  Used to decide whether to
     *  surface the "Re-OCR" button on a row with an empty
     *  snippet (P13.7 Step 2 follow-up to the audio Transcribe
     *  button). */
    const IMAGE_EXTS_SET_SEARCH = new Set<string>([
        'png', 'jpg', 'jpeg', 'tif', 'tiff', 'bmp', 'webp',
    ]);
    /** Heuristic: does this row look like an L1/L2 audio that hasn't
     *  been transcribed yet?  We don't surface audio_* through
     *  SearchResult today (display-only L2 columns); rely on the
     *  extension + empty/short snippet as a proxy.  Conservative —
     *  a long-snippet audio hit (already transcribed) hides the
     *  button so we don't re-transcribe perfectly-good rows. */
    function looksUntranscribed(r: SearchResult): boolean {
        const ext = (r.ext ?? '').toLowerCase();
        if (!AUDIO_EXTS_SET_SEARCH.has(ext)) return false;
        const snip = (r.snippet ?? '').trim();
        return snip.length < 20;
    }
    /** Parallel heuristic for images — surfaces the "Re-OCR"
     *  button when an image row has no recognisable OCR text.
     *  Same caveats as looksUntranscribed. */
    function looksUnOcred(r: SearchResult): boolean {
        const ext = (r.ext ?? '').toLowerCase();
        if (!IMAGE_EXTS_SET_SEARCH.has(ext)) return false;
        const snip = (r.snippet ?? '').trim();
        return snip.length < 20;
    }
    async function handleTranscribe(r: SearchResult): Promise<void> {
        const key = r.doc_id;
        const next = new Map(transcribes);
        next.set(key, { loading: true });
        transcribes = next;
        try {
            const stats = await invoke<{ chunk_count: number }>('index_audio_promote_l3', {
                locationUri: r.location_uri,
            });
            const after = new Map(transcribes);
            after.set(key, { loading: false, done: true, chunks: stats.chunk_count });
            transcribes = after;
        } catch (e: any) {
            const after = new Map(transcribes);
            after.set(key, { loading: false, error: String(e?.message ?? e) });
            transcribes = after;
        }
    }

    /** P13.7 Step 2 — image L3 promote.  Shares the `transcribes`
     *  state Map because the per-row UI surface mirrors the audio
     *  one (loading / done / error transitions).  Backend command
     *  differs; user-visible label says "Re-OCR" instead of
     *  "Transcribe". */
    async function handleReOcr(r: SearchResult): Promise<void> {
        const key = r.doc_id;
        const next = new Map(transcribes);
        next.set(key, { loading: true });
        transcribes = next;
        try {
            const stats = await invoke<{ chunk_count: number }>('index_image_promote_l3', {
                locationUri: r.location_uri,
            });
            const after = new Map(transcribes);
            after.set(key, { loading: false, done: true, chunks: stats.chunk_count });
            transcribes = after;
        } catch (e: any) {
            const after = new Map(transcribes);
            after.set(key, { loading: false, error: String(e?.message ?? e) });
            transcribes = after;
        }
    }

    /** Stable key for the translations map. */
    function translationKey(r: SearchResult): string {
        return `${r.doc_id}:${r.chunk_index}`;
    }

    /** Has a translation finished + succeeded for this result + current target? */
    function hasUsableTranslation(r: SearchResult): boolean {
        const ts = translations.get(translationKey(r));
        return !!(ts && !ts.loading && !ts.error
                  && ts.translated_text
                  && ts.target_lang === translateTargetLang);
    }

    async function handleTranslate(r: SearchResult): Promise<void> {
        const key = translationKey(r);

        // Fast path — row already carries a translation in the
        // matching target language (populated at index time when
        // IndexConfig.translate_to was set).  Skip the Tauri round
        // trip; just surface the cached column.
        if (r.text_translated && r.text_translated_lang === translateTargetLang) {
            const next = new Map(translations);
            next.set(key, {
                loading: false,
                translated_text: r.text_translated,
                source_lang: r.language ?? 'unknown',
                target_lang: translateTargetLang,
                cached: true,
            });
            translations = next;
            return;
        }

        // Optimistic loading state — clears on completion or error.
        const next = new Map(translations);
        next.set(key, { loading: true });
        translations = next;

        try {
            const result = await invoke<{
                translated_text: string;
                source_lang:     string;
                target_lang:     string;
                backend:         string;
                cached:          boolean;
            }>('translate_text', {
                input: {
                    text: r.snippet,
                    // Pass the row's known language as a hint when we have
                    // it — saves an LID call on the backend.  Null = let
                    // the backend run CLD3.
                    source_lang: r.language && r.language.length === 2 ? r.language : null,
                    target_lang: translateTargetLang,
                    // mt_backend / mt_model / lid_model intentionally
                    // omitted — backend defaults (m2m100 + auto-resolve
                    // CLD3) match what the IndexConfig surface uses.
                },
            });
            const after = new Map(translations);
            after.set(key, {
                loading: false,
                translated_text: result.translated_text,
                source_lang: result.source_lang,
                target_lang: result.target_lang,
                backend: result.backend,
                cached: result.cached,
            });
            translations = after;
        } catch (e) {
            const after = new Map(translations);
            after.set(key, {
                loading: false,
                error: String(e),
            });
            translations = after;
        }
    }

    /** Clear all translation state — called when the user runs a new query. */
    function clearTranslations(): void {
        translations = new Map();
    }

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
        // P13.5 — wipe per-result translation state on every new
        // query.  Otherwise the user types a different query and
        // sees stale "Translated en" badges on rows that haven't
        // been translated yet (the Map key is doc_id:chunk_index
        // which can collide across queries).
        clearTranslations();
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

    // P13.7 Stage M — search the cloud-backup VPS over HTTPS via
    // /api/v2/index/search.  Uses the same query box; remote hits
    // surface in a separate panel below the local results.  Embed
    // text matches the query so the server runs vector inference
    // alongside FTS — true hybrid retrieval.
    async function runRemoteSearch() {
        const q = query.trim();
        if (!q) return;
        remoteLoading  = true;
        remoteError    = '';
        remoteSearched = true;
        remoteResults  = [];
        try {
            const r = await invoke<any>('sync_cb_v2_search', {
                params: {
                    q,
                    embedText:  q,        // server-side fastembed for the vector arm
                    embedModel: 'e5-large', // matches the LanceDB default-dim table
                    limit,
                    rrfK: 60,
                    filters: {},
                },
            });
            remoteResults    = (r.rows ?? []) as RemoteHit[];
            remoteUsedText   = !!r.used_text;
            remoteUsedVector = !!r.used_vector;
            remoteShards     = Number(r.shards_queried ?? 0);
        } catch (e: any) {
            remoteError = String(e);
        } finally {
            remoteLoading = false;
        }
    }

    // P13.7 Stage E + M — download a remote hit's bytes to a
    // user-picked local path via sync_cb_download_file.  Streaming
    // + sha-verified on arrival; failure removes any partial.
    async function downloadRemoteRow(hit: RemoteHit) {
        const dest = await save({
            defaultPath: hit.filename ?? hit.sha256,
            title: 'Save downloaded file',
        });
        if (!dest) return;
        // Mark the row as downloading so the button shows a spinner.
        const next = new Map(remoteDownloads);
        next.set(hit.sha256, { downloading: true });
        remoteDownloads = next;
        try {
            const r = await invoke<{ bytes: number; dest_path: string }>(
                'sync_cb_download_file',
                { sha256: hit.sha256, destPath: dest as string },
            );
            const done = new Map(remoteDownloads);
            done.set(hit.sha256, {
                downloading: false,
                bytes: r.bytes,
                dest: r.dest_path,
            });
            remoteDownloads = done;
        } catch (e: any) {
            const fail = new Map(remoteDownloads);
            fail.set(hit.sha256, {
                downloading: false,
                error: String(e),
            });
            remoteDownloads = fail;
        }
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
        <!-- P13.7 Stage M — search the cloud-backup VPS over HTTPS.
             Uses the same query string; the server runs FTS5 + a
             server-side embedding pass and RRF-fuses both arms. -->
        <button class="filter-toggle" onclick={runRemoteSearch}
                disabled={remoteLoading || !query.trim()}
                title="Search the cloud-backup VPS over HTTPS">
            {#if remoteLoading}<Loader2 size={15} class="spin" />{:else}🌐{/if}
            Cloud
        </button>
        <!-- Stage S — federated search across all backends. -->
        <button class="filter-toggle fed-btn" onclick={runFederatedSearch}
                disabled={fedLoading || !query.trim() || fedBackends.length === 0}
                title="Search all backends (local + cloud-backup + CrispLens) with RRF merge">
            {#if fedLoading}<Loader2 size={15} class="spin" />{:else}🔀{/if}
            Alle
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
            <!-- P13.5 on-demand translation — target language for the
                 per-result "Translate to …" button.  m2m100 covers
                 100 langs any-to-any; this dropdown surfaces the
                 commonly-used subset (matches the index-time options
                 in Settings → Search Index → Index-time translation). -->
            <label class="filter-field" title="Target language for the per-result Translate button (m2m100 via on-demand backend)">
                <span>Translate to</span>
                <select bind:value={translateTargetLang}>
                    <option value="en">en — English</option>
                    <option value="de">de — Deutsch</option>
                    <option value="fr">fr — Français</option>
                    <option value="es">es — Español</option>
                    <option value="it">it — Italiano</option>
                    <option value="ja">ja — 日本語</option>
                    <option value="zh">zh — 中文</option>
                </select>
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

                    <!-- P13.5 on-demand translation surface.  Renders three
                         states in sequence: idle (button), loading
                         (spinner-text), finished (inline translation +
                         badge).  Skipped when the snippet itself is empty
                         (no text to translate).  Hidden when the result
                         is in the user's target language already (no
                         translation needed). -->
                    <!-- P13.7 Step 2: Re-OCR surface for image
                         rows ingested at L1/L2.  Same Map-state +
                         transition shape as the Transcribe surface
                         below; the underlying Tauri command differs
                         (`index_image_promote_l3`) but the UX is
                         identical. -->
                    {#if looksUnOcred(r)}
                        {@const tx = transcribes.get(r.doc_id)}
                        <div class="translate-surface" style="margin-top:6px;">
                            {#if !tx}
                                <button
                                    type="button"
                                    class="translate-btn"
                                    onclick={() => handleReOcr(r)}
                                    title="Run OCR + EXIF probe on this image (image L3 promote)"
                                >
                                    Re-OCR (.{r.ext})
                                </button>
                            {:else if tx.loading}
                                <div class="translate-loading">
                                    <span class="translate-spinner" aria-hidden="true"></span>
                                    Running OCR…
                                </div>
                            {:else if tx.error}
                                <div class="translate-error">
                                    Re-OCR failed: {tx.error}
                                    <button
                                        type="button"
                                        class="translate-retry"
                                        onclick={() => handleReOcr(r)}
                                    >
                                        Retry
                                    </button>
                                </div>
                            {:else if tx.done}
                                <div class="translate-result">
                                    <div class="translate-meta">
                                        <span class="translate-cached">OCR'd → {tx.chunks ?? 0} chunks. Re-run the search to see the new content.</span>
                                    </div>
                                </div>
                            {/if}
                        </div>
                    {/if}

                    <!-- P13.6 Step 8: Transcribe surface for audio
                         rows that look untranscribed (L1 or L2).
                         Disappears once the promote returns; the
                         search will need to be re-run to see the
                         new transcript. -->
                    {#if looksUntranscribed(r)}
                        {@const tx = transcribes.get(r.doc_id)}
                        <div class="translate-surface" style="margin-top:6px;">
                            {#if !tx}
                                <button
                                    type="button"
                                    class="translate-btn"
                                    onclick={() => handleTranscribe(r)}
                                    title="Run CrispASR transcription on this audio/video file"
                                >
                                    Transcribe (.{r.ext})
                                </button>
                            {:else if tx.loading}
                                <div class="translate-loading">
                                    <span class="translate-spinner" aria-hidden="true"></span>
                                    Transcribing… (this can take a few minutes)
                                </div>
                            {:else if tx.error}
                                <div class="translate-error">
                                    Transcribe failed: {tx.error}
                                    <button
                                        type="button"
                                        class="translate-retry"
                                        onclick={() => handleTranscribe(r)}
                                    >
                                        Retry
                                    </button>
                                </div>
                            {:else if tx.done}
                                <div class="translate-result">
                                    <div class="translate-meta">
                                        <span class="translate-cached">Transcribed → {tx.chunks ?? 0} chunks. Re-run the search to see the new content.</span>
                                    </div>
                                </div>
                            {/if}
                        </div>
                    {/if}

                    {#if r.snippet && r.language !== translateTargetLang}
                        {@const tkey = translationKey(r)}
                        {@const ts = translations.get(tkey)}
                        <div class="translate-surface">
                            {#if !ts}
                                <button
                                    type="button"
                                    class="translate-btn"
                                    onclick={() => handleTranslate(r)}
                                    title="Translate this snippet to {translateTargetLang} via m2m100"
                                >
                                    Translate to {translateTargetLang}
                                </button>
                            {:else if ts.loading}
                                <div class="translate-loading">
                                    <span class="translate-spinner" aria-hidden="true"></span>
                                    Translating to {translateTargetLang}…
                                </div>
                            {:else if ts.error}
                                <div class="translate-error">
                                    Translation failed: {ts.error}
                                    <button
                                        type="button"
                                        class="translate-retry"
                                        onclick={() => handleTranslate(r)}
                                    >
                                        Retry
                                    </button>
                                </div>
                            {:else if ts.translated_text}
                                <div class="translate-result">
                                    <div class="translate-meta">
                                        <span class="translate-arrow">
                                            {ts.source_lang ?? '?'} → {ts.target_lang ?? translateTargetLang}
                                        </span>
                                        {#if ts.cached}<span class="translate-cached" title="From SQLite cache">cached</span>{/if}
                                        {#if ts.backend}<span class="translate-backend">{ts.backend}</span>{/if}
                                    </div>
                                    <div class="translate-text">{ts.translated_text}</div>
                                </div>
                            {/if}
                        </div>
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

    <!-- P13.7 Stage M — remote search panel.  Shown once the user
         has clicked the "Cloud" button at least once.  Independent
         of the local results state; the user can flip between
         local + remote without losing either pane.  Per-row
         "Download" button calls sync_cb_download_file (HTTPS
         streaming, sha-verified) to fetch the file bytes from the
         VPS into a chosen local path. -->
    {#if remoteSearched}
        <section class="remote-results">
            <header class="remote-header">
                <strong>Cloud-backup hits</strong>
                {#if remoteLoading}<Loader2 size={14} class="spin" />{/if}
                <span class="remote-meta">
                    {remoteResults.length} hit(s)
                    {#if remoteUsedText && remoteUsedVector}· hybrid (FTS + vector)
                    {:else if remoteUsedText}· FTS
                    {:else if remoteUsedVector}· vector
                    {:else}· metadata
                    {/if}
                    · {remoteShards} shard(s) queried
                </span>
            </header>
            {#if remoteError}
                <p class="error">{remoteError}</p>
            {:else if remoteResults.length === 0 && !remoteLoading}
                <p class="hint">No matches on the cloud-backup VPS for that query.</p>
            {:else}
                <ul class="remote-list">
                    {#each remoteResults as hit (hit.sha256)}
                        {@const d = remoteDownloads.get(hit.sha256)}
                        <li>
                            <div class="remote-row">
                                <div class="remote-meta-row">
                                    <span class="remote-score">{hit.score.toFixed(3)}</span>
                                    <span class="remote-title">
                                        {hit.title ?? hit.filename ?? '(no title)'}
                                    </span>
                                    {#if hit.year}<span class="remote-year">{hit.year}</span>{/if}
                                    {#if hit.author}<span class="remote-author">{hit.author}</span>{/if}
                                    {#if hit.language}<span class="remote-lang">{hit.language}</span>{/if}
                                    {#if hit.collection_id}<span class="remote-coll">{hit.collection_id}</span>{/if}
                                </div>
                                <div class="remote-path">{hit.path ?? ''}</div>
                                {#if hit.full_text}
                                    <div class="remote-snippet">
                                        {hit.full_text.slice(0, 240)}{hit.full_text.length > 240 ? '…' : ''}
                                    </div>
                                {/if}
                                <div class="remote-actions">
                                    <button class="open-btn"
                                            onclick={() => downloadRemoteRow(hit)}
                                            disabled={d?.downloading}>
                                        {#if d?.downloading}<Loader2 size={13} class="spin" />{/if}
                                        Download bytes
                                    </button>
                                    {#if d?.bytes}
                                        <span class="remote-dl-ok">
                                            ✓ {d.bytes} B → {d.dest}
                                        </span>
                                    {/if}
                                    {#if d?.error}
                                        <span class="remote-dl-err">✗ {d.error}</span>
                                    {/if}
                                </div>
                            </div>
                        </li>
                    {/each}
                </ul>
            {/if}
        </section>
    {/if}

    <!-- Stage S — federated search panel -->
    {#if fedSearched}
        <section class="fed-results">
            <header class="remote-header">
                <strong>🔀 Federated results</strong>
                {#if fedLoading}<Loader2 size={14} class="spin" />{/if}
                <span class="remote-meta">{fedResults.length} hit(s) across {fedBackends.join(', ')}</span>
                <!-- backend filter checkboxes -->
                <span class="fed-toggles">
                    {#each ['local', 'cloud_backup', 'crisplens'] as b}
                        <label class="fed-toggle">
                            <input type="checkbox"
                                   checked={fedBackends.includes(b)}
                                   onchange={(e) => {
                                       const checked = (e.target as HTMLInputElement).checked;
                                       fedBackends = checked
                                           ? [...fedBackends, b]
                                           : fedBackends.filter(x => x !== b);
                                   }} />
                            {SOURCE_ICON[b] ?? b} {b.replace('_', ' ')}
                        </label>
                    {/each}
                </span>
            </header>
            {#each Object.entries(fedErrors) as [k, v]}
                <p class="error">[{k}] {v}</p>
            {/each}
            {#if fedResults.length === 0 && !fedLoading}
                <p class="hint">No federated results — run a search or enable more backends.</p>
            {:else}
                <ul class="remote-list">
                    {#each fedResults as hit (hit.id)}
                        <li>
                            <div class="remote-row">
                                <div class="remote-meta-row">
                                    <span class="fed-source-badge">{SOURCE_ICON[hit.source] ?? ''} {hit.source}</span>
                                    <span class="remote-score">{hit.score.toFixed(4)}</span>
                                    <span class="fed-rank">#{hit.rrf_rank}</span>
                                    <span class="remote-title">{hit.title ?? hit.filename ?? '(no title)'}</span>
                                    {#if hit.year}<span class="remote-year">{hit.year}</span>{/if}
                                    {#if hit.author}<span class="remote-author">{hit.author}</span>{/if}
                                    {#if hit.language}<span class="remote-lang">{hit.language}</span>{/if}
                                    {#if hit.ext}<span class="remote-lang">{hit.ext}</span>{/if}
                                </div>
                                {#if hit.path}
                                    <div class="remote-path">{hit.path}</div>
                                {/if}
                                {#if hit.snippet}
                                    <div class="remote-snippet">{hit.snippet.slice(0, 240)}{hit.snippet.length > 240 ? '…' : ''}</div>
                                {/if}
                            </div>
                        </li>
                    {/each}
                </ul>
            {/if}
        </section>
    {/if}
</div>

<style>
    .remote-results { padding: 12px 16px; border-top: 1px solid var(--color-border, #444); }
    .remote-header { display: flex; align-items: center; gap: 10px; margin-bottom: 8px; }
    .remote-meta { color: var(--color-text-muted, #aaa); font-size: 0.85em; }
    .remote-list { list-style: none; padding: 0; margin: 0; }
    .remote-list li { padding: 8px 0; border-bottom: 1px solid var(--color-border-subtle, #2c2c2c); }
    .remote-meta-row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
    .remote-score { font-family: ui-monospace, monospace; color: var(--color-accent, #4a9eff); min-width: 4em; }
    .remote-title { font-weight: 600; }
    .remote-year, .remote-author, .remote-lang, .remote-coll {
        font-size: 0.8em; color: var(--color-text-muted, #aaa);
        padding: 1px 6px; border-radius: 3px;
        background: var(--color-bg-subtle, #2c2c2c);
    }
    .remote-path { font-size: 0.85em; color: var(--color-text-muted, #aaa); margin-top: 2px; font-family: ui-monospace, monospace; }
    .remote-snippet { margin-top: 4px; font-size: 0.9em; color: var(--color-text-secondary, #ccc); }
    .remote-actions { margin-top: 6px; display: flex; gap: 10px; align-items: center; }
    .remote-dl-ok { color: var(--color-success, #2a8); font-size: 0.85em; }
    .remote-dl-err { color: var(--color-danger, #d44); font-size: 0.85em; }
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

    /* P13.5 on-demand translation surface — sits below the chunk
       preview, indented to align with the snippet text.  Three
       states share the same container so the row doesn't shift
       on state changes. */
    .translate-surface {
        padding: 4px 14px 10px 52px;
        font-size: 0.78rem;
    }
    .translate-btn {
        background: none;
        border: 1px solid #27272a;
        color: #6b7280;
        font-size: 0.72rem;
        padding: 3px 9px;
        border-radius: 4px;
        cursor: pointer;
        transition: color 0.15s, border-color 0.15s;
    }
    .translate-btn:hover {
        color: #93c5fd;
        border-color: #3b82f655;
    }
    .translate-loading {
        color: #71717a; font-size: 0.72rem;
        display: inline-flex; align-items: center; gap: 6px;
    }
    .translate-spinner {
        display: inline-block; width: 8px; height: 8px;
        border: 1.5px solid #27272a; border-top-color: #93c5fd;
        border-radius: 50%; animation: spin 0.6s linear infinite;
    }
    .translate-error {
        color: #f87171; font-size: 0.72rem;
        display: flex; align-items: center; gap: 8px;
    }
    .translate-retry {
        background: none; border: 1px solid #f8717144; color: #f87171;
        padding: 2px 8px; border-radius: 4px; cursor: pointer; font-size: 0.7rem;
    }
    .translate-result {
        border-left: 2px solid #3b82f655;
        padding-left: 10px;
        margin-top: 4px;
    }
    .translate-meta {
        font-size: 0.65rem; color: #71717a;
        display: flex; gap: 8px; align-items: center;
        margin-bottom: 3px;
    }
    .translate-arrow { font-variant-numeric: tabular-nums; }
    .translate-cached {
        background: #3b82f622; color: #93c5fd;
        padding: 0 5px; border-radius: 3px; font-size: 0.6rem;
    }
    .translate-backend {
        color: #52525b; font-size: 0.6rem;
        font-family: ui-monospace, Menlo, monospace;
    }
    .translate-text {
        color: #d4d4d8; line-height: 1.5; font-size: 0.78rem;
    }

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

    /* Stage S — federated panel */
    .fed-results { padding: 12px 16px; border-top: 1px solid var(--color-border, #444); }
    .fed-toggles { display: flex; gap: 10px; margin-left: auto; }
    .fed-toggle { display: flex; align-items: center; gap: 4px; font-size: 0.8em; cursor: pointer; }
    .fed-source-badge {
        font-size: 0.75em; padding: 1px 7px; border-radius: 10px;
        background: #27272a; color: #a1a1aa; white-space: nowrap;
    }
    .fed-rank { font-size: 0.75em; color: #71717a; }
    .fed-btn { background: #1a1a2e; }
</style>
