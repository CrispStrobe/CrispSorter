<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';
    import { open as openDialog } from '@tauri-apps/plugin-dialog';
    import { openPath } from '@tauri-apps/plugin-opener';
    import { readDir, readFile, stat, type DirEntry } from '@tauri-apps/plugin-fs';
    import { load as storeLoad } from '@tauri-apps/plugin-store';
    import { onMount } from 'svelte';
    import { i18n } from '$lib/i18n.svelte';
    import {
        FolderOpen, FileText, RefreshCw, Play, Pause, X,
        CheckCircle2, AlertCircle, Loader2, ChevronDown, ChevronRight,
        UploadCloud, Trash2, Database, Search, ExternalLink
    } from 'lucide-svelte';
    import { extractText, SUPPORTED_EXTENSIONS } from '$lib/extractors/index';
    import IndexSearch from './IndexSearch.svelte';
    import { logInfo, logWarn, logError } from '$lib/log';

    // ── Types ──────────────────────────────────────────────────────────────────

    type FileStatus = 'pending' | 'extracting' | 'embedding' | 'done' | 'error' | 'skipped';

    interface IngestEntry {
        id:        string;
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

    interface ManagedFolder {
        path:        string;
        addedAt:     number;
        lastScanned: number | null;
        fileCount:   number;
    }

    type Tab = 'overview' | 'search' | 'add' | 'sources';

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
    let indexStats  = $state<{ total_rows: number; doc_count: number; chunk_count: number } | null>(null);
    let selectedDocIds = $state<Set<string>>(new Set());
    let deletingIds = $state<Set<string>>(new Set());
    let promotingL2 = $state(false);

    // Ingest progress from Rust events
    let currentFile = $state('');
    let currentStep = $state('');
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

    const supported = new Set<string>(SUPPORTED_EXTENSIONS);

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
            // Load persisted folder list
            try {
                const store = await storeLoad('index-ingest.json', { defaults: {}, autoSave: true });
                const saved = await store.get<ManagedFolder[]>('folders');
                if (saved) folders = saved;
            } catch (e) { /* store not yet created */ }

            // Listen to ingest progress events from Rust
            unlistenProgress = await listen<{ filename: string; step: string; chunk_index: number; chunk_total: number; message: string }>(
                'index://ingest-progress',
                (ev) => {
                    currentFile       = ev.payload.filename;
                    currentStep       = ev.payload.step;
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

            // Restore pending Hinzufügen entries from a previous session so
            // navigating to Settings + back doesn't drop them.
            try {
                const store = await storeLoad('index-ingest.json', { defaults: {}, autoSave: true });
                const savedEntries = await store.get<IngestEntry[]>('entries');
                if (savedEntries && Array.isArray(savedEntries)) {
                    entries = savedEntries.filter(e => e.status !== 'done');
                }
            } catch { /* store not yet created */ }

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

    async function addFolder() {
        const selected = await openDialog({ directory: true, multiple: false });
        if (!selected) return;
        const path = selected as string;
        if (folders.some(f => f.path === path)) return;
        folders = [...folders, { path, addedAt: Date.now(), lastScanned: null, fileCount: 0 }];
        await saveFolders();
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
            filters: [{ name: 'Documents', extensions: [...SUPPORTED_EXTENSIONS] }]
        });
        if (!selected) return;
        await addPaths(Array.isArray(selected) ? selected : [selected]);
    }

    async function addPaths(paths: string[]) {
        const existing = new Set(entries.map(e => e.path));
        const toAdd: IngestEntry[] = paths
            .filter(p => !existing.has(p))
            .filter(p => supported.has(p.split('.').pop()?.toLowerCase() ?? ''))
            .map(p => {
                const parts = p.replace(/\\/g, '/').split('/');
                const filename = parts[parts.length - 1];
                const ext = filename.split('.').pop()?.toLowerCase() ?? '';
                return { id: crypto.randomUUID(), path: p, filename, ext, size: 0, status: 'pending' as FileStatus };
            });
        entries = [...entries, ...toAdd];
    }

    function removeEntry(id: string) { entries = entries.filter(e => e.id !== id); persistEntries(); }
    function clearAll()  { if (!running) { entries = []; persistEntries(); } }
    function clearDone() { entries = entries.filter(e => e.status !== 'done'); persistEntries(); }

    /** Persist the Hinzufügen queue across tab switches / app restarts.
     *  Mirrors how the Stapel batch keeps state in tauri-plugin-store. */
    async function persistEntries() {
        try {
            const store = await storeLoad('index-ingest.json', { defaults: {}, autoSave: true });
            await store.set('entries', $state.snapshot(entries));
        } catch { /* store not yet writable */ }
    }
    $effect(() => {
        const _ = entries.length;
        persistEntries();
    });

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

    /** Quick filesystem-only ingest. No text extraction, no embedding —
     *  but we DO need the underlying LocalIndex to be ready. If the user
     *  hasn't enabled the full search index yet, we transparently flip
     *  the enable flag + auto-init so L1 just works without forcing a
     *  trip to Settings. (L1 doesn't need Tantivy or an embedder model;
     *  the LocalIndex's LanceDB table is enough.)
     *
     *  When an init is *already in progress* (e.g. user clicked Files
     *  twice, or another component triggered it), we poll until it
     *  completes instead of returning an "already in progress" error.
     */
    async function ensureIndexReady(): Promise<boolean> {
        const isReady = async () => invoke<boolean>('index_is_ready').catch(() => false);

        // Fast path: already up.
        const cfg = await invoke<{ enabled: boolean }>('index_get_config').catch(() => ({ enabled: false }));
        if (cfg.enabled && (await isReady())) return true;

        // Try to (re-)init. The Rust side's `initializing` flag rejects
        // duplicate concurrent calls — when that happens we wait it out
        // by polling `index_is_ready`.
        const tryInit = async () => {
            await invoke('index_set_config', { config: { ...(cfg as any), enabled: true } });
            const dataDir = await invoke<string>('get_app_data_dir').catch(() => '');
            await invoke('index_init', { dataDir });
        };

        try {
            await tryInit();
            return true;
        } catch (e: any) {
            const msg = String(e?.message ?? e ?? '');
            if (msg.includes('already in progress')) {
                logInfo('Catalog: another init is running — waiting for it to finish');
                // Poll up to 10 minutes (long enough for a 2 GB embedder
                // download on a slow connection). Bail with a clear error
                // if we time out.
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
        if (!(await ensureIndexReady())) return;
        const pending = entries.filter(e => e.status === 'pending' || e.status === 'error');
        if (pending.length === 0) return;

        l1Running = true;
        try {
            const files = [];
            for (const e of pending) {
                const meta = await stat(e.path).catch(() => null);
                const size = (meta as any)?.size ?? 0;
                const mtime = ((meta as any)?.mtime ?? new Date()).valueOf();
                const ctime = ((meta as any)?.birthtime ?? (meta as any)?.mtime ?? new Date()).valueOf();
                const parentDir = e.path.replace(/\\/g, '/').replace(/\/[^/]+$/, '') || '';
                const docId = await hashText(e.path);
                files.push({
                    docId,
                    sourceHash: docId,
                    locationUri: e.path,
                    ownerId: 'local',
                    filename: e.filename,
                    ext: e.ext,
                    parentDir,
                    size,
                    mtimeMs: mtime,
                    ctimeMs: ctime,
                });
                updateEntry(e.id, { status: 'embedding' });
            }
            await invoke('index_ingest_l1', { files });
            for (const e of pending) updateEntry(e.id, { status: 'done', chunks: 0 });
        } catch (err: any) {
            console.error('[L1] ingest failed:', err);
            for (const e of pending) updateEntry(e.id, { status: 'error', error: String(err) });
        } finally {
            l1Running = false;
        }
    }

    /** L2 ingest: writes filesystem-only rows first (so the doc_id exists)
     *  then immediately promotes them via the embedded-metadata reader.
     *  Doesn't require Tantivy / embedder either. */
    async function startL2Ingest() {
        if (l2RunningInline) return;
        if (!(await ensureIndexReady())) return;
        const pending = entries.filter(e => e.status === 'pending' || e.status === 'error');
        if (pending.length === 0) return;
        l2RunningInline = true;
        try {
            // Step 1: L1 ingest to create doc rows.
            await startL1Ingest();
            // Step 2: collect doc_ids of just-ingested rows by hashing path.
            // Frontend doesn't easily know the doc_id without re-fetching, so
            // pull recent contents and match by filename+path.
            const docs = await invoke<any[]>('index_list_documents', { limit: 1000 }).catch(() => []);
            const ids: string[] = [];
            for (const e of pending) {
                const found = docs.find(d => (d.filename ?? '') === e.filename && String(d.location_uri ?? '').includes(e.path.replace(/\\/g, '/')));
                if (found?.doc_id) ids.push(found.doc_id);
            }
            if (ids.length > 0) {
                await invoke('index_promote_l2', { docIds: ids });
            }
        } catch (err: any) {
            console.error('[L2] ingest failed:', err);
        } finally {
            l2RunningInline = false;
        }
    }

    async function startIngest() {
        if (running) return;
        if (ingestLevel === 1) {
            await startL1Ingest();
            return;
        }
        if (ingestLevel === 2) {
            await startL2Ingest();
            return;
        }
        if (!(await ensureIndexReady())) return;

        running   = true;
        paused    = false;
        abortCtrl = new AbortController();
        const signal = abortCtrl.signal;

        const toProcess = entries.filter(e => e.status === 'pending' || e.status === 'error');

        for (const entry of toProcess) {
            if (signal.aborted) break;
            while (paused && !signal.aborted) await new Promise(r => setTimeout(r, 200));
            if (signal.aborted) break;

            updateEntry(entry.id, { status: 'extracting', error: undefined });
            logInfo(`Extract: ${entry.path} (.${entry.ext})`);

            try {
                let bytes: Uint8Array;
                try {
                    bytes = await readFile(entry.path);
                } catch (fsErr: any) {
                    logError(`Read failed: ${entry.path} -- ${fsErr?.message ?? fsErr}`);
                    updateEntry(entry.id, { status: 'error', error: `Read failed: ${fsErr}` });
                    continue;
                }
                const ab      = bytes.buffer as ArrayBuffer;
                const fileObj = new File([ab], entry.filename, { type: mimeFor(entry.ext) });

                const result = await extractText(fileObj);
                logInfo(`Extracted ${entry.filename}: ${(result.text?.length ?? 0)} chars, ${(result.headings?.length ?? 0)} headings`);

                if (!result.text || result.text.trim().length < 20) {
                    updateEntry(entry.id, { status: 'skipped', error: 'Zu wenig Text extrahiert' });
                    logWarn(`Skip ${entry.filename}: too little text (${result.text?.length ?? 0} chars)`);
                    continue;
                }

                updateEntry(entry.id, { status: 'embedding' });

                const language   = detectLanguage(result.text);
                const sourceHash = await hashText(result.text + entry.path);

                const stats_res = await invoke<{ chunk_count: number; embed_time_ms: number; write_time_ms: number }>(
                    'index_ingest_document',
                    {
                        input: {
                            fullText:    result.text,
                            fullTextMd:  result.markdownText ?? '',
                            headings:    result.headings ?? [],
                            title:       result.metadata?.title  ?? null,
                            author:      result.metadata?.author ?? null,
                            year:        result.metadata?.year   ? Number(result.metadata.year) : null,
                            filename:    entry.filename,
                            ext:         entry.ext,
                            language,
                            locationUri: entry.path,   // use raw absolute path as URI
                            ownerId:     'local',
                            sourceHash,
                            tags:        [],
                        }
                    }
                );

                updateEntry(entry.id, {
                    status:  'done',
                    chunks:  stats_res.chunk_count,
                    embedMs: stats_res.embed_time_ms,
                    writeMs: stats_res.write_time_ms,
                });

            } catch (err: any) {
                logError(`Extract/index failed for ${entry.filename}: ${err?.message ?? err}`);
                updateEntry(entry.id, { status: 'error', error: String(err) });
            }
        }

        running = false;
        paused  = false;
        abortCtrl = null;
        currentFile = '';
    }

    function pauseResume() {
        if (!running) return;
        paused = !paused;
    }

    function stopIngest() {
        abortCtrl?.abort();
        running = false;
        paused  = false;
        entries = entries.map(e =>
            (e.status === 'extracting' || e.status === 'embedding')
                ? { ...e, status: 'pending' as FileStatus }
                : e
        );
        currentFile = '';
    }

    // ── Index contents ─────────────────────────────────────────────────────────

    /** Pull `level` (1 or 3) out of the row's metadata_json, falling back to a
     *  reasonable default by inspecting which fields are populated. */
    function docLevel(d: any): 1 | 3 {
        if (d.metadata_json) {
            try {
                const m = JSON.parse(d.metadata_json);
                if (m.level === 1) return 1;
                if (m.level === 3) return 3;
            } catch { /* malformed JSON — ignore */ }
        }
        // No metadata blob: if the row has a snippet/full_text it's L3.
        return d.snippet ? 3 : 1;
    }

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

    function applyCatalogFilters(docs: any[]): any[] {
        const q = contentsQuery.trim().toLowerCase();
        const folderPrefix = contentsFolder.trim().replace(/\\/g, '/').toLowerCase().replace(/\/$/, '');
        return docs.filter(d => {
            if (q && !(
                (d.title ?? '').toLowerCase().includes(q) ||
                (d.filename ?? '').toLowerCase().includes(q) ||
                (d.author ?? '').toLowerCase().includes(q)
            )) return false;

            if (folderPrefix) {
                const p = docPath(d);
                // Subtree match: doc path must start with the folder prefix
                // followed by either nothing, "/", or the end of the string.
                if (!(p === folderPrefix || p.startsWith(folderPrefix + '/'))) return false;
            }

            if (contentsExt.size > 0) {
                const ext = (d.ext ?? '').toLowerCase();
                if (!contentsExt.has(ext)) return false;
            }

            if (contentsLevel !== 'all' && docLevel(d) !== contentsLevel) return false;

            if (contentsCompleteness !== 'any') {
                const hasAuthor = !!(d.author && d.author.trim());
                const hasTitle  = !!(d.title  && d.title.trim());
                const hasYear   = !!(d.year);
                if (contentsCompleteness === 'has_author' && !hasAuthor) return false;
                if (contentsCompleteness === 'has_title'  && !hasTitle)  return false;
                if (contentsCompleteness === 'has_year'   && !hasYear)   return false;
                if (contentsCompleteness === 'has_all'    && !(hasAuthor && hasTitle && hasYear)) return false;
            }

            return true;
        });
    }

    /** All distinct extensions present in the currently-loaded contents — used
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
    const visibleContents = $derived(applyCatalogFilters(_allContents));

    async function loadContents() {
        contentsLoading = true;
        selectedDocIds = new Set();
        try {
            // Fetch stats and document list in parallel
            const [stats, docs] = await Promise.all([
                invoke<{ total_rows: number; doc_count: number; chunk_count: number }>('index_stats').catch(() => null),
                invoke<any[]>('index_list_documents', { limit: 500 }),
            ]);
            indexStats = stats;
            _allContents = docs;
            contents = visibleContents;
        } catch {
            _allContents = [];
            contents = [];
            indexStats = null;
        } finally {
            contentsLoading = false;
        }
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
        if (typeof sel === 'string') contentsFolder = sel;
    }

    // Re-run the client-side filter whenever a filter input changes.
    $effect(() => {
        contents = applyCatalogFilters(_allContents);
    });

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
        if (!(await ensureIndexReady())) return;
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
     *  ingest, then call `index_ingest_document`. Updates the live row
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
            const cfg = await invoke<{ enabled: boolean }>('index_get_config').catch(() => ({ enabled: false }));
            if (!cfg.enabled) {
                alert('Search index is not enabled. Please enable it in Settings → Search Index first.');
                return;
            }

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
                    const fileObj = new File([ab], filename, { type: mimeFor(ext) });
                    const result = await extractText(fileObj);
                    if (!result.text || result.text.trim().length < 20) {
                        skipped++;
                        l3Progress = { done: (l3Progress?.done ?? 0) + 1, total: ids.length, current: row.filename ?? id };
                        continue;
                    }
                    const language = detectLanguage(result.text);
                    const sourceHash = await hashText(result.text + path);
                    await invoke('index_ingest_document', {
                        input: {
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
                        },
                    });
                    ok++;
                } catch (e) {
                    console.error('[L3] promote failed for', id, e);
                    fail++;
                }
                l3Progress = { done: (l3Progress?.done ?? 0) + 1, total: ids.length, current: row.filename ?? id };
            }
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

    function statusLabel(s: FileStatus): string {
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
            <Database size={14} /> Übersicht{#if indexStats !== null} ({indexStats.doc_count}){/if}
        </button>
        <button class="tab" class:active={activeTab === 'search'} onclick={() => activeTab = 'search'}>
            <Search size={14} /> Suche
        </button>
        <button class="tab" class:active={activeTab === 'add'} onclick={() => activeTab = 'add'}>
            <UploadCloud size={14} /> Hinzufügen
        </button>
        <button class="tab" class:active={activeTab === 'sources'} onclick={() => activeTab = 'sources'}>
            <FolderOpen size={14} /> Quellen ({folders.length})
        </button>
    </div>

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
                <p class="drop-hint">PDF, DOCX, EPUB, TXT, MD, HTML, WebP/PNG/JPG — oder "Dateien" klicken</p>
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
                                    <span class="file-meta">{statusLabel(entry.status)}</span>
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
        </div>
        {#if cafLastResult}
            <div class="caf-result-bar">
                {cafLastResult}
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
            <button class="tb-btn" onclick={loadContents} disabled={contentsLoading}>
                {#if contentsLoading}<Loader2 size={13} class="spin" />{:else}<RefreshCw size={13} />{/if}
                Aktualisieren
            </button>
        </div>

        <!-- Filters -->
        <div class="filter-bar">
            <span class="filter-label">Ordner:</span>
            <input class="folder-filter" type="text"
                bind:value={contentsFolder}
                placeholder="Pfad-Präfix (z. B. C:/Books/Theology)" />
            <button class="chip" onclick={pickContentsFolder} title="Ordner auswählen">
                <FolderOpen size={11} />
            </button>
            {#if contentsFolder}
                <button class="chip ghost" onclick={() => contentsFolder = ''}>×</button>
            {/if}

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
        </div>

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
                </div>
            {/if}

            <div class="contents-list">
                {#each contents as doc (doc.doc_id)}
                    {@const isSelected = selectedDocIds.has(doc.doc_id)}
                    {@const isDeleting = deletingIds.has(doc.doc_id)}
                    {@const lvl = docLevel(doc)}
                    <div class="contents-row" class:selected={isSelected} class:deleting={isDeleting}>
                        <input type="checkbox" class="row-check" checked={isSelected}
                            onchange={() => toggleSelect(doc.doc_id)} />
                        {#if doc.ext}
                            <div class="ext-badge ext-{doc.ext.toLowerCase()}">{doc.ext.toUpperCase()}</div>
                        {:else}
                            <div class="ext-badge">–</div>
                        {/if}
                        <span class="level-badge" class:l1={lvl === 1} class:l3={lvl === 3} title="Analyse-Tiefe">L{lvl}</span>
                        <div class="contents-info">
                            <span class="contents-title">{doc.title || doc.filename || doc.doc_id?.slice(0,16)}</span>
                            <span class="contents-meta">
                                {#if doc.author}{doc.author}{/if}{#if doc.author && doc.year} · {/if}{#if doc.year}{doc.year}{/if}
                                {#if doc.language} · {doc.language}{/if}
                            </span>
                            {#if doc.snippet}
                                <span class="contents-snippet">{doc.snippet.slice(0, 160)}{doc.snippet.length > 160 ? '…' : ''}</span>
                            {/if}
                        </div>
                        <div class="contents-actions">
                            <button class="icon-btn" onclick={() => openIndexedFile(doc.location_uri)} title="Öffnen">
                                <ExternalLink size={13} />
                            </button>
                            <button class="icon-btn danger-icon" onclick={() => deleteFromIndex(doc.doc_id)}
                                disabled={isDeleting} title="Aus Index löschen">
                                {#if isDeleting}<Loader2 size={13} class="spin" />{:else}<Trash2 size={13} />{/if}
                            </button>
                        </div>
                    </div>
                {/each}
            </div>
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
    .result-count, .contents-list, .folders-toolbar, .index-stats-bar {
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

    .result-count { font-size: 0.75rem; color: #71717a; margin-top: 8px; padding-bottom: 4px; }

    .selection-bar {
        display: flex; align-items: center; gap: 8px; padding: 6px 16px;
        background: #1e293b; border-bottom: 1px solid #334155;
    }
    .sel-count { font-size: 0.8rem; color: #94a3b8; flex: 1; }
    .tb-btn.danger { background: #450a0a; color: #fca5a5; border-color: #7f1d1d; }
    .tb-btn.danger:hover:not(:disabled) { background: #7f1d1d; }
    .select-all-wrap { display: inline-flex; align-items: center; margin-right: 6px; cursor: pointer; }

    .contents-list { flex: 1; overflow-y: auto; display: flex; flex-direction: column; gap: 4px; }
    .contents-row {
        display: flex; align-items: flex-start; gap: 10px; background: #18181b;
        border: 1px solid #27272a; border-radius: 6px; padding: 8px 10px;
    }
    .contents-info { flex: 1; min-width: 0; }
    .contents-title { display: block; font-size: 0.85rem; color: #e4e4e7; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .contents-meta { display: block; font-size: 0.73rem; color: #71717a; margin-top: 2px; }
    .contents-snippet { display: block; font-size: 0.75rem; color: #52525b; margin-top: 3px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .contents-actions { flex-shrink: 0; display: flex; gap: 4px; }
    .contents-row.selected { background: #1e293b; border-color: #334155; }
    .contents-row.deleting { opacity: 0.4; pointer-events: none; }
    .row-check { flex-shrink: 0; margin-top: 3px; cursor: pointer; accent-color: #3b82f6; }
    .danger-icon { color: #ef4444 !important; }
    .danger-icon:hover:not(:disabled) { color: #fca5a5 !important; }

    .ext-badge {
        font-size: 0.6rem; font-weight: 800; padding: 3px 5px; border-radius: 4px;
        background: #27272a; color: #a1a1aa; flex-shrink: 0; min-width: 32px; text-align: center; margin-top: 2px;
    }
    .level-badge {
        font-size: 0.6rem; font-weight: 800; padding: 3px 6px; border-radius: 4px;
        flex-shrink: 0; min-width: 22px; text-align: center; margin-top: 2px;
    }
    .level-badge.l1 { background: #44403c33; color: #d6d3d1; }
    .level-badge.l3 { background: #14532d33; color: #86efac; }

    :global(.spin) { animation: spin 1s linear infinite; display: inline-flex; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
