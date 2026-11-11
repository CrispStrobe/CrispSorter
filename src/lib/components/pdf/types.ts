// Wire types shared by the PDF editor and its tool panels.
// They mirror src-tauri/src/pdf_session.rs and pdf_annots.rs.

export interface PdfPageInfo { page_number: number; width_pt: number; height_pt: number; rotation: number; }
export interface PdfInfo {
    page_count: number; pages: PdfPageInfo[];
    title?: string; author?: string; subject?: string; keywords?: string;
    producer?: string; creator?: string;
}
export interface PageOrigin {
    source: string | null;
    index: number | null;
    rotation: number;
    dirty: boolean;
}
export interface SessionState {
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

export interface PdfAnnotationInput {
    page: number;
    x: number; y: number; w: number; h: number;
    ann_type: 'highlight' | 'ink' | 'rectangle' | 'note' | 'freetext';
    text: string;
    color: string;
    author: string | null;
    quads: number[];
    ink: number[][];
    stroke_width: number | null;
    font_size: number | null;
}

/** Panels that only need the options bar (no drawing on the page). */
export type DocTool =
    | 'insert' | 'merge' | 'split' | 'numbers' | 'watermark' | 'metadata'
    | 'encrypt' | 'decrypt' | 'sanitise' | 'compress' | 'form' | 'kindle'
    | 'annots' | 'images';

/** What the editor lends a tool panel: shared busy/error/log plumbing. */
export interface DocToolContext {
    /** Run an action with busy state, error banner and app-log entries. */
    run<T>(label: string, fn: () => Promise<T>): Promise<T | undefined>;
    /** Apply an undoable edit-session op. Resolves true on success. */
    apply(op: Record<string, unknown>, label: string, note: string): Promise<boolean>;
    /** Temp copy of the edited document, for one-shot file tools. */
    exportTemp(): Promise<string>;
    /** Success banner; with `openPath` it offers to open the result. */
    notify(msg: string, openPath?: string): void;
    /** Error banner + log entry for a validation failure. */
    fail(label: string, msg: string): void;
    /** PNG bytes of one page of the edited document. */
    renderPagePng(index: number, dpi: number): Promise<Uint8Array>;
    /** Open another PDF in the editor (replaces the session). */
    openPath(path: string): Promise<void>;
    close(): void;
}

/** Normalise any multiple of 90 to 0/90/180/270 (the backend's domain). */
export function normaliseRotation(degrees: number): number {
    return ((Math.round(degrees) % 360) + 360) % 360;
}

/**
 * New page order that moves `moving` (indices, any order) so that the
 * block lands before position `to` of the *current* list. Returns null when
 * nothing would change.
 */
export function moveOrder(count: number, moving: number[], to: number): number[] | null {
    const block = [...new Set(moving)].filter((i) => i >= 0 && i < count).sort((a, b) => a - b);
    if (block.length === 0) return null;
    const rest = Array.from({ length: count }, (_, i) => i).filter((i) => !block.includes(i));
    const before = rest.filter((i) => i < Math.max(0, Math.min(count, to))).length;
    const order = [...rest.slice(0, before), ...block, ...rest.slice(before)];
    return order.every((v, i) => v === i) ? null : order;
}

/** Parse "1-3,7,10-12" (1-based, inclusive) into 0-based indices. */
export function parseRange(spec: string): number[] {
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

/** Split ranges for `pdf_split`: 0-based start, exclusive end. */
export function parseSplitRanges(spec: string): [number, number][] | null {
    const out: [number, number][] = [];
    for (const part of spec.split(',')) {
        const t = part.trim();
        if (!t) continue;
        const m = t.match(/^(\d+)\s*(?:-\s*(\d+))?$/);
        if (!m) return null;
        const a = parseInt(m[1], 10);
        const b = m[2] ? parseInt(m[2], 10) : a;
        if (a < 1 || b < a) return null;
        out.push([a - 1, b]);
    }
    return out.length ? out : null;
}

export function errorMessage(e: unknown): string {
    if (e && typeof e === 'object' && 'message' in e && typeof (e as any).message === 'string') return (e as any).message;
    return String(e);
}
