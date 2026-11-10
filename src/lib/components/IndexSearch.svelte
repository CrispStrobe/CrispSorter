<script lang="ts">
    import IntendedPurposeGate from './IntendedPurposeGate.svelte';
    import AiGeneratedBadge from './AiGeneratedBadge.svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { openPath, openUrl } from '@tauri-apps/plugin-opener';
    import { save } from '@tauri-apps/plugin-dialog';
    import { writeFile } from '@tauri-apps/plugin-fs';
    import { onMount, tick } from 'svelte';
    import { getSetting, saveSetting } from '$lib/store';
    import { i18n } from '$lib/i18n.svelte';
    import { searchLabels } from '$lib/search/labels';
    import { activeSearchFilters, defaultSearchFilters, localSearchArguments, normalizeSearchRecipe,
        restoreSavedSearches, restoreSearchHistory, searchRecipeKey, validateSearchFilters,
        type SearchFilters, type SearchRecipe, type SavedSearch, type SearchHistoryEntry } from '$lib/search/recipe';
    const labels = $derived(searchLabels[i18n.lang]);
    import { caps } from '$lib/capabilities';
    import DocumentViewer from './viewer/DocumentViewer.svelte';
    import FieldEvidencePanel from './FieldEvidencePanel.svelte';
    import DocumentDecisionPanel from './DocumentDecisionPanel.svelte';
    import { experimentalCrispEmbed, loadExperimentalCrispEmbed } from '$lib/experimentalCrispEmbed';
    import { batchManager } from '$lib/batch/store.svelte';
    import { batchSourceKey } from '$lib/mobileProvider';
    import { stat } from '@tauri-apps/plugin-fs';
    import { availableDocumentTools, documentToolRequest } from '$lib/workflow';
    import { AUDIO_EXTENSIONS } from '$lib/extractors/index';
    import {
        Search, X, ChevronDown, ChevronRight,
        SlidersHorizontal, ExternalLink, Loader2, Clock,
        FileText, FolderOpen, HardDrive, Eye, Bookmark, BookmarkPlus, Trash2, Globe, Tag, Share2, ListPlus, Download, Copy,
        Image as ImageIcon
    } from 'lucide-svelte';
    import TagCloud from './TagCloud.svelte';
    import { requestBrowserContext } from '$lib/drives/browserContext';
    import { cloudDrivePanel, localPathPanel, remoteSearchPanel, searchResultsPanel } from '$lib/drives/panels';
    import { localPathFromSearchUri, pathBaseName } from '$lib/drives/browser';

    // Strip path → bare catalog filename for the badge label.
    function catalogName(path: string): string {
        return path.split(/[\\/]/).pop()?.replace(/\.caf$/i, '') ?? path;
    }

    // uriToPath was inlined here; now centralised in viewer/types.ts.

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
        source_hash?:    string;
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
        // v106/v107 — source URL provenance + structured tags, surfaced so
        // the row can render an "Open original" link and tag chips.  Absent
        // for local-only files that never had a source URL / tags.
        url?:                  string;
        tags?:                 string[];
        // P22.3 — extractive summary generated at ingest time.
        summary?:              string;
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
    let vectorEnabled = $state(true);
    let limit       = $state(20);
    let results     = $state<SearchResult[]>([]);
    let compareResults = $state<SearchResult[]>([]);
    let loading     = $state(false);
    let error       = $state('');
    let searchNotice = $state('');
    interface ResultActionFeedback {
        kind: 'success' | 'error';
        message: string;
        retry?: () => Promise<void>;
    }
    let resultActionFeedback = $state<ResultActionFeedback | null>(null);
    let resultActionsBusy = $state<Set<string>>(new Set());
    let resultActionRequest = 0;

    function dismissResultActionFeedback(): void {
        resultActionRequest += 1;
        resultActionFeedback = null;
    }

    /** Result actions must not replace the search view with a retrieval error. */
    async function runResultAction(
        key: string,
        operation: () => Promise<string | null>,
        failure: string,
        retryable: boolean | (() => boolean) = true,
    ): Promise<void> {
        if (resultActionsBusy.has(key)) return;
        const request = ++resultActionRequest;
        resultActionFeedback = null;
        resultActionsBusy = new Set([...resultActionsBusy, key]);
        try {
            const message = await operation();
            if (request === resultActionRequest && message) {
                resultActionFeedback = { kind: 'success', message };
            }
        } catch (cause) {
            if (request === resultActionRequest) {
                const detail = cause instanceof Error ? cause.message : String(cause);
                resultActionFeedback = {
                    kind: 'error', message: `${failure}: ${detail}`,
                    retry: (typeof retryable === 'function' ? retryable() : retryable)
                        ? () => runResultAction(key, operation, failure, retryable) : undefined,
                };
            }
        } finally {
            const next = new Set(resultActionsBusy);
            next.delete(key);
            resultActionsBusy = next;
        }
    }
    let searchRequest = 0;
    let displayedRecipeKey = $state<string | null>(null);
    let shareBusy    = $state<string | null>(null);
    let downloadBusy = $state<string | null>(null);
    let mutationBusy = $state<string | null>(null);
    let promoteBusy = $state<string | null>(null);
    let shareCapabilities = $state<Record<string, boolean>>({});
    let batchSelection = $state<Set<string>>(new Set());
    let searched    = $state(false);
    let showFilters = $state(false);
    let expanded    = $state<Set<string>>(new Set());

    // Advanced search filters
    let filterExt           = $state('');
    let filterLang          = $state('');
    let filterYearMin       = $state<number | null>(null);
    let filterYearMax       = $state<number | null>(null);
    let filterFolderPrefix  = $state('');
    let filterUrlDomain     = $state('');
    let filterTag           = $state('');
    let filterAudioDurMin   = $state<number | null>(null);
    let filterAudioDurMax   = $state<number | null>(null);
    let filterCameraMake    = $state('');
    let filterCameraModel   = $state('');
    let filterColbert       = $state(false);
    let filterOmniSearch    = $state(false);
    // P23 — fuzzy search toggle (auto-apply ~1 edit distance to terms)
    let filterFuzzy         = $state(false);
    // P24.4 — synonym expansion toggle
    let filterSynonyms      = $state(false);
    // PLAN P7.6 follow-up — when on (default), backend hides results
    // pinned to currently-unmounted volumes. Toggle off to show
    // everything regardless of mount state.
    let includeUnmounted = $state(false);
    // P23 — search-within-results: doc_id scope from previous results
    let refinementScope = $state<string[]>([]);

    // P21 — image search via ViT + omni embeddings.
    let imageSearchLoading = $state(false);

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
    let remotePromoteBusy = $state<string | null>(null);

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
        // v106/v107 — already populated by the Rust federated builders for
        // local + cloud_backup hits (CrispLens carries neither).
        url?: string | null;
        tags?: string[] | null;
    }
    // PLAN P36.16 — cloud-backup is only a real backend when the build
    // carries it. Derived rather than a constant so the toggle disappears
    // instead of offering a backend whose every query would fail at the
    // network seam.
    let fedBackendsAvailable = $derived(
        ['local', 'cloud_backup', 'crisplens'].filter(
            (b) => b !== 'cloud_backup' || $caps.cloud_backup,
        ),
    );
    let fedBackends     = $state<string[]>(['local', 'cloud_backup', 'crisplens']);
    // Whatever the user selected, never *send* a backend this build cannot
    // reach: `fedBackends` is seeded before the capability probe resolves,
    // and it is persisted across builds.
    let fedBackendsActive = $derived(fedBackends.filter((b) => fedBackendsAvailable.includes(b)));
    let fedResults      = $state<FederatedHit[]>([]);
    let fedLoading      = $state(false);
    let fedSearched     = $state(false);
    let fedErrors       = $state<Record<string, string>>({});
    let fedQuery = $state('');
    let fedQueryLimit = $state(20);
    let fedBody = $state<Map<string, string>>(new Map());
    let fedBodyLoading = $state<string | null>(null);

    async function readFederatedBody(hit: FederatedHit) {
        if (!hit.sha256 || !fedQuery) return;
        fedBodyLoading = hit.sha256;
        try {
            const response = await invoke<{ rows: { sha256: string; full_text?: string | null }[] }>(
                'sync_cb_search', { q: fedQuery, limit: fedQueryLimit },
            );
            const body = response.rows.find(r => r.sha256 === hit.sha256)?.full_text;
            if (!body) throw new Error('Indexed text is unavailable for this result.');
            const next = new Map(fedBody); next.set(hit.sha256, body); fedBody = next;
        } catch (e: any) {
            fedErrors = { ...fedErrors, article: String(e?.message ?? e) };
        } finally { fedBodyLoading = null; }
    }

    // Tier 2 tag cloud on the federated pane — same opt-in, client-side
    // pattern as the local pane. Tags ride the FederatedHit wire for local
    // + cloud_backup hits (CrispLens carries none). Per-hit counts.
    let showFedTagCloud = $state(false);
    let fedTags = $state<Set<string>>(new Set());

    const fedTagFacets = $derived.by(() => {
        const counts = new Map<string, number>();
        for (const h of fedResults) {
            const seen = new Set<string>();
            for (const t of (h.tags ?? []))
                if (t && !t.startsWith('collection:')) seen.add(t);
            for (const t of seen) counts.set(t, (counts.get(t) ?? 0) + 1);
        }
        return Array.from(counts.entries())
            .map(([tag, count]) => ({ tag, count }))
            .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
    });

    const displayedFedResults = $derived.by(() => {
        if (fedTags.size === 0) return fedResults;
        return fedResults.filter((h) => {
            const t = new Set(h.tags ?? []);
            for (const sel of fedTags) if (!t.has(sel)) return false;
            return true;
        });
    });

    function toggleFedTag(tag: string) {
        const next = new Set(fedTags);
        if (next.has(tag)) next.delete(tag); else next.add(tag);
        fedTags = next;
    }

    const SOURCE_ICON: Record<string, string> = {
        local: '💾',
        cloud_backup: '☁',
        crisplens: '👁',
    };

    async function runFederatedSearch() {
        const q = query.trim();
        if (!q) return;
        revealResultsOnNarrowScreen();
        if (!validateFilters()) return;
        // The current remote federation wire paths do not carry these
        // options. Never silently broaden an explicitly filtered search.
        if (filterFuzzy || filterSynonyms || includeUnmounted || smartSearch || refinementScope.length
            || (activeFilterKeys.length > 0 && fedBackendsActive.some(backend => backend !== 'local'))) {
            fedSearched = true; fedResults = []; fedErrors = { global: labels.remoteUnsupported };
            return;
        }
        fedLoading  = true;
        fedSearched = true;
        fedQuery = q;
        fedQueryLimit = limit;
        fedBody = new Map();
        fedResults  = [];
        fedErrors   = {};
        fedTags     = new Set();
        try {
            const r = await invoke<{ hits: FederatedHit[]; errors: Record<string, string> }>(
                'sync_federated_search',
                {
                    q,
                    limit,
                    backends: fedBackendsActive.join(','),
                    ext: filterExt ? filterExt.split(',').map(s => s.trim()).filter(Boolean) : null,
                    lang: filterLang || null,
                    yearMin: filterYearMin,
                    yearMax: filterYearMax,
                    folderPrefix: filterFolderPrefix || null,
                    urlDomain: filterUrlDomain || null,
                    tag: filterTag || null,
                    audioDurationMin: filterAudioDurMin,
                    audioDurationMax: filterAudioDurMax,
                    imageCameraMake: filterCameraMake || null,
                    imageCameraModel: filterCameraModel || null,
                    colbertRerank: filterColbert || null,
                    omniSearch: filterOmniSearch || null,
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

    // Stage W — skeleton hint state.
    interface SkeletonHit { name: string; doc_count: number; }
    let skeletonAuthors    = $state<SkeletonHit[]>([]);
    let skeletonParentDirs = $state<SkeletonHit[]>([]);
    let skeletonVisible    = $state(false);
    let skeletonRequest = 0;

    async function runSkeletonSearch(q: string) {
        const request = ++skeletonRequest;
        if (!q.trim()) {
            skeletonAuthors = [];
            skeletonParentDirs = [];
            skeletonVisible = false;
            return;
        }
        try {
            const r = await invoke<{ authors: SkeletonHit[]; parent_dirs: SkeletonHit[] }>(
                'sync_skeleton_search', { query: q.trim() }
            );
            if (request !== skeletonRequest || q.trim() !== query.trim()) return;
            skeletonAuthors    = r.authors    ?? [];
            skeletonParentDirs = r.parent_dirs ?? [];
            skeletonVisible    = skeletonAuthors.length > 0 || skeletonParentDirs.length > 0;
        } catch {
            if (request === skeletonRequest) skeletonVisible = false;
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

    // Per-file document tools (GUI surface for the `ocr`/`kie`/`table` CLI).
    // Keyed by doc_id; holds the in-flight tool + its last result/error.
    interface DocToolsState {
        busy?: 'pdf' | 'kie' | 'table' | null;
        error?: string;
        pdf?: { saved_path: string; pages: number };
        kie?: { label: string; value: string; score: number }[];
        table?: { rows: number; cols: number; saved_path: string | null };
    }
    let docTools = $state<Map<string, DocToolsState>>(new Map());
    // Doc tools (searchable-PDF render, LiLT/GLiNER KIE, table parse) all need
    // CrispEmbed linked in — gate the surface so we don't show failing buttons.
    let crispembedAvailable = $state(false);

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

    /** Should this row offer the per-file document tools? Image/PDF rows only,
     *  and only when CrispEmbed is linked in (the tools need it). */
    function looksDocToolable(r: SearchResult): boolean {
        if (!crispembedAvailable) return false;
        const ext = (r.ext ?? '').toLowerCase();
        return IMAGE_EXTS_SET_SEARCH.has(ext) || ext === 'pdf';
    }
    function setDocTool(key: string, patch: Partial<DocToolsState>): void {
        const next = new Map(docTools);
        next.set(key, { ...(docTools.get(key) ?? {}), ...patch });
        docTools = next;
    }
    /** OCR → structured/searchable artifact written beside the source file. */
    async function handleOcrExport(r: SearchResult, format: string): Promise<void> {
        const key = r.doc_id;
        setDocTool(key, { busy: 'pdf', error: undefined });
        try {
            const res = await invoke<{ saved_path: string; pages: number }>('tool_ocr_export', {
                locationUri: r.location_uri, format, pdfa: false,
            });
            setDocTool(key, { busy: null, pdf: { saved_path: res.saved_path, pages: res.pages } });
        } catch (e: any) {
            setDocTool(key, { busy: null, error: String(e?.message ?? e) });
        }
    }
    /** Key-information extraction (uses the Settings → NER labels by default). */
    async function handleKie(r: SearchResult): Promise<void> {
        const key = r.doc_id;
        setDocTool(key, { busy: 'kie', error: undefined });
        try {
            const res = await invoke<{ label: string; value: string; score: number }[]>('tool_kie_extract', {
                locationUri: r.location_uri, labels: [], threshold: null, lilt: false, liltModel: null,
            });
            setDocTool(key, { busy: null, kie: res });
        } catch (e: any) {
            setDocTool(key, { busy: null, error: String(e?.message ?? e) });
        }
    }
    /** Detect a table and export it as HTML beside the source file. */
    async function handleTableExtract(r: SearchResult): Promise<void> {
        const key = r.doc_id;
        setDocTool(key, { busy: 'table', error: undefined });
        try {
            const res = await invoke<{ rows: number; cols: number; saved_path: string | null }>('tool_table_extract', {
                locationUri: r.location_uri, ocrModel: null, save: true,
            });
            setDocTool(key, { busy: null, table: { rows: res.rows, cols: res.cols, saved_path: res.saved_path } });
        } catch (e: any) {
            setDocTool(key, { busy: null, error: String(e?.message ?? e) });
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

    let wallabagBodies = $state<Map<string, string>>(new Map());
    let wallabagReading = $state<string | null>(null);
    async function readWallabag(r: SearchResult) {
        if (wallabagBodies.has(r.doc_id)) {
            const next = new Map(wallabagBodies); next.delete(r.doc_id); wallabagBodies = next; return;
        }
        await runResultAction(`article:${r.doc_id}`, async () => {
            wallabagReading = r.doc_id;
            try {
                const body = await invoke<string>('wallabag_read', { docId: r.doc_id });
                const next = new Map(wallabagBodies); next.set(r.doc_id, body); wallabagBodies = next;
                return null;
            } finally { wallabagReading = null; }
        }, labels.readArticleFailed);
    }

    // ── Preview pane (PLAN P7.3) ───────────────────────────────────────────────
    // Right-side slide-in pane using the universal DocumentViewer component.
    // Format detection + rendering is handled entirely by the viewer.
    let previewing = $state<SearchResult | null>(null);
    let previewCloseButton = $state<HTMLButtonElement>();
    let previewTrigger: HTMLButtonElement | null = null;

    async function openPreview(r: SearchResult, trigger: HTMLButtonElement) {
        if (previewing && previewing.doc_id === r.doc_id) { await closePreview(); return; }
        previewTrigger = trigger;
        previewing = r;
        await tick();
        // Narrow screens show the preview in place of the result list. Move
        // focus to its close control instead of leaving it on a hidden trigger.
        if (previewing?.doc_id === r.doc_id && window.matchMedia('(max-width: 800px)').matches) {
            previewCloseButton?.focus({ preventScroll: true });
        }
    }

    async function closePreview() {
        const trigger = previewTrigger;
        previewing = null;
        previewTrigger = null;
        await tick();
        if (trigger?.isConnected) trigger.focus();
    }

    function revealResultsOnNarrowScreen() {
        if (window.matchMedia('(max-width: 800px)').matches) {
            previewing = null;
            previewTrigger = null;
        }
    }

    async function findByField(tag: string, value: string) {
        revealResultsOnNarrowScreen();
        const request = ++searchRequest;
        resetFilters(); searchNotice = ''; displayedRecipeKey = null; imageSearchLoading = false;
        loading = true; error = ''; searched = true;
        filterTag = tag; query = value; clearTranslations();
        try {
            const page = await invoke<{ rows: SearchResult[] }>('index_query_documents', {
                filter: { ext: [], tags: [tag] },
                sort: { column: 'filename', direction: 'asc' },
                page: { limit, cursor: null },
            });
            if (request === searchRequest) results = page.rows;
        } catch (e) { if (request === searchRequest) error = String(e); }
        finally { if (request === searchRequest) loading = false; }
    }

    async function useReviewedFields(values: Record<string, string>) {
        const source = previewing;
        if (!source) return;
        const path = localPathFromSearchUri(source.location_uri);
        if (!path) throw new Error('Download this document locally before adding it to the sorting inbox.');
        const sourceHash = source.source_hash || '';
        const filename = source.filename || pathBaseName(path);
        await invoke('verify_sort_field_source', { path, expectedHash: sourceHash });
        const fields = await getSetting('reviewedSortFields', {}) as Record<string, Record<string, string>>;
        await saveSetting('reviewedSortFields', { ...fields, [path]: { ...fields[path], ...values, __source_hash: sourceHash } });
        let item = batchManager.items.find(item => item.originalPath === path);
        if (!item) {
            const info = await stat(path);
            await batchManager.addItem(path, filename, info.size);
            item = batchManager.items.find(item => item.originalPath === path);
        }
        if (item) {
            await batchManager.recalculateTargetPath(item.id);
            await batchManager.setAcceptedItems([item.id], false);
        }
    }


    // Saved searches and history retain the complete query, not just its text.
    let savedSearches = $state<SavedSearch[]>([]);
    let savedSearchBusy = $state(false);
    let savedSearchesLoaded = $state(false);
    let savedSearchesLoading = $state(true);
    let savedSearchesLoadError = $state('');
    let showSavedDropdown = $state(false);
    let searchHistory = $state<SearchHistoryEntry[]>([]);
    let showHistory = $state(false);
    const MAX_HISTORY = 50;

    const currentFilters = $derived<SearchFilters>({
        ext: filterExt, language: filterLang, yearMin: filterYearMin ?? null,
        yearMax: filterYearMax ?? null, folderPrefix: filterFolderPrefix,
        urlDomain: filterUrlDomain, tag: filterTag, audioMin: filterAudioDurMin ?? null,
        audioMax: filterAudioDurMax ?? null, cameraMake: filterCameraMake,
        cameraModel: filterCameraModel, colbert: filterColbert, omni: filterOmniSearch,
        fuzzy: filterFuzzy, synonyms: filterSynonyms, includeUnmounted,
    });
    const activeFilterKeys = $derived(activeSearchFilters(currentFilters));
    const searchOptionsChanged = $derived(displayedRecipeKey !== null && displayedRecipeKey !== searchRecipeKey(currentRecipe()));

    function currentRecipe(): SearchRecipe {
        return normalizeSearchRecipe({ query, mode, limit, filters: currentFilters, smart: smartSearch, docIdScope: refinementScope });
    }

    function applyFilters(f: SearchFilters): void {
        filterExt = f.ext; filterLang = f.language;
        filterYearMin = f.yearMin; filterYearMax = f.yearMax;
        filterFolderPrefix = f.folderPrefix; filterUrlDomain = f.urlDomain; filterTag = f.tag;
        filterAudioDurMin = f.audioMin; filterAudioDurMax = f.audioMax;
        filterCameraMake = f.cameraMake; filterCameraModel = f.cameraModel;
        filterColbert = f.colbert; filterOmniSearch = f.omni;
        filterFuzzy = f.fuzzy; filterSynonyms = f.synonyms; includeUnmounted = f.includeUnmounted;
    }

    function applyRecipe(recipe: SearchRecipe): void {
        query = recipe.query;
        void runSkeletonSearch(query);
        mode = !vectorEnabled && recipe.mode !== 'text' ? 'text' : recipe.mode;
        limit = recipe.limit; smartSearch = recipe.smart;
        refinementScope = [...recipe.docIdScope];
        applyFilters(recipe.filters);
    }

    function resetFilters(): void {
        applyFilters(defaultSearchFilters());
        refinementScope = [];
        smartSearch = false;
        searchNotice = '';
    }

    function removeFilter(key: keyof SearchFilters): void {
        applyFilters({ ...currentFilters, [key]: defaultSearchFilters()[key] });
    }

    function validateFilters(): boolean {
        const issue = validateSearchFilters(currentFilters);
        if (issue) { error = labels[issue]; return false; }
        if ((filterColbert || filterOmniSearch) && !crispembedAvailable) {
            error = labels.nativeOptionsUnavailable; return false;
        }
        return true;
    }

    async function addToHistory(recipe: SearchRecipe, count: number) {
        const key = searchRecipeKey(recipe);
        searchHistory = [
            { ...recipe, ts: Date.now(), resultCount: count },
            ...searchHistory.filter(h => searchRecipeKey(h) !== key),
        ].slice(0, MAX_HISTORY);
        await saveSetting('searchHistory', searchHistory);
    }

    async function loadHistoryEntry(h: SearchHistoryEntry) {
        applyRecipe(h);
        showHistory = false;
        await runSearch();
    }

    async function deleteHistoryEntry(idx: number) {
        searchHistory = searchHistory.filter((_, i) => i !== idx);
        try { await saveSetting('searchHistory', searchHistory); }
        catch (e) { searchNotice = String(e); }
    }

    async function clearHistory() {
        searchHistory = [];
        try { await saveSetting('searchHistory', []); }
        catch (e) { searchNotice = String(e); }
    }

    async function restoreBookmarks() {
        savedSearchesLoading = true; savedSearchesLoadError = '';
        try {
            savedSearches = restoreSavedSearches(await getSetting('savedSearches', null));
            savedSearchesLoaded = true;
        } catch (e) {
            savedSearchesLoadError = String(e);
            savedSearchesLoaded = false;
        } finally { savedSearchesLoading = false; }
    }

    onMount(async () => {
        try {
            const config = await invoke<{ use_vector?: boolean }>('index_get_config');
            vectorEnabled = config.use_vector !== false;
            if (!vectorEnabled) mode = 'text';
        } catch { /* use the normal default until configuration is available */ }
        await loadExperimentalCrispEmbed().catch(e => console.warn('Could not restore experimental service settings', e));
        await restoreBookmarks();
        try {
            searchHistory = restoreSearchHistory(await getSetting('searchHistory', null));
        } catch (e) { searchNotice = String(e); }
        try {
            const caps = await invoke<{ crispembed: boolean }>('index_capabilities');
            crispembedAvailable = !!caps.crispembed;
        } catch {
            crispembedAvailable = false;
        }
    });

    async function saveCurrentSearch() {
        if (!savedSearchesLoaded || savedSearchBusy || !query.trim() || !validateFilters()) return;
        const recipe = currentRecipe();
        const key = searchRecipeKey(recipe);
        const idx = savedSearches.findIndex(s => searchRecipeKey(s) === key);
        const name = query.trim().length > 40 ? query.trim().slice(0, 37) + '…' : query.trim();
        const entry: SavedSearch = { ...recipe, name, savedAt: Date.now() };
        const next = [...savedSearches];
        if (idx >= 0) next[idx] = entry;
        else next.push(entry);
        savedSearchBusy = true;
        try {
            await saveSetting('savedSearches', next);
            savedSearches = next;
            searchNotice = labels.saved;
        } catch (e) { searchNotice = String(e); }
        finally { savedSearchBusy = false; }
    }

    async function loadSavedSearch(s: SavedSearch) {
        applyRecipe(s);
        showSavedDropdown = false;
        await runSearch();
    }

    async function deleteSavedSearch(idx: number) {
        if (!savedSearchesLoaded || savedSearchBusy) return;
        savedSearchBusy = true;
        const next = savedSearches.filter((_, i) => i !== idx);
        try {
            await saveSetting('savedSearches', next);
            savedSearches = next;
        } catch (e) { searchNotice = String(e); }
        finally { savedSearchBusy = false; }
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

    // ── Tier 2 tag cloud on the local results pane ──────────────────────────────
    // Opt-in (default-hidden) and computed entirely client-side from the hits
    // already on screen — no backend round-trip. Reuses the same TagCloud
    // component as the Übersicht browse. Counts are per-document.
    let showSearchTagCloud = $state(false);
    let searchTags = $state<Set<string>>(new Set());

    const searchTagFacets = $derived.by(() => {
        const counts = new Map<string, number>();
        for (const g of grouped) {
            const docTags = new Set<string>();
            for (const c of g.chunks)
                for (const t of (c.tags ?? []))
                    if (t && !t.startsWith('collection:')) docTags.add(t);
            for (const t of docTags) counts.set(t, (counts.get(t) ?? 0) + 1);
        }
        return Array.from(counts.entries())
            .map(([tag, count]) => ({ tag, count }))
            .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
    });

    // Groups narrowed by the AND tag selection — a document survives only if
    // its hits collectively carry every selected tag.
    const displayedGroups = $derived.by(() => {
        if (searchTags.size === 0) return grouped;
        return grouped.filter((g) => {
            const docTags = new Set<string>();
            for (const c of g.chunks) for (const t of (c.tags ?? [])) docTags.add(t);
            for (const sel of searchTags) if (!docTags.has(sel)) return false;
            return true;
        });
    });

    function toggleSearchTag(tag: string) {
        const next = new Set(searchTags);
        if (next.has(tag)) next.delete(tag); else next.add(tag);
        searchTags = next;
    }

    // ── Search ─────────────────────────────────────────────────────────────────

    async function runSearch() {
        if (!query.trim()) return;
        revealResultsOnNarrowScreen();
        if (!validateFilters()) return;
        const request = ++searchRequest;
        const recipe = currentRecipe();
        imageSearchLoading = false;
        loading = true; error = ''; searchNotice = ''; searched = true;
        resultActionFeedback = null;
        similarHeader = ''; compareResults = []; batchSelection = new Set();
        clearTranslations(); searchTags = new Set();
        try {
            let searchQuery = recipe.query;
            if (recipe.smart) {
                try {
                    const parsed = await invoke<{ cleaned_query: string; filters: any }>(
                        'index_parse_nl_query', { query: searchQuery }
                    );
                    if (request !== searchRequest) return;
                    searchQuery = parsed.cleaned_query || searchQuery;
                    const f = recipe.filters;
                    if (parsed.filters.language) f.language = parsed.filters.language;
                    if (parsed.filters.year_min != null) f.yearMin = parsed.filters.year_min;
                    if (parsed.filters.year_max != null) f.yearMax = parsed.filters.year_max;
                    if (parsed.filters.ext?.length) f.ext = parsed.filters.ext.join(',');
                    if (parsed.filters.parent_dir_prefix) f.folderPrefix = parsed.filters.parent_dir_prefix;
                    if (parsed.filters.url_domain) f.urlDomain = parsed.filters.url_domain;
                    if (parsed.filters.tag) f.tag = parsed.filters.tag;
                    recipe.filters = normalizeSearchRecipe(recipe).filters;
                    applyFilters(recipe.filters);
                    const issue = validateSearchFilters(recipe.filters);
                    if (issue) { error = labels[issue]; return; }
                } catch (e) {
                    if (request !== searchRequest) return;
                    // Retain the literal query if the optional parser is unavailable.
                    searchNotice = `${labels.warning}: ${String(e)}`;
                }
            }
            if (request !== searchRequest) return;
            let found = await invoke<SearchResult[]>('index_search', localSearchArguments(
                { ...recipe, query: searchQuery }, $experimentalCrispEmbed.moedern ? Math.max(recipe.limit, 50) : recipe.limit,
            ));
            if (request !== searchRequest) return;
            if ($experimentalCrispEmbed.moedern && $experimentalCrispEmbed.endpoint && found.length) {
                const original = found;
                try {
                    const report = await invoke<{ results: SearchResult[] }>('crispembed_rerank_passages', {
                        endpoint: $experimentalCrispEmbed.endpoint, query: searchQuery, candidates: found.slice(0, 100), limit: recipe.limit,
                    });
                    if (request !== searchRequest) return;
                    found = report.results;
                } catch (e) {
                    if (request !== searchRequest) return;
                    found = original.slice(0, recipe.limit);
                    searchNotice = `Experimental reranking unavailable; showing the original search order. ${String(e)}`;
                }
            }
            results = found;
            displayedRecipeKey = searchRecipeKey(recipe);
            // Keep the scope visible until explicitly cleared; repeated searches
            // and bookmarks must not silently broaden the user's selection.
            try { await addToHistory(recipe, results.length); }
            catch (e) { if (request === searchRequest) searchNotice = String(e); }
            void auditSearch(searchQuery);
        } catch (e: any) {
            if (request !== searchRequest) return;
            error = String(e); results = [];
        } finally {
            if (request === searchRequest) loading = false;
        }
    }

    // P22.4 — Smart search (NL→Filters) toggle.
    let smartSearch = $state(false);

    // P22.2 — "More Like This" state.
    let similarHeader = $state('');

    async function findSimilar(r: SearchResult) {
        revealResultsOnNarrowScreen();
        const request = ++searchRequest;
        displayedRecipeKey = null; imageSearchLoading = false;
        loading = true;
        error = '';
        searched = true;
        similarHeader = r.title || r.filename || r.doc_id.slice(0, 20);
        clearTranslations();
        searchTags = new Set();
        batchSelection = new Set();
        try {
            const found = await invoke<SearchResult[]>('index_find_similar', {
                docId: r.doc_id,
                chunkIndex: r.chunk_index >= 0 ? r.chunk_index : 0,
                limit,
            });
            if (request === searchRequest) results = found;
        } catch (e: any) {
            if (request !== searchRequest) return;
            error = String(e);
            results = [];
        } finally {
            if (request === searchRequest) loading = false;
        }
    }

    // P27.10 — Export a search result to HTML/DOCX
    async function exportDoc(r: SearchResult, fmt: string) {
        await runResultAction(`export:${r.doc_id}:${r.chunk_index}`, async () => {
            const savePath = await save({ filters: [{ name: fmt.toUpperCase(), extensions: [fmt] }], defaultPath: `${r.title || r.doc_id}.${fmt}` });
            if (!savePath) return null;
            await invoke(`export_to_${fmt}`, { title: r.title || r.filename || '', body: r.snippet || '', outPath: savePath });
            return `${labels.excerptExported}: ${savePath}`;
        }, labels.exportFailed);
    }

    // P25.9 — Add passage to reading list
    async function highlightPassage(r: SearchResult) {
        await runResultAction(`reading:${r.doc_id}:${r.chunk_index}`, async () => {
            await invoke('highlight_add', {
                docId: r.doc_id,
                chunkIndex: r.chunk_index >= 0 ? r.chunk_index : 0,
                startOffset: 0,
                endOffset: (r.snippet || '').length,
                text: (r.snippet || '').replace(/<\/?mark>/g, '').slice(0, 500),
                note: '',
                color: '#60a5fa',
            });
            return `${labels.readingListAdded}: ${r.title || r.filename || r.doc_id}`;
        }, labels.readingListFailed);
    }

    // P25.2 — Log search to audit trail
    async function auditSearch(q: string) {
        try { await invoke('audit_log_event', { action: 'search', docId: null, detail: `query=${q}` }); }
        catch { /* best-effort */ }
    }

    // P23 — search within current results (progressive refinement)
    function refineSearch() {
        if (results.length === 0) return;
        // Scope the next search to the doc_ids in the current result set
        const docIds = [...new Set(results.map(r => r.doc_id))];
        refinementScope = docIds;
        // Clear query so the user can type a new one
        query = '';
        void runSkeletonSearch('');
    }

    function onKeydown(e: KeyboardEvent) {
        if (e.key === 'Enter') runSearch();
    }

    // P21 — search by image similarity via ViT + omni embeddings.
    async function searchByImage() {
        const { open } = await import('@tauri-apps/plugin-dialog');
        const selected = await open({
            multiple: false,
            filters: [{
                name: 'Images',
                extensions: ['jpg', 'jpeg', 'png', 'tiff', 'tif', 'webp', 'heic', 'bmp', 'avif'],
            }],
        });
        if (!selected) return;
        const imagePath = typeof selected === 'string' ? selected : (selected as any).path;
        if (!imagePath) return;

        revealResultsOnNarrowScreen();
        const request = ++searchRequest;
        displayedRecipeKey = null;
        loading = true; imageSearchLoading = true;
        error    = '';
        searched = true;
        clearTranslations();
        searchTags = new Set();
        batchSelection = new Set();
        try {
            const found = await invoke<SearchResult[]>('index_search_by_image', {
                imagePath,
                limit,
            });
            if (request === searchRequest) results = found;
        } catch (e: any) {
            if (request !== searchRequest) return;
            error   = String(e);
            results = [];
        } finally {
            if (request === searchRequest) { loading = false; imageSearchLoading = false; }
        }
    }

    // P13.7 Stage M — search the cloud-backup VPS over HTTPS via
    // /api/v2/index/search.  Uses the same query box; remote hits
    // surface in a separate panel below the local results.  Embed
    // text matches the query so the server runs vector inference
    // alongside FTS — true hybrid retrieval.
    async function runRemoteSearch() {
        const q = query.trim();
        if (!q) return;
        revealResultsOnNarrowScreen();
        if (!validateFilters()) return;
        const recipe = currentRecipe();
        const f = recipe.filters;
        if (f.audioMin != null || f.audioMax != null || f.cameraMake || f.cameraModel || f.fuzzy
            || f.synonyms || f.colbert || f.omni || f.includeUnmounted || recipe.smart || recipe.docIdScope.length) {
            remoteSearched = true; remoteResults = []; remoteError = labels.remoteUnsupported;
            return;
        }
        remoteLoading  = true;
        remoteError    = '';
        remoteSearched = true;
        remoteResults  = [];
        try {
            const r = await invoke<any>('sync_cb_v2_search', {
                params: {
                    q: mode === 'vector' ? null : q,
                    embedText:  mode === 'text' ? null : q, // server-side semantic channel
                    embedModel: 'e5-large', // matches the LanceDB default-dim table
                    limit,
                    rrfK: 60,
                    filters: { ext: f.ext ? f.ext.split(',') : [], languages: f.language ? [f.language] : [],
                        year_min: f.yearMin, year_max: f.yearMax, parent_dir_prefix: f.folderPrefix || null,
                        url_domain: f.urlDomain || null, tag: f.tag || null },
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

    function openRemoteSearchContext(): void {
        requestBrowserContext(remoteSearchPanel('cloud-backup', query.trim()));
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

    /** Download a remote L1 hit with sha verification, then index it locally. */
    async function promoteRemoteRow(hit: RemoteHit): Promise<void> {
        if (remotePromoteBusy === hit.sha256) return;
        const dest = await save({
            defaultPath: hit.filename ?? hit.sha256,
            title: 'Save and index cloud-backup file locally',
        });
        if (!dest) return;
        remotePromoteBusy = hit.sha256;
        try {
            await invoke('sync_cb_download_file', {
                sha256: hit.sha256,
                destPath: dest as string,
            });
            await invoke('index_ingest_path', {
                path: dest as string,
                ownerId: null,
                title: hit.title ?? null,
                author: hit.author ?? null,
                year: hit.year ?? null,
                language: hit.language ?? null,
            });
            const done = new Map(remoteDownloads);
            done.set(hit.sha256, { downloading: false, dest: dest as string });
            remoteDownloads = done;
        } catch (e: any) {
            const fail = new Map(remoteDownloads);
            fail.set(hit.sha256, { downloading: false, error: String(e?.message ?? e) });
            remoteDownloads = fail;
        } finally {
            remotePromoteBusy = null;
        }
    }

    function toggleExpand(doc_id: string) {
        const next = new Set(expanded);
        if (next.has(doc_id)) next.delete(doc_id);
        else next.add(doc_id);
        expanded = next;
    }

    function clearSearch() {
        revealResultsOnNarrowScreen();
        searchRequest += 1;
        dismissResultActionFeedback();
        loading = false; imageSearchLoading = false; searchNotice = ''; refinementScope = [];
        displayedRecipeKey = null; similarHeader = '';
        void runSkeletonSearch('');
        clearTranslations(); searchTags = new Set();
        query    = '';
        results  = [];
        compareResults = [];
        batchSelection = new Set();
        searched = false;
        error    = '';
    }

    function toggleCompare(result: SearchResult): void {
        const existing = compareResults.findIndex((item) =>
            item.doc_id === result.doc_id && item.chunk_index === result.chunk_index);
        if (existing >= 0) {
            compareResults = compareResults.filter((_, index) => index !== existing);
            return;
        }
        compareResults = [...compareResults.slice(-1), result];
    }

    function compareLabel(uri: string): string {
        const drive = driveUriParts(uri);
        if (drive) return `Cloud drive ${drive.driveId}:${drive.remotePath}`;
        return localPathFromSearchUri(uri) ?? uri;
    }

    async function openFile(uri: string) {
        if (uri.startsWith('https://') || uri.startsWith('http://')) {
            await openOriginal(uri);
            return;
        }
        // uri may be: absolute path, crisp+local://... URI, or similar
        let path = uri;
        // Strip crisp+local:// scheme — everything after the scheme+authority is the path
        if (uri.startsWith('crisp+local://')) {
            // crisp+local://<authority><path>  — authority ends at second "/"
            const afterScheme = uri.slice('crisp+local://'.length);
            const slashIdx = afterScheme.indexOf('/');
            path = slashIdx >= 0 ? afterScheme.slice(slashIdx) : afterScheme;
        }
        await runResultAction(`open:${uri}`, async () => {
            await openPath(path);
            return null;
        }, `${labels.openFileFailed} (${path})`);
    }

    async function openOriginal(url: string) {
        // Opens the source URL (wallabag/pocket provenance, browser-saved
        // PDFs, …) in the default browser via the opener plugin.
        await runResultAction(`source:${url}`, async () => {
            const parsed = new URL(url);
            if (parsed.protocol !== 'https:' && parsed.protocol !== 'http:') {
                throw new Error('Only HTTP and HTTPS source links can be opened.');
            }
            await openUrl(url);
            return null;
        }, `${labels.openSourceFailed} (${url})`);
    }

    function driveUriParts(uri: string): { driveId: string; remotePath: string } | null {
        if (!uri.startsWith('crisp+drive://')) return null;
        const rest = uri.slice('crisp+drive://'.length);
        const slash = rest.indexOf('/');
        if (slash < 0) return null;
        return {
            driveId: rest.slice(0, slash),
            remotePath: decodeURIComponent(rest.slice(slash)),
        };
    }

    function driveIdFromUri(uri: string): string {
        return driveUriParts(uri)?.driveId ?? '';
    }

    async function shareDriveFile(uri: string) {
        const parts = driveUriParts(uri);
        if (!parts) return;
        // A clipboard retry can reuse the link already created by the provider.
        let link: string | null = null;
        await runResultAction(`share:${uri}`, async () => {
            shareBusy = uri;
            try {
                let supported = shareCapabilities[parts.driveId];
                if (supported === undefined) {
                    const capabilities = await invoke<{ share_links?: boolean }>('drive_capabilities', {
                        driveId: parts.driveId,
                    });
                    supported = capabilities.share_links === true;
                    shareCapabilities = { ...shareCapabilities, [parts.driveId]: supported };
                }
                if (!supported) {
                    throw new Error('This provider does not advertise public share links.');
                }
                link ??= await invoke<string>('drive_share_link', parts);
                await navigator.clipboard.writeText(link);
                return labels.shareCopied;
            } finally { shareBusy = null; }
        }, labels.shareFailed, () => shareCapabilities[parts.driveId] !== false);
    }

    /** Save a registered-drive search result locally through the shared queue. */
    async function downloadDriveFile(uri: string, filename?: string): Promise<void> {
        const parts = driveUriParts(uri);
        if (!parts) return;
        await runResultAction(`download:${uri}`, async () => {
            const destination = await save({
                defaultPath: filename || pathBaseName(parts.remotePath) || 'download',
                title: 'Save cloud file for offline use',
            });
            if (!destination) return null;
            downloadBusy = uri;
            try {
                const bytes = await invoke<number[]>('drive_read_file', parts);
                await writeFile(destination, new Uint8Array(bytes));
                return `${labels.downloaded}: ${destination}`;
            } finally { downloadBusy = null; }
        }, labels.downloadFailed);
    }

    async function copyResultPath(uri: string): Promise<void> {
        const drive = driveUriParts(uri);
        const local = localPathFromSearchUri(uri);
        const value = drive ? `${drive.driveId}:${drive.remotePath}` : local;
        if (!value) return;
        await runResultAction(`copy:${uri}`, async () => {
            await navigator.clipboard.writeText(value);
            return `${labels.pathCopied}: ${value}`;
        }, labels.copyPathFailed);
    }

    /**
     * Run a provider-aware mutation from a search hit.  Search results carry
     * remote paths, never local filesystem paths; the command boundary keeps
     * capability checks and conflict policy in Rust.  Delete is confirmed
     * separately because the drive adapter may implement it as recoverable
     * trash, while move/rename always require an explicit destination.
     */
    async function mutateDriveResult(uri: string, operation: 'move' | 'rename' | 'delete'): Promise<void> {
        const parts = driveUriParts(uri);
        if (!parts) return;
        const name = pathBaseName(parts.remotePath) || parts.remotePath;
        await runResultAction(`mutate:${uri}`, async () => {
            mutationBusy = uri;
            try {
                const capabilities = await invoke<{ delete: boolean; rename: boolean; move_path: boolean }>(
                    'drive_capabilities', { driveId: parts.driveId });
                if (operation === 'delete' && !capabilities.delete) {
                    throw new Error('This provider does not support delete/trash.');
                }
                if (operation === 'rename' && !capabilities.rename) {
                    throw new Error('This provider does not support rename.');
                }
                if (operation === 'move' && !capabilities.move_path) {
                    throw new Error('This provider does not support move.');
                }
                if (operation === 'delete') {
                    if (!window.confirm(`Move “${name}” to provider trash?`)) return null;
                    await invoke('drive_delete_path', { driveId: parts.driveId, path: parts.remotePath });
                } else {
                    const label = operation === 'rename' ? 'new remote path/name' : 'destination remote path';
                    const destination = window.prompt(`Enter ${label} for “${name}”:`, parts.remotePath);
                    if (!destination?.trim() || destination.trim() === parts.remotePath) return null;
                    await invoke('drive_move_path', {
                        driveId: parts.driveId,
                        source: parts.remotePath,
                        destination: destination.trim(),
                    });
                }
                await runSearch();
                return `${operation === 'delete' ? 'Moved to trash' : operation === 'rename' ? 'Renamed' : 'Moved'}: ${name}`;
            } finally { mutationBusy = null; }
        }, `Could not ${operation} cloud result`, false);
    }

    /** Promote a remote registered-drive L1 row through the normal ingest
     * pipeline. The Rust command fetches the remote bytes, stages them under
     * app data, and re-ingests them; no remote path is treated as local. */
    async function promoteDriveResult(result: SearchResult): Promise<void> {
        const parts = driveUriParts(result.location_uri);
        if (!parts) return;
        await runResultAction(`promote:${result.doc_id}`, async () => {
            promoteBusy = result.location_uri;
            try {
                const capabilities = await invoke<{ read?: boolean }>('drive_capabilities', {
                    driveId: parts.driveId,
                });
                if (capabilities.read !== true) {
                    throw new Error('This provider does not support remote promotion.');
                }
                await invoke('index_promote_drive_archive', {
                    docId: result.doc_id,
                    driveId: parts.driveId,
                    remotePath: parts.remotePath,
                    ownerId: null,
                });
                await runSearch();
                return `Promoted ${result.filename || parts.remotePath} to full local indexing.`;
            } finally { promoteBusy = null; }
        }, 'Could not promote remote result', false);
    }

    function cbArchiveOriginalPath(uri: string): string | null {
        if (!uri.startsWith('crisp+cb-archive://')) return null;
        const hash = uri.indexOf('#');
        if (hash < 0 || hash === uri.length - 1) return null;
        try {
            return decodeURIComponent(uri.slice(hash + 1));
        } catch {
            return null;
        }
    }

    /** Promote a cloud-backup L1 archive hit to local L3 text/embeddings. */
    async function promoteCbArchive(result: SearchResult): Promise<void> {
        const originalPath = cbArchiveOriginalPath(result.location_uri);
        if (!originalPath || promoteBusy === result.doc_id) return;
        await runResultAction(`promote:${result.doc_id}`, async () => {
            promoteBusy = result.doc_id;
            try {
                let retrievePyPath = await getSetting('cbRetrievePyPath', null) as string | null;
                if (!retrievePyPath) {
                    const { open } = await import('@tauri-apps/plugin-dialog');
                    const selected = await open({
                        multiple: false,
                        filters: [{ name: 'Python script', extensions: ['py'] }],
                        title: 'Select cloud-backup retrieve.py',
                    });
                    if (typeof selected !== 'string') return null;
                    retrievePyPath = selected;
                    await saveSetting('cbRetrievePyPath', retrievePyPath);
                }
                await invoke('index_promote_cb_archive', {
                    docId: result.doc_id,
                    originalPath,
                    retrievePyPath,
                    outputDir: null,
                    ownerId: null,
                });
                await runSearch();
                return `Promoted ${result.filename || originalPath} to local full-text index.`;
            } finally { promoteBusy = null; }
        }, 'Could not promote archive result', false);
    }

    function openDriveInContext(uri: string): void {
        const parts = driveUriParts(uri);
        if (parts) requestBrowserContext(cloudDrivePanel(parts.driveId, parts.remotePath));
    }

    function openLocalInContext(uri: string): void {
        const path = localPathFromSearchUri(uri);
        if (path) requestBrowserContext(localPathPanel(path));
    }

    function openSearchResultsContext(): void {
        if (query.trim()) requestBrowserContext(searchResultsPanel(query.trim()));
    }

    async function addResultToBatch(result: SearchResult): Promise<void> {
        const path = localPathFromSearchUri(result.location_uri);
        await runResultAction(`batch:${result.doc_id}`, async () => {
            if (!path) throw new Error(labels.onlyLocalBatch);
            const name = result.filename || pathBaseName(path);
            const existing = batchManager.items.some(item => batchSourceKey(item.originalPath) === batchSourceKey(path));
            const added = existing ? false : await batchManager.addItem(path, name, (await stat(path)).size);
            return `${added ? labels.addedToBatch : labels.alreadyInBatch}: ${name}`;
        }, labels.addBatchFailed, Boolean(path));
    }

    function toggleBatchSelection(result: SearchResult): void {
        const next = new Set(batchSelection);
        if (next.has(result.doc_id)) next.delete(result.doc_id);
        else next.add(result.doc_id);
        batchSelection = next;
    }

    async function addSelectedToBatch(): Promise<void> {
        // Snapshot documents, not matching chunks. Retry keeps the same targets
        // even if the user changes the current selection while storage is busy.
        const selected = grouped.filter(group => batchSelection.has(group.doc_id)).map(group => group.best);
        const local = new Map<string, { result: SearchResult; path: string }>();
        let skipped = 0;
        for (const result of selected) {
            const path = localPathFromSearchUri(result.location_uri);
            if (path) local.set(batchSourceKey(path), { result, path });
            else skipped += 1;
        }
        await runResultAction('batch:selection', async () => {
            if (local.size === 0) throw new Error(labels.onlyLocalBatch);
            const existing = new Set(batchManager.items.map(item => batchSourceKey(item.originalPath)));
            const entries = await Promise.all([...local.values()]
                .filter(({ path }) => !existing.has(batchSourceKey(path)))
                .map(async ({ result, path }) => ({ path, name: result.filename || pathBaseName(path), size: (await stat(path)).size })));
            const added = await batchManager.addItemsBulk(entries);
            return [
                `${labels.addedToBatch}: ${added}`,
                `${labels.alreadyInBatch}: ${local.size - added}`,
                ...(skipped ? [`${labels.skippedCloud}: ${skipped}`] : []),
            ].join(' · ');
        }, labels.addBatchFailed, local.size > 0);
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
                oninput={event => runSkeletonSearch(event.currentTarget.value)}
                onkeydown={onKeydown}
                aria-label={labels.query}
                placeholder={labels.placeholder}
                title={'Operatoren: AND OR NOT, "Phrase", w/N (proximity), pre/N (ordered), foo* (wildcard), foo~2 (fuzzy), title:karl, headings:foo, body:foo, (Klammern). Großschreibung der Operatoren beliebig.'}
                autofocus
            />
            {#if query}
                <button class="clear-btn" onclick={clearSearch} title={labels.clear} aria-label={labels.clear}><X size={14} /></button>
            {/if}
        </div>
        <button class="search-btn" onclick={runSearch} disabled={loading || !query.trim()}>
            {#if loading}<Loader2 size={15} class="spin" />{:else}<Search size={15} />{/if}
            {labels.search}
        </button>
        <!-- P13.7 Stage M — search the cloud-backup VPS over HTTPS.
             Uses the same query string; the server runs FTS5 + a
             server-side embedding pass and RRF-fuses both arms. -->
        {#if $caps.cloud_backup}
        <button class="filter-toggle" onclick={runRemoteSearch}
                disabled={remoteLoading || !query.trim()}
                title="Search the cloud-backup VPS over HTTPS">
            {#if remoteLoading}<Loader2 size={15} class="spin" />{:else}🌐{/if}
            Cloud
        </button>
        {/if}
        <!-- Stage S — federated search across all backends. -->
        <button class="filter-toggle fed-btn" onclick={runFederatedSearch}
                disabled={fedLoading || !query.trim() || fedBackendsActive.length === 0}
                title="Search all backends (local + cloud-backup + CrispLens) with RRF merge">
            {#if fedLoading}<Loader2 size={15} class="spin" />{:else}🔀{/if}
            Alle
        </button>
        <!-- P21 — search by image similarity via ViT + omni embeddings. -->
        <button class="filter-toggle" onclick={searchByImage}
                disabled={imageSearchLoading}
                title="Search by image similarity (ViT + omni embeddings)">
            {#if imageSearchLoading}<Loader2 size={15} class="spin" />{:else}<ImageIcon size={15} />{/if}
        </button>
        <button
            class="filter-toggle"
            onclick={saveCurrentSearch}
            disabled={!savedSearchesLoaded || savedSearchBusy || !query.trim()}
            aria-busy={savedSearchesLoading || savedSearchBusy}
            title={labels.save}
            aria-label={labels.save}
        >
            <BookmarkPlus size={15} />
        </button>
        <div class="saved-wrap">
            <button
                class="filter-toggle"
                class:active={showSavedDropdown}
                onclick={() => showSavedDropdown = !showSavedDropdown}
                disabled={savedSearches.length === 0}
                title={`${labels.savedList} (${savedSearches.length})`}
                aria-label={labels.savedList}
                aria-expanded={showSavedDropdown}
            >
                <Bookmark size={15} />
                {#if savedSearches.length > 0}
                    <span class="saved-count">{savedSearches.length}</span>
                {/if}
            </button>
            {#if showSavedDropdown && savedSearches.length > 0}
                <div class="saved-dropdown">
                    <p class="saved-help">{labels.restoreHint}</p>
                    {#each savedSearches as s, i}
                        <div class="saved-item">
                            <button
                                class="saved-name"
                                onclick={() => loadSavedSearch(s)}
                                title={`${s.query} · ${activeSearchFilters(s.filters).map(key => labels.filtersLabel[key]).join(", ")}`}
                            >
                                {s.name}
                                <span class="saved-meta">
                                    {s.mode} · {s.limit} · {activeSearchFilters(s.filters).length} {labels.filterCount}
                                    {#if s.docIdScope.length} · {s.docIdScope.length} {labels.scope}{/if}
                                </span>
                            </button>
                            {#if activeSearchFilters(s.filters).length === 0 && s.docIdScope.length === 0 && !s.smart}
                            <button
                                class="saved-context"
                                onclick={() => requestBrowserContext(searchResultsPanel(s.query, `Saved search: ${s.name}`))}
                                title="Open saved search as a virtual folder"
                                aria-label="Open saved search as a virtual folder"
                            >
                                <FolderOpen size={12} />
                            </button>
                            {/if}
                            <button
                                class="saved-del"
                                onclick={() => deleteSavedSearch(i)}
                                disabled={!savedSearchesLoaded || savedSearchBusy}
                                title="Delete saved search"
                            >
                                <Trash2 size={12} />
                            </button>
                        </div>
                    {/each}
                </div>
            {/if}
        </div>
        <div class="saved-wrap">
            <button
                class="filter-toggle"
                class:active={showHistory}
                onclick={() => showHistory = !showHistory}
                disabled={searchHistory.length === 0}
                title={`${labels.history} (${searchHistory.length})`}
                aria-label={labels.history}
                aria-expanded={showHistory}
            >
                <Clock size={15} />
            </button>
            {#if showHistory && searchHistory.length > 0}
                <div class="saved-dropdown history-dropdown">
                    <div class="history-header">
                        <span>{labels.history}</span>
                        <button class="saved-del" onclick={clearHistory} title="Clear all history">
                            <Trash2 size={11} /> Clear
                        </button>
                    </div>
                    {#each searchHistory as h, i}
                        <div class="saved-item">
                            <button
                                class="saved-name"
                                onclick={() => loadHistoryEntry(h)}
                                title="{h.query} ({h.mode})"
                            >
                                {h.query.length > 50 ? h.query.slice(0, 47) + '…' : h.query}
                                <span class="saved-meta">
                                    {h.mode} · {h.resultCount} hits · {activeSearchFilters(h.filters).length} {labels.filterCount} · {new Date(h.ts).toLocaleTimeString()}
                                </span>
                            </button>
                            <button
                                class="saved-del"
                                onclick={() => deleteHistoryEntry(i)}
                                title="Remove"
                            >
                                <X size={11} />
                            </button>
                        </div>
                    {/each}
                </div>
            {/if}
        </div>
        <label class="smart-toggle" title="Smart search: auto-extract year, language, file type, etc. from your query">
            <input type="checkbox" bind:checked={smartSearch} />
            <span class="smart-label">Smart</span>
        </label>
        <button class="filter-toggle" class:active={showFilters}
            onclick={() => showFilters = !showFilters} title={labels.filters}
            aria-label={labels.filters} aria-expanded={showFilters} aria-controls="library-search-filters">
            <SlidersHorizontal size={15} />
        </button>
    </div>

    <!-- ── Mode chips ─────────────────────────────────────────────────────────── -->
    <div class="mode-row">
        {#each (['hybrid', 'text', 'vector'] as const) as m}
            <button class="mode-chip" class:active={mode === m} disabled={!vectorEnabled && m !== 'text'}
                    title={!vectorEnabled && m !== 'text' ? 'Enable vector embeddings in Search Index settings.' : ''}
                    onclick={() => mode = m}>
                {labels[m]}
            </button>
        {/each}
    </div>

    <p class="search-help">{labels.modesHint}</p>
    {#if activeFilterKeys.length || refinementScope.length || smartSearch}
        <div class="active-filters" aria-label={labels.active}>
            {#each activeFilterKeys as key}
                <button class="filter-chip" onclick={() => removeFilter(key)}
                    aria-label={`${labels.remove} ${labels.filtersLabel[key]}`}>
                    {labels.filtersLabel[key]}{typeof currentFilters[key] !== 'boolean' ? `: ${currentFilters[key]}` : ''}
                    <X size={12} />
                </button>
            {/each}
            {#if refinementScope.length}
                <button class="filter-chip" onclick={() => refinementScope = []}>{refinementScope.length} {labels.scope} <X size={12} /></button>
            {/if}
            {#if smartSearch}<button class="filter-chip" onclick={() => smartSearch = false}>Smart <X size={12} /></button>{/if}
            <button class="reset-filters" onclick={resetFilters}>{labels.reset}</button>
        </div>
    {/if}
    {#if savedSearchesLoadError}
        <div class="search-notice" role="alert">
            {labels.restoreFailed} <button class="reset-filters" onclick={restoreBookmarks} disabled={savedSearchesLoading}>{labels.retry}</button>
            <details><summary>{labels.details}</summary>{savedSearchesLoadError}</details>
        </div>
    {/if}
    {#if searchOptionsChanged && !loading}<p class="search-help">{labels.changed}</p>{/if}
    {#if searchNotice}<div class="search-notice" role="status">{searchNotice}</div>{/if}

    <!-- ── Filters ─────────────────────────────────────────────────────────────── -->
    {#if showFilters}
        <section id="library-search-filters" class="search-filters" aria-label={labels.options}>
        <div class="filter-row">
            <label class="filter-field">
                <span>{labels.max}</span>
                <select bind:value={limit} aria-label={labels.max}>
                    {#each [10, 20, 50, 100] as n}<option value={n}>{n}</option>{/each}
                </select>
            </label>
            <label class="filter-field" title="Include hits from drives that aren't currently mounted">
                <input type="checkbox" bind:checked={includeUnmounted} />
                <span>{labels.mounted}</span>
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
        <!-- Advanced filters -->
        <div class="filter-row" style="flex-wrap:wrap;">
            <label class="filter-field" title="Comma-separated file extensions (e.g. pdf,docx)">
                {labels.filtersLabel.ext}
                <input type="text" bind:value={filterExt} placeholder="pdf,docx" style="width:110px;" />
            </label>
            <label class="filter-field" title="ISO 639-1 language code (e.g. en, de)">
                {labels.filtersLabel.language}
                <input type="text" bind:value={filterLang} placeholder="en" style="width:60px;" />
            </label>
            <label class="filter-field" title="Minimum publication year">
                {labels.filtersLabel.yearMin}
                <input type="number" bind:value={filterYearMin} placeholder="2000" style="width:80px;" />
            </label>
            <label class="filter-field" title="Maximum publication year">
                {labels.filtersLabel.yearMax}
                <input type="number" bind:value={filterYearMax} placeholder="2026" style="width:80px;" />
            </label>
            <label class="filter-field" title="Folder prefix filter (path starts with)">
                {labels.filtersLabel.folderPrefix}
                <input type="text" bind:value={filterFolderPrefix} placeholder="/home/user/docs" style="width:160px;" />
            </label>
            <label class="filter-field" title="URL domain filter (e.g. spiegel.de)">
                {labels.filtersLabel.urlDomain}
                <input type="text" bind:value={filterUrlDomain} placeholder="spiegel.de" style="width:120px;" />
            </label>
            <label class="filter-field" title="Tag filter">
                {labels.filtersLabel.tag}
                <input type="text" bind:value={filterTag} placeholder="invoice" style="width:100px;" />
            </label>
            <label class="filter-field" title="Min audio duration (seconds)">
                {labels.filtersLabel.audioMin}
                <input type="number" bind:value={filterAudioDurMin} placeholder="0" style="width:70px;" />
            </label>
            <label class="filter-field" title="Max audio duration (seconds)">
                {labels.filtersLabel.audioMax}
                <input type="number" bind:value={filterAudioDurMax} placeholder="3600" style="width:70px;" />
            </label>
            <label class="filter-field" title="Camera make (e.g. Canon, Nikon)">
                {labels.filtersLabel.cameraMake}
                <input type="text" bind:value={filterCameraMake} placeholder="Canon" style="width:100px;" />
            </label>
            <label class="filter-field" title="Camera model (e.g. EOS R5)">
                {labels.filtersLabel.cameraModel}
                <input type="text" bind:value={filterCameraModel} placeholder="EOS R5" style="width:100px;" />
            </label>
            <label class="filter-field" title="ColBERT late-interaction reranking" style="flex-direction:row; align-items:center; gap:6px;">
                <input type="checkbox" bind:checked={filterColbert} disabled={!crispembedAvailable} />
                {labels.filtersLabel.colbert}
            </label>
            <label class="filter-field" title="Include omni cross-modal search channel (text → image/audio)" style="flex-direction:row; align-items:center; gap:6px;">
                <input type="checkbox" bind:checked={filterOmniSearch} disabled={!crispembedAvailable} />
                {labels.filtersLabel.omni}
            </label>
            <label class="filter-field" title="Fuzzy matching: tolerate typos and OCR errors (~1 edit distance)" style="flex-direction:row; align-items:center; gap:6px;">
                <input type="checkbox" bind:checked={filterFuzzy} />
                {labels.filtersLabel.fuzzy}
            </label>
            <label class="filter-field" title="Expand query terms with offline synonyms (EN + DE)" style="flex-direction:row; align-items:center; gap:6px;">
                <input type="checkbox" bind:checked={filterSynonyms} />
                {labels.filtersLabel.synonyms}
            </label>
        </div>
        <p class="search-help">{labels.nativeOnly}</p>
        <p class="search-help">{labels.catalogHint}</p>
        {#if !crispembedAvailable}<p class="search-help">{labels.nativeOptionsUnavailable}</p>{/if}
        </section>
    {/if}

    {#if resultActionFeedback}
        <div class="result-action-feedback" class:action-error={resultActionFeedback.kind === 'error'}
            role={resultActionFeedback.kind === 'error' ? 'alert' : 'status'}>
            <span>{resultActionFeedback.message}</span>
            {#if resultActionFeedback.retry}
                <button class="reset-filters" onclick={() => void resultActionFeedback?.retry?.()}>{labels.retryAction}</button>
            {/if}
            <button class="action-dismiss" onclick={dismissResultActionFeedback} aria-label={labels.dismissAction} title={labels.dismissAction}>
                <X size={14} />
            </button>
        </div>
    {/if}

    <!-- ── Results ────────────────────────────────────────────────────────────── -->
    <div class="results-and-preview">
    <div class="results-area" class:with-preview={previewing !== null}>
        {#if loading}
            <div class="state-msg" role="status"><Loader2 size={22} class="spin" /> {labels.loading}</div>

        {:else if error}
            <div class="state-msg error" role="alert">
                {error}
                <button class="reset-filters" onclick={runSearch}>{labels.retry}</button>
                {#if error.includes('not initialised') || error.includes('disabled')}
                    <br /><small>Bitte den Index in Einstellungen → Search Index aktivieren und initialisieren.</small>
                {/if}
            </div>

        {:else if searched && grouped.length === 0}
            <div class="state-hint">
                <Search size={32} />
                <p>{labels.noResults}</p><p class="hint-sub">{labels.noResultsHint}</p>
                {#if activeFilterKeys.length || refinementScope.length || smartSearch}
                    <button class="reset-filters" onclick={() => { resetFilters(); void runSearch(); }}>{labels.reset}</button>
                {/if}
            </div>

        {:else if !searched}
            <div class="state-hint">
                <Search size={32} style="color:#3f3f46;" />
                <p>{labels.start}</p>
                <p class="hint-sub">{labels.startHint}</p>
            </div>

        {:else}
            {#if similarHeader}
                <div class="similar-header">
                    Similar to: <strong>{similarHeader}</strong>
                    <button class="clear-similar" onclick={clearSearch}>
                        <X size={12} /> Clear
                    </button>
                </div>
            {/if}
            <div class="result-count">
                {#if searchTags.size > 0}
                    {displayedGroups.length} / {grouped.length} Dokument{grouped.length !== 1 ? 'e' : ''} · {results.length} Treffer
                {:else}
                    {grouped.length} Dokument{grouped.length !== 1 ? 'e' : ''} · {results.length} Treffer
                {/if}
                {#if searchTagFacets.length > 0}
                    <button
                        class="tagcloud-toggle"
                        class:active={showSearchTagCloud}
                        onclick={() => showSearchTagCloud = !showSearchTagCloud}
                        title="Tag-Wolke ein-/ausblenden"
                    >
                        <Tag size={11} /> Tags{#if searchTags.size > 0} ({searchTags.size}){/if}
                    </button>
                {/if}
                {#if grouped.length >= 2}
                    <button
                        class="tagcloud-toggle refine-btn"
                        onclick={refineSearch}
                        title="Search within these {grouped.length} results (progressive narrowing)"
                    >
                        Refine
                    </button>
                {/if}
                {#if !activeFilterKeys.length && !refinementScope.length && !smartSearch}
                <button
                    class="tagcloud-toggle"
                    onclick={openSearchResultsContext}
                    title="Open search results in context"
                >
                    <FolderOpen size={11} /> Context
                </button>
                {/if}
                {#if batchSelection.size > 0}
                    <button class="tagcloud-toggle" onclick={() => void addSelectedToBatch()} disabled={resultActionsBusy.has('batch:selection')} title="Add selected local results to batch sorter">
                        <ListPlus size={11} /> Batch ({batchSelection.size})
                    </button>
                {/if}
                {#if refinementScope.length > 0}
                    <span class="refine-badge" title="Searching within {refinementScope.length} docs from previous results">
                        Scoped to {refinementScope.length} docs
                        <button class="clear-similar" onclick={() => refinementScope = []}>
                            <X size={10} />
                        </button>
                    </span>
                {/if}
            </div>

            {#if showSearchTagCloud && searchTagFacets.length > 0}
                <div class="search-tagcloud-wrap">
                    <TagCloud
                        facets={searchTagFacets}
                        selected={searchTags}
                        ontoggle={toggleSearchTag}
                        onclear={() => searchTags = new Set()}
                    />
                </div>
            {/if}

            {#if compareResults.length > 0}
                <section class="compare-panel" aria-label="Search result comparison">
                    <div class="compare-heading">
                        <strong>Compare results</strong>
                        <span>{compareResults.length}/2 selected</span>
                        <button class="clear-similar" onclick={() => compareResults = []}>Clear</button>
                    </div>
                    {#if compareResults.length < 2}
                        <p class="hint">Select one more result to compare metadata and snippets.</p>
                    {:else}
                        <div class="compare-columns">
                            {#each compareResults as item (`${item.doc_id}:${item.chunk_index}`)}
                                <article>
                                    <strong>{item.title || item.filename || item.doc_id}</strong>
                                    <code title={item.location_uri}>{compareLabel(item.location_uri)}</code>
                                    <dl>
                                        <dt>Score</dt><dd>{item.score.toFixed(3)}</dd>
                                        <dt>Type</dt><dd>{item.ext || '—'}</dd>
                                        <dt>Author</dt><dd>{item.author || '—'}</dd>
                                        <dt>Year</dt><dd>{item.year ?? '—'}</dd>
                                    </dl>
                                    <p>{item.snippet || 'No snippet available.'}</p>
                                </article>
                            {/each}
                        </div>
                    {/if}
                </section>
            {/if}

            {#each displayedGroups as group (group.doc_id)}
                {@const r = group.best}
                {@const resultName = r.title || r.filename || r.doc_id.slice(0, 20) + '…'}
                {@const chunksId = `library-result-chunks-${encodeURIComponent(group.doc_id)}`}
                <div class="result-card">
                    <!-- Doc header -->
                    <div class="result-header">

                        <div class="ext-badge ext-{(r.ext ?? '').toLowerCase()}">{(r.ext ?? '?').toUpperCase()}</div>

                        <div class="result-meta">
                            <span class="result-title">
                                {resultName}
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

                        <div class="result-right" role="group" aria-label={i18n.lang === 'de' ? `Aktionen für ${resultName}` : `Actions for ${resultName}`}>
                            <div class="score-wrap" title="Score {r.score.toFixed(4)}">
                                <div class="score-bar"><div class="score-fill" style="width:{scoreBarPct(r.score)}%"></div></div>
                                <span class="score-label">{r.score.toFixed(3)}</span>
                            </div>
                            {#if group.chunks.length > 1}
                                <button type="button" class="expand-result"
                                    aria-expanded={expanded.has(group.doc_id)} aria-controls={chunksId}
                                    aria-label={i18n.lang === 'de'
                                        ? `${group.chunks.length} passende Textstellen für ${resultName}`
                                        : `${group.chunks.length} matching passages for ${resultName}`}
                                    onclick={() => toggleExpand(group.doc_id)}>
                                    {#if expanded.has(group.doc_id)}<ChevronDown size={14} />{:else}<ChevronRight size={14} />{/if}
                                    {group.chunks.length} {i18n.lang === 'de' ? 'Textstellen' : 'passages'}
                                </button>
                            {/if}
                            <button class="open-btn"
                                class:active={previewing && previewing.doc_id === group.doc_id}
                                aria-pressed={previewing?.doc_id === group.doc_id}
                                onclick={(e) => { e.stopPropagation(); void openPreview(r, e.currentTarget); }}
                                title="Vorschau (Preview)">
                                <Eye size={13} />
                            </button>
                            <button class="open-btn"
                                class:active={compareResults.some((item) => item.doc_id === r.doc_id)}
                                aria-pressed={compareResults.some((item) => item.doc_id === r.doc_id)}
                                onclick={(e) => { e.stopPropagation(); toggleCompare(r); }}
                                title="Compare with another result">
                                ⇄
                            </button>
                            <button class="open-btn"
                                class:active={batchSelection.has(r.doc_id)}
                                aria-pressed={batchSelection.has(r.doc_id)}
                                onclick={(e) => { e.stopPropagation(); toggleBatchSelection(r); }}
                                title={batchSelection.has(r.doc_id) ? 'Remove from batch selection' : 'Select for batch sorter'}>
                                <ListPlus size={13} />
                            </button>
                            <button class="open-btn"
                                disabled={resultActionsBusy.has(`open:${r.location_uri}`)}
                                onclick={(e) => { e.stopPropagation(); openFile(r.location_uri); }}
                                title="Datei öffnen">
                                <ExternalLink size={13} />
                            </button>
                            {#if driveUriParts(r.location_uri) || localPathFromSearchUri(r.location_uri)}
                                <button class="open-btn"
                                    disabled={resultActionsBusy.has(`copy:${r.location_uri}`)}
                                    onclick={(e) => { e.stopPropagation(); void copyResultPath(r.location_uri); }}
                                    title="Copy path">
                                    <Copy size={13} />
                                </button>
                            {/if}
                            {#if driveUriParts(r.location_uri)}
                                <button class="open-btn"
                                    onclick={(e) => { e.stopPropagation(); openDriveInContext(r.location_uri); }}
                                    title="Im Laufwerkskontext öffnen">
                                    <FolderOpen size={13} />
                                </button>
                                <button class="open-btn"
                                    disabled={shareBusy === r.location_uri || shareCapabilities[driveIdFromUri(r.location_uri)] === false}
                                    onclick={(e) => { e.stopPropagation(); void shareDriveFile(r.location_uri); }}
                                    title={shareCapabilities[driveIdFromUri(r.location_uri)] === false ? 'Public sharing is not supported' : 'Share link kopieren'}>
                                    {#if shareBusy === r.location_uri}<Loader2 size={13} class="spin" />{:else}<Share2 size={13} />{/if}
                                </button>
                                <button class="open-btn"
                                    disabled={downloadBusy === r.location_uri}
                                    onclick={(e) => { e.stopPropagation(); void downloadDriveFile(r.location_uri, r.filename); }}
                                    title="Download for offline use">
                                    {#if downloadBusy === r.location_uri}<Loader2 size={13} class="spin" />{:else}<Download size={13} />{/if}
                                </button>
                                <button class="open-btn"
                                    disabled={promoteBusy === r.location_uri}
                                    onclick={(e) => { e.stopPropagation(); void promoteDriveResult(r); }}
                                    title="Promote remote result to full indexing">
                                    {#if promoteBusy === r.location_uri}<Loader2 size={13} class="spin" />{:else}L3{/if}
                                </button>
                                <button class="open-btn"
                                    disabled={mutationBusy === r.location_uri}
                                    onclick={(e) => { e.stopPropagation(); void mutateDriveResult(r.location_uri, 'move'); }}
                                    aria-label="Move cloud file"
                                    title="Move cloud file">
                                    ↗
                                </button>
                                <button class="open-btn"
                                    disabled={mutationBusy === r.location_uri}
                                    onclick={(e) => { e.stopPropagation(); void mutateDriveResult(r.location_uri, 'rename'); }}
                                    aria-label="Rename cloud file"
                                    title="Rename cloud file">
                                    ✎
                                </button>
                                <button class="open-btn danger-btn"
                                    disabled={mutationBusy === r.location_uri}
                                    onclick={(e) => { e.stopPropagation(); void mutateDriveResult(r.location_uri, 'delete'); }}
                                    aria-label="Move cloud file to trash"
                                    title="Move cloud file to trash">
                                    <Trash2 size={13} />
                                </button>
                            {/if}
                            {#if localPathFromSearchUri(r.location_uri)}
                                <button class="open-btn"
                                    onclick={(e) => { e.stopPropagation(); openLocalInContext(r.location_uri); }}
                                    title="Open in file context">
                                    <FolderOpen size={13} />
                                </button>
                                <button class="open-btn"
                                    disabled={resultActionsBusy.has(`batch:${r.doc_id}`)}
                                    onclick={(e) => { e.stopPropagation(); void addResultToBatch(r); }}
                                    title="Add to batch sorter">
                                    <ListPlus size={13} />
                                </button>
                            {/if}
                            {#if cbArchiveOriginalPath(r.location_uri)}
                                <button class="open-btn"
                                    disabled={promoteBusy === r.doc_id}
                                    onclick={(e) => { e.stopPropagation(); void promoteCbArchive(r); }}
                                    aria-label="Promote archive result to local full-text index"
                                    title="Restore and promote to local full-text index">
                                    {#if promoteBusy === r.doc_id}<Loader2 size={13} class="spin" />{:else}L3{/if}
                                </button>
                            {/if}
                            {#if r.filename?.startsWith('wallabag-') && r.filename?.endsWith('.html')}
                                <button class="open-btn" disabled={wallabagReading === r.doc_id}
                                    onclick={(e) => { e.stopPropagation(); void readWallabag(r); }} title="Read indexed article">Read</button>
                            {/if}
                            {#if r.url}
                                <button class="open-btn"
                                    onclick={(e) => { e.stopPropagation(); openOriginal(r.url!); }}
                                    title="Original öffnen (Open original): {r.url}">
                                    <Globe size={13} />
                                </button>
                            {/if}
                            <button class="open-btn similar-btn"
                                onclick={(e) => { e.stopPropagation(); findSimilar(r); }}
                                title="Find similar documents">
                                ≈
                            </button>
                            {#if $caps.desktop}
                            <button class="open-btn"
                                disabled={!r.snippet || resultActionsBusy.has(`export:${r.doc_id}:${r.chunk_index}`)}
                                onclick={(e) => { e.stopPropagation(); exportDoc(r, 'html'); }}
                                title={labels.exportExcerpt}>
                                ⤓
                            </button>
                            {/if}
                            <button class="open-btn"
                                disabled={!r.snippet || resultActionsBusy.has(`reading:${r.doc_id}:${r.chunk_index}`)}
                                onclick={(e) => { e.stopPropagation(); highlightPassage(r); }}
                                title="Add to reading list">
                                ★
                            </button>
                        </div>
                    </div>

                    <!-- P22.3 — Auto-summary (when available, shows above snippet) -->
                    {#if r.summary}
                        <div class="result-summary">
                            {r.summary}
                        </div>
                    {/if}

                    <!-- Best chunk snippet with highlighted terms -->
                    {#if r.snippet}
                        <div class="chunk-preview">
                            <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                            {@html highlightSnippet(r.snippet, query)}{#if r.snippet.length >= 400}…{/if}
                        </div>
                    {:else}
                        <div class="chunk-preview no-snippet">(Kein Textauszug verfügbar)</div>
                    {/if}

                    {#if wallabagBodies.has(r.doc_id)}
                        <details open><summary>Indexed article</summary><pre class="indexed-body">{wallabagBodies.get(r.doc_id)}</pre></details>
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

                    <!-- Per-file document tools: GUI surface for the
                         `ocr --render` / `kie` / `table` CLI. Image/PDF rows
                         only, gated on CrispEmbed being linked in. Each action
                         writes its artifact beside the source file and reports
                         inline. -->
                    {#if looksDocToolable(r)}
                        {@const dt = docTools.get(r.doc_id)}
                        <div class="translate-surface" style="margin-top:6px;">
                            <!-- Only the translate action here is gated in Rust;
                                 search, OCR export and the rest are not. Hence a
                                 notice inside this strip and not over the results. -->
                            <IntendedPurposeGate blocking={false} />
                            <div style="display:flex; gap:6px; flex-wrap:wrap; align-items:center;">
                                <span style="font-size:0.75rem; color:#a1a1aa;">Doc tools:</span>
                                <button type="button" class="translate-btn" disabled={dt?.busy != null}
                                    onclick={() => handleOcrExport(r, 'pdf')}
                                    title="OCR → searchable PDF, written beside the source file">Searchable PDF</button>
                                <button type="button" class="translate-btn" disabled={dt?.busy != null}
                                    onclick={() => handleKie(r)}
                                    title="Extract key fields (uses your Settings → NER labels)">Extract fields</button>
                                <button type="button" class="translate-btn" disabled={dt?.busy != null}
                                    onclick={() => handleTableExtract(r)}
                                    title="Detect a table and export it as HTML beside the source file">Extract table</button>
                            </div>
                            {#if dt?.busy}
                                <div class="translate-loading">
                                    <span class="translate-spinner" aria-hidden="true"></span>
                                    {dt.busy === 'pdf' ? 'Rendering searchable PDF…' : dt.busy === 'kie' ? 'Extracting fields…' : 'Parsing table…'}
                                </div>
                            {/if}
                            {#if dt?.error}
                                <div class="translate-error">Doc tool failed: {dt.error}</div>
                            {/if}
                            {#if dt?.pdf}
                                <div class="translate-result"><div class="translate-meta">
                                    <span class="translate-cached">Saved → {dt.pdf.saved_path} ({dt.pdf.pages} page{dt.pdf.pages === 1 ? '' : 's'})</span>
                                </div></div>
                            {/if}
                            {#if dt?.kie}
                                <div class="translate-result"><div class="translate-meta">
                                    {#if dt.kie.length === 0}
                                        <span class="translate-cached">No fields found.</span>
                                    {:else}
                                        <ul style="margin:4px 0 0; padding-left:16px;">
                                            {#each dt.kie as f}
                                                <li style="font-size:0.8125rem;"><strong>{f.label}:</strong> {f.value} <span style="color:#71717a;">({f.score.toFixed(2)})</span></li>
                                            {/each}
                                        </ul>
                                    {/if}
                                </div></div>
                            {/if}
                            {#if dt?.table}
                                <div class="translate-result"><div class="translate-meta">
                                    <span class="translate-cached">Table {dt.table.rows}×{dt.table.cols}{dt.table.saved_path ? ` — saved → ${dt.table.saved_path}` : ''}</span>
                                </div></div>
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
                                        <!-- Art 50(2): machine translation shown here is synthetic
                                             text and carries the same badge as the Translate panel.
                                             This surface was unmarked until the 2026-08-02 audit —
                                             it invokes `translate_text` directly rather than going
                                             through the AIToolkit client, so the disclosure guard
                                             could not see it. -->
                                        <AiGeneratedBadge compact />
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
                    {#if group.chunks.length > 1}
                        <div class="chunk-list" id={chunksId} hidden={!expanded.has(group.doc_id)}>
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
                                    <button class="chunk-compare"
                                        class:active={compareResults.some((item) => item.doc_id === chunk.doc_id && item.chunk_index === chunk.chunk_index)}
                                        onclick={(e) => { e.stopPropagation(); toggleCompare(chunk); }}
                                        title="Compare this chunk">
                                        ⇄
                                    </button>
                                </div>
                            {/each}
                        </div>
                    {/if}
                </div>
            {/each}
        {/if}
    </div>

    {#if previewing}
        <aside class="preview-pane" aria-label={i18n.lang === 'de' ? 'Dokumentvorschau' : 'Document preview'}>
            <header class="preview-header">
                <span class="preview-title" title={previewing.location_uri}>
                    {previewing.title || previewing.filename || (previewing.doc_id.slice(0, 24) + '…')}
                </span>
                <button class="preview-close" bind:this={previewCloseButton} onclick={closePreview}
                    title="Vorschau schließen" aria-label={i18n.lang === 'de' ? 'Vorschau schließen' : 'Close preview'}>
                    <X size={14} />
                    <span class="preview-close-label">{i18n.lang === 'de' ? 'Schließen' : 'Close preview'}</span>
                </button>
            </header>
            {#if localPathFromSearchUri(previewing.location_uri)}
                <div class="document-actions">
                    {#each availableDocumentTools(previewing.filename || previewing.location_uri, $caps) as tool}
                        <button onclick={() => { if (previewing) documentToolRequest.set({ tool, locationUri: previewing.location_uri, filename: previewing.filename || '' }); }}>{tool === 'pdf' ? 'Edit PDF' : tool === 'ocr' ? 'OCR this document' : 'Translate this document'}</button>
                    {/each}
                </div>
            {/if}
            <div class="preview-body">
                <DocumentDecisionPanel document={previewing} onUse={useReviewedFields} />
                <FieldEvidencePanel docId={previewing.doc_id}
                    onFilter={findByField}
                    onSort={useReviewedFields} />
                <DocumentViewer
                    locationUri={previewing.location_uri}
                    filename={previewing.title || previewing.filename || ''}
                />
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
                {#if remoteResults.length > 0}
                    <button class="open-btn" onclick={openRemoteSearchContext} title="Open remote results in context">
                        <FolderOpen size={13} /> Context
                    </button>
                {/if}
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
                                    <button class="open-btn"
                                            onclick={() => promoteRemoteRow(hit)}
                                            disabled={remotePromoteBusy === hit.sha256}
                                            title="Download, extract, and add to the local index">
                                        {#if remotePromoteBusy === hit.sha256}<Loader2 size={13} class="spin" />{/if}
                                        Download & index
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

    <!-- Stage W — skeleton hint panel -->
    {#if skeletonVisible}
        <section class="skeleton-hints">
            <header class="skeleton-header">
                <span class="skeleton-badge">✦ Local hints</span>
                <span class="skeleton-meta">(from skeleton index — instant, no remote call)</span>
            </header>
            {#if skeletonAuthors.length > 0}
                <div class="skeleton-group">
                    <span class="skeleton-group-label">Authors</span>
                    {#each skeletonAuthors as h}
                        <span class="skeleton-chip">{h.name} <em>{h.doc_count}</em></span>
                    {/each}
                </div>
            {/if}
            {#if skeletonParentDirs.length > 0}
                <div class="skeleton-group">
                    <span class="skeleton-group-label">Folders</span>
                    {#each skeletonParentDirs as h}
                        <span class="skeleton-chip">{h.name} <em>{h.doc_count}</em></span>
                    {/each}
                </div>
            {/if}
        </section>
    {/if}

    <!-- Stage S — federated search panel -->
    {#if fedSearched}
        <section class="fed-results">
            <header class="remote-header">
                <strong>🔀 Federated results</strong>
                {#if fedLoading}<Loader2 size={14} class="spin" />{/if}
                <span class="remote-meta">
                    {#if fedTags.size > 0}{displayedFedResults.length} / {/if}{fedResults.length} hit(s) across {fedBackendsActive.join(', ')}
                </span>
                {#if fedTagFacets.length > 0}
                    <button
                        class="tagcloud-toggle"
                        class:active={showFedTagCloud}
                        onclick={() => showFedTagCloud = !showFedTagCloud}
                        title="Tag-Wolke ein-/ausblenden"
                    >
                        <Tag size={11} /> Tags{#if fedTags.size > 0} ({fedTags.size}){/if}
                    </button>
                {/if}
                <!-- backend filter checkboxes -->
                <span class="fed-toggles">
                    {#each fedBackendsAvailable as b}
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
            {#if showFedTagCloud && fedTagFacets.length > 0}
                <div class="search-tagcloud-wrap">
                    <TagCloud
                        facets={fedTagFacets}
                        selected={fedTags}
                        ontoggle={toggleFedTag}
                        onclear={() => fedTags = new Set()}
                    />
                </div>
            {/if}
            {#if fedResults.length === 0 && !fedLoading}
                <p class="hint">No federated results — run a search or enable more backends.</p>
            {:else}
                <ul class="remote-list">
                    {#each displayedFedResults as hit (hit.id)}
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
                                {#if hit.url}
                                    <button class="open-btn" onclick={() => openOriginal(hit.url!)} title="Open original article">Open original</button>
                                {/if}
                                {#if hit.source === 'cloud_backup' && hit.sha256}
                                    <button class="open-btn" onclick={() => readFederatedBody(hit)} disabled={fedBodyLoading === hit.sha256}>Read indexed text</button>
                                    {#if fedBody.has(hit.sha256)}
                                        <details open><summary>Indexed text</summary><pre class="indexed-body">{fedBody.get(hit.sha256)}</pre></details>
                                    {/if}
                                {/if}
                                {#if hit.tags?.length}
                                    <div class="remote-path">{hit.tags.filter(t => !t.startsWith('collection:')).join(' · ')}</div>
                                {/if}
                                {#if hit.path}
                                    <div class="remote-path">{hit.path}</div>
                                {/if}
                                {#if hit.snippet}
                                    <!-- The cb-api snippet may carry server-side
                                         <mark> tags; strip them and re-highlight
                                         client-side via the XSS-safe path
                                         (highlightSnippet HTML-escapes first). -->
                                    <div class="remote-snippet">{@html highlightSnippet(hit.snippet.replace(/<\/?mark>/g, ''), query)}</div>
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
    .document-actions { display: flex; gap: 6px; padding: 8px; flex-wrap: wrap; }
    .document-actions button { background: #27272a; color: #eee; border: 1px solid #52525b; border-radius: 4px; padding: 5px; }

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
    .indexed-body { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 32rem; overflow-y: auto; }
    .remote-snippet { margin-top: 4px; font-size: 0.9em; color: var(--color-text-secondary, #ccc); }
    .remote-actions { margin-top: 6px; display: flex; gap: 10px; align-items: center; }
    .remote-dl-ok { color: var(--color-success, #2a8); font-size: 0.85em; }
    .remote-dl-err { color: var(--color-danger, #d44); font-size: 0.85em; }
    .search-root {
        display: flex; flex-direction: column; height: 100%;
        background: #09090b; color: #fafafa; padding: 20px;
        box-sizing: border-box; gap: 12px; overflow: hidden; min-width: 0; min-height: 0;
    }

    .search-help { margin: 0; font-size: .78rem; color: #a1a1aa; }
    .search-filters { flex-shrink: 0; max-height: 34vh; overflow-y: auto; display: grid; gap: 10px; padding: 10px; border: 1px solid #3f3f46; border-radius: 8px; }
    .active-filters { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; max-height: 110px; overflow-y: auto; flex-shrink: 0; }
    .filter-chip, .reset-filters { display: inline-flex; align-items: center; gap: 6px; border: 1px solid #3f3f46; border-radius: 6px; background: #18181b; color: #e4e4e7; padding: 5px 9px; font-size: .75rem; cursor: pointer; overflow-wrap: anywhere; }
    .filter-chip:hover, .reset-filters:hover { background: #27272a; border-color: #818cf8; }
    .reset-filters { color: #c7d2fe; }
    .search-notice { padding: 9px 12px; background: #182235; color: #bfdbfe; border: 1px solid #334155; border-radius: 6px; font-size: .8rem; flex-shrink: 0; overflow-wrap: anywhere; }
    .result-action-feedback { display: flex; align-items: center; gap: 8px; padding: 9px 12px; background: #102a22; color: #bbf7d0; border: 1px solid #285c45; border-radius: 6px; font-size: .8rem; flex-shrink: 0; flex-wrap: wrap; }
    .result-action-feedback > span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
    .result-action-feedback.action-error { background: #321c22; color: #fecdd3; border-color: #74323f; }
    .action-dismiss { display: inline-flex; padding: 5px; border: 0; border-radius: 4px; background: transparent; color: inherit; cursor: pointer; }
    .action-dismiss:hover { background: #ffffff16; }
    .saved-help { color: #a1a1aa; margin: 7px 9px; font-size: .75rem; }
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
    .saved-context {
        background: none;
        border: none;
        cursor: pointer;
        color: #71717a;
        padding: 4px;
    }
    .saved-context:hover { color: #60a5fa; }

    /* PLAN P7.3 — live preview pane. Slides in from the right when a
       result row's eye-icon is clicked; results-area shrinks to share
       the viewport. PDF / image render natively via tauri.convertFileSrc;
       text reads the file via the fs plugin. */
    .results-and-preview {
        display: flex;
        flex: 1;
        gap: 12px;
        overflow: hidden;
        min-height: 0; min-width: 0;
    }
    .results-area.with-preview { flex: 1; min-width: 0; }
    .preview-pane {
        flex: 1 1 360px;
        max-width: 50%;
        min-width: 0; min-height: 0;
        display: flex;
        flex-direction: column;
        background: #18181b;
        border: 1px solid #3f3f46;
        border-radius: 8px;
        overflow: hidden;
    }
    .preview-header {
        display: flex; align-items: center; justify-content: space-between;
        flex-shrink: 0; min-width: 0;
        padding: 8px 12px;
        background: #27272a;
        border-bottom: 1px solid #3f3f46;
        font-size: 0.85rem;
    }
    .preview-title {
        flex: 1; min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        color: #fafafa;
        font-weight: 600;
        margin-right: 8px;
    }
    .preview-close {
        background: none; border: none; cursor: pointer;
        color: #a1a1aa; padding: 4px; display: inline-flex; align-items: center;
        justify-content: center; gap: 5px; flex-shrink: 0; min-width: 32px; min-height: 32px;
    }
    .preview-close-label { display: none; font-size: .75rem; }
    .preview-close:hover { color: #fafafa; }
    .preview-body {
        flex: 1; min-width: 0; min-height: 0;
        overflow: auto;
        overflow-wrap: anywhere;
        background: #0a0a0c;
    }
    .preview-body :global(.document-viewer) { min-height: 240px; }
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

    .query-bar { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; flex-shrink: 0; min-width: 0; }
    .query-input-wrap {
        flex: 1 1 240px; display: flex; align-items: center; gap: 8px; min-width: 0;
        background: #18181b; border: 1px solid #3f3f46; border-radius: 8px; padding: 0 10px;
        transition: border-color 0.2s;
    }
    .query-input-wrap:focus-within { border-color: #3b82f6; }
    :global(.query-icon) { color: #71717a; flex-shrink: 0; }
    .query-input {
        flex: 1; min-width: 0; background: transparent; border: none; outline: none;
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

    .results-area { flex: 1; min-width: 0; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 8px; }

    .state-msg { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; color: #71717a; text-align: center; }
    .state-msg.error { color: #f87171; }
    .state-hint { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; color: #52525b; }
    .hint-sub { font-size: 0.8rem; color: #3f3f46; }
    .result-count { font-size: 0.75rem; color: #71717a; display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
    .tagcloud-toggle {
        display: inline-flex; align-items: center; gap: 4px;
        padding: 2px 8px; border: 1px solid #3f3f46; border-radius: 999px;
        background: #27272a; color: #d4d4d8; cursor: pointer; font-size: 0.72rem;
    }
    .tagcloud-toggle:hover { background: #3f3f46; }
    .tagcloud-toggle.active { background: #1d4ed8; border-color: #2563eb; color: #fff; }
    .search-tagcloud-wrap { margin: 6px 0 4px; }
    .compare-panel { margin: 8px 0; padding: 10px; border: 1px solid #3f3f46; border-radius: 8px; background: #151518; }
    .compare-heading { display: flex; align-items: center; gap: 10px; color: #d4d4d8; font-size: .78rem; }
    .compare-heading span { color: #a1a1aa; }
    .compare-heading .clear-similar { margin-left: auto; }
    .compare-columns { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; margin-top: 8px; }
    .compare-columns article { min-width: 0; padding: 8px; border: 1px solid #27272a; border-radius: 6px; background: #18181b; }
    .compare-columns article > strong, .compare-columns article > code { display: block; overflow-wrap: anywhere; }
    .compare-columns article > code { margin-top: 4px; color: #a1a1aa; font-size: .68rem; }
    .compare-columns dl { display: grid; grid-template-columns: auto 1fr; gap: 3px 8px; margin: 8px 0; font-size: .7rem; }
    .compare-columns dt { color: #71717a; } .compare-columns dd { margin: 0; text-align: right; }
    .compare-columns p { margin: 0; color: #d4d4d8; font-size: .74rem; white-space: pre-wrap; overflow-wrap: anywhere; }
    @media (max-width: 700px) { .compare-columns { grid-template-columns: 1fr; } }

    .result-card {
        background: #18181b; border: 1px solid #27272a; border-radius: 8px; overflow: hidden; flex-shrink: 0; min-width: 0; overflow-wrap: anywhere;
    }
    .result-card:hover { border-color: #3f3f46; }

    .result-header {
        display: flex; align-items: flex-start; gap: 10px; flex-wrap: wrap;
        padding: 12px 14px;
    }

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

    .result-meta { flex: 1 1 160px; min-width: 0; }
    .result-title { display: block; font-size: 0.9rem; font-weight: 600; color: #e4e4e7; overflow-wrap: anywhere; }
    .result-byline { display: block; font-size: 0.78rem; color: #a1a1aa; margin-top: 2px; overflow-wrap: anywhere; }
    .result-path { display: block; font-size: 0.72rem; color: #52525b; margin-top: 2px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

    .result-right { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; flex: 0 1 auto; min-width: 0; max-width: 100%; }
    .score-wrap { display: flex; align-items: center; gap: 5px; }
    .score-bar { width: 40px; height: 4px; background: #27272a; border-radius: 2px; }
    .score-fill { height: 100%; background: #3b82f6; border-radius: 2px; }
    .score-label { font-size: 0.7rem; color: #71717a; font-variant-numeric: tabular-nums; }
    .expand-result { display: inline-flex; align-items: center; gap: 4px; min-height: 32px; padding: 4px 7px;
        border: 1px solid #3f3f46; border-radius: 5px; background: transparent; color: #a1a1aa; font-size: .75rem; cursor: pointer; }
    .expand-result:hover { color: #93c5fd; border-color: #3b82f6; }

    .open-btn { display: inline-flex; align-items: center; justify-content: center; min-width: 32px; min-height: 32px;
        background: none; border: none; color: #a1a1aa; cursor: pointer; padding: 4px; border-radius: 4px; }
    .open-btn:hover { color: #3b82f6; background: #3b82f622; }
    .open-btn.danger-btn:hover { color: #ef4444; background: #ef444422; }
    .open-btn:focus-visible, .expand-result:focus-visible, .preview-close:focus-visible { outline: 2px solid #60a5fa; outline-offset: 2px; }

    .chunk-preview {
        padding: 8px 14px 12px 52px;
        font-size: 0.8rem; color: #a1a1aa; line-height: 1.55;
        border-top: 1px solid #1c1c1f; overflow-wrap: anywhere;
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
    .chunk-text { flex: 1; min-width: 0; font-size: 0.78rem; color: #a1a1aa; line-height: 1.45; overflow-wrap: anywhere; }
    .chunk-score { font-size: 0.7rem; color: #52525b; flex-shrink: 0; }
    .chunk-compare { border: 1px solid #3f3f46; border-radius: 4px; padding: 2px 5px; color: #a1a1aa; background: transparent; cursor: pointer; flex-shrink: 0; }
    .chunk-compare.active { color: #93c5fd; border-color: #2563eb; background: #1e3a8a33; }

    @media (max-width: 800px) {
        .search-root { padding: 12px; gap: 10px; }
        .query-input-wrap { flex-basis: 100%; }
        .query-input { font-size: 1rem; }
        .result-right { flex-basis: 100%; }
        .open-btn, .expand-result, .preview-close { min-width: 44px; min-height: 44px; }
        .chunk-preview, .translate-surface { padding-left: 14px; }
        .results-area.with-preview { display: none; }
        .preview-pane { flex: 1; width: 100%; max-width: 100%; }
        .preview-close-label { display: inline; }
    }

    :global(mark) { background: #854d0e55; color: #fbbf24; border-radius: 2px; padding: 0 1px; }
    :global(.spin) { animation: spin 1s linear infinite; display: inline-flex; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

    /* Stage W — skeleton hint panel */
    .skeleton-hints {
        padding: 8px 16px;
        border-bottom: 1px solid #27272a;
        background: #18181b;
    }
    .skeleton-header {
        display: flex; align-items: center; gap: 8px; margin-bottom: 6px;
    }
    .skeleton-badge {
        font-size: 0.7rem; font-weight: 600; color: #a78bfa;
        background: #3b204d40; padding: 2px 6px; border-radius: 4px;
        letter-spacing: 0.03em;
    }
    .skeleton-meta { font-size: 0.65rem; color: #52525b; }
    .skeleton-group { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 5px; align-items: center; }
    .skeleton-group-label {
        font-size: 0.65rem; color: #71717a; min-width: 46px; text-align: right; margin-right: 4px;
    }
    .skeleton-chip {
        font-size: 0.72rem; background: #27272a; color: #d4d4d8;
        padding: 2px 8px; border-radius: 12px; white-space: nowrap;
    }
    .skeleton-chip em { color: #a78bfa; font-style: normal; margin-left: 4px; }

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

    /* P22.2 — "More Like This" */
    .similar-btn {
        font-weight: 700;
        font-size: 0.9em;
        letter-spacing: -0.5px;
    }
    .similar-header {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 6px 10px;
        background: #1e293b;
        border: 1px solid #334155;
        border-radius: 6px;
        font-size: 0.85rem;
        color: #94a3b8;
        margin-bottom: 4px;
    }
    .clear-similar {
        display: inline-flex; align-items: center; gap: 3px;
        background: none; border: none; cursor: pointer;
        color: #64748b; font-size: 0.75rem; padding: 2px 6px;
        border-radius: 4px;
    }
    .clear-similar:hover { background: #334155; color: #e2e8f0; }

    /* P22.3 — Summary display */
    .result-summary {
        padding: 4px 12px 2px;
        font-size: 0.8rem;
        color: #a1a1aa;
        font-style: italic;
        border-left: 2px solid #3b82f6;
        margin: 4px 12px 0;
    }

    /* P22.4 — Smart search toggle */
    .smart-toggle {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        padding: 4px 8px;
        cursor: pointer;
        border-radius: 6px;
        font-size: 0.75rem;
        color: #a1a1aa;
        border: 1px solid transparent;
        transition: all 0.15s;
    }
    .smart-toggle:has(input:checked) {
        color: #60a5fa;
        border-color: #3b82f6;
        background: rgba(59, 130, 246, 0.1);
    }
    .smart-toggle input { width: 14px; height: 14px; margin: 0; cursor: pointer; }
    .smart-label { user-select: none; font-weight: 500; }

    /* P24.2 — Search history dropdown */
    .history-dropdown { min-width: 320px; max-width: 480px; }
    .history-header {
        display: flex; justify-content: space-between; align-items: center;
        padding: 6px 8px; border-bottom: 1px solid #27272a;
        font-size: 0.75rem; color: #71717a;
    }
    .history-header button {
        display: inline-flex; align-items: center; gap: 3px;
        font-size: 0.65rem;
    }

    /* P23 — Refine / search-within-results */
    .refine-btn { border: 1px solid #334155; }
    .refine-btn:hover { background: #1e293b; }
    .refine-badge {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        padding: 2px 8px;
        background: rgba(59, 130, 246, 0.15);
        border: 1px solid #3b82f6;
        border-radius: 10px;
        font-size: 0.7rem;
        color: #60a5fa;
    }
</style>
