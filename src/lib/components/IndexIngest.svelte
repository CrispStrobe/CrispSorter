<script lang="ts">
    import { invoke, convertFileSrc } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';
    import { open as openDialog } from '@tauri-apps/plugin-dialog';
    import { openPath } from '@tauri-apps/plugin-opener';
    import { readDir, readFile, readTextFile, stat, type DirEntry } from '@tauri-apps/plugin-fs';
    import { load as storeLoad } from '@tauri-apps/plugin-store';
    import { getSetting, saveSetting } from '$lib/store';
    import { onMount } from 'svelte';
    import { i18n } from '$lib/i18n.svelte';
    import {
        FolderOpen, Folder, FileText, RefreshCw, Play, Pause, X,
        CheckCircle2, AlertCircle, Loader2, ChevronDown, ChevronRight,
        UploadCloud, Trash2, Database, Search, ExternalLink, HardDrive, CopyCheck,
        Columns2, Eye, RotateCcw, CloudDownload, Images, Tag
    } from 'lucide-svelte';
    import { extractText, AUDIO_EXTENSIONS, MULTIMODAL_EXTENSIONS } from '$lib/extractors/index';
    import IndexSearch from './IndexSearch.svelte';
    import TagCloud from './TagCloud.svelte';
    import CafCatalog from './Catalog.svelte';
    import Duplicates from './Duplicates.svelte';
    import { logInfo, logWarn, logError } from '$lib/log';

    // ── Types ──────────────────────────────────────────────────────────────────

    type FileStatus = 'pending' | 'extracting' | 'embedding' | 'done' | 'error' | 'skipped';

    interface IngestEntry {
        id:        string;   // String(rowId) for SQLite-backed entries
        rowId?:    number;   // SQLite file_queue row id
        path:      string;
        filename:  string;
        ext:       string;
        size:      number;
        status:    FileStatus;
        error?:    string;
        chunks?:   number;
        embedMs?:  number;
        writeMs?:  number;
        chunksDone?: number;
        chunksTotal?: number;
    }

    // Returned by jobs_list_files (display view)
    interface ListedFile {
        rowId:       number;
        jobId:       string;
        filePath:    string;
        docId:       string | null;
        targetLevel: number;
        status:      string;
        errorText:   string | null;
        retryCount:  number;
    }

    // Returned by jobs_claim_batch (execution)
    interface QueuedFile {
        rowId:       number;
        jobId:       string;
        filePath:    string;
        docId:       string | null;
        targetLevel: number;
        retryCount:  number;
    }

    interface ManagedFolder {
        path:        string;
        addedAt:     number;
        lastScanned: number | null;
        fileCount:   number;
    }

    type Tab = 'overview' | 'search' | 'add' | 'sources' | 'cafCatalog' | 'duplicates' | 'images' | 'cidxArchive';

    // ── State ──────────────────────────────────────────────────────────────────

    /** Default tab is 'overview' (browse what's already in the catalog) — the
     *  Stapel/sorter flow handles "import unsorted files". The Hinzufügen tab
     *  is for users who want to add files DIRECTLY to the catalog without
     *  going through the AI sort step. */
    let activeTab   = $state<Tab>('overview');
    let entries     = $state<IngestEntry[]>([]);
    let running     = $state(false);
    let paused      = $state(false);
    let abortCtrl   = $state<AbortController | null>(null);
    let dropActive  = $state(false);

    let folders     = $state<ManagedFolder[]>([]);
    let scanningFolder = $state<string | null>(null);

    // ── P11: registered cloud drives (manifest-only ingest) ────────────────
    interface RegisteredDrive {
        id:    string;
        label: string;
        kind:  string;
        path:  string;
        username?:    string | null;
        password?:    string | null;
        insecure_tls?: boolean | null;
    }
    let driveDialogOpen   = $state(false);
    let availableDrives   = $state<RegisteredDrive[]>([]);
    let driveDialogId     = $state<string>('');     // selected drive id
    let driveDialogPath   = $state<string>('/');    // remote subtree
    let driveDialogExts   = $state<string>('');     // comma-sep exts; '' = all
    let driveDialogDepth  = $state<number | null>(null);
    let driveScanning     = $state(false);
    let driveScanResult   = $state<string>('');

    // Inline "create / edit drive" form.  When `driveEditId` is set we're
    // editing the existing drive with that id (calls drive_update); when
    // null/empty we're creating a new one (calls drive_create).
    let driveCreateOpen   = $state(false);
    let driveEditId       = $state<string | null>(null);
    let driveCreateLabel  = $state<string>('');
    let driveCreateKind   = $state<'local' | 'filen' | 'internxt' | 'sftp' | 'webdav'>('webdav');
    let driveCreatePath   = $state<string>('');
    let driveCreateUser   = $state<string>('');
    let driveCreatePass   = $state<string>('');
    let driveCreateInsecure = $state(false);
    let driveCreateBusy   = $state(false);
    let driveCreateError  = $state<string>('');
    let driveDeletingId   = $state<string | null>(null);

    /** Active durable job id in crisp_jobs.db (null = no active job yet). */
    let activeJobId = $state<string | null>(null);

    /** Ingest depth requested by the user.
     *   L1 = filesystem metadata only (fast — path/size/date),
     *   L2 = embedded file metadata (PDF Info / DOCX core / EPUB OPF / EXIF),
     *   L3 = extract text + embed (deep, slow).
     *  Default is L1 — the fastest path, doesn't require the search index
     *  or any embedder model to be set up. */
    let ingestLevel = $state<1 | 2 | 3>(1);
    let l1Running   = $state(false);
    let l2RunningInline = $state(false);

    let contents    = $state<any[]>([]);
    let contentsLoading = $state(false);
    let contentsQuery = $state('');
    let contentsExt = $state<Set<string>>(new Set());
    let contentsLevel = $state<'all' | 1 | 3>('all');
    let contentsCompleteness = $state<'any' | 'has_author' | 'has_title' | 'has_year' | 'has_all'>('any');
    /** Subtree filter: only show docs whose path starts with this prefix.
     *  Empty string = no filter. Matched against the resolved local path
     *  (after stripping any `crisp+local://` scheme prefix). */
    let contentsFolder = $state<string>('');
    // Tier 2 tag-cloud — opt-in sidebar, hidden by default. `contentsTags`
    // is the AND-combined selection pushed into the DocumentFilter; the
    // facet list is fetched on demand only while the cloud is visible.
    let contentsTags = $state<Set<string>>(new Set());
    let showTagCloud = $state(false);
    let tagFacets = $state<{ tag: string; count: number }[]>([]);
    let tagFacetsLoading = $state(false);
    let indexStats  = $state<{ total_rows: number; doc_count: number; chunk_count: number } | null>(null);
    let selectedDocIds = $state<Set<string>>(new Set());
    let deletingIds = $state<Set<string>>(new Set());
    let promotingL2 = $state(false);

    // PLAN P9 step 2 — paginated browse via index_query_documents.
    type SortColumn = 'filename' | 'title' | 'author' | 'year' | 'language' | 'indexed_at';
    let sortColumn  = $state<SortColumn>('indexed_at');
    let sortDir     = $state<'asc' | 'desc'>('desc');
    /** Total rows matching the current filter, regardless of page.
     *  Server-side via `count_rows`; cheap. */
    let totalEstimate = $state(0);
    /** Opaque cursor returned by `index_query_documents`; null on the
     *  last page. */
    let nextCursor    = $state<string | null>(null);
    /** Last clicked row index for shift-click range selection on the
     *  Übersicht table. Reset whenever the result set changes. */
    let lastClickedDocIdx = $state<number | null>(null);

    // ── P13 slice A1 — images tab state ────────────────────────────────────
    // Tier 1 only: we filter the existing index to image rows and render
    // a placeholder grid. Thumbnails land in slice A2; EXIF surfacing in
    // A2/A4. The page shape mirrors `index_query_documents` so when the
    // pHash near-dup view (A4) joins this view we keep one cursor model.
    interface ImageRow {
        docId:       string;
        locationUri: string;
        filename?:   string | null;
        ext?:        string | null;
        size?:       number | null;
        indexedAt:   number;
    }
    let imagesRows       = $state<ImageRow[]>([]);
    let imagesTotal      = $state(0);
    let imagesLoading    = $state(false);
    let imagesNextCursor = $state<string | null>(null);

    // A2: lazy-loaded thumbnail bytes per docId.  Value is either an
    // object-URL (data ready), the literal 'loading', or 'error'.
    // Reactive map so the tile re-renders on transition; we never
    // mutate in place to keep Svelte's $state proxy invalidating
    // properly.
    let imagesThumbs = $state<Record<string, string>>({});

    // A2: click-to-preview pane state.  Mirrors the Übersicht
    // preview-pane shape but lives in its own variables so the two
    // panes don't share state across tabs.
    interface ImagesExifSummary {
        cameraMake?:    string | null;
        cameraModel?:   string | null;
        lensModel?:     string | null;
        takenAt?:       string | null;
        takenAtUnix?:   number | null;
        width?:         number | null;
        height?:        number | null;
        fNumber?:       number | null;
        exposureTime?:  string | null;
        iso?:           number | null;
        focalLengthMm?: number | null;
        gpsLat?:        number | null;
        gpsLon?:        number | null;
        orientation?:   number | null;
    }
    let imagesPreview         = $state<ImageRow | null>(null);
    let imagesPreviewSrc      = $state<string>('');
    let imagesPreviewExif     = $state<ImagesExifSummary | null>(null);
    let imagesPreviewExifLoading = $state(false);
    let imagesPreviewExifError = $state<string>('');

    // P13 follow-up — CrispLens face overlays.  When the preview
    // opens for a local image AND Tier 2 is authenticated, we
    // sha256 the file → resolve to a CrispLens image_id → fetch
    // face crops → render bbox rectangles as absolute-positioned
    // divs on the displayed image.  All four fields default to the
    // "nothing to overlay" state so the no-CrispLens / no-match
    // path is silent.
    interface CrispLensFaceOverlay {
        faceId:     number;
        bbox:       { top: number; right: number; bottom: number; left: number };
        personName: string | null;
    }
    let imagesPreviewFaces        = $state<CrispLensFaceOverlay[]>([]);
    let imagesPreviewFacesLoading = $state(false);
    let imagesPreviewFacesError   = $state<string>('');

    // A3: SHA-256 dup-view state.  When `imagesDupMode` is true the
    // grid switches to grouped rendering and the linear list is
    // hidden.  Toggle is independent of the preview pane so the user
    // can drill into any duplicate without losing dup mode.
    interface ImageDuplicateGroup {
        sourceHash: string;
        items:      ImageRow[];
    }
    let imagesDupMode    = $state(false);
    let imagesDupGroups  = $state<ImageDuplicateGroup[]>([]);
    let imagesDupLoading = $state(false);
    let imagesDupError   = $state<string>('');

    // A4: pHash near-duplicate state.  Mutually exclusive with the
    // SHA-256 dup mode above — the toolbar enforces "only one
    // alternative view active at a time" to keep the grid layout
    // simple.  Threshold is tunable; default 8 per the spec.
    interface NearDupItem {
        image:           ImageRow;
        phashHex:        string;     // 16-char zero-padded lower-case hex
        distanceFromRep: number;
    }
    interface NearDupGroup {
        representativePhashHex: string;
        items:                  NearDupItem[];
    }
    let imagesNearDupMode      = $state(false);
    let imagesNearDupGroups    = $state<NearDupGroup[]>([]);
    let imagesNearDupLoading   = $state(false);
    let imagesNearDupError     = $state<string>('');
    let imagesNearDupThreshold = $state(8);

    // ── P13/B1 — CrispLens (Tier 2) settings + auth state ──────────────────
    interface ImagesCrispLensSettings {
        backend:          'local' | 'crisplens';
        url:              string;
        thumbnailSizePx:  number;
        phashThreshold:   number;
    }
    let crispLensSettings        = $state<ImagesCrispLensSettings | null>(null);
    let crispLensSettingsOpen    = $state(false);
    let crispLensAuthenticated   = $state(false);

    // P13/B4 — health monitor state.  The Tauri-side
    // `images_crisplens_status` rolls health + auth into one
    // payload so the banner state machine sees one shape per poll.
    interface CrispLensStatus {
        tier2Configured:   boolean;
        healthOk:          boolean | null;
        healthVersion?:    string | null;
        healthBackend?:    string | null;
        healthModelReady?: boolean | null;
        authenticated:     boolean;
        username?:         string | null;
        role?:             string | null;
        error?:            string;
    }
    let crispLensStatus     = $state<CrispLensStatus | null>(null);
    let crispLensPollHandle = $state<ReturnType<typeof setInterval> | null>(null);

    // ── P13/B3 — CrispLens People view state ──────────────────────────────
    interface CrispLensPerson {
        id:              number;
        name:            string;
        appearances?:    number | null;
        faceCount?:      number | null;
        sampleImagePath?: string | null;
        sampleImageId?:   number | null;
        firstSeen?:      string | null;
        lastSeen?:       string | null;
        createdAt?:      string;
    }
    let crispLensPeople        = $state<CrispLensPerson[]>([]);
    let crispLensPeopleLoading = $state(false);
    let crispLensPeopleError   = $state<string>('');
    let imagesPeopleMode       = $state(false);

    async function loadCrispLensPeople() {
        if (crispLensPeopleLoading) return;
        crispLensPeopleLoading = true;
        crispLensPeopleError = '';
        try {
            crispLensPeople = await invoke<CrispLensPerson[]>('images_crisplens_people');
        } catch (e: any) {
            crispLensPeopleError = String(e?.message ?? e ?? 'failed to load people');
            crispLensPeople = [];
        } finally {
            crispLensPeopleLoading = false;
        }
    }

    function toggleImagesPeopleMode() {
        imagesPeopleMode = !imagesPeopleMode;
        if (imagesPeopleMode) {
            // Mutual exclusion with the dup-view modes — only one
            // alternative grid layout active at a time.
            imagesDupMode = false; imagesDupGroups = [];
            imagesNearDupMode = false; imagesNearDupGroups = [];
            void loadCrispLensPeople();
        }
    }

    // ── P13/B2 — CrispLens text-search state (filename/person-name only) ───
    interface CrispLensSearchHit {
        id:           number;
        filename:     string;
        filepath:     string;
        takenAt?:     string | null;
        faceCount?:   number | null;
        description?: string | null;
        tags?:        string[];
        recognitionConfidence?: number | null;
    }
    let crispLensSearchQuery   = $state('');
    let crispLensSearchHits    = $state<CrispLensSearchHit[]>([]);
    let crispLensSearchLoading = $state(false);
    let crispLensSearchError   = $state<string>('');

    async function runCrispLensSearch() {
        const q = crispLensSearchQuery.trim();
        if (!q) {
            crispLensSearchHits = [];
            crispLensSearchError = '';
            return;
        }
        if (crispLensSearchLoading) return;
        crispLensSearchLoading = true;
        crispLensSearchError = '';
        try {
            crispLensSearchHits = await invoke<CrispLensSearchHit[]>(
                'images_crisplens_search',
                { q, limit: 50 }
            );
        } catch (e: any) {
            crispLensSearchError = String(e?.message ?? e ?? 'search failed');
            crispLensSearchHits = [];
        } finally {
            crispLensSearchLoading = false;
        }
    }

    // ── P13/B5 — CrispLens watchfolders cross-reference state ──────────────
    interface CrispLensWatchFolder {
        id?:        number | null;
        path:       string;
        // Wire shape allows int/bool/float; we use the typed helpers
        // through `recursive_bool()` on the Rust side, but on the
        // Svelte side we just need `path` for the cross-reference.
        // Other fields are passed through verbatim and ignored.
        [key: string]: any;
    }
    let crispLensWatchFolders = $state<CrispLensWatchFolder[]>([]);
    let crispLensWatchFoldersLoading = $state(false);

    async function loadCrispLensWatchFolders() {
        if (crispLensWatchFoldersLoading) return;
        crispLensWatchFoldersLoading = true;
        try {
            crispLensWatchFolders = await invoke<CrispLensWatchFolder[]>(
                'images_crisplens_watchfolders'
            );
        } catch {
            crispLensWatchFolders = [];
        } finally {
            crispLensWatchFoldersLoading = false;
        }
    }

    /** Returns the CrispLens watch_folder.path that contains the
     *  currently-previewed image's local path, or null when nothing
     *  overlaps.  Used by the preview pane to show the
     *  "this folder is also watched by CrispLens — both indexes will
     *  see new files" badge.  Path comparison is purely prefix-based
     *  (no symlink resolution / no normalisation past the existing
     *  `uriToPath` step) — false positives are far better UX than
     *  false negatives for this informational hint. */
    function watchfolderForCurrentPreview(): string | null {
        if (!imagesPreview) return null;
        const localPath = uriToPath(imagesPreview.locationUri);
        if (!localPath) return null;
        for (const wf of crispLensWatchFolders) {
            if (!wf.path) continue;
            // Treat both as directory prefixes — add trailing slash so
            // /a/b/c doesn't match /a/bc.
            const wfNorm = wf.path.endsWith('/') ? wf.path : wf.path + '/';
            if ((localPath + '/').startsWith(wfNorm)) {
                return wf.path;
            }
        }
        return null;
    }

    /** B5 deep-link: open the configured CrispLens URL in the user's
     *  default browser.  CrispLens's v4 frontend has no URL-based
     *  image routing (verified — there's no `#/images/{id}` handler
     *  in the SPA), so we land on the root page and the user
     *  navigates from there.  Future CrispLens UI work can add a
     *  hash route and we'd add the `#…` suffix here without churn. */
    async function openInCrispLens() {
        if (!crispLensSettings?.url) return;
        try {
            await openPath(crispLensSettings.url);
        } catch (e: any) {
            logWarn(`open-in-crisplens: ${e?.message ?? e}`);
        }
    }

    /** Banner status enum derived from the polled payload.  Keeps
     *  the template easy to read. */
    const crispLensBannerState = $derived.by<
        'hidden' | 'offline' | 'session_expired' | 'warming_up' | 'ok'
    >(() => {
        const s = crispLensStatus;
        if (!s || !s.tier2Configured) return 'hidden';
        if (s.healthOk === false) return 'offline';
        if (s.healthOk && !s.authenticated && s.error?.includes('session expired'))
            return 'session_expired';
        if (s.healthOk && s.healthModelReady === false) return 'warming_up';
        return 'ok';
    });

    /** One-shot status fetch — also used by `loadCrispLensSettings`
     *  so the settings pane is always in sync with the banner.
     *  Refreshes the watchfolders list piggy-back when the auth
     *  state went from unauth → auth, so the B5 cross-reference
     *  hint shows up as soon as a session lands. */
    async function fetchCrispLensStatus() {
        const wasAuthed = crispLensStatus?.authenticated ?? false;
        try {
            crispLensStatus = await invoke<CrispLensStatus>('images_crisplens_status');
            crispLensAuthenticated = crispLensStatus.authenticated;
        } catch (e: any) {
            // Network errors etc.  Failure mode is "leave the last
            // known state in place"; an alarmist banner that wipes
            // mid-poll would be worse UX than stale data.
            logWarn(`crisplens status: ${e?.message ?? e}`);
        }
        // Pull watchfolders once per transition into authenticated.
        // Avoids one HTTP call per 30 s poll when nothing's changed.
        if (crispLensStatus?.authenticated && !wasAuthed) {
            void loadCrispLensWatchFolders();
        }
        if (!crispLensStatus?.authenticated && wasAuthed) {
            crispLensWatchFolders = [];
        }
    }

    /** Manage the polling lifecycle.  The poll only runs when Tier 2
     *  is configured AND the user is on the Images tab — no point
     *  burning bandwidth on health checks while they're in the
     *  Übersicht tab.  Cadence: 30 s, matching the spec. */
    $effect(() => {
        const shouldPoll = activeTab === 'images'
            && crispLensSettings?.backend === 'crisplens'
            && (crispLensSettings?.url ?? '').trim() !== '';
        if (shouldPoll && crispLensPollHandle === null) {
            void fetchCrispLensStatus();
            crispLensPollHandle = setInterval(fetchCrispLensStatus, 30_000);
        }
        if (!shouldPoll && crispLensPollHandle !== null) {
            clearInterval(crispLensPollHandle);
            crispLensPollHandle = null;
        }
    });
    let crispLensUrlDraft        = $state('');
    let crispLensBackendDraft    = $state<'local' | 'crisplens'>('local');
    let crispLensLoginUser       = $state('');
    let crispLensLoginPassword   = $state('');
    let crispLensBusy            = $state(false);
    let crispLensError           = $state('');
    let crispLensInfo            = $state('');

    async function loadCrispLensSettings() {
        try {
            crispLensSettings   = await invoke<ImagesCrispLensSettings>('images_crisplens_settings_get');
            crispLensUrlDraft   = crispLensSettings.url;
            crispLensBackendDraft = crispLensSettings.backend;
            const status = await invoke<{ authenticated: boolean; url: string }>(
                'images_crisplens_session_status'
            );
            crispLensAuthenticated = status.authenticated;
        } catch (e: any) {
            crispLensError = String(e?.message ?? e ?? 'failed to load settings');
        }
    }

    async function saveCrispLensSettings() {
        if (crispLensBusy) return;
        crispLensBusy = true; crispLensError = ''; crispLensInfo = '';
        try {
            const next: ImagesCrispLensSettings = {
                backend:         crispLensBackendDraft,
                url:             crispLensUrlDraft.trim(),
                thumbnailSizePx: crispLensSettings?.thumbnailSizePx ?? 256,
                phashThreshold:  crispLensSettings?.phashThreshold  ?? 8,
            };
            await invoke('images_crisplens_settings_set', { settingsPayload: next });
            crispLensSettings = next;
            crispLensInfo = i18n.t.indexIngest.crisplens_saved;
            await loadCrispLensSettings(); // refresh auth status
        } catch (e: any) {
            crispLensError = String(e?.message ?? e ?? 'save failed');
        } finally {
            crispLensBusy = false;
        }
    }

    async function crispLensLoginNow() {
        if (crispLensBusy) return;
        crispLensBusy = true; crispLensError = ''; crispLensInfo = '';
        try {
            const out = await invoke<{ ok: boolean; username: string; role: string }>(
                'images_crisplens_login',
                { username: crispLensLoginUser, password: crispLensLoginPassword }
            );
            crispLensInfo = `${i18n.t.indexIngest.crisplens_login_ok}: ${out.username} (${out.role})`;
            crispLensLoginPassword = ''; // wipe in-memory copy now that the cookie's in Keychain
            await loadCrispLensSettings();
        } catch (e: any) {
            crispLensError = String(e?.message ?? e ?? 'login failed');
        } finally {
            crispLensBusy = false;
        }
    }

    async function crispLensLogoutNow() {
        if (crispLensBusy) return;
        crispLensBusy = true; crispLensError = ''; crispLensInfo = '';
        try {
            await invoke('images_crisplens_logout');
            crispLensInfo = i18n.t.indexIngest.crisplens_logout_ok;
            await loadCrispLensSettings();
        } catch (e: any) {
            crispLensError = String(e?.message ?? e ?? 'logout failed');
        } finally {
            crispLensBusy = false;
        }
    }

    function toggleCrispLensSettings() {
        crispLensSettingsOpen = !crispLensSettingsOpen;
        if (crispLensSettingsOpen && crispLensSettings === null) {
            void loadCrispLensSettings();
        }
    }

    // PLAN P9 step 6 — column registry + persistence.
    interface ColumnDef {
        id:        string;
        label:     string;
        width:     string;
        defaultOn: boolean;
        sortKey?:  SortColumn;
    }
    const COLUMN_DEFS: ColumnDef[] = [
        { id: 'name',     label: 'Name',     width: 'minmax(220px,1.6fr)', defaultOn: true,  sortKey: 'filename' },
        { id: 'author',   label: 'Autor',    width: 'minmax(120px,1fr)',   defaultOn: true,  sortKey: 'author'   },
        { id: 'year',     label: 'Jahr',     width: '60px',               defaultOn: true,  sortKey: 'year'     },
        { id: 'size',     label: 'Größe',    width: '70px',               defaultOn: true  },
        { id: 'mtime',    label: 'Geändert', width: '90px',               defaultOn: true  },
        { id: 'folder',   label: 'Ordner',   width: 'minmax(140px,1.4fr)', defaultOn: true  },
        { id: 'language', label: 'Sprache',  width: '70px',               defaultOn: false, sortKey: 'language' },
        { id: 'volume',   label: 'Volume',   width: '80px',               defaultOn: false },
        { id: 'level',    label: 'L',        width: '56px',               defaultOn: true  },
    ];
    const DEFAULT_COL_VIS = Object.fromEntries(COLUMN_DEFS.map(c => [c.id, c.defaultOn]));
    let colVisibility = $state<Record<string, boolean>>({ ...DEFAULT_COL_VIS });
    let colPickerOpen = $state(false);

    const gridCols = $derived(
        '28px 50px ' +
        COLUMN_DEFS.filter(c => colVisibility[c.id]).map(c => c.width).join(' ') +
        ' 88px'
    );

    // Close column picker when user clicks outside it.
    $effect(() => {
        if (!colPickerOpen) return;
        const handler = (e: MouseEvent) => {
            if (!(e.target as Element).closest('.col-picker-wrap')) colPickerOpen = false;
        };
        document.addEventListener('click', handler, true);
        return () => document.removeEventListener('click', handler, true);
    });

    // PLAN P9 step 8 — preview pane for Übersicht rows.
    const TEXT_EXTS  = new Set(['txt','md','markdown','rst','log','csv','tsv',
        'json','jsonl','yaml','yml','toml','xml','html',
        'rs','py','js','ts','svelte','go','java','c','cpp','h','hpp','sh','bash','zsh']);
    const IMAGE_EXTS = new Set(['png','jpg','jpeg','gif','webp','avif','bmp','svg','ico']);

    function uriToPath(uri: string): string | null {
        if (uri.startsWith('crisp+local://')) {
            const rest = uri.slice('crisp+local://'.length);
            const slashIdx = rest.indexOf('/');
            return slashIdx === -1 ? null : rest.slice(slashIdx);
        }
        if (uri.startsWith('/') || /^[A-Za-z]:[\\/]/.test(uri)) return uri;
        return null;
    }

    let previewDoc     = $state<any | null>(null);
    let cbLookupResult = $state<any | null>(null);
    let cbLookupLoading = $state(false);
    let previewKind    = $state<'pdf' | 'image' | 'text' | 'unsupported'>('unsupported');
    let previewSrc     = $state('');
    let previewText    = $state('');
    let previewLoading = $state(false);
    let previewError   = $state('');

    async function openDocPreview(doc: any) {
        cbLookupResult = null;
        if (previewDoc && previewDoc.doc_id === doc.doc_id) { closeDocPreview(); return; }
        // P12 reverse lookup for cb-archive rows.
        if (doc.location_uri?.startsWith('crisp+cb-archive://')) {
            const stored = await getSetting('cbManifestDbPath', null);
            if (stored) {
                const hashMatch = /crisp\+cb-archive:\/\/\d+\/([^#]+)/.exec(doc.location_uri ?? '');
                const fileHash = hashMatch?.[1];
                if (fileHash) {
                    cbLookupLoading = true;
                    invoke<any>('index_lookup_cb_file', { manifestDbPath: stored, fileHash })
                        .then(r => { cbLookupResult = r; cbLookupLoading = false; })
                        .catch(() => { cbLookupLoading = false; });
                }
            }
        }
        const path = uriToPath(doc.location_uri ?? '');
        if (!path) {
            previewDoc    = doc;
            previewKind   = 'unsupported';
            previewError  = 'Kein lokaler Pfad (Remote-Speicherort)';
            return;
        }
        previewDoc    = doc;
        previewLoading = true;
        previewError  = '';
        previewSrc    = '';
        previewText   = '';
        const ext = (doc.ext ?? path.split('.').pop() ?? '').toLowerCase();
        if (ext === 'pdf') {
            previewKind = 'pdf';
            previewSrc  = convertFileSrc(path);
        } else if (IMAGE_EXTS.has(ext)) {
            previewKind = 'image';
            previewSrc  = convertFileSrc(path);
        } else if (TEXT_EXTS.has(ext)) {
            previewKind = 'text';
            try {
                const raw = await readTextFile(path);
                previewText = raw.length > 512 * 1024
                    ? raw.slice(0, 512 * 1024) + '\n\n…(abgeschnitten; Datei > 512 KB)'
                    : raw;
            } catch (e: any) { previewError = `Lesefehler: ${e?.message ?? e}`; }
        } else {
            previewKind = 'unsupported';
        }
        previewLoading = false;
    }

    function closeDocPreview() {
        previewDoc    = null;
        previewSrc    = '';
        previewText   = '';
        previewError  = '';
    }

    // Ingest progress from Rust events
    let currentFile = $state('');
    let currentStep = $state('');
    let currentMessage = $state('');
    let currentChunk = $state(0);
    let currentChunkTotal = $state(0);

    // Embedder download progress (bytes-level, fired during model fetch).
    interface DownloadProgress {
        repo: string;
        file: string;
        bytes_done: number;
        bytes_total: number;
        pct: number;
    }
    let downloadProgress = $state<DownloadProgress | null>(null);

    // Accept the full multimodal set on drop / file-picker.  Audio +
    // video extensions route through extractText's audio dispatch
    // path (audio_extract_text Tauri command); documents + images
    // continue through the existing JS-side extractors.
    const supported = new Set<string>(MULTIMODAL_EXTENSIONS);

    // ── Stats ──────────────────────────────────────────────────────────────────

    const stats = $derived.by(() => ({
        total:   entries.length,
        done:    entries.filter(e => e.status === 'done').length,
        errors:  entries.filter(e => e.status === 'error').length,
        pending: entries.filter(e => e.status === 'pending' || e.status === 'error').length,
        active:  entries.find(e => e.status === 'extracting' || e.status === 'embedding'),
    }));

    // ── Lifecycle ──────────────────────────────────────────────────────────────

    let unlistenProgress: (() => void) | null = null;

    onMount(() => {
        let cleanup = () => {};
        (async () => {
            // Load persisted folder list and column visibility
            try {
                const store = await storeLoad('index-ingest.json', { defaults: {}, autoSave: true });
                const saved = await store.get<ManagedFolder[]>('folders');
                if (saved) folders = saved;
                const savedCols = await store.get<Record<string, boolean>>('catalogCols');
                if (savedCols) colVisibility = { ...DEFAULT_COL_VIS, ...savedCols };
            } catch (e) { /* store not yet created */ }

            // Listen to ingest progress events from Rust
            unlistenProgress = await listen<{ filename: string; step: string; chunk_index: number; chunk_total: number; message: string }>(
                'index://ingest-progress',
                (ev) => {
                    currentFile       = ev.payload.filename;
                    currentStep       = ev.payload.step;
                    currentMessage    = ev.payload.message;
                    currentChunk      = ev.payload.chunk_index;
                    currentChunkTotal = ev.payload.chunk_total;

                    // Update the matching entry's chunk progress
                    entries = entries.map(e =>
                        e.filename === ev.payload.filename
                            ? { ...e, chunksDone: ev.payload.chunk_index, chunksTotal: ev.payload.chunk_total }
                            : e
                    );
                }
            );

            // Tauri's webview swallows HTML5 dragenter/drop events on most
            // platforms and emits its own `tauri://drag-drop` payload
            // instead. Use that so files dropped onto the Hinzufügen
            // panel actually get added to the queue.
            const unlistenDrag = await listen<{ paths: string[] }>('tauri://drag-drop', (ev) => {
                if (activeTab !== 'add') return;
                const paths = (ev.payload?.paths ?? []).filter(p => supported.has((p.split('.').pop() ?? '').toLowerCase()));
                if (paths.length > 0) addPaths(paths);
            });

            // Embedder download progress — drives the "Lade Embedder …"
            // bar with real bytes-of-total instead of staying stuck at 5%.
            const unlistenDownload = await listen<DownloadProgress>('index://download-progress', (ev) => {
                downloadProgress = ev.payload;
                if (ev.payload.pct >= 100) {
                    setTimeout(() => { if (downloadProgress?.pct === 100) downloadProgress = null; }, 1500);
                }
            });

            // Restore the active ingest job from the SQLite durable queue.
            // Any files that were in_progress when the app was previously
            // closed are reclaimed back to pending so they'll be re-tried.
            try {
                const jobs = await invoke<{ id: string; status: string; jobType: string }[]>('jobs_list');
                const active = jobs.find(j =>
                    j.jobType === 'hinzufuegen' &&
                    (j.status === 'pending' || j.status === 'running' || j.status === 'paused')
                );
                if (active) {
                    activeJobId = active.id;
                    const reclaimed = await invoke<number>('jobs_reclaim', { jobId: active.id });
                    if (reclaimed > 0) logInfo(`Hinzufügen: reclaimed ${reclaimed} in-progress files from previous session`);
                    await loadEntriesFromQueue();
                    logInfo(`Hinzufügen: restored ${entries.length} entries from durable queue`);
                } else {
                    logInfo('Hinzufügen: no active job found — queue is empty');
                }
            } catch (e: any) {
                logError(`Hinzufügen: queue restore failed -- ${e?.message ?? e}`);
            }

            cleanup = () => { unlistenProgress?.(); unlistenDrag?.(); unlistenDownload?.(); };
        })();
        return () => cleanup();
    });

    // ── Folder management ──────────────────────────────────────────────────────

    async function saveFolders() {
        try {
            const store = await storeLoad('index-ingest.json', { defaults: {}, autoSave: true });
            await store.set('folders', folders);
        } catch (e) { console.error('Could not save folder list:', e); }
    }

    async function saveColVisibility() {
        try {
            const store = await storeLoad('index-ingest.json', { defaults: {}, autoSave: true });
            await store.set('catalogCols', colVisibility);
        } catch (e) { console.error('Could not save column visibility:', e); }
    }

    async function addFolder() {
        const selected = await openDialog({ directory: true, multiple: false });
        if (!selected) return;
        const path = selected as string;
        if (folders.some(f => f.path === path)) return;
        folders = [...folders, { path, addedAt: Date.now(), lastScanned: null, fileCount: 0 }];
        await saveFolders();
    }

    // ── P11: drive-folder ingest (Filen / Internxt / WebDAV / Local) ──────

    /** Open the "Cloud-Ordner hinzufügen" dialog, prefilling the drive list. */
    async function openDriveDialog() {
        try {
            availableDrives = await invoke<RegisteredDrive[]>('drive_list');
        } catch (e: any) {
            logError(`Drive-Liste laden fehlgeschlagen: ${e?.message ?? e}`);
            availableDrives = [];
        }
        if (availableDrives.length === 0) {
            logWarn('Keine Cloud-Laufwerke registriert. Lege zuerst eines in den Einstellungen an.');
        } else if (!driveDialogId || !availableDrives.some(d => d.id === driveDialogId)) {
            driveDialogId = availableDrives[0].id;
        }
        driveScanResult = '';
        driveDialogOpen = true;
    }

    /** Begin editing an existing drive — prefills the form. */
    function startDriveEdit(d: RegisteredDrive & { username?: string | null; password?: string | null; insecure_tls?: boolean | null }) {
        driveEditId       = d.id;
        driveCreateLabel  = d.label;
        driveCreateKind   = d.kind as any;
        driveCreatePath   = d.path;
        driveCreateUser   = d.username ?? '';
        driveCreatePass   = d.password ?? '';
        driveCreateInsecure = !!d.insecure_tls;
        driveCreateError  = '';
        driveCreateOpen   = true;
    }

    function resetDriveForm() {
        driveEditId       = null;
        driveCreateLabel  = '';
        driveCreatePath   = '';
        driveCreateUser   = '';
        driveCreatePass   = '';
        driveCreateInsecure = false;
        driveCreateError  = '';
    }

    /** Create or update a drive entry. */
    async function submitDriveCreate() {
        if (!driveCreateLabel.trim() || !driveCreatePath.trim()) {
            driveCreateError = 'Label und Pfad/URL sind erforderlich';
            return;
        }
        driveCreateBusy = true;
        driveCreateError = '';
        try {
            const args: Record<string, unknown> = {
                label:    driveCreateLabel.trim(),
                kind:     driveCreateKind,
                path:     driveCreatePath.trim(),
                username: driveCreateKind === 'webdav' && driveCreateUser ? driveCreateUser : null,
                password: driveCreateKind === 'webdav' && driveCreatePass ? driveCreatePass : null,
                insecureTls: driveCreateKind === 'webdav' && driveCreateInsecure ? true : null,
            };
            let cfg: RegisteredDrive;
            if (driveEditId) {
                cfg = await invoke<RegisteredDrive>('drive_update', { id: driveEditId, ...args });
                logInfo(`Laufwerk aktualisiert: ${cfg.label} (${cfg.kind})`);
            } else {
                cfg = await invoke<RegisteredDrive>('drive_create', args);
                logInfo(`Laufwerk angelegt: ${cfg.label} (${cfg.kind})`);
            }
            availableDrives = await invoke<RegisteredDrive[]>('drive_list');
            driveDialogId   = cfg.id;
            resetDriveForm();
            driveCreateOpen = false;
        } catch (e: any) {
            driveCreateError = `Fehler: ${e?.message ?? e}`;
        } finally {
            driveCreateBusy = false;
        }
    }

    /** Remove a drive entry (drives.json only — does not touch indexed rows). */
    async function deleteDrive(d: RegisteredDrive) {
        const ok = confirm(`Laufwerk "${d.label}" entfernen?\n\nIndexzeilen mit crisp+drive://${d.id}/... bleiben erhalten, lassen sich aber nicht mehr promoten.`);
        if (!ok) return;
        driveDeletingId = d.id;
        try {
            const removed = await invoke<boolean>('drive_delete', { id: d.id });
            if (removed) logInfo(`Laufwerk entfernt: ${d.label}`);
            availableDrives = await invoke<RegisteredDrive[]>('drive_list');
            if (driveDialogId === d.id) {
                driveDialogId = availableDrives[0]?.id ?? '';
            }
            if (driveEditId === d.id) {
                resetDriveForm();
                driveCreateOpen = false;
            }
        } catch (e: any) {
            logError(`Drive-Löschen fehlgeschlagen: ${e?.message ?? e}`);
        } finally {
            driveDeletingId = null;
        }
    }

    /** Trigger the manifest-only ingest on the selected drive + remote path. */
    async function runDriveIngest() {
        if (!driveDialogId) return;
        driveScanning   = true;
        driveScanResult = '';
        try {
            const exts = driveDialogExts
                .split(',')
                .map(s => s.trim().toLowerCase().replace(/^\./, ''))
                .filter(Boolean);
            logInfo(`P11: drive ingest — ${driveDialogId} ${driveDialogPath} (exts=${exts.join(',') || 'any'})`);
            const result = await invoke<{
                ingested: number;
                walked:   number;
                errors:   number;
                walk_errors: string[];
            }>('index_ingest_drive_manifest', {
                driveId:     driveDialogId,
                rootPath:    driveDialogPath || '/',
                allowedExts: exts.length > 0 ? exts : null,
                maxDepth:    driveDialogDepth ?? null,
                ownerId:     null,
            });
            driveScanResult =
                `${result.ingested} indexiert · ${result.walked} durchsucht · ${result.errors} Fehler`;
            logInfo(`P11: drive ingest done — ${driveScanResult}`);
            if (result.walk_errors?.length) {
                for (const we of result.walk_errors.slice(0, 5)) {
                    logWarn(`P11 walk error: ${we}`);
                }
            }
            await loadContents();
        } catch (e: any) {
            driveScanResult = `Fehler: ${e?.message ?? e}`;
            logError(`P11 drive ingest failed: ${e?.message ?? e}`);
        } finally {
            driveScanning = false;
        }
    }

    async function removeFolder(path: string) {
        folders = folders.filter(f => f.path !== path);
        await saveFolders();
    }

    async function scanFolderToQueue(folderPath: string) {
        scanningFolder = folderPath;
        try {
            const paths: string[] = [];
            async function collect(dir: string) {
                let items: DirEntry[];
                try { items = await readDir(dir); } catch { return; }
                for (const item of items) {
                    const itemPath = `${dir}/${item.name}`;
                    if (item.isDirectory) {
                        await collect(itemPath);
                    } else {
                        const ext = item.name?.split('.').pop()?.toLowerCase() ?? '';
                        if (supported.has(ext)) paths.push(itemPath);
                    }
                }
            }
            await collect(folderPath);
            await addPaths(paths);

            // Update folder metadata
            folders = folders.map(f => f.path === folderPath
                ? { ...f, lastScanned: Date.now(), fileCount: paths.length }
                : f
            );
            await saveFolders();
        } finally {
            scanningFolder = null;
        }
    }

    async function scanAllFolders() {
        for (const f of folders) {
            await scanFolderToQueue(f.path);
        }
    }

    // ── File queue ─────────────────────────────────────────────────────────────

    function onDragover(e: DragEvent) { e.preventDefault(); dropActive = true; }
    function onDragleave()            { dropActive = false; }

    async function onDrop(e: DragEvent) {
        e.preventDefault();
        dropActive = false;
        const paths: string[] = [];
        for (const item of (e.dataTransfer?.items ?? [])) {
            if (item.kind !== 'file') continue;
            const f = item.getAsFile();
            if (f) paths.push((f as any).path ?? f.name);
        }
        await addPaths(paths);
    }

    async function addFiles() {
        const selected = await openDialog({
            multiple: true,
            // Documents + images + audio/video — extension whitelist matches
            // the `supported` Set used by the drop handler so picker + drop
            // accept the same files.
            filters: [{ name: 'Documents, Audio & Video', extensions: [...MULTIMODAL_EXTENSIONS] }]
        });
        if (!selected) return;
        await addPaths(Array.isArray(selected) ? selected : [selected]);
    }

    // ── SQLite-backed queue helpers ────────────────────────────────────────────

    function fileStatusToDisplay(s: string): FileStatus {
        if (s === 'in_progress') return 'extracting';
        return (s as FileStatus) ?? 'pending';
    }

    function listedFileToEntry(f: ListedFile): IngestEntry {
        const parts = f.filePath.replace(/\\/g, '/').split('/');
        const filename = parts[parts.length - 1];
        const ext = filename.split('.').pop()?.toLowerCase() ?? '';
        return {
            id: String(f.rowId),
            rowId: f.rowId,
            path: f.filePath,
            filename,
            ext,
            size: 0,
            status: fileStatusToDisplay(f.status),
            error: f.errorText ?? undefined,
        };
    }

    async function loadEntriesFromQueue(): Promise<void> {
        if (!activeJobId) { entries = []; return; }
        try {
            const files = await invoke<ListedFile[]>('jobs_list_files', {
                jobId: activeJobId,
                statusFilter: null,
                limit: 500,
                offset: 0,
            });
            entries = files.map(listedFileToEntry);
        } catch (e: any) {
            logError(`Hinzufügen: failed to load entries from queue — ${e?.message ?? e}`);
        }
    }

    async function ensureActiveJob(): Promise<boolean> {
        if (activeJobId) return true;
        try {
            activeJobId = await invoke<string>('jobs_create', {
                jobType: 'hinzufuegen',
                sourcePaths: ['manual'],
                targetLevel: ingestLevel,
                configJson: null,
            });
            logInfo(`Hinzufügen: created job ${activeJobId}`);
            return true;
        } catch (e: any) {
            logError(`Hinzufügen: failed to create job — ${e?.message ?? e}`);
            return false;
        }
    }

    // ── File queue mutations ───────────────────────────────────────────────────

    async function addPaths(paths: string[]) {
        const validPaths = paths.filter(p => supported.has(p.split('.').pop()?.toLowerCase() ?? ''));
        if (validPaths.length === 0) return;
        if (!(await ensureActiveJob())) return;
        try {
            const added = await invoke<number>('jobs_add_files', {
                jobId: activeJobId,
                files: validPaths.map(p => ({ filePath: p, docId: null, targetLevel: ingestLevel })),
            });
            logInfo(`Hinzufügen: queued ${added} new files (${validPaths.length - added} already present)`);
        } catch (e: any) {
            logError(`Hinzufügen: failed to add files — ${e?.message ?? e}`);
        }
        await loadEntriesFromQueue();
    }

    async function removeEntry(id: string) {
        const rowId = parseInt(id, 10);
        if (!isNaN(rowId)) {
            await invoke('jobs_remove_file', { rowId }).catch((e: any) =>
                logError(`Hinzufügen: remove_file failed — ${e?.message ?? e}`)
            );
        }
        entries = entries.filter(e => e.id !== id);
    }

    async function clearAll() {
        if (running) return;
        if (activeJobId) {
            await invoke('jobs_delete', { jobId: activeJobId }).catch((e: any) =>
                logError(`Hinzufügen: delete job failed — ${e?.message ?? e}`)
            );
            activeJobId = null;
        }
        entries = [];
    }

    async function clearDone() {
        if (activeJobId) {
            await invoke<number>('jobs_remove_files_by_status', {
                jobId: activeJobId,
                status: 'done',
            }).catch((e: any) =>
                logError(`Hinzufügen: clear done failed — ${e?.message ?? e}`)
            );
        }
        await loadEntriesFromQueue();
    }

    // ── Language detection ─────────────────────────────────────────────────────

    function detectLanguage(text: string): string {
        const sample = text.slice(0, 2000).toLowerCase();
        const deWords = /\b(der|die|das|und|ist|nicht|mit|auf|an|von|zu|für|sich|auch|als|bei|aber|nach|durch|über|werden|haben|sein|eine|ein|des|dem|den|im|war|wie|ich|du|er|sie|wir|ihr)\b/g;
        const enWords = /\b(the|is|are|was|were|and|or|not|in|on|at|to|for|of|with|by|from|this|that|it|they|we|you|he|she|have|has|had|been|will|would|could|should)\b/g;
        const deCount = (sample.match(deWords) ?? []).length;
        const enCount = (sample.match(enWords) ?? []).length;
        if (deCount === 0 && enCount === 0) return 'de'; // fallback
        return deCount >= enCount ? 'de' : 'en';
    }

    // ── Ingest run ─────────────────────────────────────────────────────────────

    /** Make sure the catalog backend is ready. `withEmbedder` decides
     *  whether the (slow, multi-GB) embedder model gets loaded:
     *
     *    L1 / L2 fast-paths → `withEmbedder = false`. Only LanceDB +
     *      Tantivy are spun up; no model download, init takes < 1 s.
     *    L3 / vector-search → `withEmbedder = true`. Full pipeline.
     *
     *  When an init is already in progress (user double-clicked, or a
     *  background ingest started one), we poll until it completes
     *  instead of returning an "already in progress" error. */
    async function ensureIndexReady(withEmbedder: boolean = false): Promise<boolean> {
        const isReady = async () => invoke<boolean>('index_is_ready').catch(() => false);

        // Fast path: already up.
        const cfg = await invoke<{ enabled: boolean; use_vector?: boolean }>(
            'index_get_config'
        ).catch(() => ({ enabled: false, use_vector: true }));
        const ready = cfg.enabled && (await isReady());
        // If we don't actually need the embedder for this call AND something is
        // already initialised, take it as good enough — re-running init now
        // just to attach an embedder would block on the multi-GB download.
        if (ready && (!withEmbedder || (cfg as any).use_vector === false)) {
            return true;
        }
        if (ready) {
            // Already up + we want the embedder + use_vector is on → done.
            return true;
        }

        const tryInit = async () => {
            await invoke('index_set_config', { config: { ...(cfg as any), enabled: true } });
            const dataDir = await invoke<string>('get_app_data_dir').catch(() => '');
            await invoke('index_init', { dataDir, withEmbedder });
        };

        try {
            await tryInit();
            return true;
        } catch (e: any) {
            const msg = String(e?.message ?? e ?? '');
            if (msg.includes('already in progress')) {
                logInfo('Catalog: another init is running — waiting for it to finish');
                const deadline = Date.now() + 10 * 60 * 1000;
                while (Date.now() < deadline) {
                    await new Promise(r => setTimeout(r, 500));
                    if (await isReady()) return true;
                }
                logError('Catalog: timed out waiting for the in-progress init to finish');
                return false;
            }
            logError(`Catalog: auto-init failed — ${msg}`);
            alert(`Catalog initialisation failed: ${msg}\n\nOpen Settings → Search Index for manual setup.`);
            return false;
        }
    }

    async function startL1Ingest() {
        if (l1Running || running) return;
        if (!activeJobId || !(await ensureIndexReady(false))) return;

        l1Running = true;
        try {
            while (true) {
                const batch = await invoke<QueuedFile[]>('jobs_claim_batch', {
                    jobId: activeJobId,
                    batchSize: 64,
                });
                if (batch.length === 0) break;

                for (const qf of batch) updateEntry(String(qf.rowId), { status: 'embedding' });

                const l1Files = [];
                // Resolve volume once per batch — all files share the same mount.
                const batchDir = batch[0]?.filePath?.replace(/\\/g, '/').replace(/\/[^/]+$/, '') || '';
                const batchVolumeId: string | null = batchDir
                    ? await invoke<string | null>('index_volume_id_for_path', { path: batchDir }).catch(() => null)
                    : null;
                for (const qf of batch) {
                    const meta = await stat(qf.filePath).catch(() => null);
                    const size = (meta as any)?.size ?? 0;
                    const mtime = ((meta as any)?.mtime ?? new Date()).valueOf();
                    const ctime = ((meta as any)?.birthtime ?? (meta as any)?.mtime ?? new Date()).valueOf();
                    const parentDir = qf.filePath.replace(/\\/g, '/').replace(/\/[^/]+$/, '') || '';
                    const parts = qf.filePath.replace(/\\/g, '/').split('/');
                    const filename = parts[parts.length - 1];
                    const ext = filename.split('.').pop()?.toLowerCase() ?? '';
                    const docId = await hashText(qf.filePath);
                    l1Files.push({ docId, sourceHash: docId, locationUri: qf.filePath, ownerId: 'local', filename, ext, parentDir, size, mtimeMs: mtime, ctimeMs: ctime, volumeId: batchVolumeId ?? undefined });
                }

                try {
                    await invoke('index_ingest_l1', { files: l1Files });
                    const rowIds = batch.map(qf => qf.rowId);
                    await invoke('jobs_mark_done', { jobId: activeJobId, rowIds });
                    for (const qf of batch) updateEntry(String(qf.rowId), { status: 'done', chunks: 0 });
                } catch (err: any) {
                    logError(`[L1] batch ingest failed: ${err?.message ?? err}`);
                    for (const qf of batch) {
                        await invoke('jobs_mark_error', { jobId: activeJobId, rowId: qf.rowId, error: String(err), maxRetries: 0 }).catch(() => {});
                        updateEntry(String(qf.rowId), { status: 'error', error: String(err) });
                    }
                }

                await loadEntriesFromQueue();
            }
            await invoke('jobs_set_status', { jobId: activeJobId, status: 'done' }).catch(() => {});
        } finally {
            l1Running = false;
        }
    }

    /** L2 ingest: L1 ingest first to create doc rows, then promote via
     *  the embedded-metadata reader.  Doesn't require the embedder. */
    async function startL2Ingest() {
        if (l2RunningInline) return;
        if (!activeJobId || !(await ensureIndexReady(false))) return;
        l2RunningInline = true;
        try {
            // Step 1: L1 ingest all pending files (same claim_batch loop).
            await startL1Ingest();
            // Step 2: promote to L2 by matching doc_id.
            const docs = await invoke<any[]>('index_list_documents', { limit: 1000 }).catch(() => []);
            const ids: string[] = [];
            for (const e of entries.filter(e => e.status === 'done')) {
                const found = docs.find(d =>
                    (d.filename ?? '') === e.filename &&
                    String(d.location_uri ?? '').includes(e.path.replace(/\\/g, '/'))
                );
                if (found?.doc_id) ids.push(found.doc_id);
            }
            if (ids.length > 0) await invoke('index_promote_l2', { docIds: ids });
        } catch (err: any) {
            logError(`[L2] ingest failed: ${err?.message ?? err}`);
        } finally {
            l2RunningInline = false;
        }
    }

    async function startIngest() {
        if (running) return;
        if (!activeJobId) return;
        if (ingestLevel === 1) { await startL1Ingest(); return; }
        if (ingestLevel === 2) { await startL2Ingest(); return; }

        // L3 = full text + embedding.
        if (!(await ensureIndexReady(true))) return;

        running   = true;
        paused    = false;
        abortCtrl = new AbortController();
        const signal = abortCtrl.signal;

        const INGEST_BATCH_SIZE = 16;

        type PendingWrite = {
            rowId: number;
            input: {
                fullText: string; fullTextMd: string; headings: string[];
                title: string | null; author: string | null; year: number | null;
                filename: string; ext: string; language: string;
                locationUri: string; ownerId: string; sourceHash: string; tags: string[];
            };
        };

        await invoke('jobs_set_status', { jobId: activeJobId, status: 'running' }).catch(() => {});

        try {
            while (!signal.aborted) {
                while (paused && !signal.aborted) await new Promise(r => setTimeout(r, 200));
                if (signal.aborted) break;

                const batch = await invoke<QueuedFile[]>('jobs_claim_batch', {
                    jobId: activeJobId,
                    batchSize: INGEST_BATCH_SIZE,
                });
                if (batch.length === 0) break; // queue drained

                const pendingWrites: PendingWrite[] = [];
                const errored: { rowId: number; error: string }[] = [];
                const skipped: number[] = [];

                for (const qf of batch) {
                    if (signal.aborted) break;

                    const parts = qf.filePath.replace(/\\/g, '/').split('/');
                    const filename = parts[parts.length - 1];
                    const ext = filename.split('.').pop()?.toLowerCase() ?? '';

                    updateEntry(String(qf.rowId), { status: 'extracting', error: undefined });
                    logInfo(`Extract: ${qf.filePath} (.${ext})`);

                    try {
                        let bytes: Uint8Array;
                        try {
                            bytes = await readFile(qf.filePath);
                        } catch (fsErr: any) {
                            const msg = `Read failed: ${fsErr?.message ?? fsErr}`;
                            logError(`${msg}: ${qf.filePath}`);
                            errored.push({ rowId: qf.rowId, error: msg });
                            updateEntry(String(qf.rowId), { status: 'error', error: msg });
                            continue;
                        }

                        // Pass `path` so extractText can route audio/video
                        // extensions to the Rust audio_extract_text command
                        // (keeps the PCM buffer in Rust).
                        const result = await extractText({
                            name: filename,
                            arrayBuffer: bytes.buffer as ArrayBuffer,
                            path: qf.filePath,
                        });
                        logInfo(`Extracted ${filename}: ${result.text?.length ?? 0} chars`);

                        if (!result.text || result.text.trim().length < 20) {
                            logWarn(`Skip ${filename}: too little text (${result.text?.length ?? 0} chars)`);
                            skipped.push(qf.rowId);
                            updateEntry(String(qf.rowId), { status: 'skipped', error: 'Zu wenig Text extrahiert' });
                            continue;
                        }

                        updateEntry(String(qf.rowId), { status: 'embedding' });
                        const language   = detectLanguage(result.text);
                        const sourceHash = await hashText(result.text + qf.filePath);

                        pendingWrites.push({
                            rowId: qf.rowId,
                            input: {
                                fullText:    result.text,
                                fullTextMd:  result.markdownText ?? '',
                                headings:    result.headings ?? [],
                                title:       result.metadata?.title  ?? null,
                                author:      result.metadata?.author ?? null,
                                year:        result.metadata?.year   ? Number(result.metadata.year) : null,
                                filename,
                                ext,
                                language,
                                locationUri: qf.filePath,
                                ownerId:     'local',
                                sourceHash,
                                tags:        [],
                            },
                        });

                    } catch (err: any) {
                        const msg = String(err?.message ?? err);
                        logError(`Extract failed for ${filename}: ${msg}`);
                        errored.push({ rowId: qf.rowId, error: msg });
                        updateEntry(String(qf.rowId), { status: 'error', error: msg });
                    }
                }

                // Flush writes for this batch
                if (pendingWrites.length > 0) {
                    try {
                        const batchStats = await invoke<{ chunk_count: number; embed_time_ms: number; write_time_ms: number }>(
                            'index_ingest_batch', { inputs: pendingWrites.map(w => w.input) }
                        );
                        const n = pendingWrites.length;
                        await invoke('jobs_mark_done', {
                            jobId: activeJobId,
                            rowIds: pendingWrites.map(w => w.rowId),
                        });
                        for (const w of pendingWrites) {
                            updateEntry(String(w.rowId), {
                                status: 'done',
                                chunks:  Math.round(batchStats.chunk_count / n),
                                embedMs: Math.round(batchStats.embed_time_ms / n),
                                writeMs: Math.round(batchStats.write_time_ms / n),
                            });
                        }
                    } catch (e: any) {
                        logError(`Batch write failed: ${e?.message ?? e}`);
                        for (const w of pendingWrites) {
                            errored.push({ rowId: w.rowId, error: String(e) });
                            updateEntry(String(w.rowId), { status: 'error', error: String(e) });
                        }
                    }
                }

                // Persist errors and skips
                for (const { rowId, error } of errored) {
                    await invoke('jobs_mark_error', { jobId: activeJobId, rowId, error, maxRetries: 0 }).catch(() => {});
                }
                for (const rowId of skipped) {
                    await invoke('jobs_mark_skipped', { jobId: activeJobId, rowId }).catch(() => {});
                }

                await loadEntriesFromQueue();
            }

            if (!signal.aborted) {
                await invoke('jobs_set_status', { jobId: activeJobId, status: 'done' }).catch(() => {});
            } else {
                await invoke('jobs_reclaim', { jobId: activeJobId }).catch(() => {});
                await invoke('jobs_set_status', { jobId: activeJobId, status: 'paused' }).catch(() => {});
            }

        } finally {
            running   = false;
            paused    = false;
            abortCtrl = null;
            currentFile    = '';
            currentMessage = '';
            await loadEntriesFromQueue();
        }
    }

    function pauseResume() {
        if (!running) return;
        paused = !paused;
    }

    function stopIngest() {
        abortCtrl?.abort();
        // The finally block in startIngest calls jobs_reclaim + loadEntriesFromQueue.
        // Set flags immediately so the UI reflects the stop without waiting for the
        // async teardown to finish.
        running = false;
        paused  = false;
        currentFile    = '';
        currentMessage = '';
    }

    // ── Index contents ─────────────────────────────────────────────────────────

    /** Pull `level` (1, 2, or 3) out of the row's metadata_json. */
    function docLevel(d: any): 1 | 2 | 3 {
        if (d.metadata_json) {
            try {
                const m = JSON.parse(d.metadata_json);
                if (m.level === 1) return 1;
                if (m.level === 2) return 2;
                if (m.level === 3) return 3;
            } catch { /* malformed JSON — ignore */ }
        }
        return d.snippet ? 3 : 1;
    }

    /** Return the `extraction_failure` object from metadata_json, or null. */
    function extractionFailure(d: any): { reason: string; msg: string } | null {
        if (!d.metadata_json) return null;
        try {
            const m = JSON.parse(d.metadata_json);
            return m.extraction_failure ?? null;
        } catch { return null; }
    }

    const FAIL_LABELS: Record<string, string> = {
        drm:         'DRM',
        timeout:     'timeout',
        corrupt:     'korrupt',
        password:    'passwort',
        unsupported: 'N/A',
        other:       'fehler',
    };

    const FAIL_HINTS: Record<string, string> = {
        drm:
            'DRM-geschützt: Nur der verschlüsselte Text konnte nicht extrahiert werden.\n' +
            'Titel und Autor sind ggf. trotzdem vorhanden.\n' +
            'Die Datei ist verschlüsselt — CrispSorter kann nur Metadaten lesen.',
        timeout:     'Zeitlimit überschritten — Datei wird beim nächsten Lauf erneut versucht.',
        corrupt:     'Datei scheint beschädigt oder ist kein gültiges Format.',
        password:    'Passwortgeschützt — Entschlüsselung erforderlich.',
        unsupported: 'Kein Extraktor für diesen Dateityp verfügbar.',
        other:       'Unbekannter Extraktionsfehler.',
    };

    /** Resolve location_uri to a normalised local-path string for prefix
     *  matching (forward slashes, lowercased on Windows). Used by the
     *  folder/subtree filter. */
    function docPath(d: any): string {
        let p = String(d.location_uri ?? '');
        if (p.startsWith('crisp+local://')) {
            const after = p.slice('crisp+local://'.length);
            const slash = after.indexOf('/');
            p = slash >= 0 ? after.slice(slash) : after;
        }
        return p.replace(/\\/g, '/').toLowerCase();
    }

    /** All distinct extensions present in the currently-loaded contents -- used
     *  to populate the filter chips. */
    const contentsExtChoices = $derived.by(() => {
        const s = new Set<string>();
        for (const d of contents) {
            const e = (d.ext ?? '').toLowerCase();
            if (e) s.add(e);
        }
        return [...s].sort();
    });

    let _allContents = $state<any[]>([]);

    /** Build the DocumentFilter payload index_query_documents expects.
     *  Mirrors the chip-bar state. Completeness filter stays client-side
     *  (the Rust schema doesn't model "has_*" flags as scalar columns yet
     *  — promoting them is on P9's migration path). */
    function buildDocumentFilter() {
        const f: any = {};
        if (contentsFolder.trim()) f.parentDirPrefix = contentsFolder.trim();
        if (contentsExt.size > 0) f.ext = [...contentsExt];
        if (contentsLevel !== 'all') f.level = contentsLevel;
        if (contentsQuery.trim()) f.nameSubstring = contentsQuery.trim();
        if (contentsTags.size > 0) f.tags = [...contentsTags];
        return f;
    }

    /** Fetch the tag cloud for the current (non-tag) filter. Only called
     *  while the sidebar is visible — keeps the scan off the hot path when
     *  the user never opens it. The backend already ignores `filter.tags`
     *  so the counts reflect what's still selectable. */
    async function loadTagFacets() {
        if (!showTagCloud) return;
        tagFacetsLoading = true;
        try {
            tagFacets = await invoke<{ tag: string; count: number }[]>('index_tag_facets', {
                filter: buildDocumentFilter(),
                limit: 200,
            });
        } catch (e: any) {
            logError(`Übersicht: tag facets failed -- ${String(e?.message ?? e ?? '')}`);
            tagFacets = [];
        } finally {
            tagFacetsLoading = false;
        }
    }

    /** Toggle a tag in the AND-selection, then reload both the document
     *  page and the facet counts. */
    function toggleTag(tag: string) {
        const next = new Set(contentsTags);
        if (next.has(tag)) next.delete(tag); else next.add(tag);
        contentsTags = next;
        // loadContents refreshes the facet counts on completion (single
        // funnel), so no separate loadTagFacets call here.
        loadContents(false);
    }

    /** Show/hide the cloud; fetch facets lazily on first reveal. */
    function toggleTagCloud() {
        showTagCloud = !showTagCloud;
        if (showTagCloud && tagFacets.length === 0) loadTagFacets();
    }

    function buildSortSpec() {
        return { column: sortColumn, direction: sortDir };
    }

    async function loadContents(append = false) {
        if (contentsLoading) return;
        contentsLoading = true;
        if (!append) {
            selectedDocIds = new Set();
            lastClickedDocIdx = null;
        }
        try {
            const [stats, page] = await Promise.all([
                invoke<{ total_rows: number; doc_count: number; chunk_count: number }>('index_stats').catch(() => null),
                invoke<{ rows: any[]; nextCursor: string | null; totalEstimate: number }>(
                    'index_query_documents',
                    {
                        filter: buildDocumentFilter(),
                        sort: buildSortSpec(),
                        page: { limit: 200, cursor: append ? nextCursor : null },
                    }
                ),
            ]);
            indexStats = stats;
            const newRows = page?.rows ?? [];
            _allContents = append ? [..._allContents, ...newRows] : newRows;
            // Apply the completeness filter client-side; chip + ext + level
            // + folder + name substring are already server-applied via the
            // DocumentFilter payload.
            contents = applyClientFilters(_allContents);
            totalEstimate = page?.totalEstimate ?? 0;
            nextCursor = page?.nextCursor ?? null;
            // Keep the tag cloud counts in sync with the current filter
            // (only while it's visible; cheap no-op otherwise).
            if (!append && showTagCloud) loadTagFacets();
        } catch (e: any) {
            const msg = String(e?.message ?? e ?? '');
            // Don't flood the log when the index isn't ready yet; one
            // line per distinct error per session is enough to debug
            // and not enough to bury other useful messages.
            if (msg !== _lastQueryError) {
                logError(`Übersicht: query failed -- ${msg}`);
                _lastQueryError = msg;
            }
            if (!append) {
                _allContents = [];
                contents = [];
                indexStats = null;
                totalEstimate = 0;
                nextCursor = null;
            }
        } finally {
            contentsLoading = false;
        }
    }
    let _lastQueryError = '';

    /** P13 slice A1 — fetch one page of image rows from the local index.
     *  Pagination matches `loadContents`: `append=true` concatenates the
     *  new rows onto the existing list and reuses the cursor; `false`
     *  resets and starts over. The Tauri command (`images_list`) applies
     *  the canonical IMAGE_EXTS filter server-side via LanceDB's
     *  scalar-indexed `ext IN (...)` predicate, so this runs in the same
     *  envelope as the Übersicht query and degrades cleanly to an empty
     *  page when the index isn't ready yet. */
    let _lastImagesError = '';
    async function loadImages(append = false) {
        if (imagesLoading) return;
        imagesLoading = true;
        try {
            const page = await invoke<{
                items: ImageRow[];
                total: number;
                nextCursor: string | null;
                pageSize: number;
            }>('images_list', {
                pageSize: 200,
                cursor: append ? imagesNextCursor : null,
                filters: null,
            });
            const newItems = page?.items ?? [];
            imagesRows       = append ? [...imagesRows, ...newItems] : newItems;
            imagesTotal      = page?.total ?? 0;
            imagesNextCursor = page?.nextCursor ?? null;
        } catch (e: any) {
            const msg = String(e?.message ?? e ?? '');
            if (msg !== _lastImagesError) {
                logError(`Images: list failed -- ${msg}`);
                _lastImagesError = msg;
            }
            if (!append) {
                imagesRows       = [];
                imagesTotal      = 0;
                imagesNextCursor = null;
            }
        } finally {
            imagesLoading = false;
        }
    }

    // ── A2: lazy thumbnail loader ──────────────────────────────────────────
    //
    // Use:lazyThumbnail attaches an IntersectionObserver to a tile
    // node.  When the tile scrolls into view the action invokes
    // `images_thumbnail` and stores the resulting object-URL in
    // `imagesThumbs`.  IO is disconnected after a single hit so
    // re-scrolling doesn't re-fetch.  rootMargin pre-loads tiles
    // ~200 px before they enter the viewport so the user rarely sees
    // a blank tile during fast scrolls.
    //
    // Object-URLs leak until manually revoked.  We revoke on tab
    // change (closeImagesPreview clears the cache when appropriate)
    // and on row reload (loadImages resets imagesRows -> tiles
    // unmount -> the action's destroy() runs).  An LRU-bounded cache
    // is overkill for the spec's "no cache" intent — the spec means
    // no *server-side* cache; the per-session in-memory map is fine
    // and keeps repeat scrolls snappy.
    function lazyThumbnail(node: HTMLElement, docId: string) {
        // Already loaded / failed earlier in this session — skip IO.
        if (imagesThumbs[docId]) return { destroy() {} };

        const io = new IntersectionObserver(
            async (entries) => {
                if (!entries.some(e => e.isIntersecting)) return;
                io.disconnect();
                imagesThumbs = { ...imagesThumbs, [docId]: 'loading' };
                try {
                    const bytes = await invoke<number[]>('images_thumbnail', { docId, size: 256 });
                    const blob = new Blob([Uint8Array.from(bytes)], { type: 'image/png' });
                    imagesThumbs = { ...imagesThumbs, [docId]: URL.createObjectURL(blob) };
                } catch {
                    // Common case: HEIC (the image crate doesn't decode it
                    // by default — A2 deferred libheif binding) or remote
                    // crisp+drive:// row.  Tile keeps the placeholder.
                    imagesThumbs = { ...imagesThumbs, [docId]: 'error' };
                }
            },
            { threshold: 0.01, rootMargin: '200px' }
        );
        io.observe(node);
        return {
            destroy() { io.disconnect(); }
        };
    }

    /** A2: open the side preview pane for a clicked image tile.
     *  Loads the full image via convertFileSrc (no IPC payload —
     *  Tauri serves the file directly through tauri:// protocol)
     *  and kicks off an EXIF fetch in parallel.  When Tier 2 is
     *  authenticated, also kicks off the CrispLens face-overlay
     *  resolution (slice "P13 follow-up": by-hash → faces → render
     *  bbox rectangles). */
    async function openImagesPreview(img: ImageRow) {
        // Toggle: clicking the same tile closes the pane.
        if (imagesPreview?.docId === img.docId) {
            closeImagesPreview();
            return;
        }
        imagesPreview         = img;
        imagesPreviewExif     = null;
        imagesPreviewExifError = '';
        imagesPreviewExifLoading = true;
        imagesPreviewFaces        = [];
        imagesPreviewFacesError   = '';
        imagesPreviewFacesLoading = false;

        const path = uriToPath(img.locationUri);
        imagesPreviewSrc = path ? convertFileSrc(path) : '';

        // EXIF fetch (always runs — Tier 1 feature)
        const exifP = (async () => {
            try {
                imagesPreviewExif = await invoke<ImagesExifSummary>('images_exif', { docId: img.docId });
            } catch (e: any) {
                imagesPreviewExifError = String(e?.message ?? e ?? 'EXIF read failed');
            } finally {
                imagesPreviewExifLoading = false;
            }
        })();

        // CrispLens face-overlay fetch (only when Tier 2 + local path)
        let facesP: Promise<void> = Promise.resolve();
        if (crispLensStatus?.authenticated && path) {
            imagesPreviewFacesLoading = true;
            facesP = (async () => {
                try {
                    const resolved = await invoke<{ id: number } | null>(
                        'images_crisplens_image_by_local_path',
                        { localPath: path }
                    );
                    if (!resolved) {
                        // No CrispLens row for this hash — silent;
                        // image just isn't on the server.
                        return;
                    }
                    interface FaceRow {
                        faceId:    number;
                        bbox:      { top: number; right: number; bottom: number; left: number };
                        personName: string | null;
                    }
                    const faces = await invoke<FaceRow[]>(
                        'images_crisplens_image_faces',
                        { imageId: resolved.id }
                    );
                    // Guard against race: the user may have clicked
                    // a different tile while we were waiting on
                    // these two network calls.  Don't overwrite a
                    // newer preview's state.
                    if (imagesPreview?.docId !== img.docId) return;
                    imagesPreviewFaces = faces.map(f => ({
                        faceId:     f.faceId,
                        bbox:       f.bbox,
                        personName: f.personName,
                    }));
                } catch (e: any) {
                    if (imagesPreview?.docId !== img.docId) return;
                    imagesPreviewFacesError = String(e?.message ?? e ?? 'face fetch failed');
                } finally {
                    if (imagesPreview?.docId === img.docId) {
                        imagesPreviewFacesLoading = false;
                    }
                }
            })();
        }

        await Promise.all([exifP, facesP]);
    }

    function closeImagesPreview() {
        imagesPreview         = null;
        imagesPreviewSrc      = '';
        imagesPreviewExif     = null;
        imagesPreviewExifError = '';
        imagesPreviewFaces        = [];
        imagesPreviewFacesError   = '';
        imagesPreviewFacesLoading = false;
    }

    /** A3: fetch SHA-256 duplicate clusters for the current image set.
     *  Cheap on small indexes (server-side groups are computed in
     *  one walk through the image-extension subset of LanceDB).
     *  Refreshes every time the user toggles the mode on; the
     *  loaded groups stay in state until the user toggles off so
     *  scrolling / clicking inside dup mode doesn't re-fetch. */
    async function loadImagesDuplicates() {
        if (imagesDupLoading) return;
        imagesDupLoading = true;
        imagesDupError = '';
        try {
            imagesDupGroups = await invoke<ImageDuplicateGroup[]>('images_duplicates', { filters: null });
        } catch (e: any) {
            imagesDupError  = String(e?.message ?? e ?? 'duplicates fetch failed');
            imagesDupGroups = [];
        } finally {
            imagesDupLoading = false;
        }
    }

    function toggleImagesDupMode() {
        imagesDupMode = !imagesDupMode;
        if (imagesDupMode) {
            // Mutual exclusion with the pHash near-dup mode — only
            // one alternative grid layout is active at a time.
            imagesNearDupMode = false;
            imagesNearDupGroups = [];
            void loadImagesDuplicates();
        } else {
            imagesDupGroups = [];
            imagesDupError  = '';
        }
    }

    /** A4: fetch perceptual-hash near-duplicate clusters for the
     *  current image set.  Slower than `loadImagesDuplicates` because
     *  every row gets decoded + hashed on demand; UI surfaces a
     *  spinner while it runs.  Threshold is whatever the user set
     *  (default 8 per the spec). */
    async function loadImagesNearDuplicates() {
        if (imagesNearDupLoading) return;
        imagesNearDupLoading = true;
        imagesNearDupError = '';
        try {
            imagesNearDupGroups = await invoke<NearDupGroup[]>(
                'images_near_duplicates',
                { threshold: imagesNearDupThreshold, filters: null }
            );
        } catch (e: any) {
            imagesNearDupError  = String(e?.message ?? e ?? 'near-duplicates fetch failed');
            imagesNearDupGroups = [];
        } finally {
            imagesNearDupLoading = false;
        }
    }

    function toggleImagesNearDupMode() {
        imagesNearDupMode = !imagesNearDupMode;
        if (imagesNearDupMode) {
            imagesDupMode = false;
            imagesDupGroups = [];
            void loadImagesNearDuplicates();
        } else {
            imagesNearDupGroups = [];
            imagesNearDupError  = '';
        }
    }

    /** Client-side residual filter after the server narrowed the page.
     *  Today: only completeness; once we promote `has_*` flags to scalar
     *  columns (P9 step 3) this collapses to identity. */
    function applyClientFilters(rows: any[]): any[] {
        if (contentsCompleteness === 'any') return rows;
        return rows.filter(r => {
            if (contentsCompleteness === 'has_title' && !r.title) return false;
            if (contentsCompleteness === 'has_author' && !r.author) return false;
            if (contentsCompleteness === 'has_year' && !r.year) return false;
            if (contentsCompleteness === 'has_all' && (!r.title || !r.author || !r.year)) return false;
            return true;
        });
    }

    function toggleContentsExt(ext: string) {
        const next = new Set(contentsExt);
        if (next.has(ext)) next.delete(ext); else next.add(ext);
        contentsExt = next;
    }

    /** Open the OS folder picker and seed `contentsFolder` with the chosen
     *  directory. The Übersicht filter then narrows to docs whose
     *  `location_uri` is inside that subtree. */
    async function pickContentsFolder() {
        const sel = await openDialog({ directory: true, multiple: false });
        if (typeof sel === 'string') {
            contentsFolder = sel;
            void loadFolderChildren(sel);
            folderTreeOpen = true;
        }
    }

    // ── Folder tree (P9 step 4) ────────────────────────────────────────────────

    interface FolderChild { name: string; path: string; docCount: number; }

    let folderChildren    = $state<FolderChild[]>([]);
    let folderTreeLoading = $state(false);
    let folderTreeOpen    = $state(false);

    async function loadFolderChildren(path: string) {
        folderTreeLoading = true;
        try {
            folderChildren = await invoke<FolderChild[]>('index_folder_children', { parent: path });
        } catch { folderChildren = []; }
        finally { folderTreeLoading = false; }
    }

    function navigateFolder(path: string) {
        contentsFolder = path;
        void loadFolderChildren(path);
        folderTreeOpen = true;
    }

    function clearFolder() {
        contentsFolder = '';
        folderChildren = [];
        folderTreeOpen = false;
    }

    /** Split a path into clickable breadcrumb segments. */
    function folderSegments(path: string): { label: string; fullPath: string }[] {
        if (!path) return [];
        const isUnix = path.startsWith('/');
        const parts = path.split(/[\\/]/).filter(Boolean);
        const segs: { label: string; fullPath: string }[] = [];
        let acc = isUnix ? '' : '';
        for (const p of parts) {
            acc = isUnix
                ? (acc === '' ? `/${p}` : `${acc}/${p}`)
                : (acc === '' ? p : `${acc}\\${p}`);
            segs.push({ label: p, fullPath: acc });
        }
        return segs;
    }

    // Server-side filter / sort: any change to the chip inputs (or the
    // user landing on Übersicht for the first time) re-issues the
    // index_query_documents query. Debounced via a 200 ms timer so
    // typing in the name-substring input doesn't hammer Rust.
    //
    // This effect *also* covers the first-load case: when activeTab is
    // already 'overview' on mount, the effect runs once with the
    // initial chip values and does the load. (The earlier two-effect
    // version had a separate "auto-load if empty" effect that re-fired
    // every time `_allContents` was set to [] by the success-with-zero
    // path, which is exactly what an unconfigured index returns -- the
    // resulting infinite loop is what surfaced as "always says Lade…".)
    let queryDebounce: any = null;
    $effect(() => {
        // Tracking deps explicitly so the effect fires on the right things.
        const _trackTab = activeTab;
        const _trackName = contentsQuery;
        const _trackFolder = contentsFolder;
        const _trackExt = contentsExt;
        const _trackLevel = contentsLevel;
        const _trackSortCol = sortColumn;
        const _trackSortDir = sortDir;
        if (activeTab !== 'overview') return;
        if (queryDebounce) clearTimeout(queryDebounce);
        queryDebounce = setTimeout(() => loadContents(false), 200);
    });

    // Completeness filter is residual / client-side; just re-slice.
    $effect(() => {
        const _track = contentsCompleteness;
        contents = applyClientFilters(_allContents);
    });

    /** Mouse-click row selection mirroring BatchReview.svelte's pattern:
     *  - bare click selects exactly that row;
     *  - Shift+click extends the selection from the last anchor to here;
     *  - Ctrl/Cmd+click toggles the row in/out of the selection set.
     *  Text-selection is suppressed via `user-select: none` on .catalog-row
     *  so dragging across rows actually selects rows instead of highlighting
     *  the filename text. */
    function handleDocRowClick(e: MouseEvent | KeyboardEvent, idx: number, docId: string) {
        if (e.shiftKey && lastClickedDocIdx !== null) {
            const start = Math.min(lastClickedDocIdx, idx);
            const end   = Math.max(lastClickedDocIdx, idx);
            const next  = new Set(selectedDocIds);
            for (let i = start; i <= end; i++) {
                const id = contents[i]?.doc_id;
                if (id) next.add(id);
            }
            selectedDocIds = next;
        } else if (e.metaKey || e.ctrlKey) {
            const next = new Set(selectedDocIds);
            if (next.has(docId)) next.delete(docId); else next.add(docId);
            selectedDocIds = next;
        } else {
            selectedDocIds = new Set([docId]);
        }
        lastClickedDocIdx = idx;
    }

    function setSort(col: SortColumn) {
        if (sortColumn === col) {
            sortDir = sortDir === 'asc' ? 'desc' : 'asc';
        } else {
            sortColumn = col;
            sortDir = (col === 'indexed_at' || col === 'year') ? 'desc' : 'asc';
        }
    }

    /** Pluck a key out of a row's `metadata_json` blob. L1 rows carry
     *  fs_size / fs_mtime / parent_dir / level there until P9 step 3
     *  promotes them to real columns. Returns `null` when the field
     *  isn't present (L3 rows from the legacy ingest path don't carry
     *  fs_size yet). */
    function metaField(row: any, key: string): any {
        if (!row?.metadata_json) return null;
        try {
            const m = typeof row.metadata_json === 'string'
                ? JSON.parse(row.metadata_json)
                : row.metadata_json;
            return m?.[key] ?? null;
        } catch {
            return null;
        }
    }

    function fmtSize(bytes: number | null | undefined): string {
        if (!bytes && bytes !== 0) return '';
        const u = ['B', 'KB', 'MB', 'GB', 'TB'];
        let n = bytes, i = 0;
        while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
        return `${n < 10 && i > 0 ? n.toFixed(1) : Math.round(n)} ${u[i]}`;
    }

    function fmtMtime(ms: number | null | undefined): string {
        if (!ms && ms !== 0) return '';
        try {
            const d = new Date(ms);
            return d.toLocaleDateString();
        } catch { return ''; }
    }

    let retryingIds = $state(new Set<string>());
    let drmPopoverDocId = $state<string | null>(null);

    /** Clear the extraction_failure blob and reset to L1 so the bg_ingest
     *  worker re-attempts on next run. Only works for retryable reasons
     *  (timeout / other). Refreshes the row in-place after success. */
    async function retryExtraction(docId: string) {
        retryingIds = new Set([...retryingIds, docId]);
        try {
            const reason = await invoke<string | null>('index_retry_extraction', { docId });
            if (reason === null) {
                // No failure recorded — nothing to retry.
                return;
            }
            // Refresh just this row by reloading from the table.
            const idx = contents.findIndex((c: any) => c.doc_id === docId);
            if (idx >= 0) {
                // Optimistically clear the failure badge while bg_ingest works.
                const updated = { ...contents[idx] };
                try {
                    const parsed = JSON.parse(updated.metadata_json ?? '{}');
                    delete parsed.extraction_failure;
                    parsed.level = 1;
                    updated.metadata_json = JSON.stringify(parsed);
                } catch { /* ignore parse error */ }
                contents = [...contents.slice(0, idx), updated, ...contents.slice(idx + 1)];
            }
        } catch (e) {
            console.error('Retry extraction failed:', e);
        } finally {
            retryingIds.delete(docId);
            retryingIds = new Set(retryingIds);
        }
    }

    let promotingIds = $state(new Set<string>());

    /** Parse `crisp+cb-archive://{archive_id}/{hash}#{original_path}` → original_path */
    function cbArchiveOriginalPath(uri: string): string | null {
        if (!uri.startsWith('crisp+cb-archive://')) return null;
        const hashIdx = uri.indexOf('#');
        return hashIdx >= 0 ? decodeURIComponent(uri.slice(hashIdx + 1)) : null;
    }

    /** Parse `crisp+drive://{drive_id}/{remote-path}` → { driveId, remotePath } */
    function driveUriParts(uri: string): { driveId: string; remotePath: string } | null {
        if (!uri.startsWith('crisp+drive://')) return null;
        const rest = uri.slice('crisp+drive://'.length);
        const slash = rest.indexOf('/');
        if (slash < 0) return null;
        return {
            driveId:    rest.slice(0, slash),
            remotePath: decodeURIComponent(rest.slice(slash)),
        };
    }

    /** Promote a registered-drive row to L3: fetch via the CloudDrive,
     *  stage to a temp dir, route through the existing extract+embed
     *  pipeline (mirrors promoteCbArchive). */
    async function promoteDriveArchive(doc: any) {
        const parts = driveUriParts(doc.location_uri ?? '');
        if (!parts) return;
        promotingIds = new Set([...promotingIds, doc.doc_id]);
        try {
            logInfo(`P11: promoting ${parts.remotePath} from drive ${parts.driveId}`);
            const result = await invoke<{ doc_id: string; chunks: number }>('index_promote_drive_archive', {
                docId:      doc.doc_id,
                driveId:    parts.driveId,
                remotePath: parts.remotePath,
                ownerId:    null,
            });
            logInfo(`P11: promoted to L3: ${result.chunks} chunks`);
            await loadContents();
        } catch (e: any) {
            logError(`P11 drive-promote failed: ${e?.message ?? e}`);
        } finally {
            promotingIds.delete(doc.doc_id);
            promotingIds = new Set(promotingIds);
        }
    }

    /** Promote a cb-archive row to L3 by calling retrieve.py */
    async function promoteCbArchive(doc: any) {
        const originalPath = cbArchiveOriginalPath(doc.location_uri ?? '');
        if (!originalPath) return;

        // Ask the user for the retrieve.py path (persisted in settings).
        const stored = await getSetting('cbRetrievePyPath', null);
        let pyPath = stored;
        if (!pyPath) {
            const { open: od } = await import('@tauri-apps/plugin-dialog');
            const sel = await od({ filters: [{ name: 'Python script', extensions: ['py'] }], title: 'retrieve.py auswählen' });
            if (typeof sel !== 'string') return;
            pyPath = sel;
            saveSetting('cbRetrievePyPath', pyPath).catch(() => {});
        }

        promotingIds = new Set([...promotingIds, doc.doc_id]);
        try {
            logInfo(`P12: promoting ${originalPath} via retrieve.py`);
            const result = await invoke<{ doc_id: string; chunks: number }>('index_promote_cb_archive', {
                docId: doc.doc_id,
                originalPath,
                retrievePyPath: pyPath,
                outputDir: null,
                ownerId: null,
            });
            logInfo(`P12: promoted to L3: ${result.chunks} chunks`);
            await loadContents();
        } catch (e: any) {
            logError(`P12 promote failed: ${e?.message ?? e}`);
        } finally {
            promotingIds.delete(doc.doc_id);
            promotingIds = new Set(promotingIds);
        }
    }

    async function deleteFromIndex(docId: string) {
        deletingIds = new Set([...deletingIds, docId]);
        try {
            await invoke('index_delete_document', { docId });
            contents = contents.filter((c: any) => c.doc_id !== docId);
            selectedDocIds.delete(docId);
            selectedDocIds = new Set(selectedDocIds);
            if (indexStats) indexStats = { ...indexStats, doc_count: indexStats.doc_count - 1 };
        } catch (e) {
            console.error('Delete failed:', e);
        } finally {
            deletingIds.delete(docId);
            deletingIds = new Set(deletingIds);
        }
    }

    async function deleteSelected() {
        const ids = [...selectedDocIds];
        for (const docId of ids) {
            await deleteFromIndex(docId);
        }
    }

    let promotingL3 = $state(false);
    let l3Progress  = $state<{ done: number; total: number; current: string } | null>(null);

    // .caf round-trip state.
    let cafBusy       = $state(false);
    let cafLastResult = $state<string | null>(null);
    let cidxBusy       = $state(false);
    let cidxLastResult = $state<string | null>(null);
    let cidxIncludeFts = $state(true); // default: include FTS for offline search
    // Mounted archive
    let mountedCidx    = $state<{ path: string; docs: number; chunks: number; has_fts: boolean } | null>(null);
    let cidxContents    = $state<any[]>([]);
    let cidxPage        = $state(0);
    let cidxLoading     = $state(false);
    let cidxNextCursor  = $state<any>(null);
    let cidxSelected    = $state(new Set<string>());  // selected doc_ids
    let cidxPromoting   = $state(false);

    /** Open a `.caf` file produced by Cathy / Catfish / a previous
     *  CrispSorter session. Each entry becomes an L1 row in the active
     *  catalog (location_uri preserved so promotion to L2/L3 still
     *  works once the volume is mounted). */
    async function importCafFile() {
        const sel = await openDialog({
            multiple: false,
            filters: [{ name: 'Catfish catalog', extensions: ['caf'] }]
        });
        if (typeof sel !== 'string') return;
        if (!(await ensureIndexReady(false))) return;
        cafBusy = true;
        cafLastResult = null;
        try {
            logInfo(`CAF: importing ${sel}`);
            const result = await invoke<{
                ingested: number; skipped: number; errors: number;
                volume_label: string; volume_serial: number; volume_date: number;
            }>('index_import_caf', { path: sel });
            cafLastResult = `Imported ${result.ingested} entries from "${result.volume_label || sel}"`
                + (result.errors ? ` (${result.errors} errors)` : '')
                + (result.skipped ? ` (${result.skipped} skipped)` : '');
            logInfo(`CAF: ${cafLastResult}`);
            await loadContents();
        } catch (e: any) {
            cafLastResult = `Import failed: ${e?.message ?? e}`;
            logError(`CAF import failed: ${e?.message ?? e}`);
        } finally {
            cafBusy = false;
        }
    }

    /** Write the current catalog out as a `.caf` file readable by
     *  Cathy / Catfish / another CrispSorter installation. When the
     *  user has a selection in the Übersicht, only those rows are
     *  exported; otherwise the entire catalog. */
    async function importCbManifest() {
        const sel = await openDialog({ multiple: false, filters: [{ name: 'SQLite Database', extensions: ['db', 'sqlite', 'sqlite3'] }], title: 'cloud-backup Manifest öffnen' });
        if (typeof sel !== 'string') return;
        cafBusy = true;
        cafLastResult = null;
        // Persist manifest DB path for reverse lookups.
        saveSetting('cbManifestDbPath', sel).catch(() => {});
        try {
            const res = await invoke<{ ingested: number; total_rows: number; errors: number }>('index_ingest_cb_manifest', {
                manifestDbPath: sel,
                ownerId: null,
            });
            cafLastResult = `cloud-backup: ${res.ingested} von ${res.total_rows} Einträgen importiert (${res.errors} Fehler)`;
            logInfo(`cloud-backup manifest import: ${JSON.stringify(res)}`);
            await loadContents();
        } catch (e: any) {
            cafLastResult = `Fehler: ${e?.message ?? e}`;
            logError(`cloud-backup manifest import failed: ${e?.message ?? e}`);
        } finally {
            cafBusy = false;
        }
    }

    async function promoteSelectedCidxRows() {
        if (cidxSelected.size === 0 || cidxPromoting) return;
        const pyPath = await getSetting('cbRetrievePyPath', null);
        if (!pyPath) {
            const { open: od } = await import('@tauri-apps/plugin-dialog');
            const sel = await od({ filters: [{ name: 'Python script', extensions: ['py'] }], title: 'retrieve.py auswählen' });
            if (typeof sel !== 'string') return;
            saveSetting('cbRetrievePyPath', sel).catch(() => {});
        }
        const effectivePyPath = pyPath ?? await getSetting('cbRetrievePyPath', null);
        if (!effectivePyPath) return;

        cidxPromoting = true;
        const selected = [...cidxSelected];
        let ok = 0; let errs = 0;
        for (const docId of selected) {
            const doc = cidxContents.find(d => d.doc_id === docId);
            if (!doc) continue;
            const originalPath = cbArchiveOriginalPath(doc.location_uri ?? '');
            if (!originalPath) { errs++; continue; }
            try {
                await invoke('index_promote_cb_archive', {
                    docId, originalPath, retrievePyPath: effectivePyPath, outputDir: null, ownerId: null,
                });
                ok++;
                logInfo(`Promoted ${originalPath}`);
            } catch (e: any) {
                errs++;
                logError(`Promote failed ${originalPath}: ${e?.message ?? e}`);
            }
        }
        cidxSelected = new Set();
        cidxPromoting = false;
        logInfo(`Archiv-Promote: ${ok} ok, ${errs} Fehler`);
        if (ok > 0) await loadContents(); // refresh main catalog
    }

    async function mountCidxArchive() {
        const { open: openD } = await import('@tauri-apps/plugin-dialog');
        const sel = await openD({ directory: true, multiple: false,
            title: '.cidx-Archiv öffnen' });
        if (typeof sel !== 'string') return;
        cidxBusy = true;
        cidxLastResult = null;
        try {
            const info = await invoke<{ path: string; docs: number; chunks: number; has_fts: boolean }>('index_mount_cidx', { path: sel });
            mountedCidx = info;
            cidxContents = [];
            cidxPage = 0;
            cidxNextCursor = null;
            activeTab = 'cidxArchive';
            await loadCidxContents();
        } catch (e: any) {
            cidxLastResult = `Fehler: ${e?.message ?? e}`;
        } finally {
            cidxBusy = false;
        }
    }

    async function unmountCidxArchive() {
        await invoke('index_unmount_cidx').catch(() => {});
        mountedCidx = null;
        cidxContents = [];
        if (activeTab === 'cidxArchive') activeTab = 'overview';
    }

    async function loadCidxContents(append = false) {
        if (!mountedCidx) return;
        cidxLoading = true;
        try {
            const filter = buildDocumentFilter();
            const page = { limit: 200, cursor: append ? cidxNextCursor : null };
            const sort  = buildSortSpec();
            const res = await invoke<{ rows: any[]; next_cursor: any; total_estimate: number }>(
                'index_query_cidx_documents', { filter, sort, page }
            );
            cidxContents = append ? [...cidxContents, ...res.rows] : res.rows;
            cidxNextCursor = res.next_cursor ?? null;
        } catch (e: any) {
            console.error('cidx query failed:', e);
        } finally {
            cidxLoading = false;
        }
    }

    async function exportCidxArchive() {
        const { save } = await import('@tauri-apps/plugin-dialog');
        const savePath = await save({
            defaultPath: 'index-snapshot.cidx',
            filters: [{ name: 'CrispSorter index archive', extensions: ['cidx'] }],
        });
        if (typeof savePath !== 'string') return;
        cidxBusy = true;
        cidxLastResult = null;
        try {
            // Export all rows (no volume filter from UI; use CLI for per-volume export).
            const volumeId: string | null = null;
            logInfo(`cidx: exporting all rows → ${savePath}`);
            const rows = await invoke<number>('index_export_cidx', {
                destPath: savePath,
                volumeId,
                includeEmbeddings: false,
                includeFts: cidxIncludeFts,
            });
            cidxLastResult = `Exportiert: ${rows} Einträge → ${savePath}${cidxIncludeFts ? ' (+ Volltextsuche)' : ''}`;
            logInfo(`cidx: ${cidxLastResult}`);
        } catch (e: any) {
            cidxLastResult = `Export fehlgeschlagen: ${e?.message ?? e}`;
            logError(`cidx export failed: ${e?.message ?? e}`);
        } finally {
            cidxBusy = false;
        }
    }

    async function exportCafFile() {
        const filterDocIds = selectedDocIds.size > 0 ? [...selectedDocIds] : null;
        const out = await openDialog({
            // Tauri's open dialog with `save: false` is the picker; for
            // saving we use `dialog/save` via the same plugin. Use the
            // existing `save` import.
            ...({} as any),
        });
        // Use the dedicated save dialog for the destination path.
        const savePath = await import('@tauri-apps/plugin-dialog').then(m =>
            m.save({
                defaultPath: 'crispsorter.caf',
                filters: [{ name: 'Catfish catalog', extensions: ['caf'] }]
            })
        );
        if (typeof savePath !== 'string') return;
        cafBusy = true;
        cafLastResult = null;
        try {
            logInfo(`CAF: exporting${filterDocIds ? ' selection' : ' all rows'} to ${savePath}`);
            const written = await invoke<number>('index_export_caf', {
                path: savePath,
                docIds: filterDocIds,
            });
            cafLastResult = `Exported ${written} entries to ${savePath}`;
            logInfo(`CAF: ${cafLastResult}`);
        } catch (e: any) {
            cafLastResult = `Export failed: ${e?.message ?? e}`;
            logError(`CAF export failed: ${e?.message ?? e}`);
        } finally {
            cafBusy = false;
        }
    }

    /** Promote the selected catalog rows to full L3 (text + embedding).
     *  For each selected doc we resolve location_uri to a file path, read
     *  the bytes, extract text via the same pipeline used for fresh
     *  ingest, then batches via `index_ingest_batch`. Updates the live row
     *  in place — same `doc_id` is reused so the L1 metadata-only row is
     *  replaced by the new L3 chunks (deletion + insert is implicit
     *  because the source_hash matches). */
    async function promoteSelectedToL3() {
        const ids = [...selectedDocIds];
        if (ids.length === 0) return;
        promotingL3 = true;
        l3Progress = { done: 0, total: ids.length, current: '' };
        let ok = 0, fail = 0, skipped = 0;
        try {
            // L3 promotion needs the embedder — attach it now.
            if (!(await ensureIndexReady(true))) return;

            const BATCH_SIZE = 16;
            type L3Input = {
                fullText: string; fullTextMd: string; headings: string[];
                title: string | null; author: string | null; year: number | null;
                filename: string; ext: string; language: string;
                locationUri: string; ownerId: string; sourceHash: string; tags: string[];
            };
            const l3Buffer: L3Input[] = [];

            const flushL3 = async () => {
                if (l3Buffer.length === 0) return;
                const batch = l3Buffer.splice(0);
                try {
                    await invoke('index_ingest_batch', { inputs: batch });
                    ok += batch.length;
                } catch (e) {
                    console.error('[L3] batch promote failed:', e);
                    fail += batch.length;
                }
            };

            // Find the rows we're promoting in the currently-loaded contents.
            for (const id of ids) {
                const row = contents.find((c: any) => c.doc_id === id);
                if (!row) { fail++; continue; }
                l3Progress = { done: l3Progress?.done ?? 0, total: ids.length, current: row.filename ?? id.slice(0, 12) };

                // Resolve location_uri to a path. Strip crisp+local://… prefix.
                let path = String(row.location_uri ?? '');
                if (path.startsWith('crisp+local://')) {
                    const after = path.slice('crisp+local://'.length);
                    const slashIdx = after.indexOf('/');
                    path = slashIdx >= 0 ? after.slice(slashIdx) : after;
                }
                if (!path) { fail++; continue; }

                try {
                    const bytes = await readFile(path);
                    const ab = bytes.buffer as ArrayBuffer;
                    const filename = row.filename ?? path.split(/[\\/]/).pop() ?? id;
                    const ext = (row.ext ?? filename.split('.').pop() ?? '').toLowerCase();
                    const result = await extractText({
                        name: filename,
                        arrayBuffer: ab,
                        path,
                    });
                    if (!result.text || result.text.trim().length < 20) {
                        skipped++;
                        l3Progress = { done: (l3Progress?.done ?? 0) + 1, total: ids.length, current: row.filename ?? id };
                        continue;
                    }
                    const language = detectLanguage(result.text);
                    const sourceHash = await hashText(result.text + path);
                    l3Buffer.push({
                        fullText:    result.text,
                        fullTextMd:  result.markdownText ?? '',
                        headings:    result.headings ?? [],
                        title:       row.title  ?? null,
                        author:      row.author ?? null,
                        year:        row.year   ?? null,
                        filename,
                        ext,
                        language,
                        locationUri: row.location_uri,
                        ownerId:     row.owner_id ?? 'local',
                        sourceHash,
                        tags:        [],
                    });
                    if (l3Buffer.length >= BATCH_SIZE) await flushL3();
                } catch (e) {
                    console.error('[L3] promote failed for', id, e);
                    fail++;
                }
                l3Progress = { done: (l3Progress?.done ?? 0) + 1, total: ids.length, current: row.filename ?? id };
            }
            await flushL3();
            await loadContents();
            console.log(`[L3] ${ok} promoted, ${skipped} skipped (no text), ${fail} errors`);
        } finally {
            promotingL3 = false;
            l3Progress = null;
        }
    }

    /** Read embedded metadata (PDF Info / DOCX core / EPUB OPF) for the
     *  selected docs and write Title / Author / Year / Language back to
     *  the LanceDB row. The Rust side bumps `metadata_json.level` to 2. */
    async function promoteSelectedToL2() {
        const ids = [...selectedDocIds];
        if (ids.length === 0) return;
        promotingL2 = true;
        try {
            const results = await invoke<Array<{
                doc_id: string; updated: boolean;
                title: string | null; author: string | null; year: number | null;
                error: string | null;
            }>>('index_promote_l2', { docIds: ids });
            // Reload to reflect updates.
            await loadContents();
            // Surface a summary in the console (could be a toast later).
            const updated = results.filter(r => r.updated).length;
            const errored = results.filter(r => r.error).length;
            console.log(`[L2] ${updated}/${results.length} updated, ${errored} errors`);
        } catch (e) {
            console.error('[L2] promote failed:', e);
        } finally {
            promotingL2 = false;
        }
    }

    function toggleSelect(docId: string) {
        const next = new Set(selectedDocIds);
        if (next.has(docId)) next.delete(docId); else next.add(docId);
        selectedDocIds = next;
    }

    function toggleSelectAll() {
        if (selectedDocIds.size === contents.length) {
            selectedDocIds = new Set();
        } else {
            selectedDocIds = new Set(contents.map((d: any) => d.doc_id));
        }
    }

    async function openIndexedFile(locationUri: string) {
        let path = locationUri;
        if (path.startsWith('crisp+local://')) {
            const afterScheme = path.slice('crisp+local://'.length);
            const slashIdx = afterScheme.indexOf('/');
            path = slashIdx >= 0 ? afterScheme.slice(slashIdx) : afterScheme;
        }
        try { await openPath(path); } catch (e) { console.error('openPath failed:', e); }
    }

    // ── Helpers ────────────────────────────────────────────────────────────────

    function updateEntry(id: string, patch: Partial<IngestEntry>) {
        entries = entries.map(e => e.id === id ? { ...e, ...patch } : e);
    }

    function mimeFor(ext: string): string {
        return ({
            pdf:  'application/pdf',
            docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
            doc:  'application/msword',
            txt:  'text/plain',
            md:   'text/markdown',
            epub: 'application/epub+zip',
            html: 'text/html',
            htm:  'text/html',
            webp: 'image/webp',
            png:  'image/png',
            jpg:  'image/jpeg',
            jpeg: 'image/jpeg',
            bmp:  'image/bmp',
            tif:  'image/tiff',
            tiff: 'image/tiff',
        } as any)[ext] ?? 'application/octet-stream';
    }

    async function hashText(text: string): Promise<string> {
        const buf = new TextEncoder().encode(text.slice(0, 65536));
        const digest = await crypto.subtle.digest('SHA-256', buf);
        return Array.from(new Uint8Array(digest)).map(b => b.toString(16).padStart(2, '0')).join('');
    }

    function statusColor(s: FileStatus): string {
        return { pending: '#71717a', extracting: '#f59e0b', embedding: '#3b82f6', done: '#22c55e', error: '#ef4444', skipped: '#52525b' }[s] ?? '#71717a';
    }

    /** Audio/video extensions — render "Transcribing" instead of the
     *  generic "Extracting" so the user knows whisper is running.  Mirrors
     *  the Stapel (BatchReview) behaviour added in P13.6 Step 1. */
    const AUDIO_EXTS_SET_INGEST = new Set<string>(AUDIO_EXTENSIONS);

    function statusLabel(s: FileStatus, ext?: string): string {
        if (s === 'extracting' && ext && AUDIO_EXTS_SET_INGEST.has(ext.toLowerCase())) {
            return i18n.t.batch.status_transcribing + '…';
        }
        // Pre-existing inline DE strings — Denglish carry-over from the
        // P11 ingest UI; a full i18n cleanup is its own follow-up.
        return { pending: 'Ausstehend', extracting: 'Extrahiere…', embedding: 'Indexiere…', done: 'Fertig', error: 'Fehler', skipped: 'Übersprungen' }[s] ?? s;
    }

    function fmtDate(ts: number | null): string {
        if (!ts) return '–';
        return new Date(ts).toLocaleString('de-DE', { dateStyle: 'short', timeStyle: 'short' });
    }

    function progressPct(entry: IngestEntry): number {
        if (entry.status === 'done') return 100;
        if (entry.chunksTotal && entry.chunksTotal > 0) return Math.round(entry.chunksDone! / entry.chunksTotal * 100);
        if (entry.status === 'extracting') return 10;
        if (entry.status === 'embedding') return 40;
        return 0;
    }
</script>

<div class="ingest-root"
    role="region"
    ondragover={onDragover}
    ondragleave={onDragleave}
    ondrop={onDrop}>

    <!-- ── Tab bar (Kataloge sub-views) ──────────────────────────────────── -->
    <div class="tab-bar">
        <button class="tab" class:active={activeTab === 'overview'} onclick={() => { activeTab = 'overview'; loadContents(); }}>
            <Database size={14} /> {i18n.t.indexIngest.tab_overview}{#if indexStats !== null} ({indexStats.doc_count}){/if}
        </button>
        <button class="tab" class:active={activeTab === 'search'} onclick={() => activeTab = 'search'}>
            <Search size={14} /> {i18n.t.indexIngest.tab_search}
        </button>
        <button class="tab" class:active={activeTab === 'add'} onclick={() => activeTab = 'add'}>
            <UploadCloud size={14} /> {i18n.t.indexIngest.tab_add}
        </button>
        <button class="tab" class:active={activeTab === 'sources'} onclick={() => activeTab = 'sources'}>
            <FolderOpen size={14} /> {i18n.t.indexIngest.tab_sources} ({folders.length})
        </button>
        <button class="tab" class:active={activeTab === 'cafCatalog'} onclick={() => activeTab = 'cafCatalog'}>
            <HardDrive size={14} /> {i18n.t.indexIngest.tab_caf_catalog}
        </button>
        <button class="tab" class:active={activeTab === 'duplicates'} onclick={() => activeTab = 'duplicates'}>
            <CopyCheck size={14} /> {i18n.t.indexIngest.tab_duplicates}
        </button>
        <button class="tab" class:active={activeTab === 'images'}
                onclick={() => { activeTab = 'images'; loadImages(false); }}>
            <Images size={14} /> {i18n.t.indexIngest.tab_images}{#if imagesTotal > 0} ({imagesTotal}){/if}
        </button>
        {#if mountedCidx}
            <button class="tab cidx-tab" class:active={activeTab === 'cidxArchive'}
                    onclick={() => { activeTab = 'cidxArchive'; loadCidxContents(); }}>
                <Database size={14} /> Archiv ({mountedCidx.docs})
            </button>
        {/if}
        <button class="tab tb-btn-flat" onclick={mountCidxArchive} disabled={cidxBusy}
                title=".cidx-Archiv einlesen (offline-Index)">
            {#if cidxBusy}<Loader2 size={12} class="spin" />{:else}+.cidx{/if}
        </button>
    </div>

    {#if activeTab === 'cafCatalog'}
        <CafCatalog />
    {:else if activeTab === 'duplicates'}
        <Duplicates />
    {:else if activeTab === 'images'}
        <!-- ══════════════════ IMAGES (P13 slice A2 — thumbnails + EXIF preview) ══════════════════ -->
        <div class="images-split" class:with-preview={imagesPreview !== null}>
            <div class="images-panel">
                {#if crispLensBannerState === 'offline'}
                    <div class="crisplens-banner crisplens-banner-offline" role="status">
                        <AlertCircle size={14} />
                        <span>{i18n.t.indexIngest.crisplens_banner_offline}</span>
                        {#if crispLensStatus?.error}
                            <code class="crisplens-banner-detail">{crispLensStatus.error}</code>
                        {/if}
                        <button class="action-btn small" onclick={fetchCrispLensStatus}>
                            <RefreshCw size={12} /> {i18n.t.indexIngest.crisplens_banner_retry}
                        </button>
                    </div>
                {:else if crispLensBannerState === 'session_expired'}
                    <div class="crisplens-banner crisplens-banner-warn" role="status">
                        <AlertCircle size={14} />
                        <span>{i18n.t.indexIngest.crisplens_banner_session_expired}</span>
                        <button class="action-btn small" onclick={() => { crispLensSettingsOpen = true; }}>
                            {i18n.t.indexIngest.crisplens_banner_login_cta}
                        </button>
                    </div>
                {:else if crispLensBannerState === 'warming_up'}
                    <div class="crisplens-banner crisplens-banner-info" role="status">
                        <Loader2 size={14} class="spin" />
                        <span>{i18n.t.indexIngest.crisplens_banner_warming_up}</span>
                    </div>
                {/if}
                <div class="toolbar">
                    <div class="toolbar-actions">
                        <button class="tb-btn" onclick={() => loadImages(false)} disabled={imagesLoading || imagesDupMode}>
                            <RefreshCw size={14} /> {i18n.t.indexIngest.images_refresh}
                        </button>
                        <button class="tb-btn" class:active={imagesDupMode} onclick={toggleImagesDupMode}
                                title={i18n.t.indexIngest.images_duplicates_toggle_hint}>
                            <CopyCheck size={14} /> {i18n.t.indexIngest.images_duplicates_toggle}
                            {#if imagesDupMode && imagesDupGroups.length > 0}
                                <span class="muted-small">({imagesDupGroups.length})</span>
                            {/if}
                        </button>
                        <button class="tb-btn" class:active={imagesNearDupMode} onclick={toggleImagesNearDupMode}
                                title={i18n.t.indexIngest.images_near_duplicates_toggle_hint}>
                            <Eye size={14} /> {i18n.t.indexIngest.images_near_duplicates_toggle}
                            {#if imagesNearDupMode && imagesNearDupGroups.length > 0}
                                <span class="muted-small">({imagesNearDupGroups.length})</span>
                            {/if}
                        </button>
                        {#if imagesNearDupMode}
                            <label class="muted-small images-threshold-label">
                                {i18n.t.indexIngest.images_near_duplicates_threshold}
                                <input type="number" class="images-threshold-input"
                                       min="0" max="64" bind:value={imagesNearDupThreshold}
                                       onchange={() => loadImagesNearDuplicates()}
                                       disabled={imagesNearDupLoading} />
                            </label>
                        {/if}
                        {#if crispLensStatus?.authenticated}
                            <button class="tb-btn" class:active={imagesPeopleMode}
                                    onclick={toggleImagesPeopleMode}
                                    title={i18n.t.indexIngest.crisplens_people_toggle_hint}>
                                <Eye size={14} /> {i18n.t.indexIngest.crisplens_people_toggle}
                                {#if imagesPeopleMode && crispLensPeople.length > 0}
                                    <span class="muted-small">({crispLensPeople.length})</span>
                                {/if}
                            </button>
                            <input type="search" class="crisplens-search-input"
                                   bind:value={crispLensSearchQuery}
                                   placeholder={i18n.t.indexIngest.crisplens_search_placeholder}
                                   title={i18n.t.indexIngest.crisplens_search_hint}
                                   onkeydown={(e) => { if (e.key === 'Enter') void runCrispLensSearch(); }} />
                            <button class="tb-btn" onclick={runCrispLensSearch}
                                    disabled={crispLensSearchLoading || !crispLensSearchQuery.trim()}>
                                <Search size={14} /> {i18n.t.indexIngest.crisplens_search_button}
                            </button>
                        {/if}
                        <button class="tb-btn" class:active={crispLensSettingsOpen}
                                onclick={toggleCrispLensSettings}
                                title={i18n.t.indexIngest.crisplens_settings_toggle_hint}>
                            <ExternalLink size={14} /> {i18n.t.indexIngest.crisplens_settings_toggle}
                            {#if crispLensAuthenticated}
                                <span class="muted-small">●</span>
                            {/if}
                        </button>
                        <span class="muted images-count">
                            {#if imagesNearDupMode}
                                {#if imagesNearDupLoading}
                                    <Loader2 size={12} class="spin" />
                                {:else if imagesNearDupError}
                                    {imagesNearDupError}
                                {:else}
                                    {imagesNearDupGroups.reduce((s, g) => s + g.items.length, 0)} {i18n.t.indexIngest.images_near_duplicates_count_suffix}
                                {/if}
                            {:else if imagesDupMode}
                                {#if imagesDupLoading}
                                    <Loader2 size={12} class="spin" />
                                {:else if imagesDupError}
                                    {imagesDupError}
                                {:else}
                                    {imagesDupGroups.reduce((s, g) => s + g.items.length, 0)} {i18n.t.indexIngest.images_duplicates_count_suffix}
                                {/if}
                            {:else if imagesLoading && imagesRows.length === 0}
                                <Loader2 size={12} class="spin" />
                            {:else}
                                {imagesRows.length} / {imagesTotal} {i18n.t.indexIngest.images_count_suffix}
                            {/if}
                        </span>
                    </div>
                </div>
                {#if crispLensSearchHits.length > 0 || crispLensSearchError || crispLensSearchLoading}
                    <div class="crisplens-search-results">
                        <header class="crisplens-search-header">
                            <span class="muted-small">
                                {#if crispLensSearchLoading}
                                    <Loader2 size={12} class="spin" /> {i18n.t.indexIngest.crisplens_search_loading}
                                {:else if crispLensSearchError}
                                    {crispLensSearchError}
                                {:else}
                                    {crispLensSearchHits.length} {i18n.t.indexIngest.crisplens_search_count_suffix}
                                {/if}
                            </span>
                            <button class="action-btn small" onclick={() => { crispLensSearchHits = []; crispLensSearchQuery = ''; }}>
                                <X size={12} />
                            </button>
                        </header>
                        {#if crispLensSearchHits.length > 0}
                            <ul class="crisplens-search-list">
                                {#each crispLensSearchHits as h (h.id)}
                                    <li class="crisplens-search-hit" title={h.filepath}>
                                        <span class="crisplens-search-name">{h.filename}</span>
                                        {#if h.faceCount && h.faceCount > 0}
                                            <span class="muted-small">{h.faceCount}f</span>
                                        {/if}
                                        {#if h.tags && h.tags.length > 0}
                                            <span class="muted-small crisplens-search-tags">{h.tags.slice(0, 4).join(' · ')}</span>
                                        {/if}
                                    </li>
                                {/each}
                            </ul>
                        {/if}
                    </div>
                {/if}
                {#if crispLensSettingsOpen}
                    <div class="crisplens-settings-pane">
                        <h4 class="crisplens-heading">{i18n.t.indexIngest.crisplens_pane_heading}</h4>
                        <p class="muted-small crisplens-helper">{i18n.t.indexIngest.crisplens_pane_help}</p>
                        <label class="crisplens-row">
                            <span>{i18n.t.indexIngest.crisplens_backend}:</span>
                            <select bind:value={crispLensBackendDraft} disabled={crispLensBusy}>
                                <option value="local">{i18n.t.indexIngest.crisplens_backend_local}</option>
                                <option value="crisplens">{i18n.t.indexIngest.crisplens_backend_remote}</option>
                            </select>
                        </label>
                        <label class="crisplens-row">
                            <span>{i18n.t.indexIngest.crisplens_url}:</span>
                            <input type="url" bind:value={crispLensUrlDraft}
                                   placeholder="https://crisplens.example.com"
                                   disabled={crispLensBusy} />
                        </label>
                        <div class="crisplens-row">
                            <button class="action-btn small primary"
                                    onclick={saveCrispLensSettings} disabled={crispLensBusy}>
                                {crispLensBusy ? '…' : i18n.t.indexIngest.crisplens_save}
                            </button>
                        </div>
                        {#if crispLensBackendDraft === 'crisplens' && crispLensUrlDraft.trim() !== ''}
                            <hr class="crisplens-sep" />
                            <h5 class="crisplens-heading">{i18n.t.indexIngest.crisplens_login_heading}</h5>
                            <p class="muted-small crisplens-helper">
                                {crispLensAuthenticated ? i18n.t.indexIngest.crisplens_login_authed : i18n.t.indexIngest.crisplens_login_unauthed}
                            </p>
                            {#if !crispLensAuthenticated}
                                <label class="crisplens-row">
                                    <span>{i18n.t.indexIngest.crisplens_user}:</span>
                                    <input type="text" autocomplete="username"
                                           bind:value={crispLensLoginUser} disabled={crispLensBusy} />
                                </label>
                                <label class="crisplens-row">
                                    <span>{i18n.t.indexIngest.crisplens_password}:</span>
                                    <input type="password" autocomplete="current-password"
                                           bind:value={crispLensLoginPassword} disabled={crispLensBusy} />
                                </label>
                                <div class="crisplens-row">
                                    <button class="action-btn small primary"
                                            onclick={crispLensLoginNow}
                                            disabled={crispLensBusy || !crispLensLoginUser || !crispLensLoginPassword}>
                                        {crispLensBusy ? '…' : i18n.t.indexIngest.crisplens_login_button}
                                    </button>
                                </div>
                            {:else}
                                <div class="crisplens-row">
                                    <button class="action-btn small"
                                            onclick={crispLensLogoutNow} disabled={crispLensBusy}>
                                        {crispLensBusy ? '…' : i18n.t.indexIngest.crisplens_logout_button}
                                    </button>
                                </div>
                            {/if}
                        {/if}
                        {#if crispLensError}
                            <div class="preview-msg preview-error crisplens-msg">{crispLensError}</div>
                        {/if}
                        {#if crispLensInfo}
                            <div class="muted-small crisplens-msg">{crispLensInfo}</div>
                        {/if}
                    </div>
                {/if}
                {#if imagesPeopleMode}
                    {#if crispLensPeopleLoading && crispLensPeople.length === 0}
                        <div class="empty-state"><Loader2 size={20} class="spin" /></div>
                    {:else if crispLensPeopleError}
                        <div class="preview-msg preview-error" style="margin: 12px;">
                            {crispLensPeopleError}
                        </div>
                    {:else if crispLensPeople.length === 0}
                        <div class="empty-state">{i18n.t.indexIngest.crisplens_people_empty}</div>
                    {:else}
                        <div class="crisplens-people-grid">
                            {#each crispLensPeople as person (person.id)}
                                <div class="crisplens-person-card"
                                     title={`${person.name} — ${person.appearances ?? 0} appearances`}>
                                    <div class="crisplens-person-cover">
                                        <Eye size={28} />
                                    </div>
                                    <div class="crisplens-person-name" title={person.name}>{person.name}</div>
                                    <div class="muted-small">
                                        {person.appearances ?? 0}× · #{person.id}
                                    </div>
                                </div>
                            {/each}
                        </div>
                    {/if}
                {:else if imagesNearDupMode}
                    {#if imagesNearDupLoading && imagesNearDupGroups.length === 0}
                        <div class="empty-state"><Loader2 size={20} class="spin" /> {i18n.t.indexIngest.images_near_duplicates_loading}</div>
                    {:else if imagesNearDupGroups.length === 0}
                        <div class="empty-state">{i18n.t.indexIngest.images_near_duplicates_empty}</div>
                    {:else}
                        <div class="images-dup-list">
                            {#each imagesNearDupGroups as group (group.representativePhashHex)}
                                <section class="images-dup-group">
                                    <header class="images-dup-header">
                                        <span class="images-dup-count">{group.items.length}×</span>
                                        <code class="images-dup-hash" title={'pHash 0x' + group.representativePhashHex}>pHash 0x{group.representativePhashHex}</code>
                                    </header>
                                    <div class="images-grid">
                                        {#each group.items as it (it.image.docId)}
                                            <button class="images-tile" type="button"
                                                    class:images-tile-active={imagesPreview?.docId === it.image.docId}
                                                    title={`${it.image.locationUri}\nHamming distance from cluster rep: ${it.distanceFromRep}`}
                                                    onclick={() => openImagesPreview(it.image)}
                                                    use:lazyThumbnail={it.image.docId}>
                                                {#if imagesThumbs[it.image.docId] && imagesThumbs[it.image.docId] !== 'loading' && imagesThumbs[it.image.docId] !== 'error'}
                                                    <img src={imagesThumbs[it.image.docId]} alt={it.image.filename ?? ''} class="images-thumb" loading="lazy" />
                                                {:else}
                                                    <div class="images-tile-placeholder">
                                                        {#if imagesThumbs[it.image.docId] === 'loading'}
                                                            <Loader2 size={20} class="spin" />
                                                        {:else}
                                                            <Images size={28} />
                                                        {/if}
                                                        <span class="images-ext-badge">{(it.image.ext ?? '?').toUpperCase()}</span>
                                                    </div>
                                                {/if}
                                                <div class="images-tile-name" title={it.image.filename ?? ''}>
                                                    {it.image.filename ?? it.image.docId}
                                                    <span class="images-ndup-distance">d={it.distanceFromRep}</span>
                                                </div>
                                            </button>
                                        {/each}
                                    </div>
                                </section>
                            {/each}
                        </div>
                    {/if}
                {:else if imagesDupMode}
                    {#if imagesDupLoading && imagesDupGroups.length === 0}
                        <div class="empty-state"><Loader2 size={20} class="spin" /></div>
                    {:else if imagesDupGroups.length === 0}
                        <div class="empty-state">{i18n.t.indexIngest.images_duplicates_empty}</div>
                    {:else}
                        <div class="images-dup-list">
                            {#each imagesDupGroups as group (group.sourceHash)}
                                <section class="images-dup-group">
                                    <header class="images-dup-header">
                                        <span class="images-dup-count">{group.items.length}×</span>
                                        <code class="images-dup-hash" title={group.sourceHash}>{group.sourceHash.slice(0, 12)}…</code>
                                    </header>
                                    <div class="images-grid">
                                        {#each group.items as img (img.docId)}
                                            <button class="images-tile" type="button"
                                                    class:images-tile-active={imagesPreview?.docId === img.docId}
                                                    title={img.locationUri}
                                                    onclick={() => openImagesPreview(img)}
                                                    use:lazyThumbnail={img.docId}>
                                                {#if imagesThumbs[img.docId] && imagesThumbs[img.docId] !== 'loading' && imagesThumbs[img.docId] !== 'error'}
                                                    <img src={imagesThumbs[img.docId]} alt={img.filename ?? ''} class="images-thumb" loading="lazy" />
                                                {:else}
                                                    <div class="images-tile-placeholder">
                                                        {#if imagesThumbs[img.docId] === 'loading'}
                                                            <Loader2 size={20} class="spin" />
                                                        {:else}
                                                            <Images size={28} />
                                                        {/if}
                                                        <span class="images-ext-badge">{(img.ext ?? '?').toUpperCase()}</span>
                                                    </div>
                                                {/if}
                                                <div class="images-tile-name" title={img.filename ?? ''}>
                                                    {img.filename ?? img.docId}
                                                </div>
                                            </button>
                                        {/each}
                                    </div>
                                </section>
                            {/each}
                        </div>
                    {/if}
                {:else if imagesLoading && imagesRows.length === 0}
                    <div class="empty-state"><Loader2 size={20} class="spin" /></div>
                {:else if imagesRows.length === 0}
                    <div class="empty-state">{i18n.t.indexIngest.images_empty}</div>
                {:else}
                    <div class="images-grid">
                        {#each imagesRows as img (img.docId)}
                            <button class="images-tile" type="button"
                                    class:images-tile-active={imagesPreview?.docId === img.docId}
                                    title={img.locationUri}
                                    onclick={() => openImagesPreview(img)}
                                    use:lazyThumbnail={img.docId}>
                                {#if imagesThumbs[img.docId] && imagesThumbs[img.docId] !== 'loading' && imagesThumbs[img.docId] !== 'error'}
                                    <img src={imagesThumbs[img.docId]} alt={img.filename ?? ''} class="images-thumb" loading="lazy" />
                                {:else}
                                    <div class="images-tile-placeholder">
                                        {#if imagesThumbs[img.docId] === 'loading'}
                                            <Loader2 size={20} class="spin" />
                                        {:else}
                                            <Images size={28} />
                                        {/if}
                                        <span class="images-ext-badge">{(img.ext ?? '?').toUpperCase()}</span>
                                    </div>
                                {/if}
                                <div class="images-tile-name" title={img.filename ?? ''}>
                                    {img.filename ?? img.docId}
                                </div>
                            </button>
                        {/each}
                    </div>
                    {#if imagesNextCursor}
                        <div class="load-more-bar">
                            <button class="action-btn small" onclick={() => loadImages(true)} disabled={imagesLoading}>
                                {imagesLoading ? '…' : i18n.t.indexIngest.images_load_more}
                            </button>
                        </div>
                    {/if}
                {/if}
            </div><!-- /images-panel -->

            {#if imagesPreview !== null}
                <aside class="images-preview-pane">
                    <header class="images-preview-header">
                        <span class="images-preview-title" title={imagesPreview.locationUri}>
                            {imagesPreview.filename ?? imagesPreview.docId}
                        </span>
                        {#if crispLensStatus?.authenticated && crispLensSettings?.url}
                            <button class="preview-action"
                                    onclick={openInCrispLens}
                                    title={i18n.t.indexIngest.crisplens_open_hint}>
                                <ExternalLink size={12} /> {i18n.t.indexIngest.crisplens_open}
                            </button>
                        {/if}
                        <button class="preview-close" onclick={closeImagesPreview} title={i18n.t.indexIngest.images_preview_close}>
                            <X size={14} />
                        </button>
                    </header>
                    {#if watchfolderForCurrentPreview()}
                        <div class="crisplens-watchfolder-hint" role="status">
                            <Eye size={12} />
                            <span>{i18n.t.indexIngest.crisplens_watchfolder_hint}</span>
                            <code title={watchfolderForCurrentPreview() ?? ''}>{watchfolderForCurrentPreview()}</code>
                        </div>
                    {/if}
                    <div class="images-preview-body">
                        {#if imagesPreviewSrc}
                            <div class="images-preview-image-wrap">
                                <img src={imagesPreviewSrc} alt={imagesPreview.filename ?? ''} class="images-preview-image" />
                                {#each imagesPreviewFaces as face (face.faceId)}
                                    <!-- bbox values are normalised 0..1 of the source image; CSS % directly. -->
                                    <div class="images-face-bbox"
                                         style:left={`${face.bbox.left * 100}%`}
                                         style:top={`${face.bbox.top * 100}%`}
                                         style:width={`${(face.bbox.right  - face.bbox.left) * 100}%`}
                                         style:height={`${(face.bbox.bottom - face.bbox.top)  * 100}%`}
                                         title={face.personName ?? '(unidentified)'}>
                                        {#if face.personName}
                                            <span class="images-face-label">{face.personName}</span>
                                        {/if}
                                    </div>
                                {/each}
                            </div>
                            {#if imagesPreviewFacesLoading}
                                <div class="muted-small images-face-loading">
                                    <Loader2 size={12} class="spin" /> {i18n.t.indexIngest.crisplens_faces_loading}
                                </div>
                            {:else if imagesPreviewFacesError}
                                <div class="muted-small images-face-loading">{imagesPreviewFacesError}</div>
                            {:else if imagesPreviewFaces.length > 0}
                                <div class="muted-small images-face-loading">
                                    {imagesPreviewFaces.length} {i18n.t.indexIngest.crisplens_faces_count_suffix}
                                </div>
                            {/if}
                        {:else}
                            <div class="preview-msg preview-error">{i18n.t.indexIngest.images_preview_no_local_path}</div>
                        {/if}
                        <div class="images-exif-block">
                            <h3 class="images-exif-heading">{i18n.t.indexIngest.images_exif_heading}</h3>
                            {#if imagesPreviewExifLoading}
                                <div class="images-exif-loading"><Loader2 size={14} class="spin" /> {i18n.t.indexIngest.images_exif_loading}</div>
                            {:else if imagesPreviewExifError}
                                <div class="preview-msg preview-error">{imagesPreviewExifError}</div>
                            {:else if imagesPreviewExif}
                                <table class="images-exif-table">
                                    <tbody>
                                        {#if imagesPreviewExif.cameraMake}<tr><th>{i18n.t.indexIngest.exif_camera_make}</th><td>{imagesPreviewExif.cameraMake}</td></tr>{/if}
                                        {#if imagesPreviewExif.cameraModel}<tr><th>{i18n.t.indexIngest.exif_camera_model}</th><td>{imagesPreviewExif.cameraModel}</td></tr>{/if}
                                        {#if imagesPreviewExif.lensModel}<tr><th>{i18n.t.indexIngest.exif_lens}</th><td>{imagesPreviewExif.lensModel}</td></tr>{/if}
                                        {#if imagesPreviewExif.takenAt}<tr><th>{i18n.t.indexIngest.exif_taken_at}</th><td>{imagesPreviewExif.takenAt.replace('T', ' ')}</td></tr>{/if}
                                        {#if imagesPreviewExif.width != null && imagesPreviewExif.height != null}<tr><th>{i18n.t.indexIngest.exif_dimensions}</th><td>{imagesPreviewExif.width} × {imagesPreviewExif.height}</td></tr>{/if}
                                        {#if imagesPreviewExif.fNumber != null}<tr><th>{i18n.t.indexIngest.exif_aperture}</th><td>f/{imagesPreviewExif.fNumber.toFixed(1)}</td></tr>{/if}
                                        {#if imagesPreviewExif.exposureTime}<tr><th>{i18n.t.indexIngest.exif_exposure}</th><td>{imagesPreviewExif.exposureTime}</td></tr>{/if}
                                        {#if imagesPreviewExif.iso != null}<tr><th>{i18n.t.indexIngest.exif_iso}</th><td>{imagesPreviewExif.iso}</td></tr>{/if}
                                        {#if imagesPreviewExif.focalLengthMm != null}<tr><th>{i18n.t.indexIngest.exif_focal_length}</th><td>{imagesPreviewExif.focalLengthMm.toFixed(1)} mm</td></tr>{/if}
                                        {#if imagesPreviewExif.gpsLat != null && imagesPreviewExif.gpsLon != null}<tr><th>{i18n.t.indexIngest.exif_gps}</th><td>{imagesPreviewExif.gpsLat.toFixed(6)}, {imagesPreviewExif.gpsLon.toFixed(6)}</td></tr>{/if}
                                        {#if imagesPreviewExif.orientation != null}<tr><th>{i18n.t.indexIngest.exif_orientation}</th><td>{imagesPreviewExif.orientation}</td></tr>{/if}
                                    </tbody>
                                </table>
                                {#if !imagesPreviewExif.cameraMake && !imagesPreviewExif.cameraModel && !imagesPreviewExif.takenAt && imagesPreviewExif.width == null}
                                    <div class="images-exif-empty">{i18n.t.indexIngest.images_exif_empty}</div>
                                {/if}
                            {/if}
                        </div>
                    </div>
                </aside>
            {/if}
        </div>
    {:else if activeTab === 'cidxArchive' && mountedCidx}
        <div class="cidx-archive-panel">
            <div class="cidx-header">
                <span class="cidx-path" title={mountedCidx.path}>
                    <Database size={13} /> {mountedCidx.path.split(/[\\/]/).pop()} —
                    {mountedCidx.docs} Dok., {mountedCidx.chunks} Chunks
                    {#if mountedCidx.has_fts}<span style="color:#6366f1;font-size:0.7rem;margin-left:4px">• FTS</span>{/if}
                </span>
                <button class="icon-btn danger-icon" onclick={unmountCidxArchive} title="Archiv aushängen">
                    <X size={13} />
                </button>
            </div>
            {#if cidxLoading && cidxContents.length === 0}
                <div class="empty-state"><Loader2 size={20} class="spin" /></div>
            {:else if cidxContents.length === 0}
                <div class="empty-state">Keine Einträge</div>
            {:else}
                {#if cidxSelected.size > 0}
                    <div class="cidx-sel-bar">
                        <span>{cidxSelected.size} ausgewählt</span>
                        <button class="action-btn small primary" onclick={promoteSelectedCidxRows} disabled={cidxPromoting}>
                            {#if cidxPromoting}<Loader2 size={12} class="spin" /> Lade…{:else}<CloudDownload size={12} /> Auf L3 hochstufen{/if}
                        </button>
                        <button class="action-btn small" onclick={() => cidxSelected = new Set()}>Abwählen</button>
                    </div>
                {/if}
                <div class="catalog-tbody cidx-rows">
                    {#each cidxContents as doc (doc.doc_id)}
                        {@const failure = extractionFailure(doc)}
                        {@const lvl = docLevel(doc)}
                        {@const isCbArchive = doc.location_uri?.startsWith('crisp+cb-archive://')}
                        <div class="catalog-row cidx-row" class:cidx-sel={cidxSelected.has(doc.doc_id)} role="row" tabindex="0"
                             onclick={() => { if (cidxSelected.has(doc.doc_id)) { cidxSelected.delete(doc.doc_id); cidxSelected = new Set(cidxSelected); } else { cidxSelected = new Set([...cidxSelected, doc.doc_id]); } }}>
                            <div class="cell" style="width:20px;flex-shrink:0;">
                                <input type="checkbox" checked={cidxSelected.has(doc.doc_id)}
                                       onclick={(e) => e.stopPropagation()}
                                       onchange={() => { if (cidxSelected.has(doc.doc_id)) { cidxSelected.delete(doc.doc_id); cidxSelected = new Set(cidxSelected); } else { cidxSelected = new Set([...cidxSelected, doc.doc_id]); } }} />
                            </div>
                            <div class="cell col-title">{doc.title ?? doc.filename ?? ''}</div>
                            <div class="cell col-author">{doc.author ?? ''}</div>
                            <div class="cell col-year">{doc.year ?? ''}</div>
                            <div class="cell col-level">
                                <span class="level-badge" class:l1={lvl===1} class:l2={lvl===2} class:l3={lvl===3}>L{lvl}</span>
                                {#if failure}
                                    <span class="fail-badge fail-{failure.reason}" title={FAIL_HINTS[failure.reason] ?? failure.msg}>{FAIL_LABELS[failure.reason] ?? failure.reason}</span>
                                {/if}
                                {#if isCbArchive && lvl === 1}
                                    <span class="inline-badge" style="background:#1e3a5f55;color:#93c5fd;" title="Aus cloud-backup-Archiv — klick zum Hochstufen">archiv</span>
                                {/if}
                            </div>
                            <div class="cell col-ext">{doc.ext ?? ''}</div>
                        </div>
                    {/each}
                    {#if cidxNextCursor}
                        <div class="load-more-bar">
                            <button class="action-btn small" onclick={() => loadCidxContents(true)} disabled={cidxLoading}>
                                {cidxLoading ? 'Lade…' : 'Mehr laden'}
                            </button>
                        </div>
                    {/if}
                </div>
            {/if}
        </div>
    {/if}

    <!-- ══════════════════ HINZUFÜGEN (queue + ingest run) ══════════════════ -->
    {#if activeTab === 'add'}
        <div class="toolbar">
            <div class="toolbar-actions">
                <button class="tb-btn" onclick={addFiles}><FileText size={14} /> Dateien</button>
                <button class="tb-btn" onclick={() => { activeTab = 'sources'; }}>
                    <FolderOpen size={14} /> Ordner verwalten
                </button>
                <span class="level-inline-label">Tiefe:</span>
                <button class="chip" class:active={ingestLevel === 1} onclick={() => ingestLevel = 1} title="Nur Pfad / Größe / Datum (sehr schnell)">L1</button>
                <button class="chip" class:active={ingestLevel === 2} onclick={() => ingestLevel = 2} title="Eingebettete Metadaten (PDF Info, DOCX core, EPUB OPF, EXIF)">L2</button>
                <button class="chip" class:active={ingestLevel === 3} onclick={() => ingestLevel = 3} title="Volltext extrahieren + embedden">L3</button>
                {#if stats.done > 0}
                    <button class="tb-btn ghost" onclick={clearDone}><Trash2 size={14} /> Fertige entfernen</button>
                {/if}
                {#if entries.length > 0 && !running}
                    <button class="tb-btn ghost danger" onclick={clearAll}><X size={14} /> Alle löschen</button>
                {/if}
            </div>
            <div class="level-hint-inline">
                {#if ingestLevel === 1}
                    L1 — Pfad, Größe, Datum, Endung. Kein Embedder nötig.
                {:else if ingestLevel === 2}
                    L2 — Liest eingebettete Metadaten (PDF/DOCX/EPUB/EXIF). Kein Embedder nötig.
                {:else}
                    L3 — Volltext + Embedding. Embedder-Modell + Such-Index erforderlich.
                {/if}
            </div>
        </div>

        <!-- Stats bar -->
        {#if entries.length > 0}
            <div class="stats-bar">
                <span class="sb-chip total">{stats.total} Dateien</span>
                {#if stats.done > 0}    <span class="sb-chip done">{stats.done} fertig</span>{/if}
                {#if stats.errors > 0}  <span class="sb-chip error">{stats.errors} Fehler</span>{/if}
                {#if stats.pending > 0} <span class="sb-chip pending">{stats.pending} ausstehend</span>{/if}
            </div>
        {/if}

        <!-- Embedder model download (shown while a model is being fetched). -->
        {#if downloadProgress}
            <div class="current-progress">
                <Loader2 size={13} class="spin" />
                <span class="current-filename">Lade {downloadProgress.repo}/{downloadProgress.file}</span>
                <span class="current-step">
                    {(downloadProgress.bytes_done / 1024 / 1024).toFixed(1)} /
                    {(downloadProgress.bytes_total / 1024 / 1024).toFixed(1)} MB
                </span>
                <div class="mini-bar">
                    <div class="mini-fill" style="width:{downloadProgress.pct}%"></div>
                </div>
            </div>
        {/if}

        <!-- Current file progress (while running) -->
        {#if running && currentFile}
            <div class="current-progress">
                <Loader2 size={13} class="spin" />
                <span class="current-filename">{currentFile}</span>
                <span class="current-step">{currentStep}</span>
                {#if currentMessage}
                    <span class="current-step">{currentMessage}</span>
                {/if}
                {#if currentChunkTotal > 0}
                    <span class="current-chunks">{currentChunk + 1}/{currentChunkTotal} Chunks</span>
                    <div class="mini-bar">
                        <div class="mini-fill" style="width:{Math.round((currentChunk+1)/currentChunkTotal*100)}%"></div>
                    </div>
                {/if}
            </div>
        {/if}

        <!-- Drop area or file list -->
        {#if entries.length === 0}
            <div class="drop-area" class:active={dropActive}>
                <UploadCloud size={40} style="color:#3b82f6; opacity:0.7;" />
                <p>Dateien hier ablegen</p>
                <p class="drop-hint">PDF, DOCX, EPUB, TXT, MD, HTML, Bilder (PNG/JPG/…), Audio (MP3/WAV/FLAC/M4A/…) und Video (MP4/MOV/MKV/…) — oder "Dateien" klicken</p>
            </div>
        {:else}
            <div class="file-list">
                {#each entries as entry (entry.id)}
                    {@const pct = progressPct(entry)}
                    <div class="file-row" class:active={entry.status === 'extracting' || entry.status === 'embedding'}>
                        <div class="file-icon ext-{entry.ext}">{entry.ext.toUpperCase()}</div>
                        <div class="file-info">
                            <span class="file-name" title={entry.path}>{entry.filename}</span>
                            <div class="file-sub">
                                {#if entry.status === 'done'}
                                    <span class="file-meta">{entry.chunks} Chunks · embed {entry.embedMs}ms · write {entry.writeMs}ms</span>
                                {:else if entry.status === 'error'}
                                    <span class="file-meta" style="color:#ef4444">{entry.error}</span>
                                {:else if (entry.status === 'embedding') && entry.chunksTotal}
                                    <span class="file-meta">{entry.chunksDone}/{entry.chunksTotal} Chunks</span>
                                {:else}
                                    <span class="file-meta">{statusLabel(entry.status, entry.ext)}</span>
                                {/if}
                                {#if entry.status === 'extracting' || entry.status === 'embedding'}
                                    <div class="file-progress-bar">
                                        <div class="file-progress-fill" style="width:{pct}%"></div>
                                    </div>
                                {/if}
                            </div>
                        </div>
                        <div class="file-status" style="color:{statusColor(entry.status)}">
                            {#if entry.status === 'extracting' || entry.status === 'embedding'}
                                <Loader2 size={13} class="spin" />
                            {:else if entry.status === 'done'}
                                <CheckCircle2 size={13} />
                            {:else if entry.status === 'error'}
                                <AlertCircle size={13} />
                            {/if}
                        </div>
                        {#if !running}
                            <button class="remove-btn" onclick={() => removeEntry(entry.id)}><X size={12} /></button>
                        {/if}
                    </div>
                {/each}
            </div>
        {/if}

        <!-- Run controls -->
        {#if entries.length > 0}
            <div class="run-bar">
                {#if !running && !l1Running && !l2RunningInline}
                    <button class="run-btn primary" onclick={startIngest}
                        disabled={stats.pending === 0}>
                        <Play size={15} />
                        {ingestLevel === 1 ? 'L1 Quick-Scan' : ingestLevel === 2 ? 'L2 Metadaten-Lesen' : 'L3 Volltext-Indexierung'} ({stats.pending})
                    </button>
                {:else if l1Running || l2RunningInline}
                    <span class="status-text" style="color:#3b82f6">
                        <Loader2 size={14} class="spin" /> {l1Running ? 'L1' : 'L2'} Indexierung läuft …
                    </span>
                {:else}
                    <button class="run-btn" onclick={pauseResume}>
                        {#if paused}<Play size={15} /> Fortsetzen{:else}<Pause size={15} /> Pausieren{/if}
                    </button>
                    <button class="run-btn danger" onclick={stopIngest}><X size={15} /> Stoppen</button>
                    {#if paused}
                        <span class="status-text" style="color:#f59e0b">Pausiert</span>
                    {/if}
                {/if}
            </div>
        {/if}
    {/if}

    <!-- ══════════════════ QUELLEN (managed folders + .caf import/export) ══════════════════ -->
    {#if activeTab === 'sources'}
        <div class="folders-toolbar">
            <button class="tb-btn" onclick={addFolder}><FolderOpen size={14} /> Ordner hinzufügen</button>
            <button class="tb-btn" onclick={openDriveDialog}
                    title="Registriertes Cloud-Laufwerk (WebDAV / Filen / Internxt) als L1-Manifest indexieren — keine Dateien werden geladen">
                <CloudDownload size={14} /> Cloud-Ordner
            </button>
            {#if folders.length > 0}
                <button class="tb-btn" onclick={scanAllFolders} disabled={!!scanningFolder}>
                    <RefreshCw size={14} class={scanningFolder ? 'spin' : ''} /> Alle neu scannen
                </button>
            {/if}
            <span style="flex:1;"></span>
            <button class="tb-btn" onclick={importCafFile} disabled={cafBusy}>
                {#if cafBusy}<Loader2 size={13} class="spin" />{:else}<Database size={13} />{/if}
                .caf importieren
            </button>
            <button class="tb-btn" onclick={exportCafFile} disabled={cafBusy}>
                {#if cafBusy}<Loader2 size={13} class="spin" />{:else}<UploadCloud size={13} />{/if}
                .caf exportieren
            </button>
            <label class="tb-toggle-small" title="Volltextsuche (.cidx FTS) einschließen — ermöglicht offline BM25-Suche im Archiv">
                <input type="checkbox" bind:checked={cidxIncludeFts} />
                FTS
            </label>
            <button class="tb-btn" onclick={exportCidxArchive} disabled={cidxBusy}
                    title="Suchindex als portable .cidx-Datei exportieren (offline nutzbar)">
                {#if cidxBusy}<Loader2 size={13} class="spin" />{:else}<UploadCloud size={13} />{/if}
                .cidx exportieren
            </button>
            <button class="tb-btn" onclick={importCbManifest} disabled={cafBusy}
                    title="cloud-backup Manifest-SQLite importieren (L1-Metadaten für alle gesicherten Dateien)">
                {#if cafBusy}<Loader2 size={13} class="spin" />{:else}<Database size={13} />{/if}
                cloud-backup
            </button>
        </div>
        {#if cidxLastResult}
            <div class="caf-result-bar">{cidxLastResult}</div>
        {/if}
        {#if cafLastResult}
            <div class="caf-result-bar">
                {cafLastResult}
            </div>
        {/if}

        {#if driveDialogOpen}
            <div class="drive-dialog">
                {#if availableDrives.length > 0}
                    <div class="drive-list">
                        {#each availableDrives as d (d.id)}
                            <div class="drive-list-row">
                                <span class="drive-list-label">{d.label}</span>
                                <span class="drive-list-kind">{d.kind}</span>
                                <span class="drive-list-path" title={d.path}>{d.path}</span>
                                <button class="icon-btn" type="button"
                                        title="Bearbeiten"
                                        disabled={driveCreateBusy || driveDeletingId === d.id}
                                        onclick={() => startDriveEdit(d)}>
                                    <RotateCcw size={13} />
                                </button>
                                <button class="icon-btn danger-icon" type="button"
                                        title="Entfernen"
                                        disabled={driveCreateBusy || driveDeletingId === d.id}
                                        onclick={() => deleteDrive(d)}>
                                    {#if driveDeletingId === d.id}
                                        <Loader2 size={13} class="spin" />
                                    {:else}
                                        <Trash2 size={13} />
                                    {/if}
                                </button>
                            </div>
                        {/each}
                    </div>
                {/if}

                <label class="drive-dialog-row">
                    <span class="drive-dialog-label">Laufwerk</span>
                    {#if availableDrives.length === 0}
                        <span class="drive-dialog-hint">
                            Keine Laufwerke registriert — lege unten eines an.
                        </span>
                    {:else}
                        <select bind:value={driveDialogId} class="drive-dialog-input">
                            {#each availableDrives as d (d.id)}
                                <option value={d.id}>{d.label} ({d.kind})</option>
                            {/each}
                        </select>
                        <button class="tb-btn" type="button"
                                style="padding: 2px 8px;"
                                onclick={() => {
                                    if (driveCreateOpen) {
                                        // Toggle off: also reset edit state.
                                        driveCreateOpen = false;
                                        resetDriveForm();
                                    } else {
                                        resetDriveForm();
                                        driveCreateOpen = true;
                                    }
                                }}
                                title="Neues Laufwerk anlegen">
                            +
                        </button>
                    {/if}
                </label>

                {#if driveCreateOpen || availableDrives.length === 0}
                    <div class="drive-create-form">
                        <div class="drive-dialog-hint">
                            {#if driveEditId}
                                Laufwerk bearbeiten — Änderungen schreiben sich in <code>drives.json</code>.
                                Die ID bleibt erhalten, sodass bestehende <code>crisp+drive://</code>-Indexzeilen
                                weiterhin auflösen.
                            {:else}
                                Neues Laufwerk anlegen — wird in <code>drives.json</code> gespeichert.
                            {/if}
                        </div>
                        <label class="drive-dialog-row">
                            <span class="drive-dialog-label">Label</span>
                            <input type="text" bind:value={driveCreateLabel}
                                   class="drive-dialog-input" disabled={driveCreateBusy}
                                   placeholder="Mein Nextcloud" />
                        </label>
                        <label class="drive-dialog-row">
                            <span class="drive-dialog-label">Typ</span>
                            <select bind:value={driveCreateKind} class="drive-dialog-input"
                                    disabled={driveCreateBusy}>
                                <option value="webdav">WebDAV (Nextcloud / ownCloud / mailbox.org / Synology)</option>
                                <option value="filen">Filen (Python cli.py)</option>
                                <option value="internxt">Internxt (Python cli.py)</option>
                                <option value="local">Lokal / OS-Mount (SMB / NFS / SFTP via FUSE)</option>
                                <option value="sftp">SFTP (über OS-Mount)</option>
                            </select>
                        </label>
                        <label class="drive-dialog-row">
                            <span class="drive-dialog-label">
                                {driveCreateKind === 'webdav' ? 'URL' :
                                 driveCreateKind === 'filen' || driveCreateKind === 'internxt' ? 'cli.py' :
                                 'Pfad'}
                            </span>
                            <input type="text" bind:value={driveCreatePath}
                                   class="drive-dialog-input" disabled={driveCreateBusy}
                                   placeholder={driveCreateKind === 'webdav'
                                       ? 'https://host/remote.php/dav/files/<user>/'
                                       : driveCreateKind === 'filen' || driveCreateKind === 'internxt'
                                           ? '/Users/.../code/filen-python/cli.py'
                                           : '/Volumes/...'} />
                        </label>
                        {#if driveCreateKind === 'webdav'}
                            <label class="drive-dialog-row">
                                <span class="drive-dialog-label">Benutzer</span>
                                <input type="text" bind:value={driveCreateUser}
                                       class="drive-dialog-input" disabled={driveCreateBusy}
                                       autocomplete="off" />
                            </label>
                            <label class="drive-dialog-row">
                                <span class="drive-dialog-label">Passwort</span>
                                <input type="password" bind:value={driveCreatePass}
                                       class="drive-dialog-input" disabled={driveCreateBusy}
                                       autocomplete="new-password" />
                            </label>
                            <label class="drive-dialog-row">
                                <span class="drive-dialog-label">Selbstsigniertes Zertifikat akzeptieren</span>
                                <input type="checkbox" bind:checked={driveCreateInsecure}
                                       disabled={driveCreateBusy} />
                                <span class="drive-dialog-hint">
                                    Für lokale WebDAV-Server (z.B. internxt-cli) mit selbstsigniertem TLS.
                                </span>
                            </label>
                            <div class="drive-dialog-hint">
                                Auth wird im Klartext in <code>drives.json</code> gespeichert.
                                Für höhere Sicherheit lokal mounten und stattdessen "Lokal" wählen.
                            </div>
                        {/if}
                        <div class="drive-dialog-row">
                            <button class="tb-btn" type="button"
                                    onclick={submitDriveCreate}
                                    disabled={driveCreateBusy || !driveCreateLabel.trim() || !driveCreatePath.trim()}>
                                {#if driveCreateBusy}<Loader2 size={13} class="spin" />{:else}<HardDrive size={13} />{/if}
                                {driveEditId ? 'Speichern' : 'Anlegen'}
                            </button>
                            {#if availableDrives.length > 0}
                                <button class="tb-btn" type="button"
                                        onclick={() => { resetDriveForm(); driveCreateOpen = false; }}
                                        disabled={driveCreateBusy}>
                                    Abbrechen
                                </button>
                            {/if}
                            {#if driveCreateError}
                                <span class="drive-dialog-hint" style="color:#ef4444">{driveCreateError}</span>
                            {/if}
                        </div>
                    </div>
                {/if}
                <label class="drive-dialog-row">
                    <span class="drive-dialog-label">Pfad</span>
                    <input type="text" bind:value={driveDialogPath} placeholder="/"
                           class="drive-dialog-input" disabled={driveScanning} />
                </label>
                <label class="drive-dialog-row">
                    <span class="drive-dialog-label">Endungen</span>
                    <input type="text" bind:value={driveDialogExts}
                           placeholder="pdf, epub, docx (leer = alle)"
                           class="drive-dialog-input" disabled={driveScanning} />
                </label>
                <label class="drive-dialog-row">
                    <span class="drive-dialog-label">Tiefe</span>
                    <input type="number" min="0" placeholder="unbegrenzt"
                           class="drive-dialog-input" style="max-width:160px"
                           value={driveDialogDepth ?? ''}
                           oninput={(e) => {
                               const v = (e.target as HTMLInputElement).value;
                               driveDialogDepth = v === '' ? null : Number(v);
                           }}
                           disabled={driveScanning} />
                </label>
                <div class="drive-dialog-row">
                    <button class="tb-btn" onclick={runDriveIngest}
                            disabled={driveScanning || availableDrives.length === 0 || !driveDialogId}>
                        {#if driveScanning}<Loader2 size={13} class="spin" />{:else}<CloudDownload size={13} />{/if}
                        Indexieren (L1)
                    </button>
                    <button class="tb-btn" onclick={() => { driveDialogOpen = false; }}
                            disabled={driveScanning}>
                        Schließen
                    </button>
                    {#if driveScanResult}
                        <span class="drive-dialog-hint">{driveScanResult}</span>
                    {/if}
                </div>
            </div>
        {/if}

        {#if folders.length === 0}
            <div class="empty-state">
                <FolderOpen size={32} style="color:#3f3f46" />
                <p>Noch keine Ordner</p>
                <p class="hint-sub">Füge einen Ordner hinzu — alle darin enthaltenen Dokumente werden zur Ingest-Warteschlange hinzugefügt</p>
            </div>
        {:else}
            <div class="folder-list">
                {#each folders as folder (folder.path)}
                    <div class="folder-row">
                        <FolderOpen size={16} style="color:#f59e0b; flex-shrink:0;" />
                        <div class="folder-info">
                            <span class="folder-path">{folder.path}</span>
                            <span class="folder-meta">
                                Hinzugefügt {fmtDate(folder.addedAt)} ·
                                Zuletzt gescannt: {fmtDate(folder.lastScanned)}
                                {#if folder.fileCount > 0} · {folder.fileCount} Dateien{/if}
                            </span>
                        </div>
                        <div class="folder-actions">
                            <button class="icon-btn" onclick={() => scanFolderToQueue(folder.path)}
                                disabled={scanningFolder === folder.path}
                                title="Ordner neu scannen">
                                {#if scanningFolder === folder.path}
                                    <Loader2 size={14} class="spin" />
                                {:else}
                                    <RefreshCw size={14} />
                                {/if}
                            </button>
                            <button class="icon-btn danger" onclick={() => removeFolder(folder.path)} title="Entfernen">
                                <Trash2 size={14} />
                            </button>
                        </div>
                    </div>
                {/each}
            </div>

            {#if entries.filter(e => e.status === 'pending').length > 0}
                <div class="run-bar" style="margin-top: auto; padding-top: 12px; border-top: 1px solid #27272a;">
                    <button class="run-btn primary" onclick={() => { activeTab = 'add'; }}>
                        <UploadCloud size={15} /> Zu Hinzufügen wechseln ({entries.filter(e => e.status === 'pending').length} Dateien)
                    </button>
                </div>
            {/if}
        {/if}
    {/if}

    <!-- ══════════════════ SUCHE (semantic + full-text) ══════════════════ -->
    {#if activeTab === 'search'}
        <IndexSearch />
    {/if}

    <!-- ══════════════════ ÜBERSICHT (catalog contents) ══════════════════ -->
    {#if activeTab === 'overview'}
        <div class="contents-toolbar">
            <div class="query-input-wrap" style="flex:1">
                <Search size={14} style="color:#71717a;" />
                <input type="text" bind:value={contentsQuery}
                    placeholder="Name / Titel / Autor filtern …" class="query-input" />
            </div>
            <button class="tb-btn" onclick={() => loadContents(false)} disabled={contentsLoading}>
                {#if contentsLoading}<Loader2 size={13} class="spin" />{:else}<RefreshCw size={13} />{/if}
                Aktualisieren
            </button>
        </div>

        <!-- Filters -->
        <div class="filter-bar">
            <!-- Folder breadcrumb tree (P9 step 4) -->
            <span class="filter-label">Ordner:</span>
            <div class="folder-breadcrumb-wrap">
                <div class="folder-breadcrumb">
                    <button
                        class="crumb root"
                        onclick={() => { clearFolder(); loadFolderChildren(''); folderTreeOpen = true; }}
                        title="Alle Ordner anzeigen"
                    >/</button>
                    {#each folderSegments(contentsFolder) as seg}
                        <span class="crumb-sep">›</span>
                        <button class="crumb" onclick={() => navigateFolder(seg.fullPath)}>{seg.label}</button>
                    {/each}
                    <button
                        class="crumb-chevron"
                        class:open={folderTreeOpen}
                        onclick={() => {
                            if (!folderTreeOpen) void loadFolderChildren(contentsFolder);
                            folderTreeOpen = !folderTreeOpen;
                        }}
                        title="Unterordner anzeigen"
                    >▾</button>
                    {#if contentsFolder}
                        <button class="chip ghost" style="margin-left:4px" onclick={clearFolder}>×</button>
                    {/if}
                    <button class="chip" onclick={pickContentsFolder} title="Ordner auswählen" style="margin-left:2px">
                        <FolderOpen size={11} />
                    </button>
                </div>
                {#if folderTreeOpen}
                    <div class="folder-dropdown">
                        {#if folderTreeLoading}
                            <div class="folder-item muted"><Loader2 size={11} class="spin" /> …</div>
                        {:else if folderChildren.length === 0}
                            <div class="folder-item muted">Keine Unterordner</div>
                        {:else}
                            {#each folderChildren as child}
                                <button class="folder-item" onclick={() => navigateFolder(child.path)}>
                                    <Folder size={11} />
                                    <span class="folder-item-name">{child.name}</span>
                                    <span class="folder-item-count">{child.docCount.toLocaleString()}</span>
                                </button>
                            {/each}
                        {/if}
                    </div>
                {/if}
            </div>

            <span class="filter-label" style="margin-left:8px;">Tiefe:</span>
            <button class="chip" class:active={contentsLevel === 'all'} onclick={() => contentsLevel = 'all'}>Alle</button>
            <button class="chip" class:active={contentsLevel === 1} onclick={() => contentsLevel = 1}>L1</button>
            <button class="chip" class:active={contentsLevel === 3} onclick={() => contentsLevel = 3}>L3</button>

            {#if contentsExtChoices.length > 0}
                <span class="filter-label" style="margin-left:8px;">Ext:</span>
                {#each contentsExtChoices as ext}
                    <button class="chip" class:active={contentsExt.has(ext)} onclick={() => toggleContentsExt(ext)}>
                        {ext}
                    </button>
                {/each}
                {#if contentsExt.size > 0}
                    <button class="chip ghost" onclick={() => contentsExt = new Set()}>Reset</button>
                {/if}
            {/if}

            <span class="filter-label" style="margin-left:8px;">Vollständigkeit:</span>
            <select bind:value={contentsCompleteness} class="filter-select">
                <option value="any">Egal</option>
                <option value="has_title">Titel</option>
                <option value="has_author">Autor</option>
                <option value="has_year">Jahr</option>
                <option value="has_all">Alle</option>
            </select>

            <!-- Tag-cloud toggle — hidden by default, opt-in. -->
            <button
                class="chip"
                class:active={showTagCloud}
                onclick={toggleTagCloud}
                title="Tag-Wolke ein-/ausblenden"
                style="margin-left:8px;"
            >
                <Tag size={11} /> Tags{#if contentsTags.size > 0} ({contentsTags.size}){/if}
            </button>
        </div>

        {#if showTagCloud}
            <div class="tag-cloud-wrap">
                <TagCloud
                    facets={tagFacets}
                    selected={contentsTags}
                    loading={tagFacetsLoading}
                    ontoggle={toggleTag}
                    onclear={() => { contentsTags = new Set(); loadContents(false); }}
                />
            </div>
        {/if}

        {#if indexStats}
            <div class="index-stats-bar">
                <span class="stat-pill"><Database size={11} /> {indexStats.doc_count} Dokumente</span>
                <span class="stat-pill">{indexStats.chunk_count} Chunks</span>
                <span class="stat-pill">{indexStats.total_rows} Zeilen</span>
            </div>
        {/if}

        {#if contentsLoading}
            <div class="empty-state"><Loader2 size={22} class="spin" /> Lade …</div>
        {:else if contents.length === 0}
            <div class="empty-state">
                <Database size={32} style="color:#3f3f46" />
                <p>{indexStats?.doc_count === 0 ? 'Index ist leer' : 'Keine Treffer'}</p>
                <p class="hint-sub">{indexStats?.doc_count === 0 ? 'Indexiere Dokumente über den "Hinzufügen"-Tab' : 'Filter anpassen oder leeren'}</p>
            </div>
        {:else}
            <!-- Selection toolbar (shown when items are selected) -->
            {#if selectedDocIds.size > 0}
                <div class="selection-bar">
                    <span class="sel-count">{selectedDocIds.size} ausgewählt</span>
                    <button class="tb-btn" onclick={promoteSelectedToL2} disabled={promotingL2 || promotingL3}>
                        {#if promotingL2}<Loader2 size={13} class="spin" />{:else}<Database size={13} />{/if}
                        Auf L2 anheben (Metadaten)
                    </button>
                    <button class="tb-btn" onclick={promoteSelectedToL3} disabled={promotingL2 || promotingL3}>
                        {#if promotingL3}<Loader2 size={13} class="spin" />{:else}<UploadCloud size={13} />{/if}
                        Auf L3 anheben (Volltext + Embedding)
                    </button>
                    <button class="tb-btn danger" onclick={deleteSelected} disabled={deletingIds.size > 0}>
                        <Trash2 size={13} /> {deletingIds.size > 0 ? 'Löschen …' : 'Aus Index löschen'}
                    </button>
                    <button class="tb-btn" onclick={() => selectedDocIds = new Set()}>Abwählen</button>
                </div>
                {#if l3Progress}
                    <div class="current-progress">
                        <Loader2 size={13} class="spin" />
                        <span class="current-filename">{l3Progress.current}</span>
                        <span class="current-step">{l3Progress.done}/{l3Progress.total}</span>
                        <div class="mini-bar">
                            <div class="mini-fill" style="width:{Math.round((l3Progress.done) / Math.max(1, l3Progress.total) * 100)}%"></div>
                        </div>
                    </div>
                {/if}
            {:else}
                <div class="result-count">
                    <label class="select-all-wrap">
                        <input type="checkbox" onchange={toggleSelectAll}
                            checked={selectedDocIds.size === contents.length && contents.length > 0} />
                    </label>
                    {contents.length} Dokument{contents.length !== 1 ? 'e' : ''}{contentsQuery ? ` (gefiltert)` : ''}
                    <div class="col-picker-wrap">
                        <button class="icon-btn col-picker-btn" onclick={() => colPickerOpen = !colPickerOpen}
                            title="Spalten anpassen" class:active={colPickerOpen}>
                            <Columns2 size={13} />
                        </button>
                        {#if colPickerOpen}
                            <div class="col-picker-dropdown" role="menu">
                                {#each COLUMN_DEFS as col}
                                    <label class="col-picker-item">
                                        <input type="checkbox" bind:checked={colVisibility[col.id]}
                                            onchange={saveColVisibility} />
                                        {col.label}
                                    </label>
                                {/each}
                            </div>
                        {/if}
                    </div>
                </div>
            {/if}

            <div class="overview-split" class:with-preview={previewDoc !== null}>
            <div class="catalog-col">
            <div class="catalog-table" role="grid" style="--cat-cols: {gridCols}">
                <div class="catalog-thead" role="row">
                    <div class="cell col-check">
                        <input type="checkbox" onchange={toggleSelectAll}
                            checked={selectedDocIds.size === contents.length && contents.length > 0} />
                    </div>
                    <div class="cell col-ext">Ext</div>
                    {#if colVisibility.name}
                        <button class="cell col-name col-sortable col-header-btn" type="button" onclick={() => setSort('filename')}>
                            Name
                            {#if sortColumn === 'filename'}<span class="sort-arrow">{sortDir === 'asc' ? '↑' : '↓'}</span>{/if}
                        </button>
                    {/if}
                    {#if colVisibility.author}
                        <button class="cell col-author col-sortable col-header-btn" type="button" onclick={() => setSort('author')}>
                            Autor
                            {#if sortColumn === 'author'}<span class="sort-arrow">{sortDir === 'asc' ? '↑' : '↓'}</span>{/if}
                        </button>
                    {/if}
                    {#if colVisibility.year}
                        <button class="cell col-year col-sortable col-header-btn" type="button" onclick={() => setSort('year')}>
                            Jahr
                            {#if sortColumn === 'year'}<span class="sort-arrow">{sortDir === 'asc' ? '↑' : '↓'}</span>{/if}
                        </button>
                    {/if}
                    {#if colVisibility.size}
                        <div class="cell col-size">Größe</div>
                    {/if}
                    {#if colVisibility.mtime}
                        <div class="cell col-mtime">Geändert</div>
                    {/if}
                    {#if colVisibility.folder}
                        <div class="cell col-folder">Ordner</div>
                    {/if}
                    {#if colVisibility.language}
                        <button class="cell col-language col-sortable col-header-btn" type="button" onclick={() => setSort('language')}>
                            Sprache
                            {#if sortColumn === 'language'}<span class="sort-arrow">{sortDir === 'asc' ? '↑' : '↓'}</span>{/if}
                        </button>
                    {/if}
                    {#if colVisibility.volume}
                        <div class="cell col-volume">Volume</div>
                    {/if}
                    {#if colVisibility.level}
                        <div class="cell col-level" title="Analyse-Tiefe">L</div>
                    {/if}
                    <div class="cell col-actions"></div>
                </div>
                <div class="catalog-tbody">
                    {#each contents as doc, idx (doc.doc_id)}
                        {@const isSelected = selectedDocIds.has(doc.doc_id)}
                        {@const isDeleting = deletingIds.has(doc.doc_id)}
                        {@const lvl = docLevel(doc)}
                        {@const failure = extractionFailure(doc)}
                        {@const fsSize = metaField(doc, 'fs_size')}
                        {@const fsMtime = metaField(doc, 'fs_mtime')}
                        {@const parentDir = metaField(doc, 'parent_dir') ?? ''}
                        <div class="catalog-row" role="row" tabindex="0"
                             class:selected={isSelected} class:deleting={isDeleting}
                             onclick={(e) => handleDocRowClick(e, idx, doc.doc_id)}
                             onkeydown={(e) => {
                                if (e.key === 'Enter' || e.key === ' ') {
                                    e.preventDefault();
                                    handleDocRowClick(e, idx, doc.doc_id);
                                }
                             }}>
                            <div class="cell col-check"
                                 role="presentation"
                                 onclick={(e) => e.stopPropagation()}
                                 onkeydown={(e) => e.stopPropagation()}>
                                <input type="checkbox" checked={isSelected}
                                    onchange={() => toggleSelect(doc.doc_id)} />
                            </div>
                            <div class="cell col-ext">
                                {#if doc.ext}
                                    <span class="ext-badge ext-{doc.ext.toLowerCase()}">{doc.ext.toUpperCase()}</span>
                                {:else}
                                    <span class="ext-badge">–</span>
                                {/if}
                            </div>
                            {#if colVisibility.name}
                                <div class="cell col-name" title={doc.location_uri ?? doc.filename ?? ''}>
                                    {doc.title || doc.filename || doc.doc_id?.slice(0, 16)}
                                </div>
                            {/if}
                            {#if colVisibility.author}
                                <div class="cell col-author">{doc.author ?? ''}</div>
                            {/if}
                            {#if colVisibility.year}
                                <div class="cell col-year">{doc.year ?? ''}</div>
                            {/if}
                            {#if colVisibility.size}
                                <div class="cell col-size">{fmtSize(fsSize)}</div>
                            {/if}
                            {#if colVisibility.mtime}
                                <div class="cell col-mtime">{fmtMtime(fsMtime)}</div>
                            {/if}
                            {#if colVisibility.folder}
                                <div class="cell col-folder" title={parentDir}>{parentDir}</div>
                            {/if}
                            {#if colVisibility.language}
                                <div class="cell col-language">{doc.language ?? ''}</div>
                            {/if}
                            {#if colVisibility.volume}
                                <div class="cell col-volume" title={doc.volume_id ?? ''}>
                                    {doc.volume_id ? doc.volume_id.slice(0, 8) + '…' : ''}
                                </div>
                            {/if}
                            {#if colVisibility.level}
                                <div class="cell col-level" style="position:relative;">
                                    <span class="level-badge" class:l1={lvl === 1} class:l2={lvl === 2} class:l3={lvl === 3}>L{lvl}</span>
                                    {#if failure}
                                        {#if failure.reason === 'drm'}
                                            <button class="fail-badge fail-drm"
                                                    onclick={(e) => { e.stopPropagation(); drmPopoverDocId = drmPopoverDocId === doc.doc_id ? null : doc.doc_id; }}
                                                    title="Klick für Details zu DRM-Schutz">DRM</button>
                                            {#if drmPopoverDocId === doc.doc_id}
                                                <div class="drm-popover" role="tooltip">
                                                    <p><strong>DRM-geschützt</strong></p>
                                                    <p>Diese Datei ist verschlüsselt (ADEPT/FairPlay/AES). CrispSorter kann nur die unverschlüsselten Metadaten lesen.</p>
                                                    <p style="margin-top:6px;">Wende dich an den Anbieter oder Verlag, um eine nicht-verschlüsselte Version zu erhalten.</p>
                                                    <button class="drm-close" onclick={(e) => { e.stopPropagation(); drmPopoverDocId = null; }}>✕</button>
                                                </div>
                                            {/if}
                                        {:else}
                                            <span class="fail-badge fail-{failure.reason}"
                                                  title={FAIL_HINTS[failure.reason] ?? failure.msg}
                                            >{FAIL_LABELS[failure.reason] ?? failure.reason}</span>
                                        {/if}
                                    {/if}
                                </div>
                            {/if}
                            <div class="cell col-actions"
                                 role="presentation"
                                 onclick={(e) => e.stopPropagation()}
                                 onkeydown={(e) => e.stopPropagation()}>
                                <button class="icon-btn" onclick={() => openDocPreview(doc)}
                                    title="Vorschau"
                                    class:preview-active={previewDoc && previewDoc.doc_id === doc.doc_id}>
                                    <Eye size={13} />
                                </button>
                                <button class="icon-btn" onclick={() => openIndexedFile(doc.location_uri)} title="Öffnen">
                                    <ExternalLink size={13} />
                                </button>
                                {#if doc.location_uri?.startsWith('crisp+cb-archive://')}
                                    {@const isPromoting = promotingIds.has(doc.doc_id)}
                                    <button class="icon-btn" onclick={() => promoteCbArchive(doc)}
                                        disabled={isPromoting}
                                        title="Datei von cloud-backup abrufen und auf L3 hochstufen (retrieve.py)">
                                        {#if isPromoting}<Loader2 size={13} class="spin" />{:else}<CloudDownload size={13} />{/if}
                                    </button>
                                {:else if doc.location_uri?.startsWith('crisp+drive://')}
                                    {@const isPromoting = promotingIds.has(doc.doc_id)}
                                    <button class="icon-btn" onclick={() => promoteDriveArchive(doc)}
                                        disabled={isPromoting}
                                        title="Datei vom registrierten Cloud-Laufwerk abrufen und auf L3 hochstufen">
                                        {#if isPromoting}<Loader2 size={13} class="spin" />{:else}<CloudDownload size={13} />{/if}
                                    </button>
                                {/if}
                                {#if failure && (failure.reason === 'timeout' || failure.reason === 'other')}
                                    {@const isRetrying = retryingIds.has(doc.doc_id)}
                                    <button class="icon-btn" onclick={() => retryExtraction(doc.doc_id)}
                                        disabled={isRetrying}
                                        title="Extraktion erneut versuchen (Timeout/Fehler rückgängig machen)">
                                        {#if isRetrying}<Loader2 size={13} class="spin" />{:else}<RotateCcw size={13} />{/if}
                                    </button>
                                {/if}
                                <button class="icon-btn danger-icon" onclick={() => deleteFromIndex(doc.doc_id)}
                                    disabled={isDeleting} title="Aus Index löschen">
                                    {#if isDeleting}<Loader2 size={13} class="spin" />{:else}<Trash2 size={13} />{/if}
                                </button>
                            </div>
                        </div>
                    {/each}
                </div>
            </div>

            {#if nextCursor}
                <div class="load-more-bar">
                    <button class="tb-btn" onclick={() => loadContents(true)} disabled={contentsLoading}>
                        {#if contentsLoading}<Loader2 size={13} class="spin" />{:else}<ChevronDown size={13} />{/if}
                        Weitere {Math.min(200, totalEstimate - contents.length).toLocaleString()} laden
                        <span class="muted-small">({contents.length.toLocaleString()} / {totalEstimate.toLocaleString()})</span>
                    </button>
                </div>
            {/if}
            </div><!-- /catalog-col -->

            {#if previewDoc !== null}
                <aside class="preview-pane">
                    <header class="preview-header">
                        <span class="preview-title" title={previewDoc.location_uri}>
                            {previewDoc.title || previewDoc.filename || (previewDoc.doc_id?.slice(0, 24) + '…')}
                        </span>
                        <button class="preview-close" onclick={closeDocPreview} title="Vorschau schließen">
                            <X size={14} />
                        </button>
                    </header>
                    {#if cbLookupLoading}
                        <div class="cb-tier-bar"><Loader2 size={12} class="spin" /> Standort prüfen…</div>
                    {:else if cbLookupResult?.found}
                        <div class="cb-tier-bar">
                            <span class="cb-tier" class:tier-ok={cbLookupResult.local_available}
                                  title="Originaldatei auf dem lokalen Dateisystem">
                                Lokal: {cbLookupResult.local_available ? '✓' : '✗'}
                            </span>
                            <span class="cb-tier" class:tier-ok={cbLookupResult.archived_in != null}
                                  title="In cloud-backup-Archiv auf VPS gesichert (vor Cloud-Upload eventuell schon gelöscht)">
                                VPS: {cbLookupResult.archived_in != null ? (cbLookupResult.vps_local_deleted ? '✗ (gelöscht)' : `✓ (#${cbLookupResult.archived_in})`) : '✗'}
                            </span>
                            <span class="cb-tier" class:tier-ok={cbLookupResult.cloud_uploaded}
                                  title="Internxt-Cloud-Replik (verifiziert)">
                                Cloud: {cbLookupResult.cloud_uploaded ? '✓' : '✗'}
                            </span>
                            {#if cbLookupResult.archive_filename}
                                <span class="cb-archive-name" title={cbLookupResult.archive_filename}>{cbLookupResult.archive_filename.slice(-32)}</span>
                            {/if}
                        </div>
                    {/if}
                    <div class="preview-body">
                        {#if previewLoading}
                            <div class="preview-msg"><Loader2 size={20} class="spin" /> Lade …</div>
                        {:else if previewError}
                            <div class="preview-msg preview-error">{previewError}</div>
                        {:else if previewKind === 'pdf'}
                            <object data={previewSrc} type="application/pdf"
                                width="100%" height="100%"
                                aria-label="PDF-Vorschau: {previewDoc.title || previewDoc.filename || 'Dokument'}">
                                <p class="preview-msg">
                                    PDF nicht unterstützt.
                                    <button class="open-ext-btn" onclick={() => openIndexedFile(previewDoc.location_uri)}>
                                        <ExternalLink size={12} /> In App öffnen
                                    </button>
                                </p>
                            </object>
                        {:else if previewKind === 'image'}
                            <img src={previewSrc} alt={previewDoc.filename ?? ''} class="preview-image" />
                        {:else if previewKind === 'text'}
                            <pre class="preview-text">{previewText}</pre>
                        {:else}
                            <div class="preview-msg">
                                Vorschau für diesen Dateityp nicht verfügbar.
                                <br />
                                <button class="open-ext-btn" onclick={() => openIndexedFile(previewDoc.location_uri)}>
                                    <ExternalLink size={12} /> In App öffnen
                                </button>
                            </div>
                        {/if}
                    </div>
                </aside>
            {/if}
            </div><!-- /overview-split -->
        {/if}
    {/if}
</div>

<style>
    .ingest-root {
        display: flex; flex-direction: column; height: 100%; padding: 0;
        background: #09090b; color: #fafafa; box-sizing: border-box; overflow: hidden;
    }

    .tab-bar {
        display: flex; gap: 2px; padding: 12px 16px 0; border-bottom: 1px solid #27272a;
    }
    .tab {
        display: flex; align-items: center; gap: 6px; padding: 8px 14px;
        background: transparent; border: none; border-bottom: 2px solid transparent;
        color: #71717a; cursor: pointer; font-size: 0.8125rem; font-weight: 500;
        margin-bottom: -1px; transition: color 0.15s;
    }
    .tab:hover { color: white; }
    .tab.active { color: white; border-bottom-color: #3b82f6; }

    /* Content area padded */
    .toolbar, .folders-toolbar, .contents-toolbar,
    .stats-bar, .current-progress, .drop-area,
    .file-list, .run-bar, .folder-list, .empty-state,
    .result-count, .folders-toolbar, .index-stats-bar {
        padding-left: 16px; padding-right: 16px;
    }

    .index-stats-bar {
        display: flex; gap: 8px; flex-wrap: wrap; padding-top: 10px; padding-bottom: 2px;
    }
    .stat-pill {
        display: inline-flex; align-items: center; gap: 4px;
        background: #18181b; border: 1px solid #27272a; border-radius: 12px;
        padding: 3px 10px; font-size: 0.73rem; color: #a1a1aa;
    }
    .toolbar { padding-top: 12px; }
    .folders-toolbar, .contents-toolbar { padding-top: 12px; display: flex; gap: 8px; align-items: center; }

    .toolbar-actions { display: flex; gap: 8px; flex-wrap: wrap; }

    .tb-btn {
        display: flex; align-items: center; gap: 6px; padding: 6px 12px;
        border-radius: 6px; border: 1px solid #3f3f46; background: #18181b;
        color: #d4d4d8; cursor: pointer; font-size: 0.8125rem; font-weight: 500;
    }
    .tb-btn:hover { background: #27272a; color: white; }
    .tb-btn.ghost { background: transparent; }
    .tb-btn.danger:hover { border-color: #ef4444; color: #ef4444; }
    .tb-btn:disabled { opacity: 0.4; cursor: not-allowed; }
    /* Toggle-style toolbar button: P13/A3 added it for "Duplicates only".
       Future toggles in this region can re-use the same hook. */
    .tb-btn.active { background: #1e1b4b; color: #a5b4fc; border-color: #6366f1; }
    .tb-btn.active:hover { background: #312e81; color: white; }

    .stats-bar { display: flex; gap: 8px; margin-top: 8px; flex-wrap: wrap; }
    .sb-chip { padding: 2px 10px; border-radius: 99px; font-size: 0.75rem; font-weight: 600; }
    .sb-chip.total   { background: #27272a; color: #a1a1aa; }
    .sb-chip.done    { background: #14532d55; color: #4ade80; }
    .sb-chip.error   { background: #7f1d1d55; color: #f87171; }
    .sb-chip.pending { background: #1e3a5f55; color: #60a5fa; }

    /* Current progress bar */
    .current-progress {
        display: flex; align-items: center; gap: 8px; margin-top: 8px;
        background: #18181b; border: 1px solid #3b82f6; border-radius: 6px;
        padding: 8px 12px; font-size: 0.8rem; flex-wrap: wrap;
    }
    .current-filename { color: #e4e4e7; font-weight: 500; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .current-step { color: #a1a1aa; font-size: 0.75rem; }
    .current-chunks { color: #71717a; font-size: 0.75rem; }
    .mini-bar { flex: 1; min-width: 60px; height: 4px; background: #27272a; border-radius: 2px; overflow: hidden; }
    .mini-fill { height: 100%; background: #3b82f6; border-radius: 2px; transition: width 0.3s; }

    .drop-area {
        flex: 1; margin: 12px 16px; border: 2px dashed #3f3f46; border-radius: 12px;
        display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px;
        color: #71717a; font-size: 0.95rem; min-height: 180px;
    }
    .drop-area.active { border-color: #3b82f6; background: #3b82f610; color: #93c5fd; }
    .drop-hint { font-size: 0.8rem; color: #52525b; }

    .file-list {
        flex: 1; overflow-y: auto; display: flex; flex-direction: column; gap: 4px;
        margin-top: 8px; margin-bottom: 0; padding-bottom: 4px;
    }

    .file-row {
        display: flex; align-items: center; gap: 10px;
        background: #18181b; border: 1px solid #27272a; border-radius: 6px; padding: 8px 10px;
    }
    .file-row.active { border-color: #3b82f6; }

    .file-icon {
        font-size: 0.6rem; font-weight: 800; padding: 3px 5px; border-radius: 4px;
        background: #27272a; color: #a1a1aa; flex-shrink: 0; min-width: 32px; text-align: center;
    }
    .level-inline-label { font-size: 0.72rem; color: #71717a; font-weight: 600; margin-left: 6px; align-self: center; }
    .level-hint-inline { font-size: 0.72rem; color: #52525b; padding: 6px 16px 0; }

    .filter-bar { display: flex; align-items: center; gap: 6px; padding: 8px 16px 0; flex-wrap: wrap; }
    .tag-cloud-wrap { padding: 8px 16px 0; }
    .filter-label { font-size: 0.72rem; color: #71717a; font-weight: 600; }
    .chip {
        background: #18181b; border: 1px solid #27272a; color: #a1a1aa;
        padding: 2px 9px; border-radius: 99px; font-size: 0.72rem; cursor: pointer;
        text-transform: lowercase;
    }
    .chip:hover { color: white; border-color: #3f3f46; }
    .chip.active { background: #1e3a8a33; border-color: #3b82f6; color: #93c5fd; }
    .chip.ghost { background: transparent; color: #52525b; }
    .filter-select {
        background: #18181b; border: 1px solid #27272a; color: #d4d4d8;
        padding: 2px 6px; border-radius: 4px; font-size: 0.75rem;
    }
    .folder-filter {
        background: #18181b; border: 1px solid #27272a; color: #d4d4d8;
        padding: 3px 8px; border-radius: 4px; font-size: 0.72rem;
        min-width: 220px; max-width: 320px;
    }
    .folder-filter:focus { border-color: #3b82f6; outline: none; }

    /* P9 step 4 — folder breadcrumb tree */
    .folder-breadcrumb-wrap { position: relative; }
    .folder-breadcrumb {
        display: flex; align-items: center; gap: 2px;
        background: #18181b; border: 1px solid #27272a; border-radius: 4px;
        padding: 2px 6px; min-width: 160px; max-width: 420px;
        font-size: 0.72rem;
    }
    .crumb {
        background: none; border: none; color: #a1a1aa; cursor: pointer;
        padding: 1px 3px; border-radius: 3px; font-size: 0.72rem;
    }
    .crumb:hover { background: #27272a; color: #d4d4d8; }
    .crumb.root { color: #71717a; font-weight: 600; }
    .crumb-sep { color: #52525b; font-size: 0.65rem; user-select: none; }
    .crumb-chevron {
        background: none; border: none; color: #71717a; cursor: pointer;
        padding: 0 2px; font-size: 0.7rem; margin-left: 2px;
        transition: transform 0.15s;
    }
    .crumb-chevron.open { transform: rotate(180deg); }
    .crumb-chevron:hover { color: #a1a1aa; }
    .folder-dropdown {
        position: absolute; top: calc(100% + 3px); left: 0; z-index: 200;
        background: #18181b; border: 1px solid #27272a; border-radius: 6px;
        min-width: 220px; max-width: 380px; max-height: 280px; overflow-y: auto;
        box-shadow: 0 4px 12px rgba(0,0,0,0.5);
        padding: 4px;
    }
    .folder-item {
        display: flex; align-items: center; gap: 5px;
        width: 100%; background: none; border: none;
        color: #a1a1aa; cursor: pointer; padding: 4px 6px; border-radius: 4px;
        font-size: 0.75rem; text-align: left;
    }
    .folder-item:hover { background: #27272a; color: #d4d4d8; }
    .folder-item.muted { cursor: default; color: #52525b; }
    .folder-item.muted:hover { background: none; }
    .folder-item-name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .folder-item-count { color: #52525b; font-size: 0.7rem; flex-shrink: 0; }

    .ext-pdf  { background: #7f1d1d33; color: #fca5a5; }
    .ext-docx { background: #1e3a5f33; color: #93c5fd; }
    .ext-doc  { background: #1e3a5f33; color: #93c5fd; }
    .ext-md   { background: #14532d33; color: #86efac; }
    .ext-txt  { background: #44403c33; color: #d6d3d1; }
    .ext-epub { background: #4c1d9533; color: #c4b5fd; }
    .ext-html, .ext-htm { background: #c2410c33; color: #fdba74; }
    .ext-webp, .ext-png, .ext-jpg, .ext-jpeg, .ext-bmp, .ext-tif, .ext-tiff { background: #be185d33; color: #f9a8d4; }

    .file-info { flex: 1; min-width: 0; }
    .file-name { display: block; font-size: 0.85rem; color: #e4e4e7; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .file-sub { display: flex; align-items: center; gap: 8px; margin-top: 3px; }
    .file-meta { font-size: 0.73rem; color: #71717a; }
    .file-progress-bar { flex: 1; height: 3px; background: #27272a; border-radius: 2px; overflow: hidden; min-width: 40px; }
    .file-progress-fill { height: 100%; background: #3b82f6; border-radius: 2px; transition: width 0.3s; }

    .file-status { flex-shrink: 0; }
    .remove-btn { background: none; border: none; color: #52525b; cursor: pointer; padding: 4px; border-radius: 4px; }
    .remove-btn:hover { color: #ef4444; }

    .run-bar { display: flex; align-items: center; gap: 10px; padding-top: 10px; flex-wrap: wrap; margin-top: auto; }

    .run-btn {
        display: flex; align-items: center; gap: 6px; padding: 8px 16px;
        border-radius: 6px; border: 1px solid #3f3f46; background: #18181b;
        color: #d4d4d8; cursor: pointer; font-size: 0.875rem; font-weight: 600;
    }
    .run-btn:disabled { opacity: 0.4; cursor: not-allowed; }
    .run-btn.primary { background: #3b82f6; border-color: #3b82f6; color: white; }
    .run-btn.primary:hover:not(:disabled) { background: #2563eb; }
    .run-btn.danger { border-color: #7f1d1d; color: #fca5a5; }
    .run-btn.danger:hover { background: #7f1d1d33; }

    .status-text { font-size: 0.8rem; }

    /* Folders tab */
    .empty-state {
        flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center;
        gap: 10px; color: #52525b; text-align: center; padding: 20px;
    }
    .hint-sub { font-size: 0.8rem; color: #3f3f46; max-width: 320px; }

    .folder-list { display: flex; flex-direction: column; gap: 6px; margin-top: 12px; flex: 1; overflow-y: auto; }
    .caf-result-bar { padding: 6px 16px; background: #18181b; border-bottom: 1px solid #27272a; color: #a1a1aa; font-size: 0.8rem; }
    .drive-dialog { display: flex; flex-direction: column; gap: 8px; padding: 12px 16px; background: #18181b; border-bottom: 1px solid #27272a; }
    .drive-dialog-row { display: flex; align-items: center; gap: 8px; }
    .drive-dialog-label { color: #a1a1aa; font-size: 0.8rem; min-width: 80px; }
    .drive-dialog-input { flex: 1; padding: 4px 8px; background: #0a0a0a; color: #f4f4f5; border: 1px solid #3f3f46; border-radius: 4px; font-size: 0.85rem; }
    .drive-dialog-input:disabled { opacity: 0.5; }
    .drive-dialog-hint { color: #71717a; font-size: 0.75rem; }
    .drive-create-form { display: flex; flex-direction: column; gap: 8px; padding: 8px 12px; background: #0a0a0a; border: 1px solid #27272a; border-radius: 4px; }
    .drive-create-form code { background: #18181b; padding: 1px 4px; border-radius: 3px; font-size: 0.7rem; }
    .drive-list { display: flex; flex-direction: column; gap: 4px; }
    .drive-list-row { display: flex; align-items: center; gap: 8px; padding: 4px 8px; background: #0a0a0a; border: 1px solid #27272a; border-radius: 4px; font-size: 0.8rem; }
    .drive-list-label { color: #f4f4f5; min-width: 140px; flex-shrink: 0; font-weight: 500; }
    .drive-list-kind { color: #71717a; min-width: 60px; flex-shrink: 0; font-family: monospace; }
    .drive-list-path { color: #a1a1aa; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: monospace; font-size: 0.75rem; }
    .folder-row {
        display: flex; align-items: flex-start; gap: 10px; background: #18181b;
        border: 1px solid #27272a; border-radius: 6px; padding: 10px 12px;
    }
    .folder-info { flex: 1; min-width: 0; }
    .folder-path { display: block; font-size: 0.85rem; color: #e4e4e7; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .folder-meta { display: block; font-size: 0.73rem; color: #71717a; margin-top: 3px; }
    .folder-actions { display: flex; gap: 4px; flex-shrink: 0; }

    .icon-btn { background: none; border: none; color: #52525b; cursor: pointer; padding: 4px; border-radius: 4px; }
    .icon-btn:hover { color: white; background: #27272a; }
    .icon-btn.danger:hover { color: #ef4444; background: #7f1d1d22; }
    .icon-btn:disabled { opacity: 0.4; cursor: not-allowed; }

    /* Contents tab */
    .query-input-wrap {
        display: flex; align-items: center; gap: 8px; background: #18181b;
        border: 1px solid #3f3f46; border-radius: 6px; padding: 0 10px;
    }
    .query-input {
        flex: 1; background: transparent; border: none; outline: none;
        color: #fafafa; font-size: 0.875rem; padding: 8px 0;
    }

    .result-count {
        display: flex; align-items: center; gap: 8px;
        font-size: 0.75rem; color: #71717a; margin-top: 8px; padding-bottom: 4px;
    }

    .selection-bar {
        display: flex; align-items: center; gap: 8px; padding: 6px 16px;
        background: #1e293b; border-bottom: 1px solid #334155;
    }
    .sel-count { font-size: 0.8rem; color: #94a3b8; flex: 1; }
    .tb-btn.danger { background: #450a0a; color: #fca5a5; border-color: #7f1d1d; }
    .tb-btn.danger:hover:not(:disabled) { background: #7f1d1d; }
    .select-all-wrap { display: inline-flex; align-items: center; margin-right: 6px; cursor: pointer; }

    /* PLAN P9 step 2 -- columnar Übersicht table.
       Single grid template shared between thead + every row, so every
       column lines up. user-select:none on rows is what makes shift /
       ctrl multi-select feel right (otherwise dragging across rows
       highlights filename text instead of selecting rows). */
    .catalog-table { flex: 1; overflow-y: auto; display: flex; flex-direction: column; min-height: 0; }
    .catalog-thead, .catalog-row {
        display: grid;
        grid-template-columns: var(--cat-cols, 28px 50px minmax(220px,1.6fr) minmax(120px,1fr) 60px 70px 90px minmax(140px,1.4fr) 56px 88px);
        align-items: center;
        gap: 8px;
        padding: 0 16px;
        font-size: 0.78rem;
    }
    .catalog-thead {
        position: sticky; top: 0; z-index: 2;
        background: #0c0c0e; color: #a1a1aa;
        border-bottom: 1px solid #27272a;
        font-weight: 600; font-size: 0.7rem; text-transform: uppercase;
        padding-top: 8px; padding-bottom: 8px;
    }
    .catalog-thead .col-sortable { cursor: pointer; user-select: none; }
    .catalog-thead .col-sortable:hover { color: #fafafa; }
    /* Column-header buttons used to be plain <div onclick=...>. Now
       they're real <button>s for a11y, but they still need to look
       like the other thead cells (no border, no default font, same
       text colour, left-aligned). */
    .catalog-thead .col-header-btn {
        background: transparent; border: none; padding: 0;
        color: inherit; font: inherit; text-align: left;
        text-transform: inherit; letter-spacing: inherit;
    }
    .catalog-thead .col-header-btn:focus-visible {
        outline: 2px solid #3b82f6; outline-offset: 2px; border-radius: 3px;
    }
    .sort-arrow { color: #3b82f6; margin-left: 4px; }
    .catalog-tbody { display: contents; }
    .catalog-row {
        background: transparent;
        border-bottom: 1px solid #18181b;
        padding-top: 6px; padding-bottom: 6px;
        cursor: pointer;
        user-select: none;
        -webkit-user-select: none;
    }
    .catalog-row:hover { background: #131316; }
    .catalog-row.selected { background: #1e293b; }
    .catalog-row.selected:hover { background: #243450; }
    .catalog-row.deleting { opacity: 0.4; pointer-events: none; }
    .catalog-row .cell {
        overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }
    .catalog-row .col-name { color: #e4e4e7; font-weight: 500; }
    .catalog-row .col-author, .catalog-row .col-folder, .catalog-row .col-mtime,
    .catalog-row .col-language, .catalog-row .col-volume {
        color: #a1a1aa;
    }
    .catalog-row .col-size, .catalog-row .col-year { color: #a1a1aa; text-align: right; }
    .catalog-thead .col-size, .catalog-thead .col-year { text-align: right; }
    .catalog-row .col-check input, .catalog-thead .col-check input {
        cursor: pointer; accent-color: #3b82f6;
    }
    .catalog-row .col-actions {
        display: flex; gap: 4px; justify-content: flex-end;
    }
    .danger-icon { color: #ef4444 !important; }
    .danger-icon:hover:not(:disabled) { color: #fca5a5 !important; }
    .load-more-bar {
        padding: 12px 16px; border-top: 1px solid #27272a;
        display: flex; justify-content: center;
    }
    .load-more-bar .muted-small { font-size: 0.7rem; color: #71717a; margin-left: 8px; }

    /* P9 step 8 — preview pane */
    .overview-split {
        display: flex; flex: 1; gap: 8px; overflow: hidden; min-height: 0;
    }
    .catalog-col {
        flex: 1; min-width: 0; display: flex; flex-direction: column; overflow: hidden;
    }
    .catalog-col .catalog-table { flex: 1; min-height: 0; }
    .preview-pane {
        flex: 0 0 380px; max-width: 45%;
        display: flex; flex-direction: column;
        background: #18181b; border: 1px solid #3f3f46;
        border-radius: 8px; overflow: hidden;
    }
    .preview-header {
        display: flex; align-items: center; justify-content: space-between;
        padding: 8px 12px; background: #27272a;
        border-bottom: 1px solid #3f3f46; font-size: 0.85rem;
    }
    .preview-title {
        flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
        color: #fafafa; font-weight: 600; margin-right: 8px;
    }
    .preview-close {
        background: none; border: none; cursor: pointer; color: #a1a1aa; padding: 2px;
    }
    .preview-close:hover { color: #fafafa; }
    .preview-body { flex: 1; overflow: auto; background: #0a0a0c; }
    .preview-body object { display: block; width: 100%; height: 100%; border: 0; }
    .preview-image { max-width: 100%; max-height: 100%; display: block; margin: 0 auto; }
    .preview-text {
        margin: 0; padding: 12px;
        font-family: var(--mono, ui-monospace, monospace);
        font-size: 0.78rem; line-height: 1.4;
        white-space: pre-wrap; word-break: break-all;
        color: #d4d4d8;
    }
    .preview-msg {
        padding: 24px 16px; text-align: center; color: #71717a; font-size: 0.85rem;
    }
    .preview-error { color: #f87171; }
    .open-ext-btn {
        display: inline-flex; align-items: center; gap: 4px;
        margin-top: 8px; background: none; border: 1px solid #3f3f46;
        color: #a1a1aa; border-radius: 4px; padding: 4px 8px;
        font-size: 0.78rem; cursor: pointer;
    }
    .open-ext-btn:hover { color: #3b82f6; border-color: #3b82f6; }
    .icon-btn.preview-active { color: #3b82f6; }

    /* P9 step 6 — column picker */
    .col-picker-wrap { position: relative; margin-left: auto; }
    .col-picker-btn { color: #71717a; }
    .col-picker-btn:hover, .col-picker-btn.active { color: #a1a1aa; }
    .col-picker-dropdown {
        position: absolute; right: 0; top: calc(100% + 4px); z-index: 10;
        background: #18181b; border: 1px solid #27272a; border-radius: 6px;
        padding: 8px 0; min-width: 140px;
        box-shadow: 0 4px 16px rgba(0,0,0,0.5);
    }
    .col-picker-item {
        display: flex; align-items: center; gap: 8px;
        padding: 5px 12px; font-size: 0.78rem; color: #a1a1aa;
        cursor: pointer; user-select: none;
    }
    .col-picker-item:hover { background: #27272a; color: #e4e4e7; }
    .col-picker-item input[type="checkbox"] { cursor: pointer; accent-color: #3b82f6; }

    .ext-badge {
        font-size: 0.6rem; font-weight: 800; padding: 3px 5px; border-radius: 4px;
        background: #27272a; color: #a1a1aa; flex-shrink: 0; min-width: 32px; text-align: center; margin-top: 2px;
    }
    .level-badge {
        font-size: 0.6rem; font-weight: 800; padding: 3px 6px; border-radius: 4px;
        flex-shrink: 0; min-width: 22px; text-align: center; margin-top: 2px;
    }
    .level-badge.l1 { background: #44403c33; color: #d6d3d1; }
    .level-badge.l2 { background: #1e3a5f33; color: #93c5fd; }
    .level-badge.l3 { background: #14532d33; color: #86efac; }
    .fail-badge {
        font-size: 0.55rem; font-weight: 800; padding: 2px 4px; border-radius: 3px;
        margin-top: 2px; margin-left: 2px; text-transform: uppercase; flex-shrink: 0;
    }
    .fail-badge.fail-drm      { background: #451a0333; color: #fbbf24; cursor: pointer; }
    .fail-badge.fail-drm:hover { background: #451a0366; }
    .drm-popover {
        position: absolute; z-index: 200; background: #1c1917; border: 1px solid #451a03;
        border-radius: 8px; padding: 12px 14px; width: 280px;
        font-size: 0.78rem; color: #fef3c7; line-height: 1.5;
        box-shadow: 0 8px 24px rgba(0,0,0,0.5);
    }
    .drm-popover p { margin: 0 0 4px; }
    .drm-popover a { color: #fbbf24; text-decoration: underline; }
    .drm-close {
        position: absolute; top: 6px; right: 8px; background: none; border: none;
        color: #78716c; cursor: pointer; font-size: 0.85rem;
    }
    .drm-close:hover { color: #fef3c7; }
    .fail-badge.fail-timeout  { background: #431a0033; color: #fb923c; }
    .fail-badge.fail-corrupt  { background: #450a0a33; color: #f87171; }
    .fail-badge.fail-password { background: #2e1a5233; color: #c084fc; }
    .fail-badge.fail-unsupported { background: #27272a;   color: #71717a; }
    .fail-badge.fail-other    { background: #1a1a2e33; color: #94a3b8; }

    .cb-tier-bar { display: flex; align-items: center; gap: 8px; padding: 5px 12px;
        background: #1c1917; border-bottom: 1px solid #292524; font-size: 0.72rem; flex-wrap: wrap; }
    .cb-tier { color: #78716c; }
    .cb-tier.tier-ok { color: #10b981; }
    .cb-archive-name { color: #57534e; font-size: 0.67rem; margin-left: auto; }
    .tb-toggle-small { display: flex; align-items: center; gap: 4px; font-size: 0.72rem;
        color: #71717a; cursor: pointer; padding: 2px 6px; white-space: nowrap; }
    .tb-toggle-small input { cursor: pointer; }
    /* .cidx archive tab */
    .cidx-tab { border-bottom: 2px solid #6366f1; color: #a5b4fc; }
    .tb-btn-flat { background: none; border: 1px dashed #3f3f46; color: #71717a;
        font-size: 0.7rem; padding: 3px 7px; border-radius: 4px; cursor: pointer; }
    .tb-btn-flat:hover { border-color: #6366f1; color: #a5b4fc; }
    .cidx-archive-panel { display: flex; flex-direction: column; min-height: 0; flex: 1; }
    .cidx-header { display: flex; align-items: center; gap: 8px;
        padding: 6px 12px; background: #1e1b4b33; border-bottom: 1px solid #312e81; }
    .cidx-path { font-size: 0.78rem; color: #a5b4fc; display: flex; align-items: center;
        gap: 5px; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .cidx-rows { overflow-y: auto; }
    .cidx-row { border-left: 3px solid #6366f133; background: #0f0e2222; cursor: pointer; }
    .cidx-row:hover { background: #1e1b4b33; }
    .cidx-row.cidx-sel { background: #1e1b4b55; border-left-color: #6366f1; }
    .cidx-sel-bar { display: flex; align-items: center; gap: 8px; padding: 6px 12px;
        background: #1e1b4b33; border-bottom: 1px solid #312e81; font-size: 0.78rem; color: #a5b4fc; }

    :global(.spin) { animation: spin 1s linear infinite; display: inline-flex; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }

    /* ── P13 slice A1/A2 — images grid + preview pane ───────────────────────
       Grid renders lazy-loaded thumbnails (slice A2 — IntersectionObserver
       in lazyThumbnail action) with a placeholder fallback for unloaded /
       errored / unsupported tiles. Click toggles a side preview pane that
       shows the full image plus an EXIF metadata table. Virtual scrolling
       still deferred to a later slice once the grid grows past the
       comfortable scroll envelope. */
    .images-split { display: grid; grid-template-columns: 1fr; min-height: 0; height: 100%; gap: 0; }
    .images-split.with-preview { grid-template-columns: minmax(0, 1fr) 380px; }

    .images-panel { display: flex; flex-direction: column; gap: 6px; min-height: 0; min-width: 0; }
    .images-count { font-size: 0.78rem; }
    .images-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
        gap: 8px;
        padding: 8px 12px;
        overflow-y: auto;
    }
    .images-tile {
        display: flex; flex-direction: column; gap: 4px;
        padding: 6px;
        border: 1px solid #2a2a3a;
        border-radius: 6px;
        background: #14141e;
        cursor: pointer;
        text-align: left;
        font: inherit; color: inherit;
    }
    .images-tile:hover { background: #1a1a28; border-color: #3a3a4a; }
    .images-tile-active { border-color: #6366f1; background: #1a1a32; }
    .images-tile-placeholder {
        position: relative;
        aspect-ratio: 1 / 1;
        display: flex; align-items: center; justify-content: center;
        background: #0a0a14;
        border-radius: 4px;
        color: #4b5563;
    }
    .images-thumb {
        width: 100%;
        aspect-ratio: 1 / 1;
        object-fit: cover;
        background: #0a0a14;
        border-radius: 4px;
        display: block;
    }
    .images-ext-badge {
        position: absolute; bottom: 4px; right: 4px;
        font-size: 0.62rem; font-weight: 600;
        padding: 1px 5px;
        background: #1e1b4b; color: #a5b4fc;
        border-radius: 3px;
        letter-spacing: 0.04em;
    }
    .images-tile-name {
        font-size: 0.72rem; color: #cbd5e1;
        overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }

    .images-preview-pane {
        display: flex; flex-direction: column;
        border-left: 1px solid #27272a;
        background: #0a0a14;
        min-height: 0;
        overflow: hidden;
    }
    .images-preview-header {
        display: flex; align-items: center; justify-content: space-between;
        padding: 8px 10px;
        border-bottom: 1px solid #27272a;
        gap: 8px;
    }
    .images-preview-title {
        font-size: 0.78rem; color: #cbd5e1;
        overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
        flex: 1;
    }
    .images-preview-body {
        display: flex; flex-direction: column;
        flex: 1;
        overflow-y: auto;
        padding: 10px;
        gap: 10px;
    }
    .images-preview-image {
        width: 100%;
        max-height: 50vh;
        object-fit: contain;
        background: #000;
        border-radius: 4px;
    }
    .images-exif-block { display: flex; flex-direction: column; gap: 6px; }
    .images-exif-heading {
        margin: 0; font-size: 0.78rem; font-weight: 600; color: #94a3b8;
        text-transform: uppercase; letter-spacing: 0.04em;
    }
    .images-exif-loading { font-size: 0.78rem; color: #71717a; display: flex; gap: 6px; align-items: center; }
    .images-exif-empty   { font-size: 0.78rem; color: #71717a; font-style: italic; }
    .images-exif-table {
        width: 100%;
        font-size: 0.78rem;
        border-collapse: collapse;
    }
    .images-exif-table th {
        text-align: left; font-weight: 500;
        color: #94a3b8;
        padding: 3px 8px 3px 0;
        white-space: nowrap;
        vertical-align: top;
        width: 30%;
    }
    .images-exif-table td {
        color: #e2e8f0;
        padding: 3px 0;
        word-break: break-word;
    }

    /* ── P13 slice A3 — duplicate-cluster grouping ─────────────────────── */
    .images-dup-list {
        display: flex; flex-direction: column;
        gap: 12px;
        padding: 8px 12px;
        overflow-y: auto;
    }
    .images-dup-group {
        display: flex; flex-direction: column;
        border: 1px solid #2a2a3a;
        border-radius: 8px;
        background: #14141e;
        padding: 8px;
    }
    .images-dup-header {
        display: flex; align-items: baseline; gap: 8px;
        padding: 4px 6px 8px;
        font-size: 0.78rem;
        color: #94a3b8;
    }
    .images-dup-count {
        background: #1e1b4b; color: #a5b4fc;
        padding: 1px 8px; border-radius: 10px;
        font-weight: 600; font-size: 0.72rem;
    }
    .images-dup-hash {
        font-family: ui-monospace, monospace;
        color: #71717a;
        font-size: 0.72rem;
    }
    .muted-small { color: #71717a; font-size: 0.72rem; margin-left: 4px; }

    /* P13/A4 — pHash threshold input + per-tile distance badge.
       Threshold input piggy-backs on .muted-small for the label
       weight; the input itself is just narrow enough for a 0..64
       value without scroll arrows visually crowding the toolbar. */
    .images-threshold-label { display: inline-flex; align-items: center; gap: 4px; }
    .images-threshold-input {
        width: 48px;
        padding: 2px 4px;
        background: #18181b; color: #d4d4d8;
        border: 1px solid #3f3f46; border-radius: 4px;
        font-size: 0.72rem;
    }
    .images-threshold-input:focus { outline: 1px solid #6366f1; }
    .images-ndup-distance {
        margin-left: 6px;
        color: #71717a;
        font-family: ui-monospace, monospace;
        font-size: 0.66rem;
    }

    /* P13/B1 — CrispLens settings panel */
    .crisplens-settings-pane {
        margin: 6px 12px;
        padding: 10px 14px;
        border: 1px solid #2a2a3a;
        border-radius: 6px;
        background: #14141e;
        font-size: 0.78rem;
        display: flex; flex-direction: column; gap: 8px;
    }
    .crisplens-heading { margin: 0; color: #cbd5e1; font-size: 0.82rem; font-weight: 600; }
    .crisplens-helper  { color: #71717a; font-size: 0.72rem; margin: 0; }
    .crisplens-row     { display: flex; align-items: center; gap: 8px; }
    .crisplens-row > span { color: #94a3b8; min-width: 80px; }
    .crisplens-row input[type="url"],
    .crisplens-row input[type="text"],
    .crisplens-row input[type="password"],
    .crisplens-row select {
        flex: 1;
        padding: 4px 6px;
        background: #0a0a14; color: #e2e8f0;
        border: 1px solid #3f3f46; border-radius: 4px;
        font-size: 0.78rem;
    }
    .crisplens-sep { border: 0; border-top: 1px solid #27272a; margin: 4px 0; width: 100%; }
    .crisplens-msg { margin-top: 4px; }

    /* P13/B4 — degradation banner.  Three colour variants so the
       Offline (red), Session-expired (amber), and Warming-up (blue)
       cases distinguish themselves at a glance.  Each carries an
       inline icon + message + optional action button. */
    .crisplens-banner {
        display: flex; align-items: center; gap: 8px;
        margin: 6px 12px; padding: 6px 10px;
        border-radius: 4px; font-size: 0.78rem;
    }
    .crisplens-banner-offline { background: #4a1d1d; color: #fecaca; border: 1px solid #7f1d1d; }
    .crisplens-banner-warn    { background: #4a3a1d; color: #fcd34d; border: 1px solid #92400e; }
    .crisplens-banner-info    { background: #1e3a5f; color: #93c5fd; border: 1px solid #1e40af; }
    .crisplens-banner-detail  {
        margin-left: 4px; padding: 1px 6px;
        background: #00000044; color: inherit;
        border-radius: 3px;
        font-family: ui-monospace, monospace; font-size: 0.66rem;
    }

    /* P13/B5 — preview-pane "Open in CrispLens" action button
       (sits next to the close button in the header) + the
       watchfolder cross-reference hint row underneath the header. */
    .preview-action {
        display: inline-flex; align-items: center; gap: 4px;
        padding: 3px 8px; border-radius: 4px;
        border: 1px solid #3f3f46; background: transparent;
        color: #a5b4fc; cursor: pointer;
        font-size: 0.72rem;
    }
    .preview-action:hover { background: #1e1b4b; }
    .crisplens-watchfolder-hint {
        display: flex; align-items: center; gap: 6px;
        padding: 4px 10px;
        background: #1e1b4b22; color: #94a3b8;
        font-size: 0.72rem;
        border-bottom: 1px solid #27272a;
    }
    .crisplens-watchfolder-hint code {
        font-family: ui-monospace, monospace;
        color: #cbd5e1;
        overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }

    /* P13/B3 — People grid (face-recognition clusters from CrispLens) */
    .crisplens-people-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
        gap: 12px;
        padding: 12px;
        overflow-y: auto;
    }
    .crisplens-person-card {
        display: flex; flex-direction: column; gap: 4px;
        padding: 8px;
        border: 1px solid #2a2a3a;
        border-radius: 6px;
        background: #14141e;
    }
    .crisplens-person-card:hover { background: #1a1a28; }
    .crisplens-person-cover {
        aspect-ratio: 1 / 1;
        display: flex; align-items: center; justify-content: center;
        background: #1e1b4b22;
        border-radius: 4px;
        color: #6366f1;
    }
    .crisplens-person-name {
        font-size: 0.82rem; color: #cbd5e1; font-weight: 500;
        overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }

    /* P13/B2 — CrispLens text-search input + result panel.
       Sits in the toolbar (input) + just below the toolbar (results
       list).  Results panel is collapsible via the X button. */
    .crisplens-search-input {
        padding: 4px 8px;
        background: #0a0a14; color: #e2e8f0;
        border: 1px solid #3f3f46; border-radius: 4px;
        font-size: 0.78rem;
        min-width: 200px;
    }
    .crisplens-search-input:focus { outline: 1px solid #6366f1; }
    .crisplens-search-results {
        margin: 6px 12px;
        border: 1px solid #2a2a3a;
        border-radius: 6px;
        background: #14141e;
        max-height: 200px;
        overflow-y: auto;
        display: flex; flex-direction: column;
    }
    .crisplens-search-header {
        display: flex; justify-content: space-between; align-items: center;
        padding: 4px 10px;
        background: #1a1a28;
        border-bottom: 1px solid #27272a;
    }
    .crisplens-search-list { list-style: none; margin: 0; padding: 4px 0; }
    .crisplens-search-hit {
        display: flex; align-items: center; gap: 8px;
        padding: 4px 12px;
        font-size: 0.78rem;
    }
    .crisplens-search-hit:hover { background: #1a1a28; }
    .crisplens-search-name {
        color: #cbd5e1;
        overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
        flex: 1;
    }
    .crisplens-search-tags {
        margin-left: auto;
        max-width: 50%;
        overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }

    /* P13 follow-up — CrispLens face-overlay rectangles on the
       preview image.  The wrapping div is `position: relative` so
       the absolute-positioned bbox children land in image-local
       coordinates; bbox top/left/width/height are emitted in % so
       they scale with the rendered image (which has max-height +
       object-fit: contain on .images-preview-image). */
    .images-preview-image-wrap { position: relative; display: inline-block; max-width: 100%; }
    .images-face-bbox {
        position: absolute;
        border: 2px solid #fde68a;
        background: rgba(253, 224, 134, 0.08);
        border-radius: 2px;
        pointer-events: none;
    }
    .images-face-label {
        position: absolute; top: -18px; left: -2px;
        padding: 1px 5px;
        background: #fde68a; color: #1c1917;
        font-size: 0.66rem; font-weight: 600;
        border-radius: 2px;
        white-space: nowrap;
    }
    .images-face-loading {
        margin-top: 4px;
        display: flex; align-items: center; gap: 6px;
    }

    @media (max-width: 767px) {
        .tab-bar { padding: 8px 8px 0; overflow-x: auto; }
        .tab { padding: 6px 10px; font-size: 0.75rem; white-space: nowrap; }
    }
</style>
