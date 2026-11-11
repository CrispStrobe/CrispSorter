<script lang="ts">
    // The one PDF editor.
    //
    // Layout: a resizable page panel on the left (thumbnails, selection,
    // drag-to-reorder), the current page on the right (zoomable, and the
    // surface every drawing tool works on), one compact icon toolbar above
    // both, and a contextual options bar for whichever tool is active.
    // What used to be four separate tabs — page editing, one-shot tools,
    // DOCX surgery and document conversion — is reachable from here: page
    // operations and annotation tools in the toolbar, document tools in the
    // Tools menu, conversions in the Export menu (the DOCX and Office/e-book
    // converters open in a side drawer because they act on other files).
    //
    // Editing goes through the `pdf_session_*` commands (P32.1a): every
    // change is an undoable op in one backend session, written out once on
    // save. Thumbnails are rendered from the *source* file wherever a page is
    // still bit-identical to it (`origins`), so reordering never re-renders;
    // stamped/annotated/cropped pages fall back to rasterising the edited
    // document, and their cached tiles are dropped when it changes.

    import { invoke } from '@tauri-apps/api/core';
    import { getCurrentWebview } from '@tauri-apps/api/webview';
    import { open as openDialog, save as saveDialog, ask } from '@tauri-apps/plugin-dialog';
    import { readFile } from '@tauri-apps/plugin-fs';
    import { onDestroy, onMount, untrack } from 'svelte';
    import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf';
    import { i18n } from '$lib/i18n.svelte';
    import { flog } from '$lib/log';
    import PdfDocTools from './pdf/PdfDocTools.svelte';
    import ConvertTools from './ConvertTools.svelte';
    import DocxTools from './DocxTools.svelte';
    import {
        type DocTool, type DocToolContext, type PdfAnnotationInput, type SessionState,
        errorMessage, moveOrder, normaliseRotation,
    } from './pdf/types';
    import {
        FileUp, Save, SaveAll, Undo2, Redo2, Trash2, RotateCw, RotateCcw, Crop,
        Type, FilePlus2, FileOutput, FileInput, Loader2, Check, X, ChevronLeft, ChevronRight,
        ChevronDown, EyeOff, AlertTriangle, PenLine, LayoutTemplate, Wrench, Download,
        Highlighter, PenTool, Square, StickyNote, MessageSquareText, Eraser, MousePointer2,
        ZoomIn, ZoomOut, Maximize, MoveHorizontal, Merge, Scissors, Hash, Stamp, Info,
        ClipboardList, Shrink, Lock, Unlock, Shield, BadgeCheck, Gauge, Archive, BookOpen,
        MessageSquare, FileText, Image, Printer, Share2, ExternalLink, FileType, FileType2,
        ChevronsLeft, ChevronsRight,
    } from 'lucide-svelte';

    pdfjsLib.GlobalWorkerOptions.workerSrc = '/pdf.worker.js';
    // Font faces must stay enabled: with `disableFontFace` pdf.js draws
    // glyph outlines itself, which needs bundled standard-font data for the
    // 14 base fonts — without it, text in non-embedded Helvetica/Times
    // (common in generated PDFs, and what free-text annotations use) is
    // simply not drawn. `useSystemFonts` maps those to the OS fonts.
    const PDFJS_OPTS = { useSystemFonts: true, disableFontFace: false } as const;

    let { initialPath = '' }: { initialPath?: string } = $props();
    // Only `initialPath` may trigger this. openSession reads `session` (via
    // closeSession) before its first await; tracking that re-ran the effect
    // whenever the session changed, closing and reopening it in a loop.
    $effect(() => { const path = initialPath; if (path) untrack(() => openSession(path)); });

    const t = $derived(i18n.t.pdfeditor);

    // ── Tools ───────────────────────────────────────────────────────────
    type StageTool = 'crop' | 'textbox' | 'redact' | 'overprint' | 'region';
    type AnnotTool = 'highlight' | 'ink' | 'rectangle' | 'note' | 'freetext' | 'eraser';
    type Tool = StageTool | AnnotTool | DocTool;
    const DOC_TOOLS: DocTool[] = ['insert', 'merge', 'split', 'numbers', 'watermark', 'metadata', 'encrypt', 'decrypt', 'sanitise', 'compress', 'form', 'kindle', 'annots', 'images'];
    const RECT_TOOLS: Tool[] = ['crop', 'redact', 'overprint', 'region', 'highlight', 'rectangle', 'freetext'];
    const isDocTool = (x: Tool | null): x is DocTool => !!x && (DOC_TOOLS as Tool[]).includes(x);
    const isRectTool = (x: Tool | null) => !!x && RECT_TOOLS.includes(x);

    // ── State ───────────────────────────────────────────────────────────
    let session = $state<SessionState | null>(null);
    /** Bumped on every session change, so renders key off identity, not op count. */
    let docVersion = $state(0);
    let busyCount = $state(0);
    const busy = $derived(busyCount > 0);
    let error = $state('');
    let notice = $state<{ msg: string; openPath?: string } | null>(null);
    let encryptedPath = $state('');
    let unlockPassword = $state('');

    let selected = $state<Set<number>>(new Set());
    let lastAnchor = $state<number | null>(null);
    let viewPage = $state(0);
    let activeTool = $state<Tool | null>(null);
    let menu = $state<'tools' | 'export' | null>(null);
    let drawer = $state<'convert' | 'docx' | null>(null);
    let drawerMounted = $state<{ convert: boolean; docx: boolean }>({ convert: false, docx: false });
    let drawerPath = $state<{ convert: string; docx: string }>({ convert: '', docx: '' });
    let dropHover = $state(false);
    let isEncrypted = $state(false);

    /** Current page index → rendered thumbnail data URL. */
    let thumbs = $state<Record<number, string>>({});
    let previewEpoch = $state(0);

    // Non-reactive caches — pdf.js documents are heavy and must not be
    // proxied by Svelte's reactivity.
    const sourceDocs = new Map<string, any>();
    let previewDoc: any = null;
    let previewSession: SessionState | null = null;
    let previewLoad: { session: SessionState; generation: number; promise: Promise<any> } | null = null;
    let previewGeneration = 0;
    const thumbCache = new Map<string, string>();
    let thumbRun = 0;
    const THUMB_RENDER_WIDTH = 240;

    // ── Layout (persisted per viewer) ───────────────────────────────────
    const SIDEBAR_KEY = 'crispsorter.pdfEditor.sidebarWidth';
    const THUMB_KEY = 'crispsorter.pdfEditor.thumbSize';
    function readNumber(key: string, fallback: number, min: number, max: number): number {
        try {
            const v = Number(localStorage.getItem(key));
            return Number.isFinite(v) && v >= min && v <= max ? v : fallback;
        } catch { return fallback; }
    }
    function writeNumber(key: string, v: number) {
        try { localStorage.setItem(key, String(Math.round(v))); } catch { /* private mode / blocked storage */ }
    }
    let sideWidth = $state(readNumber(SIDEBAR_KEY, 220, 120, 1200));
    let thumbSize = $state(readNumber(THUMB_KEY, 140, 70, 320));
    let mainEl = $state<HTMLDivElement | null>(null);

    // ── Logging / action plumbing ───────────────────────────────────────
    const fileName = (p: string) => p.split(/[/\\]/).pop() ?? p;

    /** Run one user action: busy state, error banner, app-log entries. */
    async function run<T>(label: string, fn: () => Promise<T>): Promise<T | undefined> {
        busyCount += 1;
        error = '';
        notice = null;
        const started = performance.now();
        try {
            const r = await fn();
            flog('info', `[pdf] ${label}: ok (${Math.round(performance.now() - started)} ms)`);
            return r;
        } catch (e) {
            const msg = errorMessage(e);
            error = `${label}: ${msg}`;
            flog('error', `[pdf] ${label} failed: ${msg}`);
            return undefined;
        } finally {
            busyCount -= 1;
        }
    }

    function fail(label: string, msg: string) {
        error = `${label}: ${msg}`;
        flog('warn', `[pdf] ${label}: ${msg}`);
    }

    function notify(msg: string, openPath?: string) { notice = { msg, openPath }; }

    // ── Session lifecycle ───────────────────────────────────────────────
    async function openFile() {
        const path = await openDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], multiple: false });
        if (path) await openSession(path as string);
    }

    async function confirmDiscard(): Promise<boolean> {
        if (!session?.modified) return true;
        try {
            return await ask(t.discard_changes, { title: t.unsaved, kind: 'warning' });
        } catch {
            return window.confirm(t.discard_changes);
        }
    }

    export async function openSession(path: string) {
        if (!(await confirmDiscard())) return;
        flog('info', `[pdf] opening ${path}`);
        await closeSession();
        encryptedPath = '';
        activeTool = null;
        const opened = await run(`${t.open} ${fileName(path)}`, () => invoke<SessionState>('pdf_session_open', { path }));
        if (!opened) {
            try {
                if (await invoke<boolean>('pdf_is_encrypted', { path })) {
                    encryptedPath = path;
                    flog('warn', `[pdf] ${path} is password-protected`);
                }
            } catch { /* the open error above already says what went wrong */ }
            return;
        }
        setSession(opened);
        selected = new Set();
        lastAnchor = null;
        viewPage = 0;
        flog('info', `[pdf] opened ${path}: ${opened.info.page_count} page(s)`);
        invoke<boolean>('pdf_is_encrypted', { path }).then((v) => (isEncrypted = !!v)).catch(() => (isEncrypted = false));
        void refreshThumbs();
    }

    async function unlockEncrypted() {
        if (!encryptedPath) return;
        if (!unlockPassword) { fail(t.decrypt, i18n.t.pdftools.enter_password); return; }
        const out = await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: encryptedPath.replace(/\.pdf$/i, '') + '-decrypted.pdf',
        }) as string | null;
        if (!out) return;
        const src = encryptedPath;
        const ok = await run(t.decrypt, async () => {
            await invoke('pdf_decrypt', { path: src, password: unlockPassword, outPath: out });
            return true;
        });
        if (ok) { unlockPassword = ''; await openSession(out); }
    }

    function setSession(next: SessionState) {
        session = next;
        docVersion += 1;
        const n = next.origins.length;
        if (viewPage >= n) viewPage = Math.max(0, n - 1);
        if ([...selected].some((i) => i >= n)) selected = new Set([...selected].filter((i) => i < n));
    }

    async function closeSession() {
        thumbRun += 1;
        previewGeneration += 1;
        previewLoad = null;
        previewSession = null;
        const old = session;
        session = null;
        thumbs = {};
        thumbCache.clear();
        for (const d of sourceDocs.values()) { try { d.destroy(); } catch { /* ignore */ } }
        sourceDocs.clear();
        if (previewDoc) { try { previewDoc.destroy(); } catch { /* ignore */ } previewDoc = null; }
        if (old) {
            try { await invoke('pdf_session_close', { id: old.id }); flog('info', `[pdf] closed ${old.source}`); } catch { /* already gone */ }
        }
    }

    /** Apply an edit op and re-sync state + thumbnails. */
    async function applyOp(op: Record<string, unknown>, label: string, note?: string): Promise<boolean> {
        const s = session;
        if (!s) { fail(label, t.no_session); return false; }
        const next = await run(label, () => invoke<SessionState>('pdf_session_apply', { id: s.id, op }));
        if (!next) return false;
        setSession(next);
        if (note) notify(note);
        void refreshThumbs();
        return true;
    }

    async function undo() {
        const s = session;
        if (!s?.can_undo) return;
        const next = await run(t.undo, () => invoke<SessionState>('pdf_session_undo', { id: s.id }));
        if (next) { setSession(next); void refreshThumbs(); }
    }

    async function redo() {
        const s = session;
        if (!s?.can_redo) return;
        const next = await run(t.redo, () => invoke<SessionState>('pdf_session_redo', { id: s.id }));
        if (next) { setSession(next); void refreshThumbs(); }
    }

    async function save(saveAs: boolean) {
        const s = session;
        if (!s) return;
        let outPath: string | null = null;
        if (saveAs) {
            outPath = await saveDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], defaultPath: s.source }) as string | null;
            if (!outPath) return;
        }
        const written = await run(saveAs ? t.save_as : t.save, () => invoke<string>('pdf_session_save', { id: s.id, outPath }));
        if (!written) return;
        flog('info', `[pdf] saved ${written}`);
        const next = await run(t.save, () => invoke<SessionState>('pdf_session_state', { id: s.id }));
        if (next) setSession(next);
        notify(`${t.saved} ${written}`);
    }

    async function exportTemp(): Promise<string> {
        if (!session) throw new Error(t.no_session);
        return await invoke<string>('pdf_session_export_temp', { id: session.id });
    }

    // ── Rendering (thumbnails + page view) ──────────────────────────────
    async function sourceDocFor(path: string, isCurrent: () => boolean = () => true) {
        if (!isCurrent()) return null;
        let doc = sourceDocs.get(path);
        if (!doc) {
            const bytes = await readFile(path);
            if (!isCurrent()) return null;
            doc = await pdfjsLib.getDocument({ data: new Uint8Array(bytes), ...PDFJS_OPTS }).promise;
            if (!isCurrent()) { try { await doc.destroy(); } catch { /* ignore */ } return null; }
            sourceDocs.set(path, doc);
        }
        return doc;
    }

    /** Rasterise the edited document — only needed for stamped/blank pages. */
    async function refreshPreviewDoc(isCurrent: () => boolean = () => true) {
        const requestedSession = session;
        const generation = previewGeneration;
        const current = () => session === requestedSession && generation === previewGeneration;
        if (!requestedSession || !isCurrent()) return null;
        if (previewDoc && previewSession === requestedSession) return previewDoc;

        // The view and thumbnail refresh can request the same edited snapshot
        // together. Share its load so neither replaces a document in use by the
        // other. A caller's cancellation must not cancel the shared owner.
        let pending = previewLoad;
        if (!pending || pending.session !== requestedSession || pending.generation !== generation) {
            const promise = (async () => {
                const tmp = await invoke<string>('pdf_session_export_temp', { id: requestedSession.id });
                if (!current()) return null;
                const bytes = await readFile(tmp);
                if (!current()) return null;
                const nextPreview = await pdfjsLib.getDocument({ data: new Uint8Array(bytes), ...PDFJS_OPTS }).promise;
                if (!current()) { try { await nextPreview.destroy(); } catch { /* ignore */ } return null; }
                if (previewDoc) { try { previewDoc.destroy(); } catch { /* ignore */ } }
                previewDoc = nextPreview;
                previewSession = requestedSession;
                previewEpoch += 1;
                // Drop prior edited thumbnails; source pages remain reusable.
                for (const key of [...thumbCache.keys()]) {
                    if (key.startsWith('edit:')) thumbCache.delete(key);
                }
                return nextPreview;
            })();
            pending = { session: requestedSession, generation, promise };
            previewLoad = pending;
        }
        try {
            const doc = await pending.promise;
            return current() && isCurrent() ? doc : null;
        } finally {
            if (previewLoad === pending) previewLoad = null;
        }
    }

    /**
     * Which pdf.js document holds page `i` of the edited document. Clean
     * pages come straight from their source file (plus the session's extra
     * rotation); stamped or generated pages need the edited preview.
     */
    async function docAndPageFor(i: number): Promise<{ doc: any; pageNo: number; rotation: number } | null> {
        if (!session) return null;
        const o = session.origins[i];
        if (!o) return null;
        if (o.source && o.index !== null && !o.dirty) {
            const doc = await sourceDocFor(o.source);
            return doc ? { doc, pageNo: o.index + 1, rotation: o.rotation } : null;
        }
        const preview = await refreshPreviewDoc();
        return preview ? { doc: preview, pageNo: i + 1, rotation: 0 } : null;
    }

    async function renderThumb(doc: any, pageNo: number, extraRotation: number): Promise<string> {
        const page = await doc.getPage(pageNo);
        const rotation = normaliseRotation(page.rotate + extraRotation);
        const base = page.getViewport({ scale: 1, rotation });
        const viewport = page.getViewport({ scale: THUMB_RENDER_WIDTH / base.width, rotation });
        const canvas = document.createElement('canvas');
        canvas.width = Math.max(1, Math.floor(viewport.width));
        canvas.height = Math.max(1, Math.floor(viewport.height));
        await page.render({ canvasContext: canvas.getContext('2d')!, viewport }).promise;
        return canvas.toDataURL('image/png');
    }

    async function refreshThumbs() {
        const requestedSession = session;
        if (!requestedSession) return;
        const epoch = ++thumbRun;
        const isCurrent = () => epoch === thumbRun && session === requestedSession;
        const origins = requestedSession.origins;
        const started = performance.now();

        // Pages that can't be served from a source file need the edited doc.
        const needsPreview = origins.some((o) => o.dirty || !o.source);
        let preview: any = null;
        if (needsPreview) {
            try { preview = await refreshPreviewDoc(isCurrent); } catch (e) {
                flog('error', `[pdf] rendering the edited document failed: ${errorMessage(e)}`);
            }
        }
        if (!isCurrent()) return;
        const editedEpoch = previewEpoch;

        const next: Record<number, string> = {};
        let rendered = 0;
        let failed = 0;
        // Show each tile as soon as it renders: large documents (hundreds of
        // pages) otherwise stay on spinners until the last page is done.
        for (let i = 0; i < origins.length; i++) {
            if (!isCurrent()) return;
            const o = origins[i];
            const fromSource = !!o.source && o.index !== null && !o.dirty;
            // Edited pages are keyed by the preview epoch, which moves on every
            // change to the edited document — a cropped or annotated page can
            // never be served from a tile rendered before the change.
            const key = fromSource ? `src:${o.source}#${o.index}#${o.rotation}` : `edit:${i}#${editedEpoch}`;
            const cached = thumbCache.get(key);
            if (cached) { next[i] = cached; thumbs[i] = cached; continue; }
            try {
                const doc = fromSource ? await sourceDocFor(o.source!, isCurrent) : preview;
                if (!isCurrent()) return;
                const url = doc ? await renderThumb(doc, fromSource ? o.index! + 1 : i + 1, fromSource ? o.rotation : 0) : '';
                if (!isCurrent()) return;
                if (url) { thumbCache.set(key, url); next[i] = url; thumbs[i] = url; rendered += 1; }
            } catch (e) {
                // A page that fails to rasterise still gets a tile.
                failed += 1;
                flog('error', `[pdf] thumbnail for page ${i + 1} failed: ${errorMessage(e)}`);
            }
        }
        if (!isCurrent()) return;
        thumbs = next;
        if (rendered > 0 || failed > 0) {
            flog('info', `[pdf] rendered ${rendered} thumbnail(s)${failed ? `, ${failed} failed` : ''} in ${Math.round(performance.now() - started)} ms`);
        }
    }

    // ── Page view (zoom) ────────────────────────────────────────────────
    let viewEl = $state<HTMLDivElement | null>(null);
    let pageCanvas = $state<HTMLCanvasElement | null>(null);
    let viewW = $state(800);
    let viewH = $state(600);
    let zoomMode = $state<'fit-page' | 'fit-width' | 'custom'>('fit-page');
    let zoom = $state(1);
    /** Effective CSS px per PDF point of the page on screen. */
    let viewScale = $state(1);
    let vpW = $state(0);
    let vpH = $state(0);
    /** Bumped whenever the on-screen viewport changes; overlays depend on it. */
    let vpVersion = $state(0);
    let viewRendering = $state(false);
    let curViewport: any = null;
    let renderTask: any = null;
    let renderSeq = 0;
    const MIN_ZOOM = 0.1;
    const MAX_ZOOM = 8;

    $effect(() => {
        const el = viewEl;
        if (!el) return;
        const ro = new ResizeObserver(() => { viewW = el.clientWidth; viewH = el.clientHeight; });
        ro.observe(el);
        viewW = el.clientWidth; viewH = el.clientHeight;
        return () => ro.disconnect();
    });

    $effect(() => {
        void docVersion; void viewPage; void zoomMode; void zoom; void viewW; void viewH;
        if (!session || !pageCanvas) return;
        untrack(() => { void renderView(); });
    });

    async function renderView() {
        const seq = ++renderSeq;
        const s = session;
        const canvas = pageCanvas;
        if (!s || !canvas) return;
        const idx = Math.min(viewPage, s.origins.length - 1);
        viewRendering = true;
        try {
            const target = await docAndPageFor(idx);
            if (!target || seq !== renderSeq) return;
            const page = await target.doc.getPage(target.pageNo);
            if (seq !== renderSeq) return;
            const rotation = normaliseRotation(page.rotate + target.rotation);
            const base = page.getViewport({ scale: 1, rotation });
            const availW = Math.max(80, viewW - 40);
            const availH = Math.max(80, viewH - 84);
            let scale = zoomMode === 'fit-page'
                ? Math.min(availW / base.width, availH / base.height)
                : zoomMode === 'fit-width' ? availW / base.width : zoom;
            scale = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, scale));
            const vp = page.getViewport({ scale, rotation });
            const dpr = window.devicePixelRatio || 1;
            if (renderTask) {
                const prev = renderTask;
                renderTask = null;
                try { prev.cancel(); await prev.promise; } catch { /* cancelled */ }
                if (seq !== renderSeq) return;
            }
            canvas.width = Math.max(1, Math.floor(vp.width * dpr));
            canvas.height = Math.max(1, Math.floor(vp.height * dpr));
            const task = page.render({
                canvasContext: canvas.getContext('2d')!,
                viewport: vp,
                transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : undefined,
            });
            renderTask = task;
            await task.promise;
            if (renderTask === task) renderTask = null;
            if (seq !== renderSeq) return;
            curViewport = vp;
            viewScale = scale;
            vpW = vp.width;
            vpH = vp.height;
            vpVersion += 1;
            flog('debug', `[pdf] rendered page ${idx + 1} at ${Math.round(scale * 100)} %`);
        } catch (e: any) {
            if (e?.name === 'RenderingCancelledException') return;
            const msg = errorMessage(e);
            error = `${t.page} ${idx + 1}: ${msg}`;
            flog('error', `[pdf] rendering page ${idx + 1} failed: ${msg}`);
        } finally {
            if (seq === renderSeq) viewRendering = false;
        }
    }

    function setZoom(z: number) {
        zoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, z));
        zoomMode = 'custom';
    }
    const zoomIn = () => setZoom(viewScale * 1.25);
    const zoomOut = () => setZoom(viewScale / 1.25);
    const fitPage = () => { zoomMode = 'fit-page'; };
    const fitWidth = () => { zoomMode = 'fit-width'; };

    // Ctrl/Cmd + wheel zooms; the listener must be non-passive to cancel the
    // browser's own page zoom.
    $effect(() => {
        const el = viewEl;
        if (!el) return;
        const onWheel = (ev: WheelEvent) => {
            if (!(ev.ctrlKey || ev.metaKey)) return;
            ev.preventDefault();
            setZoom(viewScale * (ev.deltaY < 0 ? 1.1 : 1 / 1.1));
        };
        el.addEventListener('wheel', onWheel, { passive: false });
        return () => el.removeEventListener('wheel', onWheel);
    });

    function goToPage(i: number) {
        if (!session) return;
        viewPage = Math.max(0, Math.min(session.origins.length - 1, i));
        resetPending();
        document.querySelector(`.pe-tile[data-index="${viewPage}"]`)?.scrollIntoView({ block: 'nearest' });
    }

    /** PNG bytes of one page of the edited document at `dpi`. */
    async function renderPagePng(index: number, dpi: number): Promise<Uint8Array> {
        const target = await docAndPageFor(index);
        if (!target) throw new Error(`page ${index + 1} unavailable`);
        const page = await target.doc.getPage(target.pageNo);
        const vp = page.getViewport({ scale: dpi / 72, rotation: normaliseRotation(page.rotate + target.rotation) });
        const canvas = document.createElement('canvas');
        canvas.width = Math.max(1, Math.floor(vp.width));
        canvas.height = Math.max(1, Math.floor(vp.height));
        const ctx = canvas.getContext('2d')!;
        ctx.fillStyle = '#fff';
        ctx.fillRect(0, 0, canvas.width, canvas.height);
        await page.render({ canvasContext: ctx, viewport: vp }).promise;
        const blob: Blob = await new Promise((res, rej) => canvas.toBlob((b) => (b ? res(b) : rej(new Error('PNG encoding failed'))), 'image/png'));
        return new Uint8Array(await blob.arrayBuffer());
    }

    // ── Coordinates ─────────────────────────────────────────────────────
    // The page is shown with its rotation applied; pdf.js' viewport maps
    // between screen pixels and unrotated PDF user space (CropBox offsets
    // included), which is where crop boxes, annotations and text live.
    type Box = { x: number; y: number; w: number; h: number };

    function toPdfPoint(x: number, y: number): [number, number] {
        return curViewport ? curViewport.convertToPdfPoint(x, y) : [x, y];
    }
    function toPdfRect(b: Box): Box {
        const [x1, y1] = toPdfPoint(b.x, b.y);
        const [x2, y2] = toPdfPoint(b.x + b.w, b.y + b.h);
        return { x: Math.min(x1, x2), y: Math.min(y1, y2), w: Math.abs(x2 - x1), h: Math.abs(y2 - y1) };
    }
    function toViewRect(r: Box): Box | null {
        void vpVersion;
        if (!curViewport) return null;
        const [a, b, c, d] = curViewport.convertToViewportRectangle([r.x, r.y, r.x + r.w, r.y + r.h]);
        return { x: Math.min(a, c), y: Math.min(b, d), w: Math.abs(c - a), h: Math.abs(d - b) };
    }

    // ── Drawing on the page ─────────────────────────────────────────────
    let overlayEl = $state<HTMLDivElement | null>(null);
    /** Live rectangle in CSS pixels (crop, redact, region, highlight, …). */
    let dragBox = $state<Box | null>(null);
    let dragStart: { x: number; y: number } | null = null;
    let dragging = $state(false);
    let inkPoints = $state<{ x: number; y: number }[]>([]);
    /** Click point (PDF points) for text box / sticky note placement. */
    let anchor = $state<[number, number] | null>(null);
    /** Committed-but-pending rectangle (PDF points): overprint, region, free text. */
    let pendingRect = $state<Box | null>(null);

    function resetPending() {
        dragBox = null; dragStart = null; dragging = false; inkPoints = []; anchor = null; pendingRect = null;
    }

    function toViewPoint(p: [number, number]): { x: number; y: number } | null {
        void vpVersion;
        if (!curViewport) return null;
        const [x, y] = curViewport.convertToViewportPoint(p[0], p[1]);
        return { x, y };
    }

    /** The rectangle to draw for the active tool, in CSS pixels. */
    function shownBox(): Box | null {
        if (dragging && dragBox) return dragBox;
        if (activeTool === 'crop') return cropW > 0 && cropH > 0 ? toViewRect({ x: cropX, y: cropY, w: cropW, h: cropH }) : null;
        return pendingRect ? toViewRect(pendingRect) : null;
    }

    function local(ev: PointerEvent): { x: number; y: number } {
        const rect = overlayEl!.getBoundingClientRect();
        return {
            x: Math.max(0, Math.min(ev.clientX - rect.left, rect.width)),
            y: Math.max(0, Math.min(ev.clientY - rect.top, rect.height)),
        };
    }

    function onOverlayDown(ev: PointerEvent) {
        if (!activeTool || !overlayEl || ev.button !== 0) return;
        const p = local(ev);
        if (isRectTool(activeTool)) {
            overlayEl.setPointerCapture?.(ev.pointerId);
            dragging = true;
            dragStart = p;
            dragBox = { x: p.x, y: p.y, w: 0, h: 0 };
        } else if (activeTool === 'ink') {
            overlayEl.setPointerCapture?.(ev.pointerId);
            dragging = true;
            inkPoints = [p];
        } else if (activeTool === 'textbox') {
            const [x, y] = toPdfPoint(p.x, p.y);
            anchor = [x, y];
            boxX = Math.round(x);
            boxY = Math.round(y);
        } else if (activeTool === 'note') {
            anchor = toPdfPoint(p.x, p.y);
        } else if (activeTool === 'eraser') {
            const [x, y] = toPdfPoint(p.x, p.y);
            void applyOp({ kind: 'erase_annotation', page: viewPage, x, y }, `${t.ann_eraser.split('—')[0].trim()} (${t.page} ${viewPage + 1})`, t.ann_erased);
        }
    }

    function onOverlayMove(ev: PointerEvent) {
        if (!dragging) return;
        const p = local(ev);
        if (activeTool === 'ink') {
            const last = inkPoints[inkPoints.length - 1];
            if (!last || Math.hypot(p.x - last.x, p.y - last.y) >= 1.5) inkPoints = [...inkPoints, p];
            return;
        }
        if (!dragStart) return;
        dragBox = {
            x: Math.min(dragStart.x, p.x), y: Math.min(dragStart.y, p.y),
            w: Math.abs(p.x - dragStart.x), h: Math.abs(p.y - dragStart.y),
        };
    }

    function onOverlayUp() {
        if (!dragging) return;
        dragging = false;
        dragStart = null;
        if (activeTool === 'ink') { void commitInk(); return; }
        const box = dragBox;
        if (!box || box.w < 4 || box.h < 4) { dragBox = null; return; }
        switch (activeTool) {
            case 'crop': {
                const r = toPdfRect(box);
                cropX = Math.round(r.x); cropY = Math.round(r.y); cropW = Math.round(r.w); cropH = Math.round(r.h);
                dragBox = null;
                break;
            }
            case 'redact':
                redactRects = [...redactRects, { page: viewPage, ...toPdfRect(box) }];
                dragBox = null;
                break;
            case 'highlight':
            case 'rectangle':
                void addAnnotation(activeTool, toPdfRect(box));
                dragBox = null;
                break;
            default:
                // overprint / region / freetext keep the box until committed,
                // in PDF points so a zoom change does not move it.
                pendingRect = toPdfRect(box);
                dragBox = null;
                break;
        }
    }

    // ── Annotations ─────────────────────────────────────────────────────
    const SWATCHES = ['#facc15', '#22c55e', '#3b82f6', '#ef4444', '#a855f7', '#111111'];
    let annotColors = $state<Record<string, string>>({
        highlight: '#facc15', ink: '#ef4444', rectangle: '#ef4444', note: '#facc15', freetext: '#1d4ed8',
    });
    let annotWidth = $state(2);
    let annotFontSize = $state(12);
    let annotText = $state('');

    const annotLabel = (k: string) => ({
        highlight: t.ann_highlight, ink: t.ann_ink, rectangle: t.ann_rect, note: t.ann_note, freetext: t.ann_freetext,
    } as Record<string, string>)[k] ?? k;

    async function addAnnotation(kind: PdfAnnotationInput['ann_type'], r: Box, extra: Partial<PdfAnnotationInput> = {}) {
        const a: PdfAnnotationInput = {
            page: viewPage, x: r.x, y: r.y, w: r.w, h: r.h,
            ann_type: kind, text: '', color: annotColors[kind] ?? '#facc15', author: null,
            quads: [], ink: [], stroke_width: kind === 'ink' || kind === 'rectangle' ? annotWidth : null,
            font_size: kind === 'freetext' ? annotFontSize : null,
            ...extra,
        };
        return await applyOp({ kind: 'annotate', annotations: [a] }, `${annotLabel(kind)} (${t.page} ${viewPage + 1})`, t.ann_added);
    }

    async function commitInk() {
        const pts = inkPoints;
        inkPoints = [];
        if (pts.length === 0) return;
        const flat: number[] = [];
        let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
        for (const p of pts) {
            const [x, y] = toPdfPoint(p.x, p.y);
            flat.push(x, y);
            minX = Math.min(minX, x); minY = Math.min(minY, y); maxX = Math.max(maxX, x); maxY = Math.max(maxY, y);
        }
        const pad = annotWidth;
        await addAnnotation('ink', { x: minX - pad, y: minY - pad, w: maxX - minX + 2 * pad, h: maxY - minY + 2 * pad }, { ink: [flat] });
    }

    async function commitTextAnnotation() {
        if (!annotText.trim()) { fail(annotLabel(activeTool ?? 'note'), t.text_required); return; }
        if (activeTool === 'note') {
            if (!anchor) { fail(t.ann_note, t.ann_place_first); return; }
            const [x, y] = anchor;
            // A 20 pt note whose top-left corner sits on the click.
            const ok = await addAnnotation('note', { x, y: y - 20, w: 20, h: 20 }, { text: annotText });
            if (ok) { annotText = ''; anchor = null; }
        } else if (activeTool === 'freetext') {
            if (!pendingRect) { fail(t.ann_freetext, t.ann_place_first); return; }
            const ok = await addAnnotation('freetext', pendingRect, { text: annotText });
            if (ok) { annotText = ''; pendingRect = null; }
        }
    }

    // ── Crop ────────────────────────────────────────────────────────────
    let cropAllPages = $state(false);
    let cropX = $state(0);
    let cropY = $state(0);
    let cropW = $state(0);
    let cropH = $state(0);

    function initCropFromView() {
        const vb = curViewport?.viewBox as number[] | undefined;
        if (vb) {
            cropX = Math.round(vb[0]); cropY = Math.round(vb[1]);
            cropW = Math.round(vb[2] - vb[0]); cropH = Math.round(vb[3] - vb[1]);
        } else if (session) {
            const p = session.info.pages[viewPage];
            cropX = 0; cropY = 0; cropW = Math.round(p?.width_pt ?? 612); cropH = Math.round(p?.height_pt ?? 792);
        }
        dragBox = null;
    }

    const cropPages = () => {
        if (!session) return [];
        if (cropAllPages) return session.origins.map((_, i) => i);
        return selected.size > 0 ? selectedSorted() : [viewPage];
    };

    async function doCrop() {
        if (!session) return;
        if (cropW < 1 || cropH < 1) { fail(t.crop, t.overprint_drag_first); return; }
        const pages = cropPages();
        const ok = await applyOp({ kind: 'crop', pages, x: cropX, y: cropY, w: cropW, h: cropH },
            `${t.crop} ${pages.map((p) => p + 1).join(',')}`, t.cropped);
        if (ok) { dragBox = null; activeTool = null; }
    }

    // ── Text box ────────────────────────────────────────────────────────
    let boxText = $state('');
    let boxSize = $state(12);
    let boxX = $state(72);
    let boxY = $state(72);

    async function doTextBox() {
        if (!boxText.trim()) { fail(t.text_box, t.text_required); return; }
        const ok = await applyOp({
            kind: 'text_box',
            config: { page: viewPage, x: boxX, y: boxY, text: boxText, font_size: boxSize, color: [0, 0, 0], line_height: null },
        }, `${t.text_box} (${t.page} ${viewPage + 1})`, t.text_added);
        if (ok) { anchor = null; boxText = ''; }
    }

    // ── Overprint (cover and retype) ────────────────────────────────────
    let overprintText = $state('');
    let overprintSize = $state(11);

    async function doOverprint() {
        if (!session || !pendingRect) { fail(t.overprint, t.overprint_drag_first); return; }
        const out = await saveDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], defaultPath: 'overprinted.pdf' }) as string | null;
        if (!out) return;
        const rect = pendingRect;
        const ok = await run(t.overprint, async () => {
            const tmp = await exportTemp();
            await invoke<number>('pdf_overprint', {
                path: tmp,
                specs: [{ page: viewPage, ...rect, text: overprintText, font_size: overprintSize, color: [0, 0, 0], background: [1, 1, 1] }],
                outPath: out,
            });
            return true;
        });
        if (ok) { pendingRect = null; notify(`${t.overprinted} ${out}`, out); }
    }

    // ── Text regions (P32.9) ────────────────────────────────────────────
    interface LayoutReport { lines: number; lines_dropped: number; overflow: boolean; unsupported_chars: string[]; }
    const REGION_FONTS = [
        'helvetica', 'helvetica-bold', 'helvetica-oblique', 'helvetica-bold-oblique',
        'times-roman', 'times-bold', 'times-italic', 'times-bold-italic', 'courier', 'courier-bold',
    ];
    let regionText = $state('');
    let regionFont = $state('helvetica');
    let regionSize = $state(11);
    let regionAlign = $state<'left' | 'center' | 'right' | 'justify'>('left');
    let regionVAlign = $state<'top' | 'middle' | 'bottom'>('top');
    let regionLineHeight = $state(1.2);
    let regionColor = $state('#000000');
    let regionBorder = $state(false);
    let regionReport = $state<LayoutReport | null>(null);

    function hexToRgb(hex: string): [number, number, number] {
        const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim());
        return m ? [parseInt(m[1], 16) / 255, parseInt(m[2], 16) / 255, parseInt(m[3], 16) / 255] : [0, 0, 0];
    }

    function buildRegion(box: Box) {
        return {
            page: viewPage, ...box, text: regionText, font: regionFont, font_size: regionSize,
            color: hexToRgb(regionColor), align: regionAlign, vertical_align: regionVAlign,
            line_height: regionLineHeight, padding: null, draw_border: regionBorder,
        };
    }

    // Live fit preview. Measuring is pure layout — no file is touched.
    $effect(() => {
        if (activeTool !== 'region' || !pendingRect) { regionReport = null; return; }
        const region = buildRegion(pendingRect);
        let stale = false;
        invoke<LayoutReport>('pdf_measure_text_region', { region })
            .then((r) => { if (!stale) regionReport = r; })
            .catch(() => { /* the commit path reports layout errors */ });
        return () => { stale = true; };
    });

    async function doDrawRegion() {
        if (!session || !pendingRect) { fail(t.region, t.region_drag_first); return; }
        if (!regionText.trim()) { fail(t.region, t.text_required); return; }
        const out = await saveDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], defaultPath: 'text-region.pdf' }) as string | null;
        if (!out) return;
        const region = buildRegion(pendingRect);
        const reports = await run(t.region, async () => {
            const tmp = await exportTemp();
            return await invoke<LayoutReport[]>('pdf_draw_text_regions', { path: tmp, regions: [region], outPath: out });
        });
        if (reports) { regionReport = reports[0] ?? null; notify(`${t.region_written} ${out}`, out); }
    }

    // ── Redaction ───────────────────────────────────────────────────────
    interface RedactSpec { page: number; x: number; y: number; w: number; h: number }
    interface RedactReport { glyphs_removed: number; runs_dropped: number; pages_changed: number; warnings: string[]; }
    let redactRects = $state<RedactSpec[]>([]);
    let redactReport = $state<RedactReport | null>(null);

    async function doRedact() {
        if (!session || redactRects.length === 0) return;
        const out = await saveDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], defaultPath: 'redacted.pdf' }) as string | null;
        if (!out) return;
        redactReport = null;
        const regions = redactRects;
        const r = await run(t.redact, async () => {
            const tmp = await exportTemp();
            return await invoke<RedactReport>('pdf_redact_hard', { path: tmp, regions, outPath: out });
        });
        if (r) { redactReport = r; notify(`${t.redacted} ${out}`, out); }
    }

    // ── Selection ───────────────────────────────────────────────────────
    function clickPage(i: number, ev: MouseEvent | KeyboardEvent) {
        if (suppressClick) { suppressClick = false; return; }
        const s = new Set(selected);
        if (ev.shiftKey && lastAnchor !== null) {
            const [a, b] = lastAnchor < i ? [lastAnchor, i] : [i, lastAnchor];
            for (let k = a; k <= b; k++) s.add(k);
        } else if (ev.metaKey || ev.ctrlKey) {
            if (s.has(i)) s.delete(i); else s.add(i);
            lastAnchor = i;
        } else {
            s.clear();
            s.add(i);
            lastAnchor = i;
        }
        selected = s;
        goToPage(i);
    }

    function selectAll() {
        if (!session) return;
        selected = new Set(session.origins.map((_, i) => i));
    }
    function selectNone() {
        selected = new Set();
        lastAnchor = null;
        // WebKit does not move focus to a clicked <button>, so the last
        // clicked tile kept its blue focus ring and still *looked* selected
        // after "Select none". Drop that focus explicitly.
        const active = document.activeElement as HTMLElement | null;
        if (active?.classList.contains('pe-tile')) active.blur();
        flog('debug', '[pdf] selection cleared');
    }
    const selectedSorted = () => [...selected].sort((a, b) => a - b);
    /** Selected pages, or the page on screen when nothing is selected. */
    const targetPages = () => (selected.size > 0 ? selectedSorted() : [viewPage]);

    // ── Page operations ─────────────────────────────────────────────────
    async function removePages(pages: number[]) {
        if (!session || pages.length === 0) return;
        if (pages.length >= session.origins.length) { fail(t.remove, 'Cannot remove all pages'); return; }
        const ok = await applyOp({ kind: 'remove_pages', pages }, `${t.remove} ${pages.map((p) => p + 1).join(',')}`, t.removed);
        if (ok) selectNone();
    }

    async function rotatePages(pages: number[], degrees: number) {
        if (pages.length === 0) return;
        // The backend's domain is 0/90/180/270; counter-clockwise is 270.
        const d = normaliseRotation(degrees);
        await applyOp({ kind: 'rotate', pages, degrees: d },
            `${degrees < 0 ? t.rotate_left : t.rotate_right} ${pages.map((p) => p + 1).join(',')}`, t.rotated);
    }

    /** Move `pages` so they land before index `to` of the current order. */
    async function movePages(pages: number[], to: number) {
        if (!session) return;
        const order = moveOrder(session.origins.length, pages, to);
        if (!order) return;
        const moving = [...new Set(pages)].sort((a, b) => a - b);
        const ok = await applyOp({ kind: 'reorder', order }, `${t.reordered}: ${moving.map((p) => p + 1).join(',')} → ${to + 1}`, t.moved);
        if (!ok) return;
        const newPos = moving.map((p) => order.indexOf(p));
        if (selected.size > 0) selected = new Set(newPos);
        viewPage = order.indexOf(viewPage);
    }

    function moveBack(pages: number[]) {
        if (pages.length === 0) return;
        const first = Math.min(...pages);
        if (first > 0) void movePages(pages, first - 1);
    }
    function moveForward(pages: number[]) {
        if (!session || pages.length === 0) return;
        const last = Math.max(...pages);
        if (last < session.origins.length - 1) void movePages(pages, last + 2);
    }

    async function extractSelected() {
        const s = session;
        if (!s) return;
        const pages = targetPages();
        const out = await saveDialog({ filters: [{ name: 'PDF', extensions: ['pdf'] }], defaultPath: 'extracted.pdf' }) as string | null;
        if (!out) return;
        const ok = await run(`${t.extract} ${pages.map((p) => p + 1).join(',')}`, async () => {
            // From the *edited* document, so pending edits are in the extract.
            const tmp = await exportTemp();
            await invoke('pdf_extract_pages', { path: tmp, pageIndices: pages, outPath: out });
            return true;
        });
        if (ok) notify(`${t.extracted} ${out}`, out);
    }

    async function doInsertBlank() {
        if (!session) return;
        const at = selected.size > 0 ? selectedSorted()[selected.size - 1] + 1 : viewPage + 1;
        const ref = session.info.pages[Math.min(Math.max(0, at - 1), session.info.pages.length - 1)];
        const ok = await applyOp({ kind: 'insert_blank', position: at, width: ref?.width_pt ?? 612, height: ref?.height_pt ?? 792 },
            `${t.insert_blank} @${at + 1}`, t.blank_inserted);
        if (ok) goToPage(at);
    }

    // ── Pointer-driven reorder of thumbnails ────────────────────────────
    // HTML5 drag-and-drop is unreliable in the macOS WebView, so tiles are
    // dragged with pointer events and the drop slot is hit-tested by hand.
    let dragFrom = $state<number | null>(null);
    let dropAt = $state<{ index: number; after: boolean } | null>(null);
    let dragPos = $state<{ x: number; y: number } | null>(null);
    let tileDrag: { from: number; x: number; y: number; id: number; active: boolean } | null = null;
    let suppressClick = false;
    let thumbsEl = $state<HTMLDivElement | null>(null);

    function onTilePointerDown(i: number, ev: PointerEvent) {
        if (ev.button !== 0 || (ev.target as HTMLElement).closest('button')) return;
        tileDrag = { from: i, x: ev.clientX, y: ev.clientY, id: ev.pointerId, active: false };
        window.addEventListener('pointermove', onTilePointerMove);
        window.addEventListener('pointerup', onTilePointerUp);
        window.addEventListener('pointercancel', onTilePointerCancel);
    }

    function hitTile(x: number, y: number): { index: number; after: boolean } | null {
        const el = (document.elementFromPoint(x, y) as HTMLElement | null)?.closest('.pe-tile') as HTMLElement | null;
        if (!el || el.dataset.index === undefined) return null;
        const index = Number(el.dataset.index);
        const r = el.getBoundingClientRect();
        // One column: the vertical half decides; several: the horizontal one.
        const singleColumn = !!thumbsEl && r.width * 2 > thumbsEl.clientWidth;
        const after = singleColumn ? y > r.top + r.height / 2 : x > r.left + r.width / 2;
        return { index, after };
    }

    function onTilePointerMove(ev: PointerEvent) {
        const d = tileDrag;
        if (!d || ev.pointerId !== d.id) return;
        if (!d.active) {
            if (Math.hypot(ev.clientX - d.x, ev.clientY - d.y) < 6) return;
            d.active = true;
            dragFrom = d.from;
        }
        ev.preventDefault();
        dragPos = { x: ev.clientX, y: ev.clientY };
        dropAt = hitTile(ev.clientX, ev.clientY);
        // Auto-scroll near the edges of the page panel.
        if (thumbsEl) {
            const r = thumbsEl.getBoundingClientRect();
            if (ev.clientY < r.top + 30) thumbsEl.scrollTop -= 12;
            else if (ev.clientY > r.bottom - 30) thumbsEl.scrollTop += 12;
        }
    }

    function endTileDrag() {
        window.removeEventListener('pointermove', onTilePointerMove);
        window.removeEventListener('pointerup', onTilePointerUp);
        window.removeEventListener('pointercancel', onTilePointerCancel);
        tileDrag = null;
        dragFrom = null;
        dropAt = null;
        dragPos = null;
    }

    function onTilePointerUp(ev: PointerEvent) {
        const d = tileDrag;
        const target = dropAt;
        endTileDrag();
        if (!d || !d.active) return;
        suppressClick = true;
        setTimeout(() => { suppressClick = false; }, 0);
        const hit = target ?? hitTile(ev.clientX, ev.clientY);
        if (!hit) return;
        // Moving a selected page drags the whole selection along.
        const moving = selected.has(d.from) ? selectedSorted() : [d.from];
        void movePages(moving, hit.index + (hit.after ? 1 : 0));
    }

    function onTilePointerCancel() { endTileDrag(); }

    // ── Resizable page panel ────────────────────────────────────────────
    let resizing = $state(false);
    function onDividerDown(ev: PointerEvent) {
        if (ev.button !== 0) return;
        (ev.currentTarget as HTMLElement).setPointerCapture?.(ev.pointerId);
        resizing = true;
    }
    function maxSideWidth() { return Math.max(160, (mainEl?.clientWidth ?? 1200) * 0.7); }
    function onDividerMove(ev: PointerEvent) {
        if (!resizing || !mainEl) return;
        const left = mainEl.getBoundingClientRect().left;
        sideWidth = Math.round(Math.max(120, Math.min(maxSideWidth(), ev.clientX - left)));
    }
    function onDividerUp() {
        if (!resizing) return;
        resizing = false;
        writeNumber(SIDEBAR_KEY, sideWidth);
    }
    function onDividerKey(ev: KeyboardEvent) {
        if (ev.key !== 'ArrowLeft' && ev.key !== 'ArrowRight') return;
        ev.preventDefault();
        sideWidth = Math.round(Math.max(120, Math.min(maxSideWidth(), sideWidth + (ev.key === 'ArrowLeft' ? -16 : 16))));
        writeNumber(SIDEBAR_KEY, sideWidth);
    }
    function onThumbSize(v: number) {
        thumbSize = v;
        writeNumber(THUMB_KEY, v);
    }

    // ── Tool switching ──────────────────────────────────────────────────
    function setTool(next: Tool | null) {
        const target = activeTool === next ? null : next;
        resetPending();
        regionReport = null;
        menu = null;
        if (target && !session) { fail(t.open, t.no_session); return; }
        activeTool = target;
        if (target) flog('debug', `[pdf] tool: ${target}`);
        if (target === 'crop') {
            // Crop always starts with the whole page in view, so the box
            // can be drawn anywhere on it.
            zoomMode = 'fit-page';
            initCropFromView();
        }
        if (target === 'redact') redactReport = null;
    }

    function toolHint(x: Tool | null): string {
        switch (x) {
            case 'crop': return t.drag_crop_hint;
            case 'redact': return t.drag_redact_hint;
            case 'overprint': return t.drag_overprint_hint;
            case 'region': return t.drag_region_hint;
            case 'textbox': return t.click_text_hint;
            case 'highlight': return t.ann_hint_highlight;
            case 'ink': return t.ann_hint_ink;
            case 'rectangle': return t.ann_hint_rect;
            case 'note': return t.ann_hint_note;
            case 'freetext': return t.ann_hint_freetext;
            case 'eraser': return t.ann_hint_eraser;
            default: return '';
        }
    }

    // ── One-click document tools & exports ──────────────────────────────
    async function askOut(name: string, ext = 'pdf', label = 'PDF'): Promise<string | null> {
        const base = session?.source.replace(/\.pdf$/i, '') ?? 'document';
        return await saveDialog({ filters: [{ name: label, extensions: [ext] }], defaultPath: `${base}-${name}.${ext}` }) as string | null;
    }

    async function doSignatures() {
        menu = null;
        const s = session;
        if (!s) return;
        const sigs = await run(t.signatures, () => invoke<any[]>('pdf_detect_signatures', { path: s.source }));
        if (!sigs) return;
        notify(sigs.length === 0
            ? t.no_signatures
            : t.signatures_found.replace('{n}', String(sigs.length)).replace('{names}', sigs.map((x: any) => x.name || x.filter || '?').join(', ')));
    }

    async function doFileTool(label: string, suffix: string, command: string, useSource = false) {
        menu = null;
        const s = session;
        if (!s) return;
        const out = await askOut(suffix);
        if (!out) return;
        const r = await run(label, async () => {
            const path = useSource ? s.source : await exportTemp();
            return await invoke<unknown>(command, { path, outPath: out });
        });
        if (r !== undefined) {
            notify(typeof r === 'number' && command === 'pdf_repair'
                ? i18n.t.pdftools.repaired.replace('{n}', String(r)).replace('{path}', out)
                : `${t.written_to} ${out}`, out);
        }
    }

    async function exportText(format: 'docx' | 'markdown' | 'text') {
        menu = null;
        if (!session) return;
        const ext = format === 'docx' ? 'docx' : format === 'markdown' ? 'md' : 'txt';
        const out = await askOut('export', ext, ext.toUpperCase());
        if (!out) return;
        const label = format === 'docx' ? t.export_docx : format === 'markdown' ? t.export_md : t.export_txt;
        const n = await run(label.replace('…', ''), async () => {
            const tmp = await exportTemp();
            return format === 'docx'
                ? await invoke<number>('pdf_export_docx', { path: tmp, outPath: out })
                : await invoke<number>('pdf_export_text', { path: tmp, format, outPath: out });
        });
        if (n !== undefined) notify(t.exported.replace('{n}', String(n)).replace('{path}', out));
    }

    async function exportAnnots(format: 'markdown' | 'csv' | 'json') {
        menu = null;
        if (!session) return;
        const ext = format === 'markdown' ? 'md' : format;
        const out = await askOut('annotations', ext, format.toUpperCase());
        if (!out) return;
        const n = await run(`${t.export_annots} ${format}`, async () => {
            const tmp = await exportTemp();
            return await invoke<number>('pdf_export_annotations', { path: tmp, format, outPath: out });
        });
        if (n !== undefined) notify(t.annots_exported.replace('{n}', String(n)).replace('{path}', out));
    }

    // Print / share act on the *edited* document via a temp copy.
    interface PlatformCapabilities { system_share_sheet: boolean; direct_print: boolean; platform: string; }
    interface ShareResult { kind: string; note: string | null }
    let platformCaps = $state<PlatformCapabilities | null>(null);
    onMount(() => {
        invoke<PlatformCapabilities>('platform_capabilities').then((c) => (platformCaps = c)).catch(() => {});
    });

    async function doPrint() {
        menu = null;
        if (!session) return;
        busyCount += 1;
        error = '';
        try {
            const tmp = await exportTemp();
            const msg = await invoke<string>('platform_print', { path: tmp });
            flog('info', `[pdf] ${t.print}: ok`);
            notify(msg?.trim() ? msg : t.sent_to_printer);
        } catch (e) {
            // No printer configured is the common failure; point at the viewer.
            error = `${t.print}: ${errorMessage(e)} — ${t.print_fallback_hint}`;
            flog('error', `[pdf] ${t.print} failed: ${errorMessage(e)}`);
        } finally { busyCount -= 1; }
    }

    async function doOpenExternal() {
        menu = null;
        if (!session) return;
        await run(t.open_external, async () => { await invoke('platform_open_external', { path: await exportTemp() }); });
    }

    async function doShare() {
        menu = null;
        if (!session) return;
        const res = await run(t.share, async () => await invoke<ShareResult>('platform_share', { path: await exportTemp() }));
        if (res) notify(res.note ?? t.shared);
    }

    function openDrawer(which: 'convert' | 'docx', path = '') {
        menu = null;
        drawerMounted = { ...drawerMounted, [which]: true };
        if (path) drawerPath = { ...drawerPath, [which]: path };
        drawer = which;
        flog('info', `[pdf] opened ${which === 'docx' ? 'Word document tools' : 'document converter'}${path ? ` for ${path}` : ''}`);
    }

    const docToolCtx: DocToolContext = {
        run: (label, fn) => run(label, fn),
        apply: (op, label, note) => applyOp(op, label, note),
        exportTemp: () => exportTemp(),
        notify: (msg, openPath) => notify(msg, openPath),
        fail: (label, msg) => fail(label, msg),
        renderPagePng: (i, dpi) => renderPagePng(i, dpi),
        openPath: (p) => openSession(p),
        close: () => { activeTool = null; },
    };

    // ── Drag and drop of files onto the editor ──────────────────────────
    const CONVERTIBLE = ['pptx', 'ppt', 'odp', 'doc', 'docm', 'odt', 'rtf', 'epub', 'xlsx', 'xls', 'ods', 'csv', 'html', 'htm', 'md', 'txt'];
    function openDropped(paths: string[]) {
        if (!paths?.length) return;
        flog('info', `[pdf] dropped ${paths.length} file(s): ${paths.join(', ')}`);
        const pdf = paths.find((p) => /\.pdf$/i.test(p));
        if (pdf) { void openSession(pdf); return; }
        const first = paths[0];
        const ext = first.split('.').pop()?.toLowerCase() ?? '';
        if (ext === 'docx') openDrawer('docx', first);
        else if (CONVERTIBLE.includes(ext)) openDrawer('convert', first);
        else fail(t.open, t.drop_unsupported.replace('{name}', fileName(first)));
    }

    let unlistenDrop: (() => void) | null = null;
    let destroyed = false;
    onMount(() => {
        try {
            getCurrentWebview().onDragDropEvent((event) => {
                const p = event.payload as { type: string; paths?: string[] };
                if (p.type === 'enter' || p.type === 'over') dropHover = true;
                else if (p.type === 'leave') dropHover = false;
                else if (p.type === 'drop') { dropHover = false; openDropped(p.paths ?? []); }
            }).then((fn) => { if (destroyed) fn(); else unlistenDrop = fn; })
              .catch((e) => flog('warn', `[pdf] drag-and-drop unavailable: ${errorMessage(e)}`));
        } catch (e) {
            flog('warn', `[pdf] drag-and-drop unavailable: ${errorMessage(e)}`);
        }
    });

    onDestroy(() => {
        destroyed = true;
        unlistenDrop?.();
        endTileDrag();
        renderSeq += 1;
        void closeSession();
    });

    // ── Keyboard ────────────────────────────────────────────────────────
    function onKeydown(ev: KeyboardEvent) {
        const target = ev.target as HTMLElement | null;
        if (target && (/^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName) || target.isContentEditable)) return;
        if (ev.key === 'Escape') {
            if (menu) { menu = null; return; }
            if (drawer) { drawer = null; return; }
            if (activeTool) { setTool(null); return; }
            selectNone();
            return;
        }
        if (!session) return;
        const mod = ev.metaKey || ev.ctrlKey;
        const k = ev.key.toLowerCase();
        if (mod && k === 'z') { ev.preventDefault(); if (ev.shiftKey) redo(); else undo(); }
        else if (mod && k === 'y') { ev.preventDefault(); redo(); }
        else if (mod && k === 's') { ev.preventDefault(); save(ev.shiftKey); }
        else if (mod && k === 'a') { ev.preventDefault(); selectAll(); }
        else if (mod && (ev.key === '+' || ev.key === '=')) { ev.preventDefault(); zoomIn(); }
        else if (mod && (ev.key === '-' || ev.key === '_')) { ev.preventDefault(); zoomOut(); }
        else if (mod && ev.key === '0') { ev.preventDefault(); fitPage(); }
        else if (target?.getAttribute('role') === 'separator') return;
        // Unmodified keys (Delete!) only act when focus is on the editor
        // itself — never from the drawer's tools, a menu, or another panel.
        else if (target && target !== document.body && (!target.closest('.pe') || target.closest('.pe-drawer, .pe-menu'))) return;
        else if (ev.key === 'Delete' || ev.key === 'Backspace') { ev.preventDefault(); void removePages(targetPages()); }
        else if (ev.key === 'PageDown' || ev.key === 'ArrowRight' || ev.key === 'ArrowDown') { ev.preventDefault(); goToPage(viewPage + 1); }
        else if (ev.key === 'PageUp' || ev.key === 'ArrowLeft' || ev.key === 'ArrowUp') { ev.preventDefault(); goToPage(viewPage - 1); }
        else if (ev.key === 'Home') { ev.preventDefault(); goToPage(0); }
        else if (ev.key === 'End') { ev.preventDefault(); goToPage(session.origins.length - 1); }
    }

    function onWindowPointerDown(ev: PointerEvent) {
        if (menu && !(ev.target as HTMLElement | null)?.closest('.pe-menu-wrap')) menu = null;
    }

    const pageCount = $derived(session?.origins.length ?? 0);
    const redactOnPage = $derived(redactRects.filter((r) => r.page === viewPage));
