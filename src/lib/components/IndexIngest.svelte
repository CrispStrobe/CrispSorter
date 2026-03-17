<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';
    import { open as openDialog } from '@tauri-apps/plugin-dialog';
    import { openPath } from '@tauri-apps/plugin-opener';
    import { readDir, readFile, type DirEntry } from '@tauri-apps/plugin-fs';
    import { load as storeLoad } from '@tauri-apps/plugin-store';
    import { onMount } from 'svelte';
    import {
        FolderOpen, FileText, RefreshCw, Play, Pause, X,
        CheckCircle2, AlertCircle, Loader2, ChevronDown, ChevronRight,
        UploadCloud, Trash2, Database, Search, ExternalLink
    } from 'lucide-svelte';
    import { extractText } from '$lib/extractors/index';

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

    type Tab = 'ingest' | 'folders' | 'contents';

    // ── State ──────────────────────────────────────────────────────────────────

    let activeTab   = $state<Tab>('ingest');
    let entries     = $state<IngestEntry[]>([]);
    let running     = $state(false);
    let paused      = $state(false);
    let abortCtrl   = $state<AbortController | null>(null);
    let dropActive  = $state(false);

    let folders     = $state<ManagedFolder[]>([]);
    let scanningFolder = $state<string | null>(null);

    let contents    = $state<any[]>([]);
    let contentsLoading = $state(false);
    let contentsQuery = $state('');
    let indexStats  = $state<{ total_rows: number; doc_count: number; chunk_count: number } | null>(null);
    let selectedDocIds = $state<Set<string>>(new Set());
    let deletingIds = $state<Set<string>>(new Set());

    // Ingest progress from Rust events
    let currentFile = $state('');
    let currentStep = $state('');
    let currentChunk = $state(0);
    let currentChunkTotal = $state(0);

    const supported = new Set(['pdf', 'docx', 'txt', 'md', 'epub']);

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

            cleanup = () => { unlistenProgress?.(); };
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
            filters: [{ name: 'Documents', extensions: ['pdf','docx','txt','md','epub'] }]
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

    function removeEntry(id: string) { entries = entries.filter(e => e.id !== id); }
    function clearAll()  { if (!running) entries = []; }
    function clearDone() { entries = entries.filter(e => e.status !== 'done'); }

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

    async function startIngest() {
        if (running) return;
        const cfg = await invoke<{ enabled: boolean }>('index_get_config').catch(() => ({ enabled: false }));
        if (!cfg.enabled) {
            alert('Search index is not enabled. Please enable it in Settings → Search Index first.');
            return;
        }

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

            try {
                const bytes   = await readFile(entry.path);
                const ab      = bytes.buffer as ArrayBuffer;
                const fileObj = new File([ab], entry.filename, { type: mimeFor(entry.ext) });

                const result = await extractText(fileObj);

                if (!result.text || result.text.trim().length < 20) {
                    updateEntry(entry.id, { status: 'skipped', error: 'Zu wenig Text extrahiert' });
                    continue;
                }

                updateEntry(entry.id, { status: 'embedding' });

                const language   = detectLanguage(result.text);
                const sourceHash = await hashText(result.text + entry.path);

                const stats_res = await invoke<{ chunk_count: number; embed_time_ms: number; write_time_ms: number }>(
                    'index_ingest_document',
                    {
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
                );

                updateEntry(entry.id, {
                    status:  'done',
                    chunks:  stats_res.chunk_count,
                    embedMs: stats_res.embed_time_ms,
                    writeMs: stats_res.write_time_ms,
                });

            } catch (err: any) {
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
            // If there's a filter query, filter client-side
            const q = contentsQuery.trim().toLowerCase();
            contents = q
                ? docs.filter(d =>
                    (d.title ?? '').toLowerCase().includes(q) ||
                    (d.filename ?? '').toLowerCase().includes(q) ||
                    (d.author ?? '').toLowerCase().includes(q))
                : docs;
        } catch {
            contents = [];
            indexStats = null;
        } finally {
            contentsLoading = false;
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
        return ({ pdf: 'application/pdf', docx: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document', txt: 'text/plain', md: 'text/markdown', epub: 'application/epub+zip' } as any)[ext] ?? 'application/octet-stream';
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

    <!-- ── Tab bar ──────────────────────────────────────────────────────────── -->
    <div class="tab-bar">
        <button class="tab" class:active={activeTab === 'ingest'}   onclick={() => activeTab = 'ingest'}>
            <UploadCloud size={14} /> Ingest
        </button>
        <button class="tab" class:active={activeTab === 'folders'}  onclick={() => activeTab = 'folders'}>
            <FolderOpen size={14} /> Ordner ({folders.length})
        </button>
        <button class="tab" class:active={activeTab === 'contents'} onclick={() => { activeTab = 'contents'; loadContents(); }}>
            <Database size={14} /> Index-Inhalt{#if indexStats !== null} ({indexStats.doc_count}){/if}
        </button>
    </div>

    <!-- ══════════════════ INGEST TAB ══════════════════ -->
    {#if activeTab === 'ingest'}
        <div class="toolbar">
            <div class="toolbar-actions">
                <button class="tb-btn" onclick={addFiles}><FileText size={14} /> Dateien</button>
                <button class="tb-btn" onclick={() => { activeTab = 'folders'; }}>
                    <FolderOpen size={14} /> Ordner verwalten
                </button>
                {#if stats.done > 0}
                    <button class="tb-btn ghost" onclick={clearDone}><Trash2 size={14} /> Fertige entfernen</button>
                {/if}
                {#if entries.length > 0 && !running}
                    <button class="tb-btn ghost danger" onclick={clearAll}><X size={14} /> Alle löschen</button>
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
                <p class="drop-hint">PDF, DOCX, TXT, MD, EPUB — oder "Dateien" klicken</p>
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
                {#if !running}
                    <button class="run-btn primary" onclick={startIngest}
                        disabled={stats.pending === 0}>
                        <Play size={15} /> Indexierung starten ({stats.pending})
                    </button>
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

    <!-- ══════════════════ FOLDERS TAB ══════════════════ -->
    {#if activeTab === 'folders'}
        <div class="folders-toolbar">
            <button class="tb-btn" onclick={addFolder}><FolderOpen size={14} /> Ordner hinzufügen</button>
            {#if folders.length > 0}
                <button class="tb-btn" onclick={scanAllFolders} disabled={!!scanningFolder}>
                    <RefreshCw size={14} class={scanningFolder ? 'spin' : ''} /> Alle neu scannen
                </button>
            {/if}
        </div>

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
                    <button class="run-btn primary" onclick={() => { activeTab = 'ingest'; }}>
                        <UploadCloud size={15} /> Zu Ingest wechseln ({entries.filter(e => e.status === 'pending').length} Dateien)
                    </button>
                </div>
            {/if}
        {/if}
    {/if}

    <!-- ══════════════════ CONTENTS TAB ══════════════════ -->
    {#if activeTab === 'contents'}
        <div class="contents-toolbar">
            <div class="query-input-wrap" style="flex:1">
                <Search size={14} style="color:#71717a;" />
                <input type="text" bind:value={contentsQuery}
                    onkeydown={e => e.key === 'Enter' && loadContents()}
                    placeholder="Filtern …" class="query-input" />
            </div>
            <button class="tb-btn" onclick={loadContents} disabled={contentsLoading}>
                {#if contentsLoading}<Loader2 size={13} class="spin" />{:else}<RefreshCw size={13} />{/if}
                Aktualisieren
            </button>
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
                <p class="hint-sub">{indexStats?.doc_count === 0 ? 'Indexiere Dokumente über den "Ingest"-Tab' : 'Filter anpassen oder leeren'}</p>
            </div>
        {:else}
            <!-- Selection toolbar (shown when items are selected) -->
            {#if selectedDocIds.size > 0}
                <div class="selection-bar">
                    <span class="sel-count">{selectedDocIds.size} ausgewählt</span>
                    <button class="tb-btn danger" onclick={deleteSelected} disabled={deletingIds.size > 0}>
                        <Trash2 size={13} /> {deletingIds.size > 0 ? 'Löschen …' : 'Aus Index löschen'}
                    </button>
                    <button class="tb-btn" onclick={() => selectedDocIds = new Set()}>Abwählen</button>
                </div>
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
                    <div class="contents-row" class:selected={isSelected} class:deleting={isDeleting}>
                        <input type="checkbox" class="row-check" checked={isSelected}
                            onchange={() => toggleSelect(doc.doc_id)} />
                        {#if doc.ext}
                            <div class="ext-badge ext-{doc.ext.toLowerCase()}">{doc.ext.toUpperCase()}</div>
                        {:else}
                            <div class="ext-badge">–</div>
                        {/if}
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
    .ext-pdf  { background: #7f1d1d33; color: #fca5a5; }
    .ext-docx { background: #1e3a5f33; color: #93c5fd; }
    .ext-md   { background: #14532d33; color: #86efac; }
    .ext-txt  { background: #44403c33; color: #d6d3d1; }
    .ext-epub { background: #4c1d9533; color: #c4b5fd; }

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

    :global(.spin) { animation: spin 1s linear infinite; display: inline-flex; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
