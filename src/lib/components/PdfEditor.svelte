<script lang="ts">
    // P32.1b — direct-manipulation PDF page editor.
    //
    // Talks to the `pdf_session_*` commands (P32.1a) rather than the
    // one-shot `pdf_*` ones, so a whole run of edits accumulates in one
    // backend session and is written out once, with undo/redo throughout.
    //
    // Thumbnails are rendered from the *source* file wherever a page is
    // still bit-identical to it (which the backend reports per page via
    // `origins`), so dragging pages around or deleting them never costs a
    // re-render.  Only pages whose content was stamped — crop, page
    // numbers, watermark, text box — or pages that came from nowhere
    // (blank inserts) fall back to rasterising the edited document.

    import { invoke } from '@tauri-apps/api/core';
    import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
    import { readFile } from '@tauri-apps/plugin-fs';
    import * as pdfjsLib from 'pdfjs-dist/legacy/build/pdf';
    import { i18n } from '$lib/i18n.svelte';
    import {
        FileUp, Save, Undo2, Redo2, Trash2, RotateCw, RotateCcw, Crop,
        Hash, Type, FilePlus2, FileOutput, Loader2, Check, X, Copy,
        Printer, Share2, EyeOff, AlertTriangle, MessageSquare, ClipboardList, BookOpen, PenLine,
        LayoutTemplate, Shrink,
    } from 'lucide-svelte';

    pdfjsLib.GlobalWorkerOptions.workerSrc = '/pdf.worker.js';

    // ── Wire types (mirror src-tauri/src/pdf_session.rs) ────────────────
    interface PdfPageInfo { page_number: number; width_pt: number; height_pt: number; rotation: number; }
    interface PdfInfo {
        page_count: number; pages: PdfPageInfo[];
        title?: string; author?: string; subject?: string; keywords?: string;
        producer?: string; creator?: string;
    }
    interface PageOrigin {
        source: string | null;
        index: number | null;
        rotation: number;
        dirty: boolean;
    }
    interface SessionState {
        id: string;
        source: string;
        info: PdfInfo;
        origins: PageOrigin[];
        can_undo: boolean;
        can_redo: boolean;
        undo_label: string | null;
        modified: boolean;
        ops: string[];
    }

    // ── State ───────────────────────────────────────────────────────────
    let session = $state<SessionState | null>(null);
    let busy = $state(false);
    let error = $state('');
    let success = $state('');

    let selected = $state<Set<number>>(new Set());
    let lastAnchor = $state<number | null>(null);

    /** Current page index → rendered thumbnail data URL. */
    let thumbs = $state<Record<number, string>>({});
    /** Bumped whenever the edited-document preview is refreshed. */
    let previewEpoch = $state(0);

    let activePanel = $state<'numbers' | 'crop' | 'textbox' | 'insert' | 'redact' | 'annots' | 'form' | 'kindle' | 'overprint' | 'region' | 'compress' | null>(null);

    /** Panels that drive the interactive page stage. */
    const STAGE_PANELS = ['crop', 'textbox', 'redact', 'overprint', 'region'] as const;
    /** Panels whose gesture is dragging a rectangle. */
    const RECT_PANELS = ['crop', 'redact', 'overprint', 'region'] as const;
    const usesStage = (p: typeof activePanel) => STAGE_PANELS.includes(p as any);
    const usesRect = (p: typeof activePanel) => RECT_PANELS.includes(p as any);
    let dragFrom = $state<number | null>(null);
    let dragOver = $state<number | null>(null);

    // Panel inputs
    let numPosition = $state('bottom-center');
    let numFormat = $state('arabic');
    let numFontSize = $state(10);
    let numStart = $state(1);
    let numSkipFirst = $state(0);

    let cropAllPages = $state(true);
    let cropX = $state(0);
    let cropY = $state(0);
    let cropW = $state(0);
    let cropH = $state(0);

    let boxText = $state('');
    let boxSize = $state(12);
    let boxX = $state(72);
    let boxY = $state(72);
    let boxPage = $state(0);

    let insertPath = $state('');
    let insertRange = $state('');
    let insertAt = $state(0);

    // ── Interactive stage (drag-to-crop / click-to-place text) ──────────
    // Crop and text placement are direct-manipulation: the panel numbers
    // stay as the precise-entry path, but the normal way to use them is to
    // drag a rectangle or click a spot on a rendered page.
    let stageEl = $state<HTMLDivElement | null>(null);
    let stageCanvas = $state<HTMLCanvasElement | null>(null);
    let stagePage = $state(0);
    /** CSS pixels per PDF point on the stage. */
    let stageScale = $state(1);
    let stageBusy = $state(false);
    /** Live crop rectangle in CSS pixels, while dragging or after a drag. */
    let cropBox = $state<{ x: number; y: number; w: number; h: number } | null>(null);
    let cropDragging = $state(false);
    let cropStart: { x: number; y: number } | null = null;
    /** Text insertion point in CSS pixels. */
    let textAnchor = $state<{ x: number; y: number } | null>(null);

    // Non-reactive caches — pdf.js documents are heavy and must not be
    // proxied by Svelte's reactivity.
    const sourceDocs = new Map<string, any>();
    let previewDoc: any = null;
    const thumbCache = new Map<string, string>();

    const THUMB_WIDTH = 150;
    const STAGE_MAX_WIDTH = 560;

    // ── Session lifecycle ───────────────────────────────────────────────
    async function openFile() {
        const path = await openDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            multiple: false,
        });
        if (path) await openSession(path as string);
    }

    export async function openSession(path: string) {
        busy = true;
        error = '';
        success = '';
        await closeSession();
        try {
            session = await invoke<SessionState>('pdf_session_open', { path });
            selected = new Set();
            await refreshThumbs();
        } catch (e: any) {
            error = e?.message ?? String(e);
        }
        busy = false;
    }

    async function closeSession() {
        if (session) {
            try { await invoke('pdf_session_close', { id: session.id }); } catch { /* already gone */ }
        }
        session = null;
        thumbs = {};
        thumbCache.clear();
        for (const d of sourceDocs.values()) { try { d.destroy(); } catch { /* ignore */ } }
        sourceDocs.clear();
        if (previewDoc) { try { previewDoc.destroy(); } catch { /* ignore */ } previewDoc = null; }
    }

    /** Apply an edit op and re-sync state + thumbnails. */
    async function applyOp(op: Record<string, unknown>, note?: string) {
        if (!session) return;
        busy = true;
        error = '';
        try {
            session = await invoke<SessionState>('pdf_session_apply', { id: session.id, op });
            if (note) success = note;
            await refreshThumbs();
        } catch (e: any) {
            error = e?.message ?? String(e);
        }
        busy = false;
    }

    async function undo() {
        if (!session?.can_undo) return;
        busy = true;
        try {
            session = await invoke<SessionState>('pdf_session_undo', { id: session.id });
            selected = new Set();
            await refreshThumbs();
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    async function redo() {
        if (!session?.can_redo) return;
        busy = true;
        try {
            session = await invoke<SessionState>('pdf_session_redo', { id: session.id });
            selected = new Set();
            await refreshThumbs();
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    async function save(saveAs: boolean) {
        if (!session) return;
        let outPath: string | null = null;
        if (saveAs) {
            outPath = await saveDialog({
                filters: [{ name: 'PDF', extensions: ['pdf'] }],
                defaultPath: session.source,
            }) as string | null;
            if (!outPath) return;
        }
        busy = true;
        error = '';
        try {
            const written = await invoke<string>('pdf_session_save', {
                id: session.id,
                outPath,
            });
            success = `${i18n.t.pdfeditor.saved} ${written}`;
            session = await invoke<SessionState>('pdf_session_state', { id: session.id });
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    // ── Annotations ─────────────────────────────────────────────────────
    // The annotation store is keyed by doc_id; the source path is a stable
    // identifier for a document the user opened from disk, so it doubles
    // as one here. Import reads the PDF's own /Annots into the store (which
    // is what makes third-party markup searchable); stamp writes the
    // store's rows back out as real annotation objects.
    let annotCount = $state<number | null>(null);

    async function doImportAnnots() {
        if (!session) return;
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (tmp) {
                const n = await invoke<number>('pdf_import_annotations', {
                    path: tmp,
                    docId: session.source,
                });
                annotCount = n;
                success = i18n.t.pdfeditor.annots_imported.replace('{n}', String(n));
            }
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    async function doExportAnnots(format: 'markdown' | 'csv' | 'json') {
        if (!session) return;
        const ext = format === 'markdown' ? 'md' : format;
        const out = await saveDialog({
            filters: [{ name: format.toUpperCase(), extensions: [ext] }],
            defaultPath: `annotations.${ext}`,
        }) as string | null;
        if (!out) return;
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (tmp) {
                const n = await invoke<number>('pdf_export_annotations', {
                    path: tmp, format, outPath: out,
                });
                success = i18n.t.pdfeditor.annots_exported
                    .replace('{n}', String(n)).replace('{path}', out);
            }
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    async function doStampAnnots() {
        if (!session) return;
        const out = await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: 'annotated.pdf',
        }) as string | null;
        if (!out) return;
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (tmp) {
                const n = await invoke<number>('pdf_stamp_annotations', {
                    path: tmp, docId: session.source, outPath: out,
                });
                success = i18n.t.pdfeditor.annots_stamped
                    .replace('{n}', String(n)).replace('{path}', out);
            }
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    // ── Forms ───────────────────────────────────────────────────────────
    interface FormField {
        name: string;
        kind: string;
        value: string | null;
        on_value: string | null;
        options: string[];
        read_only: boolean;
        required: boolean;
        page: number | null;
        rect: [number, number, number, number] | null;
    }

    let formFields = $state<FormField[]>([]);
    let formEdits = $state<Record<string, string>>({});
    let formLoaded = $state(false);

    async function loadForm() {
        if (!session) return;
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (tmp) {
                formFields = await invoke<FormField[]>('pdf_read_form_fields', { path: tmp });
                formEdits = Object.fromEntries(
                    formFields.map((f) => [f.name, f.value ?? ''])
                );
                formLoaded = true;
                if (formFields.length === 0) success = i18n.t.pdfeditor.form_none;
            }
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    /** Only send fields the user actually changed. */
    function changedFormValues(): Record<string, string> {
        const out: Record<string, string> = {};
        for (const f of formFields) {
            const now = formEdits[f.name] ?? '';
            if (now !== (f.value ?? '')) out[f.name] = now;
        }
        return out;
    }

    async function doFillForm(flatten: boolean) {
        if (!session) return;
        const values = changedFormValues();
        if (!flatten && Object.keys(values).length === 0) {
            error = i18n.t.pdfeditor.form_no_changes;
            return;
        }
        const out = await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: flatten ? 'filled-flat.pdf' : 'filled.pdf',
        }) as string | null;
        if (!out) return;
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (!tmp) return;
            let n = 0;
            if (Object.keys(values).length > 0) {
                n = await invoke<number>('pdf_fill_form', { path: tmp, values, outPath: out });
            }
            if (flatten) {
                // Flatten the filled result, not the original.
                const src = Object.keys(values).length > 0 ? out : tmp;
                const k = await invoke<number>('pdf_flatten_form', { path: src, outPath: out });
                success = i18n.t.pdfeditor.form_flattened
                    .replace('{n}', String(k)).replace('{path}', out);
            } else {
                success = i18n.t.pdfeditor.form_filled
                    .replace('{n}', String(n)).replace('{path}', out);
            }
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    // ── Kindle clippings ────────────────────────────────────────────────
    interface ImportSummary {
        parsed: number; deduped: number; imported: number;
        matched: number; fuzzy_matched: number;
        titles: string[]; duplicates_skipped: number;
    }

    let clippingsPath = $state('');
    let kindleBooks = $state<string[]>([]);
    let kindleTitle = $state('');
    let kindleAnchor = $state(true);
    let kindleSummary = $state<ImportSummary | null>(null);

    async function pickClippings() {
        const p = await openDialog({
            filters: [{ name: 'Kindle clippings', extensions: ['txt'] }],
            multiple: false,
        });
        if (!p) return;
        clippingsPath = p as string;
        busy = true;
        error = '';
        kindleSummary = null;
        try {
            kindleBooks = await invoke<string[]>('kindle_list_books', { clippingsPath });
            kindleTitle = kindleBooks[0] ?? '';
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    async function doKindleImport() {
        if (!session || !clippingsPath) { error = i18n.t.pdfeditor.kindle_pick_first; return; }
        busy = true;
        error = '';
        try {
            // Anchoring needs a document; without one the highlights still
            // import, they just carry no offsets.
            const documentPath = kindleAnchor ? await currentTempCopy() : null;
            kindleSummary = await invoke<ImportSummary>('kindle_import', {
                clippingsPath,
                docId: session.source,
                titleFilter: kindleTitle || null,
                documentPath,
                minScore: null,
            });
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    // ── Overprint (tier 1 text edit) ────────────────────────────────────
    let overprintText = $state('');
    let overprintSize = $state(11);

    async function doOverprint() {
        if (!session || !cropBox) { error = i18n.t.pdfeditor.overprint_drag_first; return; }
        const out = await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: 'overprinted.pdf',
        }) as string | null;
        if (!out) return;
        const rect = stageRectToPt(cropBox);
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (!tmp) return;
            await invoke<number>('pdf_overprint', {
                path: tmp,
                specs: [{
                    page: stagePage,
                    ...rect,
                    text: overprintText,
                    font_size: overprintSize,
                    color: [0, 0, 0],
                    background: [1, 1, 1],
                }],
                outPath: out,
            });
            success = `${i18n.t.pdfeditor.overprinted} ${out}`;
            cropBox = null;
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    // ── Text regions (P32.9) ────────────────────────────────────────────
    // Unlike the text box, which places a line at a point, a region lays
    // the text out inside the dragged rectangle: it wraps, aligns and can
    // justify. The backend measures the same layout it draws, so the panel
    // previews the fit — line count and overflow — before anything is
    // written. Overflow is never drawn, so seeing it in advance matters.
    interface LayoutReport {
        lines: number;
        lines_dropped: number;
        overflow: boolean;
        unsupported_chars: string[];
    }

    /** The base-14 faces the backend has width tables for (kebab wire names). */
    const REGION_FONTS = [
        'helvetica', 'helvetica-bold', 'helvetica-oblique', 'helvetica-bold-oblique',
        'times-roman', 'times-bold', 'times-italic', 'times-bold-italic',
        'courier', 'courier-bold',
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
        if (!m) return [0, 0, 0];
        return [
            parseInt(m[1], 16) / 255,
            parseInt(m[2], 16) / 255,
            parseInt(m[3], 16) / 255,
        ];
    }

    /** The region the panel currently describes, in PDF user space. */
    function buildRegion(box: { x: number; y: number; w: number; h: number }) {
        return {
            page: stagePage,
            ...stageRectToPt(box),
            text: regionText,
            font: regionFont,
            font_size: regionSize,
            color: hexToRgb(regionColor),
            align: regionAlign,
            vertical_align: regionVAlign,
            line_height: regionLineHeight,
            padding: null,
            draw_border: regionBorder,
        };
    }

    // Live fit preview. Measuring is pure layout — no file is touched — so
    // it can run on every keystroke.
    $effect(() => {
        if (activePanel !== 'region' || !cropBox) { regionReport = null; return; }
        const region = buildRegion(cropBox);
        let stale = false;
        invoke<LayoutReport>('pdf_measure_text_region', { region })
            .then((r) => { if (!stale) regionReport = r; })
            .catch(() => { /* the commit path reports layout errors */ });
        return () => { stale = true; };
    });

    async function doDrawRegion() {
        if (!session || !cropBox) { error = i18n.t.pdfeditor.region_drag_first; return; }
        if (!regionText.trim()) { error = i18n.t.pdfeditor.text_required; return; }
        const out = await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: 'text-region.pdf',
        }) as string | null;
        if (!out) return;
        const region = buildRegion(cropBox);
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (!tmp) return;
            const reports = await invoke<LayoutReport[]>('pdf_draw_text_regions', {
                path: tmp,
                regions: [region],
                outPath: out,
            });
            regionReport = reports[0] ?? null;
            success = `${i18n.t.pdfeditor.region_written} ${out}`;
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    // ── Compression (P32.6) ─────────────────────────────────────────────
    interface CompressReport {
        bytes_before: number;
        bytes_after: number;
        streams_compressed: number;
        used_object_streams: boolean;
    }

    let compressObjectStreams = $state(true);
    let compressReport = $state<CompressReport | null>(null);

    async function doCompress() {
        if (!session) return;
        const out = await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: 'compressed.pdf',
        }) as string | null;
        if (!out) return;
        busy = true;
        error = '';
        compressReport = null;
        try {
            const tmp = await currentTempCopy();
            if (!tmp) return;
            compressReport = await invoke<CompressReport>('pdf_compress', {
                path: tmp,
                options: {
                    compress_streams: true,
                    use_object_streams: compressObjectStreams,
                    max_objects_per_stream: null,
                },
                outPath: out,
            });
            success = `${i18n.t.pdfeditor.compressed} ${out}`;
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    function formatBytes(n: number): string {
        if (n < 1024) return `${n} B`;
        if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} kB`;
        return `${(n / (1024 * 1024)).toFixed(2)} MB`;
    }

    // ── Redaction ───────────────────────────────────────────────────────
    // Rectangles accumulate as the user drags, then one call removes the
    // text under all of them and covers what it could not reach. This is
    // deliberately not an edit-session op: redaction rewrites content
    // streams, so it produces a new file rather than something the undo
    // stack can walk back through.
    interface RedactSpec { page: number; x: number; y: number; w: number; h: number }
    interface RedactReport {
        glyphs_removed: number;
        runs_dropped: number;
        pages_changed: number;
        warnings: string[];
    }

    let redactRects = $state<RedactSpec[]>([]);
    let redactReport = $state<RedactReport | null>(null);

    function addRedactRect() {
        if (!cropBox || cropBox.w < 4 || cropBox.h < 4) return;
        redactRects = [...redactRects, { page: stagePage, ...stageRectToPt(cropBox) }];
        cropBox = null;
    }

    function clearRedactRects() { redactRects = []; redactReport = null; }

    async function doRedact() {
        if (!session || redactRects.length === 0) return;
        const out = await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: 'redacted.pdf',
        }) as string | null;
        if (!out) return;
        busy = true;
        error = '';
        redactReport = null;
        try {
            const tmp = await invoke<string>('pdf_session_export_temp', { id: session.id });
            redactReport = await invoke<RedactReport>('pdf_redact_hard', {
                path: tmp,
                regions: redactRects,
                outPath: out,
            });
            success = `${i18n.t.pdfeditor.redacted} ${out}`;
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    // ── Print / share ───────────────────────────────────────────────────
    // Both act on the *edited* document, so they export a temp copy first
    // rather than handing the OS the stale file on disk. Exporting a temp
    // copy deliberately does not clear the unsaved-changes flag.
    interface PlatformCapabilities {
        system_share_sheet: boolean;
        direct_print: boolean;
        platform: string;
    }
    interface ShareResult { kind: string; note: string | null }

    let platformCaps = $state<PlatformCapabilities | null>(null);

    $effect(() => {
        if (platformCaps) return;
        invoke<PlatformCapabilities>('platform_capabilities')
            .then((c) => (platformCaps = c))
            .catch(() => { /* leave the buttons in their default state */ });
    });

    async function currentTempCopy(): Promise<string | null> {
        if (!session) return null;
        return await invoke<string>('pdf_session_export_temp', { id: session.id });
    }

    async function doPrint() {
        if (!session) return;
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (tmp) {
                const msg = await invoke<string>('platform_print', { path: tmp });
                success = msg?.trim() ? msg : i18n.t.pdfeditor.sent_to_printer;
            }
        } catch (e: any) {
            // No printer configured is the overwhelmingly common failure;
            // offer the viewer as the way to reach a print dialog.
            error = `${e?.message ?? String(e)} — ${i18n.t.pdfeditor.print_fallback_hint}`;
        }
        busy = false;
    }

    async function doOpenExternal() {
        if (!session) return;
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (tmp) await invoke('platform_open_external', { path: tmp });
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    async function doShare() {
        if (!session) return;
        busy = true;
        error = '';
        try {
            const tmp = await currentTempCopy();
            if (tmp) {
                const res = await invoke<ShareResult>('platform_share', { path: tmp });
                success = res.note ?? i18n.t.pdfeditor.shared;
            }
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    // ── Thumbnails ──────────────────────────────────────────────────────
    async function sourceDocFor(path: string) {
        let doc = sourceDocs.get(path);
        if (!doc) {
            const bytes = await readFile(path);
            doc = await pdfjsLib.getDocument({
                data: new Uint8Array(bytes),
                useSystemFonts: true,
                disableFontFace: true,
            }).promise;
            sourceDocs.set(path, doc);
        }
        return doc;
    }

    /** Rasterise the edited document — only needed for stamped/blank pages. */
    async function refreshPreviewDoc() {
        if (!session) return null;
        const tmp = await invoke<string>('pdf_session_export_temp', { id: session.id });
        const bytes = await readFile(tmp);
        if (previewDoc) { try { previewDoc.destroy(); } catch { /* ignore */ } }
        previewDoc = await pdfjsLib.getDocument({
            data: new Uint8Array(bytes),
            useSystemFonts: true,
            disableFontFace: true,
        }).promise;
        previewEpoch += 1;
        // Preview-derived thumbnails are keyed by epoch, so the previous
        // epoch's entries are unreachable — drop them rather than let the
        // cache grow with every content edit. Source-derived entries stay:
        // those pages are unchanged and re-rendering them is the cost this
        // cache exists to avoid.
        for (const key of [...thumbCache.keys()]) {
            if (key.startsWith('edit:')) thumbCache.delete(key);
        }
        return previewDoc;
    }

    /**
     * Resolve which pdf.js document holds page `i` of the edited document.
     * Clean pages come straight from their source file; stamped or
     * generated pages need the rasterised edit preview.
     */
    async function docAndPageFor(i: number): Promise<{ doc: any; pageNo: number; rotation: number } | null> {
        if (!session) return null;
        const o = session.origins[i];
        if (!o) return null;
        if (o.source && o.index !== null && !o.dirty) {
            return { doc: await sourceDocFor(o.source), pageNo: o.index + 1, rotation: o.rotation };
        }
        const preview = previewDoc ?? (await refreshPreviewDoc());
        return preview ? { doc: preview, pageNo: i + 1, rotation: 0 } : null;
    }

    async function renderThumb(doc: any, pageNo: number, extraRotation: number): Promise<string> {
        const page = await doc.getPage(pageNo);
        const base = page.getViewport({ scale: 1 });
        const scale = THUMB_WIDTH / base.width;
        const viewport = page.getViewport({
            scale,
            rotation: (page.rotate + extraRotation) % 360,
        });
        const canvas = document.createElement('canvas');
        canvas.width = Math.max(1, Math.floor(viewport.width));
        canvas.height = Math.max(1, Math.floor(viewport.height));
        const ctx = canvas.getContext('2d')!;
        await page.render({ canvasContext: ctx, viewport }).promise;
        return canvas.toDataURL('image/png');
    }

    async function refreshThumbs() {
        if (!session) return;
        const origins = session.origins;

        // Pages that can't be served from a source file need the edited doc.
        const needsPreview = origins.some((o) => o.dirty || !o.source);
        let preview: any = null;
        if (needsPreview) preview = await refreshPreviewDoc();

        const next: Record<number, string> = {};
        for (let i = 0; i < origins.length; i++) {
            const o = origins[i];
            const fromSource = !!o.source && o.index !== null && !o.dirty;
            const key = fromSource
                ? `src:${o.source}#${o.index}#${o.rotation}`
                : `edit:${i}#${previewEpoch}`;

            const cached = thumbCache.get(key);
            if (cached) { next[i] = cached; continue; }

            try {
                const url = fromSource
                    ? await renderThumb(await sourceDocFor(o.source!), o.index! + 1, o.rotation)
                    : preview
                        ? await renderThumb(preview, i + 1, 0)
                        : '';
                if (url) { thumbCache.set(key, url); next[i] = url; }
            } catch {
                // A page that fails to rasterise still gets a tile — the
                // placeholder is better than dropping it from the grid.
            }
        }
        thumbs = next;
    }

    /**
     * Render the stage page at rotation 0.
     *
     * Deliberately unrotated: /CropBox and text-box coordinates live in
     * *unrotated* user space, so showing the page as-authored keeps the
     * mapping from screen pixels to PDF points a plain scale + y-flip.
     * Rendering with the page's /Rotate applied would require undoing that
     * rotation in every conversion, which is where off-by-90 bugs live.
     */
    async function renderStage() {
        if (!session || !stageCanvas) return;
        const target = await docAndPageFor(stagePage);
        if (!target) return;
        stageBusy = true;
        try {
            const page = await target.doc.getPage(target.pageNo);
            const base = page.getViewport({ scale: 1, rotation: 0 });
            const scale = Math.min(STAGE_MAX_WIDTH / base.width, 1.6);
            const viewport = page.getViewport({ scale, rotation: 0 });
            stageCanvas.width = Math.max(1, Math.floor(viewport.width));
            stageCanvas.height = Math.max(1, Math.floor(viewport.height));
            const ctx = stageCanvas.getContext('2d')!;
            ctx.clearRect(0, 0, stageCanvas.width, stageCanvas.height);
            await page.render({ canvasContext: ctx, viewport }).promise;
            stageScale = scale;
        } catch (e: any) {
            error = e?.message ?? String(e);
        }
        stageBusy = false;
    }

    // Re-render whenever the stage becomes visible or its page changes.
    // Every panel that *shows* the stage must be listed here — a panel that
    // is shown but not rendered leaves the user dragging on a blank canvas.
    $effect(() => {
        if (!usesStage(activePanel) || !stageCanvas || !session) return;
        // Touch the reactive deps so the effect re-runs on change.
        void stagePage;
        void session.ops.length;
        renderStage();
    });

    /** Page height in points, for the y-axis flip. */
    function stagePageHeightPt(): number {
        return session?.info.pages[stagePage]?.height_pt ?? 792;
    }

    /**
     * Convert a stage rectangle (CSS pixels, origin top-left) to PDF user
     * space (points, origin bottom-left). Every rectangle tool needs
     * exactly this, so they share it rather than each flipping y again.
     */
    function stageRectToPt(box: { x: number; y: number; w: number; h: number }) {
        const pageH = stagePageHeightPt();
        return {
            x: box.x / stageScale,
            y: pageH - (box.y + box.h) / stageScale,
            w: box.w / stageScale,
            h: box.h / stageScale,
        };
    }

    function stageLocal(ev: MouseEvent): { x: number; y: number } {
        const rect = (stageCanvas as HTMLCanvasElement).getBoundingClientRect();
        return {
            x: Math.max(0, Math.min(ev.clientX - rect.left, rect.width)),
            y: Math.max(0, Math.min(ev.clientY - rect.top, rect.height)),
        };
    }

    function onStageMouseDown(ev: MouseEvent) {
        if (!stageCanvas) return;
        const p = stageLocal(ev);
        // Redaction drags the same rectangle as crop; only what happens on
        // release differs.
        if (usesRect(activePanel)) {
            cropDragging = true;
            cropStart = p;
            cropBox = { x: p.x, y: p.y, w: 0, h: 0 };
        } else if (activePanel === 'textbox') {
            textAnchor = p;
            // PDF origin is bottom-left; the canvas origin is top-left.
            boxX = Math.round(p.x / stageScale);
            boxY = Math.round(stagePageHeightPt() - p.y / stageScale);
            boxPage = stagePage;
        }
    }

    function onStageMouseMove(ev: MouseEvent) {
        if (!cropDragging || !cropStart) return;
        const p = stageLocal(ev);
        cropBox = {
            x: Math.min(cropStart.x, p.x),
            y: Math.min(cropStart.y, p.y),
            w: Math.abs(p.x - cropStart.x),
            h: Math.abs(p.y - cropStart.y),
        };
    }

    function onStageMouseUp() {
        if (!cropDragging || !cropBox) { cropDragging = false; return; }
        cropDragging = false;
        cropStart = null;
        // Ignore a stray click that produced a degenerate rectangle.
        if (cropBox.w < 4 || cropBox.h < 4) { cropBox = null; return; }
        if (activePanel === 'redact') {
            // Each drag banks another rectangle; nothing is applied until
            // the user commits, since redaction is not undoable.
            addRedactRect();
            return;
        }
        const pageH = stagePageHeightPt();
        cropX = Math.round(cropBox.x / stageScale);
        cropY = Math.round(pageH - (cropBox.y + cropBox.h) / stageScale);
        cropW = Math.round(cropBox.w / stageScale);
        cropH = Math.round(cropBox.h / stageScale);
    }

    /** Keep the drawn rectangle in step with typed values. */
    function syncCropBoxFromNumbers() {
        if (activePanel !== 'crop') return;
        const pageH = stagePageHeightPt();
        cropBox = {
            x: cropX * stageScale,
            y: (pageH - cropY - cropH) * stageScale,
            w: cropW * stageScale,
            h: cropH * stageScale,
        };
    }

    // ── Selection ───────────────────────────────────────────────────────
    function clickPage(i: number, ev: MouseEvent) {
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
    }

    function selectAll() {
        if (!session) return;
        selected = new Set(session.origins.map((_, i) => i));
    }
    function selectNone() { selected = new Set(); lastAnchor = null; }
    const selectedSorted = () => [...selected].sort((a, b) => a - b);

    // ── Page operations ─────────────────────────────────────────────────
    async function removeSelected() {
        if (selected.size === 0) return;
        const pages = selectedSorted();
        selected = new Set();
        await applyOp({ kind: 'remove_pages', pages }, i18n.t.pdfeditor.removed);
    }

    async function rotateSelected(degrees: number) {
        const pages = selected.size > 0 ? selectedSorted() : [];
        if (pages.length === 0) return;
        await applyOp({ kind: 'rotate', pages, degrees });
    }

    async function rotateOne(i: number, degrees: number) {
        await applyOp({ kind: 'rotate', pages: [i], degrees });
    }

    async function extractSelected() {
        if (!session || selected.size === 0) return;
        const out = await saveDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            defaultPath: 'extracted.pdf',
        }) as string | null;
        if (!out) return;
        busy = true;
        try {
            // Extract from the *edited* document, not the file on disk, so
            // pending edits are reflected in the extract.
            const tmp = await invoke<string>('pdf_session_export_temp', { id: session.id });
            await invoke('pdf_extract_pages', {
                path: tmp,
                pageIndices: selectedSorted(),
                outPath: out,
            });
            success = `${i18n.t.pdfeditor.extracted} ${out}`;
        } catch (e: any) { error = e?.message ?? String(e); }
        busy = false;
    }

    /** Parse "1-3,7,10-12" (1-based, inclusive) into 0-based indices. */
    function parseRange(spec: string): number[] {
        const out: number[] = [];
        for (const part of spec.split(',')) {
            const t = part.trim();
            if (!t) continue;
            const m = t.match(/^(\d+)\s*-\s*(\d+)$/);
            if (m) {
                const a = parseInt(m[1], 10), b = parseInt(m[2], 10);
                for (let k = Math.min(a, b); k <= Math.max(a, b); k++) out.push(k - 1);
            } else {
                const n = parseInt(t, 10);
                if (!Number.isNaN(n)) out.push(n - 1);
            }
        }
        return out.filter((n) => n >= 0);
    }

    async function pickInsertFile() {
        const p = await openDialog({
            filters: [{ name: 'PDF', extensions: ['pdf'] }],
            multiple: false,
        });
        if (p) insertPath = p as string;
    }

    async function doInsertFrom() {
        if (!insertPath) { error = i18n.t.pdfeditor.pick_file_first; return; }
        await applyOp({
            kind: 'insert_from',
            path: insertPath,
            pages: parseRange(insertRange),
            position: Math.max(0, Math.min(insertAt, session?.origins.length ?? 0)),
        }, i18n.t.pdfeditor.inserted);
        activePanel = null;
    }

    async function doInsertBlank() {
        if (!session) return;
        const at = selected.size > 0 ? selectedSorted()[0] + 1 : session.origins.length;
        const ref = session.info.pages[Math.min(at, session.info.pages.length - 1)];
        await applyOp({
            kind: 'insert_blank',
            position: at,
            width: ref?.width_pt ?? 612,
            height: ref?.height_pt ?? 792,
        }, i18n.t.pdfeditor.blank_inserted);
    }

    async function doPageNumbers() {
        await applyOp({
            kind: 'page_numbers',
            config: {
                position: numPosition,
                font_size: numFontSize,
                format: numFormat,
                start_number: numStart,
                skip_first: numSkipFirst,
            },
        }, i18n.t.pdfeditor.numbers_added);
        activePanel = null;
    }

    function openCropPanel() {
        activePanel = activePanel === 'crop' ? null : 'crop';
        cropBox = null;
        if (activePanel === 'crop' && session) {
            const i = selected.size > 0 ? selectedSorted()[0] : 0;
            stagePage = i;
            const p = session.info.pages[i];
            if (p) { cropX = 0; cropY = 0; cropW = Math.round(p.width_pt); cropH = Math.round(p.height_pt); }
        }
    }

    async function doCrop() {
        if (!session) return;
        const pages = cropAllPages
            ? session.origins.map((_, i) => i)
            : (selected.size > 0 ? selectedSorted() : [0]);
        await applyOp({
            kind: 'crop',
            pages, x: cropX, y: cropY, w: cropW, h: cropH,
        }, i18n.t.pdfeditor.cropped);
        activePanel = null;
    }

    function openTextBoxPanel() {
        activePanel = activePanel === 'textbox' ? null : 'textbox';
        textAnchor = null;
        if (activePanel === 'textbox') {
            boxPage = selected.size > 0 ? selectedSorted()[0] : 0;
            stagePage = boxPage;
        }
    }

    async function doTextBox() {
        if (!boxText.trim()) { error = i18n.t.pdfeditor.text_required; return; }
        await applyOp({
            kind: 'text_box',
            config: {
                page: boxPage,
                x: boxX,
                y: boxY,
                text: boxText,
                font_size: boxSize,
                color: [0, 0, 0],
                line_height: null,
            },
        }, i18n.t.pdfeditor.text_added);
        activePanel = null;
    }

    // ── Drag to reorder ─────────────────────────────────────────────────
    function onDragStart(i: number, ev: DragEvent) {
        dragFrom = i;
        ev.dataTransfer?.setData('text/plain', String(i));
        if (ev.dataTransfer) ev.dataTransfer.effectAllowed = 'move';
    }

    function onDragOver(i: number, ev: DragEvent) {
        ev.preventDefault();
        dragOver = i;
        if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'move';
    }

    async function onDrop(to: number, ev: DragEvent) {
        ev.preventDefault();
        const from = dragFrom;
        dragFrom = null;
        dragOver = null;
        if (from === null || from === to || !session) return;

        // Moving a multi-selection keeps the whole block together.
        const moving = selected.has(from) ? selectedSorted() : [from];
        const rest = session.origins.map((_, i) => i).filter((i) => !moving.includes(i));
        // `to` indexes the pre-move list; translate to a slot in `rest`.
        const before = rest.filter((i) => i < to).length;
        const order = [...rest.slice(0, before), ...moving, ...rest.slice(before)];
        selected = new Set(moving.map((_, k) => before + k));
        await applyOp({ kind: 'reorder', order }, i18n.t.pdfeditor.reordered);
    }

    // ── Keyboard ────────────────────────────────────────────────────────
    function onKeydown(ev: KeyboardEvent) {
        if (!session) return;
        const target = ev.target as HTMLElement | null;
        if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;

        const mod = ev.metaKey || ev.ctrlKey;
        if (mod && ev.key.toLowerCase() === 'z') {
            ev.preventDefault();
            if (ev.shiftKey) redo(); else undo();
        } else if (mod && ev.key.toLowerCase() === 's') {
            ev.preventDefault();
            save(ev.shiftKey);
        } else if (mod && ev.key.toLowerCase() === 'a') {
            ev.preventDefault();
            selectAll();
        } else if (ev.key === 'Delete' || ev.key === 'Backspace') {
            ev.preventDefault();
            removeSelected();
        } else if (ev.key === 'Escape') {
            selectNone();
            activePanel = null;
        }
    }
</script>

<svelte:window on:keydown={onKeydown} />

<div class="pe">
    <!-- Toolbar -->
    <div class="pe-toolbar">
        <button class="pe-btn pe-primary" onclick={openFile} title={i18n.t.pdfeditor.open}>
            <FileUp size={14} /> {i18n.t.pdfeditor.open}
        </button>

        {#if session}
            <span class="pe-sep"></span>

            <button class="pe-btn" disabled={!session.can_undo || busy} onclick={undo} title={i18n.t.pdfeditor.undo}>
                <Undo2 size={14} />
            </button>
            <button class="pe-btn" disabled={!session.can_redo || busy} onclick={redo} title={i18n.t.pdfeditor.redo}>
                <Redo2 size={14} />
            </button>

            <span class="pe-sep"></span>

            <button class="pe-btn pe-danger" disabled={selected.size === 0 || busy} onclick={removeSelected} title={i18n.t.pdfeditor.remove}>
                <Trash2 size={14} />
            </button>
            <button class="pe-btn" disabled={selected.size === 0 || busy} onclick={() => rotateSelected(-90)} title={i18n.t.pdfeditor.rotate_left}>
                <RotateCcw size={14} />
            </button>
            <button class="pe-btn" disabled={selected.size === 0 || busy} onclick={() => rotateSelected(90)} title={i18n.t.pdfeditor.rotate_right}>
                <RotateCw size={14} />
            </button>
            <button class="pe-btn" disabled={selected.size === 0 || busy} onclick={extractSelected} title={i18n.t.pdfeditor.extract}>
                <FileOutput size={14} />
            </button>

            <span class="pe-sep"></span>

            <button class="pe-btn" class:active={activePanel === 'insert'} disabled={busy}
                    onclick={() => activePanel = activePanel === 'insert' ? null : 'insert'} title={i18n.t.pdfeditor.insert_pages}>
                <Copy size={14} />
            </button>
            <button class="pe-btn" disabled={busy} onclick={doInsertBlank} title={i18n.t.pdfeditor.insert_blank}>
                <FilePlus2 size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'crop'} disabled={busy} onclick={openCropPanel} title={i18n.t.pdfeditor.crop}>
                <Crop size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'numbers'} disabled={busy}
                    onclick={() => activePanel = activePanel === 'numbers' ? null : 'numbers'} title={i18n.t.pdfeditor.page_numbers}>
                <Hash size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'textbox'} disabled={busy} onclick={openTextBoxPanel} title={i18n.t.pdfeditor.text_box}>
                <Type size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'redact'} disabled={busy}
                    onclick={() => { activePanel = activePanel === 'redact' ? null : 'redact'; cropBox = null; if (activePanel === 'redact' && selected.size > 0) stagePage = selectedSorted()[0]; }}
                    title={i18n.t.pdfeditor.redact}>
                <EyeOff size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'annots'} disabled={busy}
                    onclick={() => activePanel = activePanel === 'annots' ? null : 'annots'}
                    title={i18n.t.pdfeditor.annotations}>
                <MessageSquare size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'form'} disabled={busy}
                    onclick={() => { activePanel = activePanel === 'form' ? null : 'form'; if (activePanel === 'form' && !formLoaded) loadForm(); }}
                    title={i18n.t.pdfeditor.form}>
                <ClipboardList size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'overprint'} disabled={busy}
                    onclick={() => { activePanel = activePanel === 'overprint' ? null : 'overprint'; cropBox = null; }}
                    title={i18n.t.pdfeditor.overprint}>
                <PenLine size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'region'} disabled={busy}
                    onclick={() => {
                        activePanel = activePanel === 'region' ? null : 'region';
                        cropBox = null;
                        regionReport = null;
                        if (activePanel === 'region' && selected.size > 0) stagePage = selectedSorted()[0];
                    }}
                    title={i18n.t.pdfeditor.region}>
                <LayoutTemplate size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'compress'} disabled={busy}
                    onclick={() => { activePanel = activePanel === 'compress' ? null : 'compress'; compressReport = null; }}
                    title={i18n.t.pdfeditor.compress}>
                <Shrink size={14} />
            </button>
            <button class="pe-btn" class:active={activePanel === 'kindle'} disabled={busy}
                    onclick={() => activePanel = activePanel === 'kindle' ? null : 'kindle'}
                    title={i18n.t.pdfeditor.kindle}>
                <BookOpen size={14} />
            </button>

            <span class="pe-spacer"></span>

            <span class="pe-count">
                {session.info.page_count} {i18n.t.pdfeditor.pages}
                {#if selected.size > 0}· {selected.size} {i18n.t.pdfeditor.selected}{/if}
                {#if session.modified}<span class="pe-dot" title={i18n.t.pdfeditor.unsaved}>●</span>{/if}
            </span>

            <button class="pe-btn" disabled={busy} onclick={doPrint} title={i18n.t.pdfeditor.print}>
                <Printer size={14} />
            </button>
            <button class="pe-btn" disabled={busy} onclick={doShare}
                    title={platformCaps?.system_share_sheet
                        ? i18n.t.pdfeditor.share
                        : i18n.t.pdfeditor.share_reveal}>
                <Share2 size={14} />
            </button>

            <button class="pe-btn" disabled={busy} onclick={() => save(false)} title={i18n.t.pdfeditor.save}>
                <Save size={14} /> {i18n.t.pdfeditor.save}
            </button>
            <button class="pe-btn" disabled={busy} onclick={() => save(true)}>
                {i18n.t.pdfeditor.save_as}
            </button>
        {/if}
    </div>

    <!-- Panels -->
    {#if session && activePanel}
        <div class="pe-panel">
            {#if activePanel === 'numbers'}
                <label>{i18n.t.pdfeditor.position}
                    <select bind:value={numPosition} class="pe-input">
                        <option value="bottom-center">bottom-center</option>
                        <option value="bottom-left">bottom-left</option>
                        <option value="bottom-right">bottom-right</option>
                        <option value="top-center">top-center</option>
                        <option value="top-left">top-left</option>
                        <option value="top-right">top-right</option>
                    </select>
                </label>
                <label>{i18n.t.pdfeditor.format}
                    <select bind:value={numFormat} class="pe-input">
                        <option value="arabic">1, 2, 3</option>
                        <option value="roman">i, ii, iii</option>
                        <option value="page-of">Page 1 of N</option>
                    </select>
                </label>
                <label>{i18n.t.pdfeditor.size} <input type="number" min="6" max="48" bind:value={numFontSize} class="pe-input-sm" /></label>
                <label>{i18n.t.pdfeditor.start_at} <input type="number" min="1" bind:value={numStart} class="pe-input-sm" /></label>
                <label>{i18n.t.pdfeditor.skip_first} <input type="number" min="0" bind:value={numSkipFirst} class="pe-input-sm" /></label>
                <button class="pe-btn pe-go" onclick={doPageNumbers}>{i18n.t.pdfeditor.apply}</button>

            {:else if activePanel === 'crop'}
                <label><input type="checkbox" bind:checked={cropAllPages} /> {i18n.t.pdfeditor.crop_all}</label>
                <label>X <input type="number" bind:value={cropX} oninput={syncCropBoxFromNumbers} class="pe-input-sm" /></label>
                <label>Y <input type="number" bind:value={cropY} oninput={syncCropBoxFromNumbers} class="pe-input-sm" /></label>
                <label>W <input type="number" bind:value={cropW} oninput={syncCropBoxFromNumbers} class="pe-input-sm" /></label>
                <label>H <input type="number" bind:value={cropH} oninput={syncCropBoxFromNumbers} class="pe-input-sm" /></label>
                <span class="pe-hint">{i18n.t.pdfeditor.points_hint}</span>
                <button class="pe-btn pe-go" onclick={doCrop}>{i18n.t.pdfeditor.apply}</button>

            {:else if activePanel === 'textbox'}
                <label>{i18n.t.pdfeditor.page} <input type="number" min="1" max={session.info.page_count}
                    value={boxPage + 1} oninput={(e) => { boxPage = Math.max(0, (+(e.currentTarget as HTMLInputElement).value || 1) - 1); stagePage = boxPage; }}
                    class="pe-input-sm" /></label>
                <label>X <input type="number" bind:value={boxX} class="pe-input-sm" /></label>
                <label>Y <input type="number" bind:value={boxY} class="pe-input-sm" /></label>
                <label>{i18n.t.pdfeditor.size} <input type="number" min="4" max="96" bind:value={boxSize} class="pe-input-sm" /></label>
                <label class="pe-grow">{i18n.t.pdfeditor.text} <input type="text" bind:value={boxText} class="pe-input" /></label>
                <button class="pe-btn pe-go" onclick={doTextBox}>{i18n.t.pdfeditor.apply}</button>

            {:else if activePanel === 'annots'}
                <button class="pe-btn" disabled={busy} onclick={doImportAnnots}>
                    {i18n.t.pdfeditor.annots_import}
                </button>
                <button class="pe-btn" disabled={busy} onclick={doStampAnnots}>
                    {i18n.t.pdfeditor.annots_stamp}
                </button>
                <span class="pe-hint">{i18n.t.pdfeditor.annots_export}:</span>
                <button class="pe-btn-sm" disabled={busy} onclick={() => doExportAnnots('markdown')}>Markdown</button>
                <button class="pe-btn-sm" disabled={busy} onclick={() => doExportAnnots('csv')}>CSV</button>
                <button class="pe-btn-sm" disabled={busy} onclick={() => doExportAnnots('json')}>JSON</button>
                {#if annotCount !== null}
                    <span class="pe-hint">{i18n.t.pdfeditor.annots_in_store}: {annotCount}</span>
                {/if}

            {:else if activePanel === 'form'}
                {#if formFields.length === 0}
                    <span class="pe-hint">{i18n.t.pdfeditor.form_none}</span>
                    <button class="pe-btn" disabled={busy} onclick={loadForm}>{i18n.t.pdfeditor.form_reload}</button>
                {:else}
                    <div class="pe-formgrid">
                        {#each formFields as f (f.name)}
                            <label class="pe-formrow">
                                <span class="pe-formname" title={f.name}>
                                    {f.name}{#if f.required}<span class="pe-req">*</span>{/if}
                                </span>
                                {#if f.kind === 'checkbox' || f.kind === 'radio'}
                                    <input type="checkbox" disabled={f.read_only}
                                        checked={!!formEdits[f.name] && formEdits[f.name].toLowerCase() !== 'off'}
                                        onchange={(e) => formEdits[f.name] = (e.currentTarget as HTMLInputElement).checked ? 'true' : 'false'} />
                                {:else if f.options.length > 0}
                                    <select class="pe-input" disabled={f.read_only} bind:value={formEdits[f.name]}>
                                        {#each f.options as o}<option value={o}>{o}</option>{/each}
                                    </select>
                                {:else}
                                    <input type="text" class="pe-input" disabled={f.read_only}
                                        bind:value={formEdits[f.name]} />
                                {/if}
                            </label>
                        {/each}
                    </div>
                    <button class="pe-btn pe-go" disabled={busy} onclick={() => doFillForm(false)}>
                        {i18n.t.pdfeditor.form_save}
                    </button>
                    <button class="pe-btn" disabled={busy} onclick={() => doFillForm(true)}
                            title={i18n.t.pdfeditor.form_flatten_hint}>
                        {i18n.t.pdfeditor.form_flatten}
                    </button>
                {/if}

            {:else if activePanel === 'overprint'}
                <label class="pe-grow">{i18n.t.pdfeditor.text}
                    <input type="text" class="pe-input" bind:value={overprintText} /></label>
                <label>{i18n.t.pdfeditor.size}
                    <input type="number" min="4" max="96" bind:value={overprintSize} class="pe-input-sm" /></label>
                <button class="pe-btn pe-go" disabled={busy || !cropBox} onclick={doOverprint}>
                    {i18n.t.pdfeditor.apply}
                </button>
                <span class="pe-hint pe-warn">{i18n.t.pdfeditor.overprint_caveat}</span>

            {:else if activePanel === 'region'}
                <label class="pe-grow">{i18n.t.pdfeditor.text}
                    <textarea class="pe-input pe-textarea" rows="2" bind:value={regionText}></textarea>
                </label>
                <label>{i18n.t.pdfeditor.region_font}
                    <select class="pe-input" bind:value={regionFont}>
                        {#each REGION_FONTS as f}<option value={f}>{f}</option>{/each}
                    </select>
                </label>
                <label>{i18n.t.pdfeditor.size}
                    <input type="number" min="4" max="96" step="0.5" bind:value={regionSize} class="pe-input-sm" /></label>
                <label>{i18n.t.pdfeditor.region_align}
                    <select class="pe-input" bind:value={regionAlign}>
                        <option value="left">left</option>
                        <option value="center">center</option>
                        <option value="right">right</option>
                        <option value="justify">justify</option>
                    </select>
                </label>
                <label>{i18n.t.pdfeditor.region_valign}
                    <select class="pe-input" bind:value={regionVAlign}>
                        <option value="top">top</option>
                        <option value="middle">middle</option>
                        <option value="bottom">bottom</option>
                    </select>
                </label>
                <label>{i18n.t.pdfeditor.region_line_height}
                    <input type="number" min="0.8" max="3" step="0.1" bind:value={regionLineHeight} class="pe-input-sm" /></label>
                <label>{i18n.t.pdfeditor.region_color}
                    <input type="color" bind:value={regionColor} class="pe-input-color" /></label>
                <label><input type="checkbox" bind:checked={regionBorder} />
                    {i18n.t.pdfeditor.region_border}</label>
                <button class="pe-btn pe-go" disabled={busy || !cropBox} onclick={doDrawRegion}>
                    {i18n.t.pdfeditor.region_apply}
                </button>
                {#if regionReport}
                    <span class="pe-hint">
                        {i18n.t.pdfeditor.region_fit.replace('{n}', String(regionReport.lines))}
                    </span>
                    {#if regionReport.overflow}
                        <span class="pe-hint pe-warn">
                            <AlertTriangle size={12} />
                            {i18n.t.pdfeditor.region_overflow
                                .replace('{n}', String(regionReport.lines_dropped))}
                        </span>
                    {/if}
                    {#if regionReport.unsupported_chars.length > 0}
                        <span class="pe-hint pe-warn">
                            <AlertTriangle size={12} />
                            {i18n.t.pdfeditor.region_unsupported
                                .replace('{chars}', regionReport.unsupported_chars.join(' '))}
                        </span>
                    {/if}
                {/if}

            {:else if activePanel === 'compress'}
                <label><input type="checkbox" bind:checked={compressObjectStreams} />
                    {i18n.t.pdfeditor.compress_object_streams}</label>
                <button class="pe-btn pe-go" disabled={busy} onclick={doCompress}>
                    {i18n.t.pdfeditor.compress_apply}
                </button>
                <span class="pe-hint">{i18n.t.pdfeditor.compress_images_hint}</span>
                {#if compressReport}
                    <span class="pe-hint">
                        {#if compressReport.bytes_after < compressReport.bytes_before}
                            {i18n.t.pdfeditor.compress_result
                                .replace('{before}', formatBytes(compressReport.bytes_before))
                                .replace('{after}', formatBytes(compressReport.bytes_after))
                                .replace('{pct}', (100 * (1 - compressReport.bytes_after / compressReport.bytes_before)).toFixed(1))}
                        {:else}
                            {i18n.t.pdfeditor.compress_no_gain}
                        {/if}
                    </span>
                {/if}

            {:else if activePanel === 'kindle'}
                <button class="pe-btn" disabled={busy} onclick={pickClippings}>
                    {i18n.t.pdfeditor.kindle_choose}
                </button>
                <span class="pe-hint">{clippingsPath || i18n.t.pdfeditor.no_file}</span>
                {#if kindleBooks.length > 0}
                    <label>{i18n.t.pdfeditor.kindle_book}
                        <select class="pe-input" bind:value={kindleTitle}>
                            {#each kindleBooks as b}<option value={b}>{b}</option>{/each}
                        </select>
                    </label>
                    <label><input type="checkbox" bind:checked={kindleAnchor} />
                        {i18n.t.pdfeditor.kindle_anchor}</label>
                    <button class="pe-btn pe-go" disabled={busy} onclick={doKindleImport}>
                        {i18n.t.pdfeditor.kindle_import}
                    </button>
                {/if}
                {#if kindleSummary}
                    <span class="pe-hint">
                        {i18n.t.pdfeditor.kindle_result
                            .replace('{imported}', String(kindleSummary.imported))
                            .replace('{matched}', String(kindleSummary.matched))
                            .replace('{dupes}', String(kindleSummary.duplicates_skipped))}
                    </span>
                {/if}

            {:else if activePanel === 'redact'}
                <span class="pe-hint">{i18n.t.pdfeditor.redact_marked}: {redactRects.length}</span>
                <button class="pe-btn" disabled={redactRects.length === 0} onclick={clearRedactRects}>
                    {i18n.t.pdfeditor.redact_clear}
                </button>
                <button class="pe-btn pe-go pe-danger" disabled={redactRects.length === 0 || busy} onclick={doRedact}>
                    {i18n.t.pdfeditor.redact_apply}
                </button>
                <span class="pe-hint pe-warn">{i18n.t.pdfeditor.redact_caveat}</span>

            {:else if activePanel === 'insert'}
                <button class="pe-btn" onclick={pickInsertFile}>{i18n.t.pdfeditor.choose_pdf}</button>
                <span class="pe-hint">{insertPath || i18n.t.pdfeditor.no_file}</span>
                <label>{i18n.t.pdfeditor.range} <input type="text" bind:value={insertRange} placeholder="1-3,7" class="pe-input-sm" /></label>
                <label>{i18n.t.pdfeditor.at_position} <input type="number" min="0" bind:value={insertAt} class="pe-input-sm" /></label>
                <button class="pe-btn pe-go" onclick={doInsertFrom}>{i18n.t.pdfeditor.apply}</button>
            {/if}
        </div>
    {/if}

    <!-- Interactive stage: drag a crop rectangle / click to place text -->
    {#if session && usesStage(activePanel)}
        <div class="pe-stage-wrap">
            <div class="pe-stage-bar">
                <span class="pe-hint">
                    {activePanel === 'crop'
                        ? i18n.t.pdfeditor.drag_crop_hint
                        : activePanel === 'redact'
                            ? i18n.t.pdfeditor.drag_redact_hint
                            : activePanel === 'overprint'
                                ? i18n.t.pdfeditor.drag_overprint_hint
                                : activePanel === 'region'
                                    ? i18n.t.pdfeditor.drag_region_hint
                                    : i18n.t.pdfeditor.click_text_hint}
                </span>
                <span class="pe-spacer"></span>
                <button class="pe-mini" disabled={stagePage === 0}
                        onclick={() => stagePage = Math.max(0, stagePage - 1)}>‹</button>
                <span class="pe-hint">{i18n.t.pdfeditor.page} {stagePage + 1} / {session.info.page_count}</span>
                <button class="pe-mini" disabled={stagePage >= session.info.page_count - 1}
                        onclick={() => stagePage = Math.min((session?.info.page_count ?? 1) - 1, stagePage + 1)}>›</button>
            </div>
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
                class="pe-stage"
                bind:this={stageEl}
                onmousedown={onStageMouseDown}
                onmousemove={onStageMouseMove}
                onmouseup={onStageMouseUp}
                onmouseleave={onStageMouseUp}
            >
                <canvas bind:this={stageCanvas} class="pe-stage-canvas"></canvas>
                {#if usesRect(activePanel) && cropBox}
                    <div class="pe-croprect"
                         style:left="{cropBox.x}px" style:top="{cropBox.y}px"
                         style:width="{cropBox.w}px" style:height="{cropBox.h}px"></div>
                {/if}
                {#if activePanel === 'redact'}
                    {#each redactRects.filter((r) => r.page === stagePage) as r}
                        <div class="pe-redactrect"
                             style:left="{r.x * stageScale}px"
                             style:top="{(stagePageHeightPt() - r.y - r.h) * stageScale}px"
                             style:width="{r.w * stageScale}px"
                             style:height="{r.h * stageScale}px"></div>
                    {/each}
                {/if}
                {#if activePanel === 'textbox' && textAnchor}
                    <div class="pe-textpin" style:left="{textAnchor.x}px" style:top="{textAnchor.y}px">
                        <span class="pe-textpin-dot"></span>
                        <span class="pe-textpin-label" style:font-size="{Math.max(8, boxSize * stageScale)}px">
                            {boxText || i18n.t.pdfeditor.text}
                        </span>
                    </div>
                {/if}
                {#if stageBusy}<div class="pe-stage-busy"><Loader2 size={20} class="spin" /></div>{/if}
            </div>
        </div>
    {/if}

    <!-- Status -->
    {#if error}<div class="pe-status pe-err"><X size={13} /> {error}</div>{/if}
    {#if success && !error}<div class="pe-status pe-ok"><Check size={13} /> {success}</div>{/if}
    {#if busy}<div class="pe-status pe-busy"><Loader2 size={13} class="spin" /> {i18n.t.pdfeditor.working}</div>{/if}
    {#if redactReport}
        <div class="pe-status pe-report">
            <span>
                {i18n.t.pdfeditor.redact_removed
                    .replace('{glyphs}', String(redactReport.glyphs_removed))
                    .replace('{pages}', String(redactReport.pages_changed))}
            </span>
            {#if redactReport.runs_dropped > 0}
                <span class="pe-warn">
                    <AlertTriangle size={12} />
                    {i18n.t.pdfeditor.redact_runs_dropped.replace('{n}', String(redactReport.runs_dropped))}
                </span>
            {/if}
            {#each redactReport.warnings as w}
                <span class="pe-warn"><AlertTriangle size={12} /> {w}</span>
            {/each}
        </div>
    {/if}

    <!-- Page grid -->
    <div class="pe-body">
        {#if !session}
            <div class="pe-empty">
                <FileUp size={44} strokeWidth={1} />
                <p>{i18n.t.pdfeditor.empty_hint}</p>
                <button class="pe-btn pe-primary" onclick={openFile}>{i18n.t.pdfeditor.open}</button>
            </div>
        {:else}
            <div class="pe-selbar">
                <button class="pe-link" onclick={selectAll}>{i18n.t.pdfeditor.select_all}</button>
                <button class="pe-link" onclick={selectNone}>{i18n.t.pdfeditor.select_none}</button>
                <span class="pe-hint">{i18n.t.pdfeditor.drag_hint}</span>
            </div>
            <div class="pe-grid">
                {#each session.origins as origin, i (`${origin.source ?? 'blank'}#${origin.index ?? 'x'}#${i}`)}
                    <div
                        class="pe-tile"
                        class:selected={selected.has(i)}
                        class:dragover={dragOver === i}
                        draggable="true"
                        role="button"
                        tabindex="0"
                        ondragstart={(e) => onDragStart(i, e)}
                        ondragover={(e) => onDragOver(i, e)}
                        ondragleave={() => dragOver = null}
                        ondrop={(e) => onDrop(i, e)}
                        ondragend={() => { dragFrom = null; dragOver = null; }}
                        onclick={(e) => clickPage(i, e)}
                        onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); clickPage(i, e as unknown as MouseEvent); } }}
                    >
                        <div class="pe-thumb">
                            {#if thumbs[i]}
                                <img src={thumbs[i]} alt={`${i18n.t.pdfeditor.page} ${i + 1}`} />
                            {:else}
                                <div class="pe-thumb-ph"><Loader2 size={18} class="spin" /></div>
                            {/if}
                            <div class="pe-tile-ops">
                                <button class="pe-mini" title={i18n.t.pdfeditor.rotate_left}
                                        onclick={(e) => { e.stopPropagation(); rotateOne(i, -90); }}>
                                    <RotateCcw size={11} />
                                </button>
                                <button class="pe-mini" title={i18n.t.pdfeditor.rotate_right}
                                        onclick={(e) => { e.stopPropagation(); rotateOne(i, 90); }}>
                                    <RotateCw size={11} />
                                </button>
                                <button class="pe-mini pe-mini-danger" title={i18n.t.pdfeditor.remove}
                                        onclick={(e) => { e.stopPropagation(); applyOp({ kind: 'remove_pages', pages: [i] }); }}>
                                    <Trash2 size={11} />
                                </button>
                            </div>
                        </div>
                        <div class="pe-tile-label">
                            <span class="pe-pageno">{i + 1}</span>
                            {#if !origin.source}<span class="pe-badge">{i18n.t.pdfeditor.blank}</span>
                            {:else if origin.source !== session.source}<span class="pe-badge pe-badge-alt">{i18n.t.pdfeditor.imported}</span>{/if}
                        </div>
                    </div>
                {/each}
            </div>
        {/if}
    </div>
</div>

<style>
    .pe { display: flex; flex-direction: column; height: 100%; background: #09090b; color: #e4e4e7; }

    .pe-toolbar {
        display: flex; align-items: center; gap: 4px; flex-wrap: wrap;
        padding: 8px 10px; border-bottom: 1px solid #27272a; background: #0c0c0f;
    }
    .pe-sep { width: 1px; height: 18px; background: #27272a; margin: 0 4px; }
    .pe-spacer { flex: 1; }
    .pe-count { font-size: 12px; color: #a1a1aa; margin-right: 8px; }
    .pe-dot { color: #f59e0b; margin-left: 6px; }

    .pe-btn {
        display: inline-flex; align-items: center; gap: 5px;
        padding: 5px 9px; font-size: 12px; border-radius: 6px;
        border: 1px solid #27272a; background: #18181b; color: #e4e4e7; cursor: pointer;
    }
    .pe-btn:hover:not(:disabled) { background: #27272a; }
    .pe-btn:disabled { opacity: 0.4; cursor: default; }
    .pe-btn.active { background: #1e3a5f; border-color: #2563eb; }
    .pe-primary { background: #1d4ed8; border-color: #1d4ed8; color: #fff; }
    .pe-primary:hover:not(:disabled) { background: #2563eb; }
    .pe-danger:hover:not(:disabled) { background: #7f1d1d; border-color: #b91c1c; }
    .pe-go { background: #15803d; border-color: #15803d; color: #fff; }

    .pe-panel {
        display: flex; align-items: center; gap: 12px; flex-wrap: wrap;
        padding: 8px 12px; background: #111114; border-bottom: 1px solid #27272a; font-size: 12px;
    }
    .pe-panel label { display: inline-flex; align-items: center; gap: 5px; color: #a1a1aa; }
    .pe-grow { flex: 1; min-width: 200px; }
    .pe-grow .pe-input { flex: 1; }
    .pe-input, .pe-input-sm, .pe-panel select {
        background: #18181b; border: 1px solid #27272a; border-radius: 5px;
        color: #e4e4e7; padding: 4px 7px; font-size: 12px;
    }
    .pe-input-sm { width: 68px; }
    .pe-textarea { resize: vertical; min-height: 32px; font-family: inherit; }
    .pe-input-color {
        width: 34px; height: 24px; padding: 1px; cursor: pointer;
        background: #18181b; border: 1px solid #27272a; border-radius: 5px;
    }
    .pe-hint { color: #71717a; font-size: 11px; }

    .pe-status {
        display: flex; align-items: center; gap: 6px;
        padding: 6px 12px; font-size: 12px; border-bottom: 1px solid #27272a;
    }
    .pe-err { background: #2a1215; color: #fca5a5; }
    .pe-ok { background: #0f2417; color: #86efac; }
    .pe-busy { background: #111827; color: #93c5fd; }

    .pe-stage-wrap {
        border-bottom: 1px solid #27272a; background: #0c0c0f;
        padding: 8px 12px 12px; max-height: 55vh; overflow: auto;
    }
    .pe-stage-bar { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
    .pe-stage {
        position: relative; display: inline-block; line-height: 0;
        cursor: crosshair; user-select: none; background: #fff;
        box-shadow: 0 1px 8px rgba(0, 0, 0, 0.5);
    }
    .pe-stage-canvas { display: block; }
    .pe-stage-busy {
        position: absolute; inset: 0; display: flex;
        align-items: center; justify-content: center;
        background: rgba(9, 9, 11, 0.45); color: #93c5fd;
    }
    .pe-croprect {
        position: absolute; pointer-events: none;
        border: 1.5px solid #22c55e; background: rgba(34, 197, 94, 0.14);
        box-shadow: 0 0 0 9999px rgba(9, 9, 11, 0.42);
    }
    .pe-redactrect {
        position: absolute; pointer-events: none;
        background: rgba(9, 9, 11, 0.88);
        border: 1.5px solid #ef4444;
    }
    .pe-report {
        display: flex; flex-direction: column; gap: 3px; align-items: flex-start;
        background: #1a1206; color: #fcd34d;
    }
    .pe-warn { display: inline-flex; align-items: center; gap: 4px; color: #fbbf24; }

    .pe-formgrid {
        display: flex; flex-direction: column; gap: 4px;
        max-height: 220px; overflow-y: auto; flex: 1 1 320px; min-width: 260px;
    }
    .pe-formrow { display: flex; align-items: center; gap: 8px; }
    .pe-formname {
        flex: 0 0 150px; overflow: hidden; text-overflow: ellipsis;
        white-space: nowrap; color: #a1a1aa;
    }
    .pe-formrow .pe-input { flex: 1; }
    .pe-req { color: #f87171; margin-left: 2px; }

    .pe-textpin { position: absolute; pointer-events: none; }
    .pe-textpin-dot {
        position: absolute; left: -3px; top: -3px;
        width: 6px; height: 6px; border-radius: 50%; background: #2563eb;
    }
    .pe-textpin-label {
        position: absolute; left: 2px; bottom: 0; white-space: pre;
        color: #111; line-height: 1.1; opacity: 0.75;
    }

    .pe-body { flex: 1; overflow-y: auto; padding: 12px; }
    .pe-empty {
        display: flex; flex-direction: column; align-items: center; justify-content: center;
        gap: 14px; height: 100%; color: #52525b;
    }

    .pe-selbar { display: flex; align-items: center; gap: 12px; margin-bottom: 10px; }
    .pe-link {
        background: none; border: none; color: #60a5fa; font-size: 12px;
        cursor: pointer; padding: 0;
    }
    .pe-link:hover { text-decoration: underline; }

    .pe-grid {
        display: grid; gap: 12px;
        grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    }
    .pe-tile {
        border: 2px solid transparent; border-radius: 8px; padding: 6px;
        background: #111114; cursor: grab; user-select: none;
    }
    .pe-tile:hover { background: #17171b; }
    .pe-tile.selected { border-color: #2563eb; background: #14203a; }
    .pe-tile.dragover { border-color: #22c55e; }
    .pe-tile:active { cursor: grabbing; }

    .pe-thumb { position: relative; background: #fff; border-radius: 4px; overflow: hidden; }
    .pe-thumb img { display: block; width: 100%; height: auto; }
    .pe-thumb-ph {
        display: flex; align-items: center; justify-content: center;
        min-height: 180px; color: #71717a; background: #18181b;
    }
    .pe-tile-ops {
        position: absolute; top: 4px; right: 4px; display: none; gap: 3px;
    }
    .pe-tile:hover .pe-tile-ops { display: flex; }
    .pe-mini {
        display: inline-flex; align-items: center; justify-content: center;
        width: 20px; height: 20px; border-radius: 4px; cursor: pointer;
        border: 1px solid #3f3f46; background: rgba(9, 9, 11, 0.85); color: #e4e4e7;
    }
    .pe-mini:hover { background: #27272a; }
    .pe-mini-danger:hover { background: #7f1d1d; border-color: #b91c1c; }

    .pe-tile-label {
        display: flex; align-items: center; gap: 6px;
        margin-top: 5px; font-size: 11px; color: #a1a1aa;
    }
    .pe-pageno { font-variant-numeric: tabular-nums; }
    .pe-badge {
        padding: 1px 5px; border-radius: 3px; font-size: 10px;
        background: #27272a; color: #a1a1aa;
    }
    .pe-badge-alt { background: #1e3a5f; color: #93c5fd; }

    :global(.spin) { animation: pe-spin 1s linear infinite; }
    @keyframes pe-spin { to { transform: rotate(360deg); } }
</style>