</script>

<svelte:window onkeydown={onKeydown} onpointerdown={onWindowPointerDown} />

{#snippet tb(label: string, onclick: () => void, disabled: boolean, active: boolean, Icon: any, extraClass = '')}
    <button class="pe-tb {extraClass}" class:active aria-pressed={active ? 'true' : undefined}
            {disabled} {onclick} title={label} aria-label={label}>
        <Icon size={15} />
    </button>
{/snippet}

{#snippet mi(label: string, onclick: () => void, disabled: boolean, Icon: any)}
    <button class="pe-mi" role="menuitem" {disabled} {onclick}><Icon size={14} /> <span>{label}</span></button>
{/snippet}

<div class="pe" class:drop-hover={dropHover}>
    <!-- One toolbar -->
    <div class="pe-toolbar" role="toolbar" aria-label="PDF">
        <div class="pe-group" role="group" aria-label={t.group_file}>
            {@render tb(t.open, openFile, busy, false, FileUp)}
            {@render tb(t.save, () => save(false), !session || busy, false, Save)}
            {@render tb(t.save_as, () => save(true), !session || busy, false, SaveAll)}
            <span class="pe-sep"></span>
            {@render tb(t.undo, undo, !session?.can_undo || busy, false, Undo2)}
            {@render tb(t.redo, redo, !session?.can_redo || busy, false, Redo2)}
        </div>
        <span class="pe-sep"></span>
        <div class="pe-group" role="group" aria-label={t.group_pages}>
            {@render tb(t.rotate_left, () => rotatePages(targetPages(), -90), !session || busy, false, RotateCcw)}
            {@render tb(t.rotate_right, () => rotatePages(targetPages(), 90), !session || busy, false, RotateCw)}
            {@render tb(t.move_back, () => moveBack(targetPages()), !session || busy, false, ChevronsLeft)}
            {@render tb(t.move_forward, () => moveForward(targetPages()), !session || busy, false, ChevronsRight)}
            {@render tb(t.remove, () => removePages(targetPages()), !session || busy, false, Trash2, 'danger')}
            {@render tb(t.extract, extractSelected, !session || busy, false, FileOutput)}
            {@render tb(t.insert_pages, () => setTool('insert'), !session || busy, activeTool === 'insert', FileInput)}
            {@render tb(t.insert_blank, doInsertBlank, !session || busy, false, FilePlus2)}
            {@render tb(t.crop, () => setTool('crop'), !session || busy, activeTool === 'crop', Crop)}
        </div>
        <span class="pe-sep"></span>
        <div class="pe-group" role="group" aria-label={t.group_edit}>
            {@render tb(t.text_box, () => setTool('textbox'), !session || busy, activeTool === 'textbox', Type)}
            {@render tb(t.region, () => setTool('region'), !session || busy, activeTool === 'region', LayoutTemplate)}
            {@render tb(t.overprint, () => setTool('overprint'), !session || busy, activeTool === 'overprint', PenLine)}
            {@render tb(t.redact, () => setTool('redact'), !session || busy, activeTool === 'redact', EyeOff)}
        </div>
        <span class="pe-sep"></span>
        <div class="pe-group" role="group" aria-label={t.group_annotate}>
            {@render tb(t.tool_select, () => setTool(null), !session, activeTool === null, MousePointer2)}
            {@render tb(t.ann_highlight, () => setTool('highlight'), !session || busy, activeTool === 'highlight', Highlighter)}
            {@render tb(t.ann_ink, () => setTool('ink'), !session || busy, activeTool === 'ink', PenTool)}
            {@render tb(t.ann_rect, () => setTool('rectangle'), !session || busy, activeTool === 'rectangle', Square)}
            {@render tb(t.ann_note, () => setTool('note'), !session || busy, activeTool === 'note', StickyNote)}
            {@render tb(t.ann_freetext, () => setTool('freetext'), !session || busy, activeTool === 'freetext', MessageSquareText)}
            {@render tb(t.ann_eraser, () => setTool('eraser'), !session || busy, activeTool === 'eraser', Eraser)}
        </div>
        <span class="pe-sep"></span>
        <div class="pe-menu-wrap">
            <button class="pe-tb pe-tb-label" class:active={menu === 'tools' || isDocTool(activeTool)} aria-haspopup="menu"
                    aria-expanded={menu === 'tools'} onclick={() => menu = menu === 'tools' ? null : 'tools'} title={t.tools_menu}>
                <Wrench size={15} /> <span>{t.tools_menu}</span> <ChevronDown size={12} />
            </button>
            {#if menu === 'tools'}
                <div class="pe-menu" role="menu" aria-label={t.tools_menu}>
                    {@render mi(t.merge_append, () => setTool('merge'), !session, Merge)}
                    {@render mi(t.insert_pages, () => setTool('insert'), !session, FileInput)}
                    {@render mi(t.split, () => setTool('split'), !session, Scissors)}
                    {@render mi(t.page_numbers, () => setTool('numbers'), !session, Hash)}
                    {@render mi(t.watermark, () => setTool('watermark'), !session, Stamp)}
                    {@render mi(t.metadata, () => setTool('metadata'), !session, Info)}
                    {@render mi(t.form, () => setTool('form'), !session, ClipboardList)}
                    {@render mi(t.compress, () => setTool('compress'), !session, Shrink)}
                    <div class="pe-mi-sep"></div>
                    {@render mi(t.encrypt, () => setTool('encrypt'), !session, Lock)}
                    {#if isEncrypted || !session}{@render mi(t.decrypt, () => setTool('decrypt'), !session, Unlock)}{/if}
                    {@render mi(t.sanitise, () => setTool('sanitise'), !session, Shield)}
                    {@render mi(t.signatures, doSignatures, !session, BadgeCheck)}
                    <div class="pe-mi-sep"></div>
                    {@render mi(t.linearize, () => doFileTool(t.linearize, 'web', 'pdf_linearize'), !session, Gauge)}
                    {@render mi(t.repair, () => doFileTool(t.repair, 'repaired', 'pdf_repair', true), !session, Wrench)}
                    {@render mi(t.pdfa, () => doFileTool(t.pdfa, 'pdfa', 'pdf_convert_pdfa'), !session, Archive)}
                    <div class="pe-mi-sep"></div>
                    {@render mi(t.kindle, () => setTool('kindle'), !session, BookOpen)}
                    {@render mi(t.annots_store, () => setTool('annots'), !session, MessageSquare)}
                </div>
            {/if}
        </div>
        <div class="pe-menu-wrap">
            <button class="pe-tb pe-tb-label" class:active={menu === 'export' || !!drawer} aria-haspopup="menu"
                    aria-expanded={menu === 'export'} onclick={() => menu = menu === 'export' ? null : 'export'} title={t.export_menu}>
                <Download size={15} /> <span>{t.export_menu}</span> <ChevronDown size={12} />
            </button>
            {#if menu === 'export'}
                <div class="pe-menu" role="menu" aria-label={t.export_menu}>
                    {@render mi(t.export_docx, () => exportText('docx'), !session, FileText)}
                    {@render mi(t.export_md, () => exportText('markdown'), !session, FileText)}
                    {@render mi(t.export_txt, () => exportText('text'), !session, FileText)}
                    {@render mi(t.export_images, () => setTool('images'), !session, Image)}
                    <div class="pe-mi-note">{t.export_text_hint}</div>
                    <div class="pe-mi-sep"></div>
                    {@render mi(`${t.export_annots} Markdown…`, () => exportAnnots('markdown'), !session, MessageSquare)}
                    {@render mi(`${t.export_annots} CSV…`, () => exportAnnots('csv'), !session, MessageSquare)}
                    {@render mi(`${t.export_annots} JSON…`, () => exportAnnots('json'), !session, MessageSquare)}
                    <div class="pe-mi-sep"></div>
                    {@render mi(t.print, doPrint, !session, Printer)}
                    {@render mi(platformCaps?.system_share_sheet ? t.share : t.share_reveal, doShare, !session, Share2)}
                    {@render mi(t.open_external, doOpenExternal, !session, ExternalLink)}
                    <div class="pe-mi-sep"></div>
                    {@render mi(t.convert_other, () => openDrawer('convert'), false, FileType)}
                    {@render mi(t.docx_tools, () => openDrawer('docx'), false, FileType2)}
                </div>
            {/if}
        </div>


    </div>

    <!-- Options for the active tool -->
    {#if session && activeTool && isDocTool(activeTool)}
        {#key activeTool}
            <PdfDocTools tool={activeTool} {session} selected={selectedSorted()} ctx={docToolCtx} />
        {/key}
    {:else if session && activeTool}
        <div class="pe-opts" role="group" aria-label={toolHint(activeTool)}>
            <span class="pe-hint">{toolHint(activeTool)}</span>
            {#if activeTool === 'crop'}
                <label><input type="checkbox" bind:checked={cropAllPages} /> {t.crop_all}</label>
                <label>X <input type="number" bind:value={cropX} class="pe-in-sm" /></label>
                <label>Y <input type="number" bind:value={cropY} class="pe-in-sm" /></label>
                <label>W <input type="number" bind:value={cropW} class="pe-in-sm" /></label>
                <label>H <input type="number" bind:value={cropH} class="pe-in-sm" /></label>
                <span class="pe-hint">{cropAllPages ? '' : t.crop_scope} · {t.points_hint}</span>
                <button class="pe-btn" onclick={initCropFromView}>{t.reset}</button>
                <button class="pe-btn pe-go" disabled={busy} onclick={doCrop}>{t.apply}</button>
            {:else if activeTool === 'textbox'}
                <label>X <input type="number" bind:value={boxX} class="pe-in-sm" /></label>
                <label>Y <input type="number" bind:value={boxY} class="pe-in-sm" /></label>
                <label>{t.size} <input type="number" min="4" max="96" bind:value={boxSize} class="pe-in-sm" /></label>
                <label class="pe-grow">{t.text} <input type="text" bind:value={boxText} class="pe-in" /></label>
                <button class="pe-btn pe-go" disabled={busy} onclick={doTextBox}>{t.apply}</button>
            {:else if activeTool === 'overprint'}
                <label class="pe-grow">{t.text} <input type="text" class="pe-in" bind:value={overprintText} /></label>
                <label>{t.size} <input type="number" min="4" max="96" bind:value={overprintSize} class="pe-in-sm" /></label>
                <button class="pe-btn pe-go" disabled={busy || !pendingRect} onclick={doOverprint}>{t.apply}</button>
                <span class="pe-hint pe-warn">{t.overprint_caveat}</span>
            {:else if activeTool === 'region'}
                <label class="pe-grow">{t.text} <textarea class="pe-in pe-textarea" rows="2" bind:value={regionText}></textarea></label>
                <label>{t.region_font}
                    <select class="pe-in" bind:value={regionFont}>{#each REGION_FONTS as f}<option value={f}>{f}</option>{/each}</select>
                </label>
                <label>{t.size} <input type="number" min="4" max="96" step="0.5" bind:value={regionSize} class="pe-in-sm" /></label>
                <label>{t.region_align}
                    <select class="pe-in" bind:value={regionAlign}>
                        <option value="left">left</option><option value="center">center</option>
                        <option value="right">right</option><option value="justify">justify</option>
                    </select>
                </label>
                <label>{t.region_valign}
                    <select class="pe-in" bind:value={regionVAlign}>
                        <option value="top">top</option><option value="middle">middle</option><option value="bottom">bottom</option>
                    </select>
                </label>
                <label>{t.region_line_height} <input type="number" min="0.8" max="3" step="0.1" bind:value={regionLineHeight} class="pe-in-sm" /></label>
                <label>{t.region_color} <input type="color" bind:value={regionColor} class="pe-color" /></label>
                <label><input type="checkbox" bind:checked={regionBorder} /> {t.region_border}</label>
                <button class="pe-btn pe-go" disabled={busy || !pendingRect} onclick={doDrawRegion}>{t.region_apply}</button>
                {#if regionReport}
                    <span class="pe-hint">{t.region_fit.replace('{n}', String(regionReport.lines))}</span>
                    {#if regionReport.overflow}
                        <span class="pe-hint pe-warn"><AlertTriangle size={12} /> {t.region_overflow.replace('{n}', String(regionReport.lines_dropped))}</span>
                    {/if}
                    {#if regionReport.unsupported_chars.length > 0}
                        <span class="pe-hint pe-warn"><AlertTriangle size={12} /> {t.region_unsupported.replace('{chars}', regionReport.unsupported_chars.join(' '))}</span>
                    {/if}
                {/if}
            {:else if activeTool === 'redact'}
                <span class="pe-hint">{t.redact_marked}: {redactRects.length}</span>
                <button class="pe-btn" disabled={redactRects.length === 0} onclick={() => { redactRects = []; redactReport = null; }}>{t.redact_clear}</button>
                <button class="pe-btn pe-go pe-danger-go" disabled={redactRects.length === 0 || busy} onclick={doRedact}>{t.redact_apply}</button>
                <span class="pe-hint pe-warn">{t.redact_caveat}</span>
            {:else if activeTool === 'eraser'}
                <span class="pe-hint">{t.ann_saved_hint}</span>
            {:else}
                <!-- highlight / ink / rectangle / note / freetext -->
                <span class="pe-swatches" role="radiogroup" aria-label={t.ann_color}>
                    {#each SWATCHES as c}
                        <button class="pe-swatch" class:on={annotColors[activeTool ?? ''] === c} style:background={c}
                                role="radio" aria-checked={annotColors[activeTool ?? ''] === c} aria-label={c}
                                onclick={() => annotColors = { ...annotColors, [activeTool as string]: c }}></button>
                    {/each}
                    <input type="color" class="pe-color" aria-label={t.ann_color} value={annotColors[activeTool ?? '']}
                           oninput={(e) => annotColors = { ...annotColors, [activeTool as string]: (e.currentTarget as HTMLInputElement).value }} />
                </span>
                {#if activeTool === 'ink' || activeTool === 'rectangle'}
                    <label>{t.ann_width} <input type="number" min="0.5" max="20" step="0.5" bind:value={annotWidth} class="pe-in-sm" /></label>
                {/if}
                {#if activeTool === 'freetext'}
                    <label>{t.size} <input type="number" min="6" max="72" bind:value={annotFontSize} class="pe-in-sm" /></label>
                {/if}
                {#if activeTool === 'note' || activeTool === 'freetext'}
                    <label class="pe-grow">
                        <input type="text" class="pe-in" placeholder={t.ann_text_placeholder} bind:value={annotText}
                               onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); void commitTextAnnotation(); } }} />
                    </label>
                    <button class="pe-btn pe-go" disabled={busy || (activeTool === 'note' ? !anchor : !pendingRect)} onclick={commitTextAnnotation}>{t.ann_add}</button>
                {/if}
                <span class="pe-hint">{t.ann_saved_hint}</span>
            {/if}
            <span class="pe-spacer"></span>
            <button class="pe-x" onclick={() => setTool(null)} aria-label={t.close_tool} title={t.close_tool}><X size={13} /></button>
        </div>
    {/if}

    <!-- Status -->
    {#if error}
        <div class="pe-status pe-err" role="alert">
            <button class="pe-dismiss" onclick={() => error = ''} aria-label={t.dismiss} title={t.dismiss}><X size={13} /></button>
            <span class="pe-status-msg">{error}</span>
        </div>
    {/if}
    {#if notice && !error}
        <div class="pe-status pe-ok" role="status">
            <Check size={13} />
            <span class="pe-status-msg">{notice.msg}</span>
            {#if notice.openPath}
                {@const target = notice.openPath}
                <button class="pe-link" onclick={() => openSession(target)}>{t.open_result}</button>
            {/if}
            <button class="pe-dismiss" onclick={() => notice = null} aria-label={t.dismiss} title={t.dismiss}><X size={13} /></button>
        </div>
    {/if}
    {#if redactReport}
        <div class="pe-status pe-report">
            <span>{t.redact_removed.replace('{glyphs}', String(redactReport.glyphs_removed)).replace('{pages}', String(redactReport.pages_changed))}</span>
            {#if redactReport.runs_dropped > 0}
                <span class="pe-warn"><AlertTriangle size={12} /> {t.redact_runs_dropped.replace('{n}', String(redactReport.runs_dropped))}</span>
            {/if}
            {#each redactReport.warnings as w}<span class="pe-warn"><AlertTriangle size={12} /> {w}</span>{/each}
            <button class="pe-dismiss pe-dismiss-end" onclick={() => redactReport = null} aria-label={t.dismiss}><X size={13} /></button>
        </div>
    {/if}

    <!-- Body: page panel | divider | page view -->
    <div class="pe-main" bind:this={mainEl}>
        {#if !session}
            <div class="pe-empty">
                <FileUp size={44} strokeWidth={1} />
                <p>{t.empty_hint}</p>
                <p class="pe-hint">{t.drop_hint}</p>
                <button class="pe-btn pe-primary" onclick={openFile}>{t.open}</button>
                {#if encryptedPath}
                    <div class="pe-unlock">
                        <span>{t.encrypted_open}</span>
                        <input type="password" class="pe-in" bind:value={unlockPassword} aria-label={i18n.t.pdftools.password}
                               onkeydown={(e) => { if (e.key === 'Enter') void unlockEncrypted(); }} />
                        <button class="pe-btn pe-go" onclick={unlockEncrypted}>{i18n.t.pdftools.decrypt}</button>
                    </div>
                {/if}
                <div class="pe-empty-links">
                    <button class="pe-link" onclick={() => openDrawer('convert')}>{t.convert_other}</button>
                    <button class="pe-link" onclick={() => openDrawer('docx')}>{t.docx_tools}</button>
                </div>
            </div>
        {:else}
            <aside class="pe-side" style:width="{sideWidth}px" aria-label={t.pages_panel}>
                <div class="pe-side-head">
                    <button class="pe-link" onclick={selectAll}>{t.select_all}</button>
                    <button class="pe-link" onclick={selectNone} disabled={selected.size === 0}>{t.select_none}</button>
                    <span class="pe-side-count">{selected.size > 0 ? `${selected.size} ${t.selected}` : `${pageCount} ${t.pages}`}</span>
                    {#if session.modified}<span class="pe-dot" title={t.unsaved} aria-label={t.unsaved}>●</span>{/if}
                </div>
                <div class="pe-side-size">
                    <input type="range" min="70" max="320" step="10" value={thumbSize} aria-label={t.thumb_size} title={t.thumb_size}
                           oninput={(e) => onThumbSize(+(e.currentTarget as HTMLInputElement).value)} />
                </div>
                <div class="pe-thumbs" bind:this={thumbsEl} style:--thumb="{thumbSize}px" title={t.reorder_hint}
                     role="listbox" aria-multiselectable="true" aria-label={t.pages_panel}>
                    {#each session.origins as origin, i (`${origin.source ?? 'blank'}#${origin.index ?? 'x'}#${i}`)}
                        <div
                            class="pe-tile"
                            class:selected={selected.has(i)}
                            class:current={viewPage === i}
                            class:dragging={dragFrom !== null && (dragFrom === i || (selected.has(dragFrom) && selected.has(i)))}
                            class:drop-before={dropAt?.index === i && !dropAt.after}
                            class:drop-after={dropAt?.index === i && dropAt.after}
                            data-index={i}
                            role="option"
                            aria-selected={selected.has(i)}
                            aria-current={viewPage === i ? 'page' : undefined}
                            aria-label={`${t.page} ${i + 1}`}
                            tabindex="0"
                            onpointerdown={(e) => onTilePointerDown(i, e)}
                            onclick={(e) => clickPage(i, e)}
                            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); clickPage(i, e); } }}
                        >
                            <div class="pe-thumb">
                                {#if thumbs[i]}
                                    <img src={thumbs[i]} alt={`${t.page} ${i + 1}`} draggable="false" />
                                {:else}
                                    <div class="pe-thumb-ph"><Loader2 size={18} class="spin" /></div>
                                {/if}
                                <div class="pe-tile-ops">
                                    <button class="pe-mini" title={t.move_back} aria-label={`${t.move_back} (${t.page} ${i + 1})`} disabled={i === 0 || busy}
                                            onclick={(e) => { e.stopPropagation(); moveBack([i]); }}><ChevronLeft size={11} /></button>
                                    <button class="pe-mini" title={t.rotate_left} aria-label={`${t.rotate_left} (${t.page} ${i + 1})`} disabled={busy}
                                            onclick={(e) => { e.stopPropagation(); rotatePages([i], -90); }}><RotateCcw size={11} /></button>
                                    <button class="pe-mini" title={t.rotate_right} aria-label={`${t.rotate_right} (${t.page} ${i + 1})`} disabled={busy}
                                            onclick={(e) => { e.stopPropagation(); rotatePages([i], 90); }}><RotateCw size={11} /></button>
                                    <button class="pe-mini" title={t.move_forward} aria-label={`${t.move_forward} (${t.page} ${i + 1})`} disabled={i >= pageCount - 1 || busy}
                                            onclick={(e) => { e.stopPropagation(); moveForward([i]); }}><ChevronRight size={11} /></button>
                                    <button class="pe-mini pe-mini-danger" title={t.remove} aria-label={`${t.remove} (${t.page} ${i + 1})`} disabled={busy || pageCount <= 1}
                                            onclick={(e) => { e.stopPropagation(); removePages([i]); }}><Trash2 size={11} /></button>
                                </div>
                            </div>
                            <div class="pe-tile-label">
                                <span class="pe-pageno">{i + 1}</span>
                                {#if !origin.source}<span class="pe-badge">{t.blank}</span>
                                {:else if origin.source !== session.source}<span class="pe-badge pe-badge-alt">{t.imported}</span>{/if}
                            </div>
                        </div>
                    {/each}
                </div>
            </aside>
            <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
            <div class="pe-divider" class:active={resizing} role="separator" aria-orientation="vertical"
                 aria-label={t.resize_sidebar} aria-valuenow={sideWidth} tabindex="0" title={t.resize_sidebar}
                 onpointerdown={onDividerDown} onpointermove={onDividerMove} onpointerup={onDividerUp}
                 onpointercancel={onDividerUp} onkeydown={onDividerKey}></div>

            <div class="pe-view-wrap">
            <div class="pe-view" bind:this={viewEl}>
                <div class="pe-view-inner">
                    <div class="pe-page" style:width="{vpW}px" style:height="{vpH}px">
                        <canvas bind:this={pageCanvas} class="pe-canvas" style:width="{vpW}px" style:height="{vpH}px"></canvas>
                        <!-- svelte-ignore a11y_no_static_element_interactions -->
                        <div class="pe-overlay" bind:this={overlayEl}
                             class:tool-rect={isRectTool(activeTool)} class:tool-ink={activeTool === 'ink'}
                             class:tool-click={activeTool === 'textbox' || activeTool === 'note'}
                             class:tool-eraser={activeTool === 'eraser'}
                             data-testid="pdf-page-overlay"
                             onpointerdown={onOverlayDown} onpointermove={onOverlayMove}
                             onpointerup={onOverlayUp} onpointercancel={onOverlayUp}>
                            {#if isRectTool(activeTool)}
                                {@const b = shownBox()}
                                {#if b}
                                    <div class="pe-box" class:crop={activeTool === 'crop'} class:hl={activeTool === 'highlight'}
                                         data-testid="pdf-tool-box"
                                         style:left="{b.x}px" style:top="{b.y}px" style:width="{b.w}px" style:height="{b.h}px"
                                         style:--c={annotColors[activeTool ?? ''] ?? '#3b82f6'}></div>
                                {/if}
                            {/if}
                            {#if activeTool === 'redact'}
                                {#each redactOnPage as r}
                                    {@const v = toViewRect(r)}
                                    {#if v}<div class="pe-redactrect" style:left="{v.x}px" style:top="{v.y}px" style:width="{v.w}px" style:height="{v.h}px"></div>{/if}
                                {/each}
                            {/if}
                            {#if inkPoints.length > 0}
                                <svg class="pe-ink" width={vpW} height={vpH} aria-hidden="true">
                                    <polyline points={inkPoints.map((p) => `${p.x},${p.y}`).join(' ')}
                                              fill="none" stroke={annotColors.ink} stroke-width={annotWidth * viewScale}
                                              stroke-linecap="round" stroke-linejoin="round" />
                                </svg>
                            {/if}
                            {#if anchor && (activeTool === 'textbox' || activeTool === 'note')}
                            {@const pin = toViewPoint(anchor)}
                            {#if pin}
                                <div class="pe-pin" style:left="{pin.x}px" style:top="{pin.y}px">
                                    <span class="pe-pin-dot"></span>
                                    {#if activeTool === 'textbox'}
                                        <span class="pe-pin-label" style:font-size="{Math.max(8, boxSize * viewScale)}px">{boxText || t.text}</span>
                                    {:else}
                                        <span class="pe-note-icon" style:background={annotColors.note}><StickyNote size={12} /></span>
                                    {/if}
                                </div>
                            {/if}
                            {/if}
                        </div>
                        {#if viewRendering && vpW === 0}<div class="pe-view-busy"><Loader2 size={22} class="spin" /></div>{/if}
                    </div>
                </div>
            </div>
            <div class="pe-viewbar" role="toolbar" aria-label={t.group_view}>
                {@render tb(t.prev_page, () => goToPage(viewPage - 1), !session || viewPage === 0, false, ChevronLeft)}
                <span class="pe-pageno-ind" aria-live="polite">{session ? `${viewPage + 1} / ${pageCount}` : '–'}</span>
                {@render tb(t.next_page, () => goToPage(viewPage + 1), !session || viewPage >= pageCount - 1, false, ChevronRight)}
                <span class="pe-sep"></span>
                {@render tb(t.zoom_out, zoomOut, !session, false, ZoomOut)}
                <span class="pe-zoom" data-testid="pdf-zoom">{Math.round(viewScale * 100)}%</span>
                {@render tb(t.zoom_in, zoomIn, !session, false, ZoomIn)}
                {@render tb(t.fit_page, fitPage, !session, zoomMode === 'fit-page', Maximize)}
                {@render tb(t.fit_width, fitWidth, !session, zoomMode === 'fit-width', MoveHorizontal)}
            </div>
            </div>
        {/if}

        {#if dropHover}
            <div class="pe-drop" aria-hidden="true"><FileUp size={36} /> <span>{t.drop_hint}</span></div>
        {/if}

        <!-- Side drawer for tools that act on other files -->
        {#if drawerMounted.convert || drawerMounted.docx}
            <div class="pe-drawer" style:display={drawer ? 'flex' : 'none'} role="dialog" aria-label={drawer === 'docx' ? t.docx_tools : t.convert_other}>
                <div class="pe-drawer-head">
                    <span>{drawer === 'docx' ? t.docx_tools.replace('…', '') : t.convert_other.replace('…', '')}</span>
                    <span class="pe-spacer"></span>
                    <button class="pe-x" onclick={() => drawer = null} aria-label={t.dismiss} title={t.dismiss}><X size={14} /></button>
                </div>
                {#if drawerMounted.convert}
                    <div class="pe-drawer-body" style:display={drawer === 'convert' ? 'flex' : 'none'}>
                        <ConvertTools initialPath={drawerPath.convert} />
                    </div>
                {/if}
                {#if drawerMounted.docx}
                    <div class="pe-drawer-body" style:display={drawer === 'docx' ? 'flex' : 'none'}>
                        <DocxTools initialPath={drawerPath.docx} />
                    </div>
                {/if}
            </div>
        {/if}
    </div>

    {#if dragPos && dragFrom !== null}
        <div class="pe-drag-ghost" style:left="{dragPos.x + 12}px" style:top="{dragPos.y + 12}px">
            {selected.has(dragFrom) && selected.size > 1 ? `${selected.size} ${t.pages}` : `${t.page} ${dragFrom + 1}`}
        </div>
    {/if}
    {#if busy}<div class="pe-busy-chip" role="status"><Loader2 size={13} class="spin" /> {t.working}</div>{/if}
</div>

<style>
    .pe { position: relative; display: flex; flex-direction: column; height: 100%; min-height: 0; background: #18181b; color: #e4e4e7; }

    /* Toolbar */
    .pe-toolbar {
        display: flex; align-items: center; gap: 2px; flex-wrap: wrap;
        padding: 5px 8px; border-bottom: 1px solid #3f3f46; background: #18181b;
    }
    .pe-group { display: inline-flex; align-items: center; gap: 2px; }
    .pe-sep { width: 1px; height: 20px; background: #3f3f46; margin: 0 5px; flex: none; }
    .pe-spacer { flex: 1; }
    .pe-tb {
        display: inline-flex; align-items: center; justify-content: center; gap: 4px;
        min-width: 28px; height: 28px; padding: 0 6px; border-radius: 6px;
        border: 1px solid transparent; background: none; color: #e4e4e7; cursor: pointer; font-size: 12px;
    }
    .pe-tb:hover:not(:disabled) { background: #27272a; border-color: #3f3f46; }
    .pe-tb:disabled { opacity: 0.35; cursor: default; }
    .pe-tb.active { background: rgba(59, 130, 246, 0.18); border-color: #3b82f6; color: #fff; }
    .pe-tb.danger:hover:not(:disabled) { background: #7f1d1d; border-color: #b91c1c; }
    .pe-tb-label { padding: 0 8px; }
    .pe-pageno-ind, .pe-zoom {
        font-size: 12px; color: #a1a1aa; font-variant-numeric: tabular-nums; min-width: 44px; text-align: center;
    }
    .pe-menu-wrap { position: relative; }
    .pe-menu {
        position: absolute; top: calc(100% + 4px); left: 0; z-index: 40; min-width: 270px;
        display: flex; flex-direction: column; padding: 4px; border-radius: 8px;
        background: #18181b; border: 1px solid #3f3f46; box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
    }
    .pe-mi {
        display: flex; align-items: center; gap: 8px; padding: 6px 9px; border-radius: 5px;
        background: none; border: none; color: #e4e4e7; font-size: 12px; text-align: left; cursor: pointer;
    }
    .pe-mi:hover:not(:disabled) { background: #27272a; }
    .pe-mi:disabled { opacity: 0.4; cursor: default; }
    .pe-mi-sep { height: 1px; background: #3f3f46; margin: 4px 2px; }
    .pe-mi-note { font-size: 10.5px; color: #a1a1aa; padding: 2px 9px 4px 31px; }

    /* Options bar */
    .pe-opts {
        display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
        padding: 7px 12px; background: #18181b; border-bottom: 1px solid #3f3f46; font-size: 12px;
    }
    .pe-opts label { display: inline-flex; align-items: center; gap: 5px; color: #a1a1aa; }
    .pe-grow { flex: 1; min-width: 180px; }
    .pe-grow .pe-in { flex: 1; }
    .pe-in, .pe-in-sm, .pe-opts select {
        background: #27272a; border: 1px solid #3f3f46; border-radius: 5px; color: #e4e4e7; padding: 4px 7px; font-size: 12px;
    }
    .pe-in-sm { width: 64px; }
    .pe-textarea { resize: vertical; min-height: 30px; font-family: inherit; }
    .pe-color { width: 30px; height: 24px; padding: 1px; cursor: pointer; background: #27272a; border: 1px solid #3f3f46; border-radius: 5px; }
    .pe-hint { color: #a1a1aa; font-size: 11px; }
    .pe-warn { display: inline-flex; align-items: center; gap: 4px; color: #fbbf24; }
    .pe-swatches { display: inline-flex; align-items: center; gap: 4px; }
    .pe-swatch { width: 18px; height: 18px; border-radius: 50%; border: 2px solid #3f3f46; cursor: pointer; padding: 0; }
    .pe-swatch.on { border-color: #e4e4e7; box-shadow: 0 0 0 2px #3b82f6; }
    .pe-btn {
        display: inline-flex; align-items: center; gap: 5px; padding: 4px 10px; font-size: 12px; border-radius: 6px;
        border: 1px solid #3f3f46; background: #27272a; color: #e4e4e7; cursor: pointer;
    }
    .pe-btn:hover:not(:disabled) { background: #3f3f46; }
    .pe-btn:disabled { opacity: 0.45; cursor: default; }
    .pe-primary, .pe-go { background: #3b82f6; border-color: #3b82f6; color: #fff; }
    .pe-primary:hover:not(:disabled), .pe-go:hover:not(:disabled) { background: #2563eb; }
    .pe-danger-go { background: #b91c1c; border-color: #b91c1c; }
    .pe-x, .pe-dismiss {
        display: inline-flex; align-items: center; justify-content: center; flex: none;
        width: 22px; height: 22px; border-radius: 5px; cursor: pointer;
        border: 1px solid transparent; background: none; color: inherit; padding: 0;
    }
    .pe-x:hover, .pe-dismiss:hover { background: rgba(255, 255, 255, 0.08); border-color: #3f3f46; }
    .pe-dismiss-end { margin-left: auto; }

    /* Status */
    .pe-status {
        position: relative; z-index: 5; display: flex; align-items: center; gap: 8px;
        padding: 5px 10px; font-size: 12px; border-bottom: 1px solid #3f3f46;
    }
    .pe-status-msg { flex: 1; min-width: 0; overflow-wrap: anywhere; }
    .pe-err { background: #2a1215; color: #fca5a5; }
    .pe-ok { background: #0f2417; color: #86efac; }
    .pe-report { flex-wrap: wrap; background: #1a1206; color: #fcd34d; }
    .pe-link { background: none; border: none; color: #60a5fa; font-size: 12px; cursor: pointer; padding: 0; }
    .pe-link:hover:not(:disabled) { text-decoration: underline; }
    .pe-link:disabled { color: #52525b; cursor: default; }
    .pe-busy-chip {
        position: absolute; right: 12px; bottom: 12px; z-index: 30; display: inline-flex; align-items: center; gap: 6px;
        padding: 5px 10px; border-radius: 14px; font-size: 12px; background: #27272a; color: #93c5fd; border: 1px solid #3f3f46;
        pointer-events: none;
    }

    /* Body */
    .pe-main { position: relative; flex: 1; min-height: 0; display: flex; }
    .pe-empty {
        flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center;
        gap: 12px; color: #71717a; border: 2px dashed transparent; margin: 16px; border-radius: 12px;
    }
    .pe-empty p { margin: 0; }
    .pe-empty-links { display: flex; gap: 18px; margin-top: 8px; }
    .pe-unlock { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; justify-content: center; color: #fbbf24; font-size: 12px; max-width: 520px; }
    .drop-hover .pe-empty { border-color: #3b82f6; }

    .pe-side { flex: none; display: flex; flex-direction: column; min-width: 120px; background: #18181b; border-right: 1px solid #27272a; }
    .pe-side-head { display: flex; align-items: center; gap: 10px; padding: 7px 10px 2px; font-size: 12px; }
    .pe-side-count { margin-left: auto; color: #a1a1aa; font-size: 11px; }
    .pe-dot { color: #f59e0b; font-size: 10px; }
    .pe-side-size { padding: 2px 10px 6px; }
    .pe-side-size input { width: 100%; accent-color: #3b82f6; }
    .pe-thumbs {
        flex: 1; overflow-y: auto; padding: 6px 10px 16px;
        display: grid; gap: 10px; align-content: start; justify-content: center;
        grid-template-columns: repeat(auto-fill, minmax(var(--thumb), var(--thumb)));
    }
    .pe-tile {
        position: relative; border: 2px solid transparent; border-radius: 7px; padding: 5px;
        background: #27272a33; cursor: grab; user-select: none; touch-action: none;
    }
    .pe-tile:hover { background: #27272a; }
    .pe-tile:focus { outline: none; }
    .pe-tile:focus-visible { outline: 1px dashed #a1a1aa; outline-offset: 1px; }
    .pe-tile.current { border-color: #3f3f46; background: #27272a; }
    .pe-tile.selected { border-color: #3b82f6; background: rgba(59, 130, 246, 0.14); }
    .pe-tile.dragging { opacity: 0.45; }
    .pe-tile.drop-before::before, .pe-tile.drop-after::after {
        content: ''; position: absolute; background: #3b82f6; border-radius: 2px; z-index: 2;
        left: -6px; top: 0; bottom: 0; width: 3px;
    }
    .pe-tile.drop-after::after { left: auto; right: -6px; }
    .pe-thumb { position: relative; background: #fff; border-radius: 3px; overflow: hidden; }
    .pe-thumb img { display: block; width: 100%; height: auto; pointer-events: none; }
    .pe-thumb-ph { display: flex; align-items: center; justify-content: center; aspect-ratio: 0.72; color: #71717a; background: #27272a; }
    .pe-tile-ops {
        position: absolute; left: 0; right: 0; bottom: 0; display: flex; justify-content: center; gap: 2px;
        padding: 3px; background: linear-gradient(transparent, rgba(9, 9, 11, 0.75));
        opacity: 0; transition: opacity 0.12s;
    }
    .pe-tile:hover .pe-tile-ops, .pe-tile:focus-within .pe-tile-ops, .pe-tile.current .pe-tile-ops { opacity: 1; }
    .pe-mini {
        display: inline-flex; align-items: center; justify-content: center;
        width: 20px; height: 20px; border-radius: 4px; cursor: pointer; padding: 0;
        border: 1px solid #3f3f46; background: rgba(24, 24, 27, 0.92); color: #e4e4e7;
    }
    .pe-mini:hover:not(:disabled) { background: #3f3f46; }
    .pe-mini:disabled { opacity: 0.35; cursor: default; }
    .pe-mini-danger:hover:not(:disabled) { background: #7f1d1d; border-color: #b91c1c; }
    .pe-tile-label { display: flex; align-items: center; justify-content: center; gap: 6px; margin-top: 4px; font-size: 11px; color: #a1a1aa; }
    .pe-pageno { font-variant-numeric: tabular-nums; }
    .pe-badge { padding: 1px 5px; border-radius: 3px; font-size: 10px; background: #3f3f46; color: #e4e4e7; }
    .pe-badge-alt { background: rgba(59, 130, 246, 0.25); color: #93c5fd; }

    .pe-divider {
        flex: none; width: 6px; cursor: col-resize; background: #18181b;
        border-left: 1px solid #27272a; border-right: 1px solid #27272a; touch-action: none;
    }
    .pe-divider:hover, .pe-divider.active, .pe-divider:focus-visible { background: #3b82f6; outline: none; }

    .pe-view-wrap { position: relative; flex: 1; min-width: 0; display: flex; }
    .pe-view { flex: 1; min-width: 0; overflow: auto; background: #0f0f12; }
    .pe-viewbar {
        position: absolute; left: 50%; bottom: 14px; transform: translateX(-50%); z-index: 20;
        display: inline-flex; align-items: center; gap: 2px; padding: 3px 6px; border-radius: 9px;
        background: rgba(24, 24, 27, 0.92); border: 1px solid #3f3f46; box-shadow: 0 6px 20px rgba(0, 0, 0, 0.45);
    }
    .pe-view-inner {
        min-width: 100%; min-height: 100%; width: max-content; box-sizing: border-box;
        display: flex; align-items: center; justify-content: center; padding: 20px 20px 64px;
    }
    .pe-page { position: relative; flex: none; background: #fff; box-shadow: 0 2px 14px rgba(0, 0, 0, 0.6); }
    .pe-canvas { display: block; }
    .pe-overlay { position: absolute; inset: 0; touch-action: none; }
    .pe-overlay.tool-rect, .pe-overlay.tool-click { cursor: crosshair; }
    .pe-overlay.tool-ink { cursor: crosshair; }
    .pe-overlay.tool-eraser { cursor: cell; }
    .pe-view-busy { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; color: #93c5fd; }
    .pe-box { position: absolute; pointer-events: none; border: 1.5px dashed var(--c, #3b82f6); background: color-mix(in srgb, var(--c, #3b82f6) 16%, transparent); }
    .pe-box.crop { border: 1.5px solid #22c55e; background: rgba(34, 197, 94, 0.1); box-shadow: 0 0 0 9999px rgba(9, 9, 11, 0.45); }
    .pe-box.hl { border-style: solid; background: color-mix(in srgb, var(--c) 40%, transparent); }
    .pe-redactrect { position: absolute; pointer-events: none; background: rgba(9, 9, 11, 0.88); border: 1.5px solid #ef4444; }
    .pe-ink { position: absolute; inset: 0; pointer-events: none; }
    .pe-pin { position: absolute; pointer-events: none; }
    .pe-pin-dot { position: absolute; left: -3px; top: -3px; width: 6px; height: 6px; border-radius: 50%; background: #3b82f6; }
    .pe-pin-label { position: absolute; left: 2px; bottom: 0; white-space: pre; color: #111; line-height: 1.1; opacity: 0.75; }
    .pe-note-icon { position: absolute; left: 0; top: 0; display: inline-flex; padding: 2px; border-radius: 3px; color: #111; border: 1px solid #3f3f46; }

    .pe-drop {
        position: absolute; inset: 8px; z-index: 35; display: flex; flex-direction: column; gap: 10px;
        align-items: center; justify-content: center; pointer-events: none;
        border: 2px dashed #3b82f6; border-radius: 12px; background: rgba(59, 130, 246, 0.12); color: #bfdbfe; font-size: 14px;
    }
    .pe-drawer {
        position: absolute; top: 0; right: 0; bottom: 0; z-index: 32; width: min(560px, 92%);
        flex-direction: column; background: #18181b; border-left: 1px solid #3f3f46; box-shadow: -10px 0 30px rgba(0, 0, 0, 0.45);
    }
    .pe-drawer-head { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-bottom: 1px solid #3f3f46; font-size: 13px; }
    .pe-drawer-body { flex: 1; min-height: 0; flex-direction: column; overflow: auto; }
    .pe-drag-ghost {
        position: fixed; z-index: 60; pointer-events: none; padding: 4px 9px; border-radius: 6px; font-size: 12px;
        background: #3b82f6; color: #fff; box-shadow: 0 4px 14px rgba(0, 0, 0, 0.5);
    }

    :global(.spin) { animation: pe-spin 1s linear infinite; }
    @keyframes pe-spin { to { transform: rotate(360deg); } }
</style>
