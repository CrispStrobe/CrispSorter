// src/lib/batch/store.svelte.ts:

import { get, writable } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { readFile } from '@tauri-apps/plugin-fs';
import { join } from '@tauri-apps/api/path';
import { getSetting, saveSetting } from '../store';
import { llmClient } from '../llm/client';
import { getWebLLMLoadedModel } from '../llm/webllm';
import { getORTLoadedModel } from '../llm/ort';
import type { BatchItem, Metadata } from '../types';
import type { BatchSession } from '../types';
import { extractText, AUDIO_EXTENSIONS } from '../extractors';
import { flog, logDebug } from '../log';

export interface ProcessOverrides {
    providerId?: string;
    modelId?: string;
    maxChars?: number;
    authorSort?: boolean;
    enforceOcr?: boolean;
    extractionOnly?: boolean;
}

/** Recognise the family of "no real value here" strings the LLM
 *  produces when it can't extract an author/title/year. The default
 *  prompt instructs it to output `Unknown Author` etc., but smaller
 *  models drift to "Unknown", "unknown", "n.n.", "N.N.", "?", "-",
 *  or a single bracket character. We never want any of those to
 *  trigger the auto-accept green check on the Stapel row. */
export function isUnknownSentinel(v: string | null | undefined): boolean {
    if (!v) return true;
    const u = v.trim().toLowerCase();
    if (u.length === 0) return true;
    if (u.length <= 2) return true; // "?", "-", "n.", "[]", etc.
    if (u === 'unknown' || u === 'unbekannt') return true;
    if (u.startsWith('unknown ') || u.startsWith('unbekannt ')) return true; // "Unknown Author", "Unbekannt Autor"
    if (u === 'n.n.' || u === 'nn' || u === 'n. n.') return true;            // anonymous-author conventions
    if (u === 'anonymous' || u === 'anonym') return true;
    if (u === 'none' || u === 'null' || u === 'n/a' || u === 'na') return true;
    if (u === '0000') return true;                                           // year sentinel
    return false;
}

export function getDefaultPrompt(format: 'xml' | 'json', lang: string = 'en') {
    if (format === 'json') {
        return lang === 'de' 
            ? 'Extrahiere Metadaten im JSON-Format: {"title": "...", "author": "...", "year": "..."}. Nutze "Unknown Title" falls unbekannt.'
            : 'Extract metadata in JSON format: {"title": "...", "author": "...", "year": "..."}. Use "Unknown Title" if unknown.';
    }
    return lang === 'de'
        ? 'Du bist ein Assistent zur Extraktion von Metadaten. Extrahiere bibliographische Informationen aus dem bereitgestellten Text und Dateinamen.\nGib das Ergebnis STRENG in diesem XML-Format aus:\n<METADATA>\n  <TITLE>Titel des Dokuments</TITLE>\n  <AUTHOR>Nachname, Vorname</AUTHOR>\n  <YEAR>YYYY</YEAR>\n  <LANGUAGE>de/en/...</LANGUAGE>\n</METADATA>\nNutze "Unknown Title" falls nicht ermittelbar.'
        : 'You are a metadata extraction assistant. Extract bibliographic metadata from the provided document text and filename.\nOutput the result STRICTLY in this XML format:\n<METADATA>\n  <TITLE>Document Title</TITLE>\n  <AUTHOR>Lastname, Firstname</AUTHOR>\n  <YEAR>YYYY</YEAR>\n  <LANGUAGE>en/de/...</LANGUAGE>\n</METADATA>\nUse "Unknown Title" if indeterminable.';
}

export class BatchManager {
    items = $state<BatchItem[]>([]);
    isProcessing = $state(false);
    isExecuting = $state(false);
    searchQuery = $state('');
    filterExtension = $state('all');
    filterStatus = $state('all');
    filterMinSize = $state(0);
    isMetadataExtractionEnabled = $state(true);
    /** Live worker counts -- bound to the bottom-left throughput chip
     *  in +page.svelte. Updated as workers enter / leave their critical
     *  section. `*Active` is "currently in flight"; `*Done` is the
     *  cumulative count for the current `processAll` run.
     *  `runStartTs` resets each `processAll`; the UI uses it +
     *  `extractionDone` / `llmDone` to compute docs/min. */
    extractionActive = $state(0);
    llmActive = $state(0);
    extractionDone = $state(0);
    llmDone = $state(0);
    extractionTargetWorkers = $state(1);
    llmTargetWorkers = $state(1);
    runStartTs = $state(0);
    private stopRequested = false;
    private extractionAbort: AbortController | null = null;
    private llmAbort: AbortController | null = null;

    filteredItems = $derived.by(() => {
        return this.items.filter(item => {
            const matchesSearch = item.originalName.toLowerCase().includes(this.searchQuery.toLowerCase()) ||
                                item.suggestedTitle?.toLowerCase().includes(this.searchQuery.toLowerCase()) ||
                                item.suggestedAuthor?.toLowerCase().includes(this.searchQuery.toLowerCase());
            const matchesExt = this.filterExtension === 'all' || item.extension.toLowerCase() === this.filterExtension.toLowerCase();
            const matchesStatus = this.filterStatus === 'all' || item.status === this.filterStatus;
            const matchesSize = item.size >= this.filterMinSize * 1024;
            return matchesSearch && matchesExt && matchesStatus && matchesSize;
        });
    });

    addItem(path: string, name: string, size: number) {
        const norm = (p: string) => p.replace(/\\/g, '/').toLowerCase();
        if (this.items.some(i => norm(i.originalPath) === norm(path))) return; // skip duplicates (case-insensitive, cross-separator)
        const id = crypto.randomUUID();
        const extension = name.split('.').pop() || '';
        this.items.push({
            id,
            originalPath: path,
            originalName: name,
            size,
            status: 'queued',
            extension,
            modifiedAt: Date.now(),
            // New items start UNaccepted. They auto-flip to accepted only
            // once an Author is recognised (see `markReviewed`); files
            // whose extraction never produces an Author stay unaccepted
            // and require an explicit user click to be included in a sort.
            isAccepted: false,
            isIgnored: false
        });
        this.saveCurrentSession();
    }

    async removeItems(ids: string[]) {
        this.items = this.items.filter(i => !ids.includes(i.id));
        await this.saveCurrentSession();
    }

    async clear() {
        if (this.isProcessing) return;
        this.items = [];
        await this.saveCurrentSession();
    }

    private lastStopAtMs = 0;
    stopAll() {
        this.stopRequested = true;
        this.extractionAbort?.abort();
        this.llmAbort?.abort();

        // First click: signal stop and wait for workers to drain.
        // For CPU-bound extractors (pdfjs-dist on a 50+ MB PDF) the
        // abort signal is honored only at the next page boundary,
        // so the worker can be busy for ~30–60 s before the finally
        // runs and `isProcessing` flips false.  Users repeatedly
        // clicking Stop think the button is broken.
        //
        // Second click within 5 s: force-finalize the UI state
        // immediately.  The workers will still run to a clean exit
        // in the background (they own the AbortControllers, no
        // dangling subprocesses), but the toolbar unfreezes now —
        // the user can start a fresh batch without waiting.
        const now = Date.now();
        if (this.isProcessing && now - this.lastStopAtMs < 5000) {
            flog('warn', 'Stop forced — second click within 5 s. Workers still draining in the background.');
            this.isProcessing = false;
            this.stopRequested = false;
            this.llmAbort = null;
            this.extractionAbort = null;
            this.extractionActive = 0;
            this.llmActive = 0;
        } else {
            flog('info', 'Stop requested by user — aborting extraction and LLM. Click Stop again to force.');
        }
        this.lastStopAtMs = now;
    }

    /** Reset all items in 'review' (or any given statuses) back to 'queued'
     *  so processAll() will re-extract + re-analyse them.
     *  Useful when the user wants to re-run with a different model / prompt
     *  or after manually editing metadata on some items. */
    async resetToQueued(ids?: string[]) {
        const targets = ids
            ? this.items.filter(i => ids.includes(i.id))
            : this.items;
        for (const item of targets) {
            if (['review', 'error', 'unfinished', 'ready'].includes(item.status)) {
                item.status = 'queued';
                item.errorMessage = undefined;
                item.statusDetail = undefined;
            }
        }
        await this.saveCurrentSession();
    }

    resetStuckItems() {
        for (const item of this.items) {
            if (item.status === 'extracting' || item.status === 'analyzing' || item.status === 'unfinished') {
                item.status = 'queued';
                item.statusDetail = undefined;
            }
        }
        this.saveCurrentSession();
    }

    /** Nuclear escape hatch — when the graceful Stop path is wedged and
     *  even the force-stop second-click within 5 s didn't help (i.e.
     *  the user thinks the UI is taken down by a deeper hang),
     *  this combines all the recovery primitives in one click:
     *
     *  1. Force-finalize the processAll/executeBatch state machine
     *     (isProcessing / isExecuting flags, abort controllers, worker
     *     counters) so the toolbar unfreezes.
     *  2. Lift every `extracting` / `analyzing` / `unfinished` item
     *     back to `queued` so they survive the next Start press.
     *  3. Clear any leftover statusDetail / errorMessage stuck on items.
     *
     *  Workers that were genuinely mid-flight in the background will
     *  hit `stopRequested` or their abort signal at the next yield and
     *  exit; nothing here can leak a subprocess (the Rust side owns
     *  its own scoped tasks). */
    forceReset() {
        flog('warn', 'Force reset — abandoning in-flight workers, lifting stuck items back to queued.');
        // 1. Force-finalize the state machine.  Abort controllers
        // first so any awaiting worker wakes up before we null them.
        this.extractionAbort?.abort();
        this.llmAbort?.abort();
        this.stopRequested = true;
        this.isProcessing = false;
        this.isExecuting = false;
        this.llmAbort = null;
        this.extractionAbort = null;
        this.extractionActive = 0;
        this.llmActive = 0;
        this.lastStopAtMs = 0;
        // 2. Lift stuck items.
        for (const item of this.items) {
            if (item.status === 'extracting' || item.status === 'analyzing' || item.status === 'unfinished') {
                item.status = 'queued';
                item.statusDetail = undefined;
                item.errorMessage = undefined;
            }
        }
        // 3. Persist so a re-launch doesn't restore the wedged shape.
        this.saveCurrentSession();
    }

    async reextractItems(ids: string[], enforceOcr: boolean = false) {
        ids.forEach(id => {
            const item = this.items.find(i => i.id === id);
            if (item) {
                item.extractedText = undefined;
                item.status = 'queued';
                item.errorMessage = undefined;
                item.statusDetail = undefined;
            }
        });
        await this.processAll({ enforceOcr, extractionOnly: true }, new Set(ids));
    }

    async reprocessItems(ids: string[], overrides?: ProcessOverrides) {
        ids.forEach(id => {
            const item = this.items.find(i => i.id === id);
            if (item) {
                item.status = 'queued';
                item.errorMessage = undefined;
                item.statusDetail = undefined;
                if (overrides?.enforceOcr) item.extractedText = undefined;
            }
        });
        await this.processAll(overrides, new Set(ids));
    }

    async setAcceptedItems(ids: string[], accepted: boolean) {
        ids.forEach(id => {
            const item = this.items.find(i => i.id === id);
            if (item) item.isAccepted = accepted;
        });
        await this.saveCurrentSession();
    }

    async processAll(overrides?: ProcessOverrides, onlyIds?: Set<string>) {
        flog('info', `processAll started${onlyIds ? ` (${onlyIds.size} items)` : ''}`);
        if (this.isProcessing) return;
        this.isProcessing = true;
        this.stopRequested = false;
        this.llmAbort = new AbortController();
        this.extractionAbort = new AbortController();
        this.runStartTs = Date.now();
        this.extractionDone = 0;
        this.llmDone = 0;

        // Wrap the whole body in try/finally so isProcessing always
        // resets — earlier shape only had the producer/consumer block
        // inside try, so any throw from the ~45 lines of setup
        // (getSetting × 8, ensureProviderRunning, detectAndMark…) left
        // isProcessing stuck true forever.  Symptom: after one
        // successful batch, the toolbar got stuck on "Stop" and the
        // user couldn't start the next run, even after `clear()` was
        // refused-no-op'd by its own `isProcessing` guard.
        try {
        const providers = await getSetting('providers', []);
        const activeProviderId = overrides?.providerId || await getSetting('activeProviderId', 'ollama');
        const activeProvider = (providers as any[]).find(p => p.id === activeProviderId) || providers[0];

        let modelId = overrides?.modelId || activeProvider?.selectedModel || activeProvider?.models?.[0];

        if (!overrides?.modelId && ['mistralrs', 'llamacpp'].includes(activeProviderId)) {
            const localModels = await getSetting('localModels', []) as any[];
            const activeLocal = localModels.find(m => m.isActive && m.isDownloaded);
            modelId = activeLocal?.path || modelId;
        }
        // For browser-based engines, fall back to the in-memory loaded model
        if (activeProviderId === 'webllm') modelId = modelId || getWebLLMLoadedModel();
        if (activeProviderId === 'ort') modelId = modelId || getORTLoadedModel();

        const llmMaxChars = overrides?.maxChars ?? await getSetting('llmMaxChars', 5000);
        const parsingFormat = await getSetting('parsingFormat', 'xml') as 'xml' | 'json';
        const authorSortEnabled = overrides?.authorSort ?? await getSetting('authorSortEnabled', false);
        const pdfBackend = await getSetting('pdfBackend', 'js');
        const extractionMaxPages = await getSetting('extractionMaxPages', 0) as number; // 0 = all pages (no limit)
        // PLAN P8.1 — per-file conversion timeout. 0 = no whole-file
        // ceiling (the page watchdog still catches "extractor froze").
        // Default 120s catches the "extractor making slow but real
        // progress on a too-big file" case that the page watchdog can't.
        const conversionTimeoutSec = (await getSetting('conversionTimeoutSeconds', 120)) as number;
        const requestDelayMs = await getSetting('requestDelayMs', 0) as number;
        llmClient.requestDelayMs = requestDelayMs;
        const language = await getSetting('language', 'en') as string;
        const ocrEnabledGlobal = await getSetting('ocrEnabled', false);
        const noThinking = await getSetting('noThinking', true);

        // Sync llmClient state
        llmClient.noThinking = noThinking;
        llmClient.llamacppPort = await getSetting('llamacppPort', 8080) as number;
        llmClient.mlxPort = await getSetting('mlxPort', 8000) as number;

        // Auto-start local server if needed
        if (['ollama', 'llamacpp', 'mlx'].includes(activeProviderId)) {
            try {
                await llmClient.ensureProviderRunning(activeProviderId, modelId);
            } catch (e) {
                console.warn(`[BatchManager] Could not auto-start ${activeProviderId}:`, e);
            }
        }

        const defaultPrompt = getDefaultPrompt(parsingFormat, language);
        const basePrompt = await getSetting('llmPrompt', defaultPrompt);

        // Build round-robin provider list: active provider first, then user-defined fallbacks.
        // Each entry: { id, modelId, apiKey }. Used in phase 2 to switch on provider failure.
        const roundRobinIds: string[] = (await getSetting('roundRobinProviders', [])) as string[];
        type RRProvider = { id: string; modelId: string; apiKey: string };
        const rrProviders: RRProvider[] = [
            { id: activeProvider.id, modelId, apiKey: activeProvider.apiKey || '' },
            ...roundRobinIds
                .filter(id => id !== activeProviderId)
                .map(id => {
                    const p = (providers as any[]).find(pp => pp.id === id);
                    if (!p) return null;
                    return { id, modelId: p.selectedModel || p.models?.[0] || '', apiKey: p.apiKey || '' };
                })
                .filter((p): p is RRProvider => p !== null),
        ];
        let rrIdx = 0; // advances when a provider exhausts its retries

        flog('info', `processAll config: format=${parsingFormat} lang=${language} provider=${activeProviderId} model=${modelId} maxChars=${llmMaxChars} fallbacks=${rrProviders.length - 1}`);

        // P15a — content-dedup: mark duplicates before extraction so
        // UI shows orange badges immediately and sort-skip works.
        if (!onlyIds) {
            const dupeCount = await this.detectAndMarkDuplicates();
            if (dupeCount > 0) flog('info', `P15a dedup: ${dupeCount} non-primary duplicate(s) marked`);
        }
        // P15b — chapter groups: detect ISBN/suffix patterns.
        if (!onlyIds) {
            const groupCount = this.detectChapterGroups();
            if (groupCount > 0) flog('info', `P15b chapters: ${groupCount} book-chapter group(s) detected`);
        }

        // Producer/consumer pipeline (PLAN P10 step c, foreground variant).
        //
        // Pre-P10 this was a strict two-phase EXTRACT-all -> ANALYZE-all
        // loop, which held the LLM idle while extraction worked through
        // the queue (and vice-versa). Now both run concurrently:
        //   - the producer pulls items off `this.items` one at a time,
        //     runs extraction with the existing per-stage timeouts +
        //     page watchdog, and pushes ready items onto `llmQueue`;
        //   - the consumer drains `llmQueue` and runs queryRR.
        //
        // An LLM stall stops draining the queue but doesn't stop
        // extraction -- the queue back-pressures at QUEUE_CAP via the
        // wakeProducer promise. An extraction error doesn't block
        // already-extracted items from being analysed; the consumer
        // keeps draining. Stop button aborts both via the existing
        // extractionAbort + llmAbort signals.
        const needsProcessing = (item: BatchItem) =>
            (item.status === 'queued' || item.status === 'error' || item.status === 'unfinished') &&
            (!onlyIds || onlyIds.has(item.id));

        const PAGE_WATCHDOG_MS = 30_000; // abort if no page progress for 30 s
        // P8.1 — whole-file timeout. 0 sentinel = no timeout (current
        // pre-P8.1 behaviour for users who explicitly opted out).
        const FILE_TIMEOUT_MS = conversionTimeoutSec > 0
            ? conversionTimeoutSec * 1000
            : 0;

        // PLAN P10 step c -- N extraction workers, M LLM workers.
        //
        // Worker counts come from Settings (Extraktion / KI-Optionen
        // panels), clamped to [1, 16]. Default is 1 + 1, matching the
        // pre-this-commit producer/consumer pipeline; users with fast
        // CPUs / large round-robin pools can scale up.
        const extractionN = Math.max(1, Math.min(16, (await getSetting('extractionWorkers', 1)) as number));
        const llmN = Math.max(1, Math.min(16, (await getSetting('llmWorkers', 1)) as number));
        this.extractionTargetWorkers = extractionN;
        this.llmTargetWorkers = llmN;

        // Async queue between extractors and LLM consumers. Workers
        // park on `take()` when empty; the queue is `close()`d after
        // all extractor workers finish, which wakes any consumer
        // waiting on `take()` with a `null` sentinel so they can
        // exit cleanly.
        type Waiter = (v: BatchItem | null) => void;
        const llmQueueItems: BatchItem[] = [];
        const llmQueueWaiters: Waiter[] = [];
        let llmQueueClosed = false;
        const queuePush = (item: BatchItem) => {
            if (llmQueueWaiters.length > 0) llmQueueWaiters.shift()!(item);
            else llmQueueItems.push(item);
        };
        const queueClose = () => {
            llmQueueClosed = true;
            while (llmQueueWaiters.length > 0) llmQueueWaiters.shift()!(null);
        };
        const queueTake = (): Promise<BatchItem | null> => {
            if (llmQueueItems.length > 0) return Promise.resolve(llmQueueItems.shift()!);
            if (llmQueueClosed) return Promise.resolve(null);
            return new Promise<BatchItem | null>(r => llmQueueWaiters.push(r));
        };

        // Single shared next-item index. JS is single-threaded for
        // state mutations, so the read-then-increment is atomic by
        // construction; no lock needed.
        let nextIdx = 0;

        try {
            // ── Worker: extraction ──────────────────────────────────────────
            // N workers run this body concurrently. Each pulls the next
            // item via the shared `nextIdx`, runs the existing
            // extraction logic with its own per-item watchdog, and
            // pushes the ready item onto the queue. Stop button aborts
            // via the shared `extractionAbort` signal.
            const extractWorker = async (workerId: number) => {
            while (true) {
                if (this.stopRequested) break;
                const idx = nextIdx++;
                if (idx >= this.items.length) break;
                const item = this.items[idx];
                if (!needsProcessing(item) || item.extractedText) {
                    // Already-extracted items still go to the LLM
                    // queue so the consumer covers re-runs.
                    if (item.extractedText && !overrides?.extractionOnly) {
                        queuePush(item);
                    }
                    continue;
                }

                logDebug(`extract worker ${workerId} -> ${item.originalName} (queue=${llmQueueItems.length})`);
                this.extractionActive++;
                item.status = 'extracting';
                const forceOCR = overrides?.enforceOcr ?? false;

                try {
                    if (item.originalName.toLowerCase().endsWith('.pdf') && pdfBackend === 'rust' && !forceOCR) {
                        flog('info', `Rust extraction: ${item.originalName}`);
                        const extractCall = invoke<string>('extract_pdf_native', { path: item.originalPath });
                        const text = FILE_TIMEOUT_MS > 0
                            ? await Promise.race([
                                extractCall,
                                new Promise<never>((_, reject) =>
                                    setTimeout(() => reject(new Error('EXTRACT_TIMEOUT')), FILE_TIMEOUT_MS)
                                )
                            ])
                            : await extractCall;
                        item.extractedText = text;
                    } else {
                        flog('info', `JS extraction: ${item.originalName} (forceOCR=${forceOCR})`);
                        // Per-item AbortController. Listens to the shared
                        // `this.extractionAbort` (Stop button) so cancelling
                        // the run aborts every in-flight worker, but each
                        // worker also has its own watchdog + file timeout
                        // that can fire independently without affecting
                        // sibling workers.
                        const itemAbort = new AbortController();
                        const onSharedAbort = () => itemAbort.abort(this.extractionAbort?.signal.reason);
                        this.extractionAbort?.signal.addEventListener('abort', onSharedAbort, { once: true });

                        // Per-page watchdog: abort if no page progress for PAGE_WATCHDOG_MS
                        let watchdogId: ReturnType<typeof setTimeout> | null = null;
                        const resetWatchdog = () => {
                            if (watchdogId) clearTimeout(watchdogId);
                            watchdogId = setTimeout(() => {
                                itemAbort.abort(new Error('EXTRACT_PAGE_TIMEOUT'));
                                flog('warn', `Page watchdog fired (${PAGE_WATCHDOG_MS / 1000}s no progress): ${item.originalName}`);
                            }, PAGE_WATCHDOG_MS);
                        };
                        resetWatchdog(); // start watchdog immediately

                        // P8.1 — whole-file timeout. Aborts the same
                        // signal the page watchdog uses; coexists with
                        // the watchdog (page watchdog catches "frozen",
                        // file timeout catches "slow but progressing").
                        let fileTimeoutId: ReturnType<typeof setTimeout> | null = null;
                        if (FILE_TIMEOUT_MS > 0) {
                            fileTimeoutId = setTimeout(() => {
                                itemAbort.abort(new Error('EXTRACT_FILE_TIMEOUT'));
                                flog('warn', `File timeout fired (${FILE_TIMEOUT_MS / 1000}s wall-clock): ${item.originalName}`);
                            }, FILE_TIMEOUT_MS);
                        }

                        const fileData = await readFile(item.originalPath);
                        if (this.stopRequested) {
                            if (watchdogId) clearTimeout(watchdogId);
                            this.extractionAbort?.signal.removeEventListener('abort', onSharedAbort);
                            item.status = 'queued';
                            break;
                        }
                        let extraction;
                        try {
                            extraction = await extractText(
                                {
                                    name: item.originalName,
                                    arrayBuffer: fileData.buffer,
                                    // P13.5 audio dispatch: extractText hands the
                                    // path to audio_extract_text when the extension
                                    // is in AUDIO_EXTENSIONS, keeping PCM in Rust.
                                    path: item.originalPath,
                                },
                                {
                                    forceOCR,
                                    signal: itemAbort.signal,
                                    maxPages: extractionMaxPages || undefined,
                                    onProgress: (page, total) => {
                                        item.statusDetail = `${page}/${total} pages`;
                                        resetWatchdog();
                                    }
                                }
                            );
                        } finally {
                            if (watchdogId) clearTimeout(watchdogId);
                            if (fileTimeoutId) clearTimeout(fileTimeoutId);
                            this.extractionAbort?.signal.removeEventListener('abort', onSharedAbort);
                        }
                        if (this.stopRequested) { item.status = 'queued'; break; }
                        item.extractedText = extraction.text;
                        // P13.6 Step 2: surface the whisper-detected source
                        // language for audio/video items so the Stapel
                        // language column can render it.  Documents pass
                        // through unchanged (metadata.language stays
                        // undefined for them).
                        const detected = extraction.metadata?.language;
                        if (typeof detected === 'string' && detected.length > 0) {
                            item.detectedLanguage = detected.toLowerCase();
                        }
                    }
                    item.statusDetail = (item.extractedText?.trim().length ?? 0) < 100 ? '⚠ poor extraction' : undefined;

                    // PDF metadata pre-fill — read /Info dict on the Rust side
                    // and populate suggestedTitle/Author/Year before phase 2.
                    // The LLM (when enabled) will overwrite these in phase 2,
                    // so this is purely a fallback for users who skip the LLM
                    // or for runs where phase 2 errors out. See lib.rs
                    // extract_pdf_metadata for the lopdf decoding details.
                    const ext = (item.extension ?? '').toLowerCase();
                    const isPdf = ext === 'pdf';
                    const hasMetadataConvention =
                        // file types where metadata-read makes sense
                        ['pdf', 'docx', 'epub', 'jpg', 'jpeg', 'png', 'webp', 'tif', 'tiff'].includes(ext);
                    if (isPdf && (await getSetting('pdfMetadataPrefill', true))) {
                        try {
                            // 30 s wall-clock cap on the Rust-side lopdf call.
                            // `lopdf::Document::load` is fully synchronous and
                            // doesn't honor an abort signal — a pathological
                            // PDF (broken xref, deeply-nested compressed
                            // streams, encrypted content) can stall it
                            // indefinitely.  Without this race, the
                            // extraction worker is stuck on the metadata
                            // pre-fill BEFORE the page watchdog has any
                            // signal to trip on, so the whole batch
                            // appears frozen with no log line.
                            const meta = await Promise.race([
                                invoke<{
                                    title?: string | null;
                                    author?: string | null;
                                    year?: number | null;
                                }>('extract_pdf_metadata', { path: item.originalPath }),
                                new Promise<never>((_, rej) =>
                                    setTimeout(
                                        () => rej(new Error('PDF metadata timeout (30 s)')),
                                        30_000
                                    )
                                ),
                            ]);
                            if (meta?.title && !item.suggestedTitle) {
                                item.suggestedTitle = meta.title.trim();
                            }
                            if (meta?.author && !item.suggestedAuthor) {
                                item.suggestedAuthor = meta.author.trim();
                            }
                            if (typeof meta?.year === 'number' && !item.suggestedYear) {
                                item.suggestedYear = String(meta.year);
                            }
                            const got = !!(meta?.title || meta?.author || meta?.year);
                            // 'ok' even when /Info was empty -- the read
                            // succeeded, the dict just had nothing useful.
                            // 'failed' is reserved for actual exceptions.
                            item.metadataReadStatus = 'ok';
                            if (got) {
                                flog('info', `PDF metadata prefilled: ${item.originalName} (title=${meta?.title ? 'y' : '-'}, author=${meta?.author ? 'y' : '-'}, year=${meta?.year ?? '-'})`);
                            } else {
                                logDebug(`PDF /Info empty: ${item.originalName} (no usable Title/Author/Year)`);
                            }
                        } catch (e) {
                            // Non-fatal -- most PDFs have an Info dict, but
                            // missing or malformed ones shouldn't break extraction.
                            item.metadataReadStatus = 'failed';
                            flog('warn', `PDF metadata read failed: ${item.originalName}: ${e}`);
                        }
                    } else if (!hasMetadataConvention) {
                        // Plain-text-ish formats have no embedded metadata
                        // convention to read; render the M-pip as N/A.
                        item.metadataReadStatus = 'na';
                    }
                    // (DOCX / EPUB / image metadata are handled by the
                    //  separate L2-promote path; the M-pip stays
                    //  undefined until that runs.)

                    // P13.6 Step 3b — audio L2 metadata pre-fill.  Cheap
                    // symphonia probe (no decode) populating the row's
                    // duration / codec / sample rate / channels / bitrate
                    // fields.  Runs after the ASR transcription so it
                    // doesn't add latency on the critical path; the
                    // duration column / row tooltip just light up a beat
                    // later.  Pure-decoration call — never errors out
                    // the item; on failure we log and move on.
                    const isAudio = AUDIO_EXTENSIONS.includes(ext as any);
                    if (isAudio) {
                        try {
                            const m = await invoke<{
                                duration_seconds?: number | null;
                                codec?: string | null;
                                sample_rate_hz?: number | null;
                                channels?: number | null;
                                bitrate_kbps?: number | null;
                            }>('audio_metadata', { path: item.originalPath });
                            if (typeof m?.duration_seconds === 'number') item.audioDurationSeconds = m.duration_seconds;
                            if (typeof m?.codec === 'string')             item.audioCodec           = m.codec;
                            if (typeof m?.sample_rate_hz === 'number')    item.audioSampleRateHz    = m.sample_rate_hz;
                            if (typeof m?.channels === 'number')          item.audioChannels        = m.channels;
                            if (typeof m?.bitrate_kbps === 'number')      item.audioBitrateKbps     = m.bitrate_kbps;
                            // Metadata-read pip mirrors the PDF path: 'ok'
                            // even when fields are sparse — the probe
                            // succeeded.
                            item.metadataReadStatus = 'ok';
                        } catch (e: any) {
                            // Don't fail the item; the transcript is what
                            // matters.  Just log and leave the L2 fields
                            // undefined so the UI renders dashes.
                            item.metadataReadStatus = 'failed';
                            flog('warn', `Audio metadata probe failed: ${item.originalName}: ${e}`);
                        }
                    }

                    // Park at 'queued' with text so the consumer picks it up (unless extraction-only)
                    item.status = overrides?.extractionOnly ? 'review' : 'queued';
                    if (overrides?.extractionOnly) await this.calculateTargetPath(item);
                    const extractedBytes = item.extractedText?.length ?? 0;
                    const tool = isPdf && pdfBackend === 'rust' && !forceOCR ? 'pdf-extract (Rust)'
                                : forceOCR ? 'OCR (Tesseract)'
                                : `JS extractor (.${ext})`;
                    flog('info', `Extracted: ${item.originalName} -- ${extractedBytes.toLocaleString()} chars via ${tool}`);
                    // Hand the freshly-extracted item to the LLM consumer.
                    if (!overrides?.extractionOnly && item.extractedText) {
                        queuePush(item);
                    }
                } catch (e: any) {
                    const isAbort = this.stopRequested || e?.name === 'AbortError' || e?.message?.includes('EXTRACT');
                    if (isAbort) {
                        item.status = 'unfinished';
                        item.statusDetail = 'interrupted';
                        flog('warn', `Extraction interrupted: ${item.originalName}`);
                        if (this.stopRequested) break;
                    } else {
                        item.status = 'error';
                        item.errorMessage = e.message || String(e);
                        flog('error', `Extraction error: ${item.originalName}: ${e.message || e}`);
                    }
                } finally {
                    this.extractionActive = Math.max(0, this.extractionActive - 1);
                    this.extractionDone++;
                }
                await this.saveCurrentSession();
            }
            };

            // ── Worker: LLM analysis ────────────────────────────────────────
            // M workers run this concurrently. Each `take()`s the next
            // item off the shared queue; when the queue is closed AND
            // empty, `take()` resolves null and the worker exits.
            const llmWorker = async (workerId: number) => {
            // Skip entirely when we're only extracting, or when metadata
            // extraction is disabled in Settings.
            if (overrides?.extractionOnly || !this.isMetadataExtractionEnabled) return;

            while (true) {
                if (this.stopRequested || this.llmAbort?.signal.aborted) {
                    flog('info', `LLM worker ${workerId} halting (stop)`);
                    break;
                }
                const item = await queueTake();
                if (item === null) break; // queue closed and empty

                logDebug(`llm worker ${workerId} -> ${item.originalName} (remaining=${llmQueueItems.length})`);

                // Re-check: the producer may have queued an item that
                // the user has since edited, or the item may have
                // already been analysed in a prior partial run.
                // P15b: skip LLM for non-representative chapter files —
                // metadata will be propagated from the representative.
                const isNonRepChapter = !!item.chapterGroupId && item.isChapterRepresentative === false;
                const needsAnalysis = (item.status === 'queued' || item.status === 'unfinished') &&
                    item.extractedText &&
                    (!onlyIds || onlyIds.has(item.id)) &&
                    !isNonRepChapter;
                if (!needsAnalysis) {
                    if (isNonRepChapter) item.status = 'review';
                    continue;
                }
                this.llmActive++;

                item.status = 'analyzing';
                try {
                    const textSample = item.extractedText!.substring(0, llmMaxChars);
                    const prompt = `${basePrompt}\n\nFilename: "${item.originalName}"\n\nDocument snippet:\n${textSample}`;

                    // Try providers in round-robin order; advance index on non-abort failure.
                    const queryRR = async (p: string): Promise<string> => {
                        let lastErr: any = null;
                        for (let i = rrIdx; i < rrProviders.length; i++) {
                            const rr = rrProviders[i];
                            try {
                                const res = await llmClient.query(rr.id, rr.modelId, p, rr.apiKey, 0.3, this.llmAbort?.signal);
                                rrIdx = i; // stick with this provider for subsequent items
                                return res;
                            } catch (e: any) {
                                if (e?.name === 'AbortError' || e?.message?.includes('LLM_TIMEOUT')) throw e;
                                lastErr = e;
                                flog('warn', `Provider ${rr.id} failed — trying next fallback (${i + 1}/${rrProviders.length}): ${e.message}`);
                                rrIdx = i + 1;
                            }
                        }
                        // All providers exhausted — reset index and throw last error
                        rrIdx = 0;
                        throw lastErr ?? new Error('All LLM providers exhausted');
                    };

                    const activeRR = rrProviders[rrIdx] ?? rrProviders[0];
                    flog(
                        'info',
                        `LLM analyze: ${item.originalName} via ${activeRR?.id ?? '?'}/${activeRR?.modelId ?? '?'}, sent ${prompt.length.toLocaleString()} chars (${textSample.length.toLocaleString()} from doc body)`
                    );
                    const llmStart = performance.now();
                    const response = await queryRR(prompt);
                    const llmMs = Math.round(performance.now() - llmStart);
                    if (this.stopRequested || this.llmAbort?.signal.aborted) { item.status = 'queued'; break; }
                    const metadata = this.parseLLMResponse(response, parsingFormat);

                    item.suggestedTitle = metadata.title || 'Unknown Title';
                    item.suggestedAuthor = metadata.author || 'Unknown Author';
                    item.suggestedYear = metadata.year || 'Unknown Year';
                    flog(
                        'info',
                        `Analyzed: ${item.originalName} -> title="${item.suggestedTitle}" author="${item.suggestedAuthor}" year=${item.suggestedYear} (${llmMs} ms, ${response.length.toLocaleString()} chars in response)`
                    );

                    if (authorSortEnabled && item.suggestedAuthor && !isUnknownSentinel(item.suggestedAuthor)) {
                        const sortPrompt = `Reformat author to "Lastname Firstname": "${item.suggestedAuthor}". Output ONLY <AUTHOR> tags.`;
                        const sortRes = await queryRR(sortPrompt);
                        if (this.stopRequested || this.llmAbort?.signal.aborted) { item.status = 'queued'; break; }
                        const match = sortRes.match(/<AUTHOR>(.*?)<\/AUTHOR>/i);
                        if (match) item.suggestedAuthor = match[1].trim();
                    }

                    item.status = 'review';
                    // Auto-accept (green check) only when the LLM came back
                    // with a *real* Author -- not a sentinel like "Unknown",
                    // "Unknown Author", "n.n.", "?", or a single
                    // punctuation char. Items without a real author stay
                    // unaccepted (red X) until the user explicitly ticks
                    // them — see Stapel UX in PLAN.md.
                    const hasAuthor = !!item.suggestedAuthor &&
                        !isUnknownSentinel(item.suggestedAuthor);
                    item.isAccepted = hasAuthor;
                    await this.calculateTargetPath(item);

                    if (requestDelayMs > 0) await new Promise(r => setTimeout(r, requestDelayMs));
                } catch (e: any) {
                    const isAbort = this.stopRequested || e?.name === 'AbortError';
                    if (isAbort) {
                        item.status = 'queued';
                        item.statusDetail = undefined;
                        flog('info', `Analysis stopped: ${item.originalName}`);
                        break;
                    } else {
                        item.status = 'error';
                        item.errorMessage = e.message || String(e);
                        flog('error', `Analysis error: ${item.originalName}: ${e.message || e}`);
                    }
                } finally {
                    this.llmActive = Math.max(0, this.llmActive - 1);
                    this.llmDone++;
                }
                await this.saveCurrentSession();
            }
            };

            // Spawn N extraction workers + M LLM workers. The extraction
            // workers' `Promise.all` finishes when every item has been
            // processed; we then `queueClose()` so any LLM workers still
            // waiting on `take()` see the queue end and exit.
            const extractWorkers = Array.from({ length: extractionN }, (_, i) => extractWorker(i));
            const llmWorkers = Array.from({ length: llmN }, (_, i) => llmWorker(i));

            // When all extractors finish, close the queue so consumers
            // can drain and exit.
            const extractAllDone = Promise.all(extractWorkers).then(() => {
                logDebug('all extraction workers finished -> closing llmQueue');
                queueClose();
            });

            await Promise.all([extractAllDone, ...llmWorkers]);

            // P15b — propagate chapter representative metadata to group members.
            if (!onlyIds) this.propagateChapterMetadata();
        } finally {
            this.isProcessing = false;
            this.stopRequested = false;
            this.llmAbort = null;
            this.extractionAbort = null;
            this.extractionActive = 0;
            this.llmActive = 0;
            flog('info', 'processAll finished');
            await this.saveCurrentSession();
        }
        } catch (e) {
            // Outer-scope catch: any throw from the ~45 lines of
            // setup BEFORE the inner try (getSetting × 8, provider
            // boot, dedup detection) used to leave `isProcessing`
            // stuck `true` forever — toolbar froze on "Stop", clear()
            // refused to fire (its own isProcessing guard), Start
            // button could never reappear.  Now any setup-time error
            // also resets the flag and logs.
            flog('error', `processAll setup threw: ${(e as any)?.message ?? e}`);
            this.isProcessing = false;
            this.stopRequested = false;
            this.llmAbort = null;
            this.extractionAbort = null;
            await this.saveCurrentSession();
            throw e;
        }
    }

    private parseLLMResponse(text: string, format: 'xml' | 'json'): Metadata {
        if (format === 'json') {
            try {
                const match = text.match(/\{[\s\S]*\}/);
                if (match) return JSON.parse(match[0]);
            } catch (e) {}
        }
        const title = text.match(/<TITLE>(.*?)<\/TITLE>/i)?.[1] || text.match(/Title:\s*(.*)/i)?.[1];
        const author = text.match(/<AUTHOR>(.*?)<\/AUTHOR>/i)?.[1] || text.match(/Author:\s*(.*)/i)?.[1];
        const year = text.match(/<YEAR>(.*?)<\/YEAR>/i)?.[1] || text.match(/Year:\s*(.*)/i)?.[1];
        return { title: title?.trim(), author: author?.trim(), year: year?.trim() };
    }

    async recalculateTargetPath(itemId: string) {
        const item = this.items.find(i => i.id === itemId);
        if (item) await this.calculateTargetPath(item);
    }

    private async calculateTargetPath(item: BatchItem) {
        const exportPath = await getSetting('exportPath', '');
        const pathTemplate = await getSetting('pathTemplate', '{Author}/{Year}/{Title}') as string;

        const sanitize = (s: string) => s.replace(/[\\/:*?"<>|]/g, '_').substring(0, 100);
        const author   = sanitize(item.suggestedAuthor || 'Unknown Author');
        const year     = sanitize(item.suggestedYear   || '0000');
        const title    = sanitize(item.suggestedTitle  || item.originalName);
        const ext      = item.extension;
        const filename = item.originalName;

        const hasExtToken = /\{Ext\}/i.test(pathTemplate);
        let relative = pathTemplate
            .replace(/\{Author\}/gi,   author)
            .replace(/\{Year\}/gi,     year)
            .replace(/\{Title\}/gi,    title)
            .replace(/\{Ext\}/gi,      ext)
            .replace(/\{Filename\}/gi, filename);

        if (!hasExtToken) relative = `${relative}.${ext}`;

        // Determine base dir
        let baseDir: string;
        if (exportPath) {
            baseDir = exportPath;
        } else {
            // Support both / and \ separators (Windows uses backslash)
            const lastSep = Math.max(
                item.originalPath.lastIndexOf('/'),
                item.originalPath.lastIndexOf('\\')
            );
            const parent = lastSep >= 0 ? item.originalPath.substring(0, lastSep) : '';
            baseDir = await join(parent || '.', 'Sorted');
        }

        // Split on forward-slash (template separator) and join with OS path separator
        const parts = relative.split('/').filter(Boolean);
        item.targetPath = await join(baseDir, ...parts);
    }

    async saveCurrentSession() {
        const session = {
            id: 'current',
            items: $state.snapshot(this.items),
            timestamp: Date.now()
        };
        await saveSetting('lastSession', session);
    }

    async resumeLastSession() {
        const last = await getSetting('lastSession');
        if (last && (last as any).items) {
            this.items = (last as any).items.map((item: any) => {
                if (item.status === 'extracting' || item.status === 'analyzing') {
                    return { ...item, status: 'unfinished', statusDetail: undefined };
                }
                // Repair: a previous build's auto-accept rule only
                // recognised the literal string "Unknown Author" and
                // would mark items with "Unknown" / "n.n." / "?" /
                // empty author as accepted (green check). Tighten on
                // resume so existing sessions don't carry the bad
                // accepts forward.
                if (item.isAccepted && isUnknownSentinel(item.suggestedAuthor)) {
                    return { ...item, isAccepted: false };
                }
                return item;
            });
        }
    }

    async executeBatch(mode: string = 'move', opts: { skipNonPrimaryDupes?: boolean } = {}) {
        // The user's `isAccepted` flag is the affirmative consent;
        // `status` is informational. Items at 'review' (LLM-analysed),
        // 'error' (re-try), 'unfinished' (resumed from prior session),
        // and 'queued' (waiting / metadata-prefilled, never analysed)
        // are all sortable as long as the user ticked the green check
        // and a target path is computable. Pre-fix, only 'review' and
        // 'error' passed -- so resumed-session items the user OK'd
        // would show a green check + enabled "Sortieren" button but
        // executeBatch silently filtered them out and returned null.
        const SORTABLE = new Set(['review', 'error', 'unfinished', 'queued']);
        const accepted = this.items.filter(
            i => i.isAccepted && i.targetPath && SORTABLE.has(i.status)
                && !(opts.skipNonPrimaryDupes && i.isDuplicatePrimary === false)
        );
        flog('info', `executeBatch(${mode}) starting: ${accepted.length} sortable / ${this.items.length} total`);

        // Per-item rejection diagnosis at debug level. Each rejected
        // item logs WHICH gate it failed (no green check / no
        // targetPath / unsortable status). Set Log-Verbosity=debug
        // in Settings -> Allgemein to see this.
        for (const i of this.items) {
            const reasons: string[] = [];
            if (!i.isAccepted)              reasons.push('not accepted');
            if (!i.targetPath)              reasons.push('no targetPath');
            if (!SORTABLE.has(i.status))    reasons.push(`unsortable status='${i.status}'`);
            const verdict = reasons.length === 0
                ? 'will sort'
                : `skip (${reasons.join(', ')})`;
            flog('debug', `  item ${i.originalName}: status=${i.status} accepted=${i.isAccepted} target=${i.targetPath ? 'set' : 'MISSING'} -> ${verdict}`);
        }

        if (accepted.length === 0) {
            // Surface the reason at warn level so the silent-null case
            // isn't silent.
            const greenCount = this.items.filter(i => i.isAccepted).length;
            const noTargetCount = this.items.filter(i => i.isAccepted && !i.targetPath).length;
            const wrongStatusCount = this.items.filter(i => i.isAccepted && i.targetPath && !SORTABLE.has(i.status)).length;
            flog('warn',
                `executeBatch returning null: 0 sortable items. ` +
                `${greenCount} have green check, ${noTargetCount} of those lack a targetPath, ` +
                `${wrongStatusCount} have a non-sortable status. Re-process the batch and try again.`
            );
            return null;
        }

        const saveTxt = await getSetting('saveTxt', true);
        const payload = {
            items: accepted.map(i => ({
                id: i.id,
                originalPath: i.originalPath,
                targetPath: i.targetPath!,
                extractedText: i.extractedText
            })),
            saveTxt,
            mode
        };

        this.isExecuting = true;
        try {
            const results = await invoke<Record<string, { success: boolean, error?: string }>>('execute_batch', { payload });

            let successCount = 0;
            let notFound = 0;
            let notWritable = 0;
            let errorCount = 0;
            let copiedFallback = 0;
            let locked = 0;

            for (const [id, res] of Object.entries(results)) {
                const item = this.items.find(i => i.id === id);
                if (item) {
                    if (res.success) {
                        item.status = 'done';
                        if (res.error === 'COPY_FALLBACK') {
                            // Copied to destination but original not deleted (was locked by OS)
                            item.errorMessage = 'COPY_FALLBACK';
                            item.statusDetail = 'copied (orig. kept)';
                            copiedFallback++;
                        } else {
                            item.errorMessage = undefined;
                            item.statusDetail = mode.includes('copy') ? 'copied' : 'moved';
                            successCount++;
                            // Update search index location if the file was moved (not copied)
                            if (!mode.includes('copy') && item.targetPath) {
                                invoke('index_update_location_by_path', {
                                    oldPath: item.originalPath,
                                    newPath: item.targetPath,
                                }).catch(e => console.warn('[Index] update_location_by_path failed:', e));
                            }
                        }
                    } else {
                        item.status = 'error';
                        item.errorMessage = res.error;
                        if (res.error === 'SOURCE_NOT_FOUND') notFound++;
                        else if (res.error?.startsWith('NOT_WRITABLE')) notWritable++;
                        else if (res.error === 'LOCKED') locked++;
                        else errorCount++;
                    }
                }
            }

            await this.saveCurrentSession();
            return { success: successCount, notFound, notWritable, error: errorCount, copiedFallback, locked, mode };
        } finally {
            this.isExecuting = false;
        }
    }

    async loadSession(id: string) {
        const saved = await getSetting('sessions', {}) as Record<string, BatchSession>;
        const session = saved[id];
        if (session?.items) {
            this.items = session.items;
            await this.saveCurrentSession();
        }
    }

    async exportBatch() {
        const { save } = await import('@tauri-apps/plugin-dialog');
        const { writeTextFile } = await import('@tauri-apps/plugin-fs');
        const path = await save({ defaultPath: 'batch.json', filters: [{ name: 'JSON', extensions: ['json'] }] });
        if (path) {
            await writeTextFile(path, JSON.stringify($state.snapshot(this.items), null, 2));
        }
    }

    async importBatch() {
        const { open } = await import('@tauri-apps/plugin-dialog');
        const { readTextFile } = await import('@tauri-apps/plugin-fs');
        const path = await open({ filters: [{ name: 'JSON', extensions: ['json'] }] });
        if (typeof path === 'string') {
            const text = await readTextFile(path);
            const items = JSON.parse(text) as BatchItem[];
            if (Array.isArray(items)) {
                this.items = items;
                await this.saveCurrentSession();
            }
        }
    }

    async getDuplicateGroups(deep: boolean = false) {
        const groups: Record<string, BatchItem[]> = {};
        for (const item of this.items) {
            const key = deep ? `${item.size}_${item.originalName}` : `${item.size}`;
            if (!groups[key]) groups[key] = [];
            groups[key].push(item);
        }
        return Object.values(groups)
            .filter(g => g.length > 1)
            .map(items => ({ size: items[0].size, items }));
    }

    // ── P15a — content dedup ────────────────────────────────────────────────

    /** SHA-256 via Rust (avoids reading large files into JS). */
    private async fileHash(path: string): Promise<string> {
        try {
            return await invoke<string>('file_sha256', { path });
        } catch {
            return `__error_${path}`;
        }
    }

    /** Group by size → hash; mark duplicateGroupId / isDuplicatePrimary.
     *  Returns number of non-primary duplicates found. */
    async detectAndMarkDuplicates(): Promise<number> {
        // Clear old marks.
        for (const item of this.items) {
            item.duplicateGroupId = undefined;
            item.isDuplicatePrimary = undefined;
        }
        // Skip zero-size and very large files.
        const MAX_HASH_BYTES = 500 * 1024 * 1024;
        const candidates = this.items.filter(i => i.size > 0 && i.size <= MAX_HASH_BYTES);

        // Bucket by size.
        const bySize = new Map<number, BatchItem[]>();
        for (const item of candidates) {
            const g = bySize.get(item.size) ?? [];
            g.push(item);
            bySize.set(item.size, g);
        }
        let dupeCount = 0;
        for (const [, sizeGroup] of bySize) {
            if (sizeGroup.length < 2) continue;
            // Hash only size-collision candidates in parallel.
            const hashed = await Promise.all(
                sizeGroup.map(async item => ({ item, hash: await this.fileHash(item.originalPath) }))
            );
            // Re-group by hash.
            const byHash = new Map<string, BatchItem[]>();
            for (const { item, hash } of hashed) {
                const g = byHash.get(hash) ?? [];
                g.push(item);
                byHash.set(hash, g);
            }
            for (const [hash, dupeGroup] of byHash) {
                if (dupeGroup.length < 2) continue;
                const shortId = hash.slice(0, 8);
                for (let i = 0; i < dupeGroup.length; i++) {
                    dupeGroup[i].duplicateGroupId = shortId;
                    dupeGroup[i].isDuplicatePrimary = i === 0;
                }
                dupeCount += dupeGroup.length - 1;
            }
        }
        return dupeCount;
    }

    // ── P15b — book chapter grouping ────────────────────────────────────────

    /** Detect ISBN-13-prefixed chapter sets and generic `stem_suffix` groups.
     *  Marks chapterGroupId / chapterSuffix / isChapterRepresentative /
     *  chapterGroupSize on each item. Returns number of groups found. */
    detectChapterGroups(): number {
        // Clear old marks.
        for (const item of this.items) {
            item.chapterGroupId = undefined;
            item.chapterSuffix = undefined;
            item.isChapterRepresentative = undefined;
            item.chapterGroupSize = undefined;
        }

        // ISBN-13 pattern: 97[89] + 10 digits + _ + suffix + .ext
        const isbnRe = /^(97[89]\d{10})[_-]([a-z0-9]+)\.(pdf|epub|docx)$/i;
        // Generic suffix pattern: stem + _ or - + known suffix or 2-4 digit number
        const genericRe = /^(.+)[_-](fm|bm|ind|toc|cover|ck|ch\d+|part\d+|kap\d+|vol\d+|\d{2,4})\.(pdf|epub|docx)$/i;

        const groups = new Map<string, Array<{ item: BatchItem; suffix: string }>>();

        for (const item of this.items) {
            const name = item.originalName;
            let groupId: string | null = null;
            let suffix: string | null = null;

            const isbnM = isbnRe.exec(name);
            if (isbnM) {
                groupId = isbnM[1].toLowerCase();
                suffix  = isbnM[2].toLowerCase();
            } else {
                const genM = genericRe.exec(name);
                if (genM) {
                    groupId = genM[1].toLowerCase();
                    suffix  = genM[2].toLowerCase();
                }
            }
            if (!groupId || !suffix) continue;
            const g = groups.get(groupId) ?? [];
            g.push({ item, suffix });
            groups.set(groupId, g);
        }

        // Suffix priority for representative selection (lower = higher priority).
        const suffixPriority = (s: string): number => {
            if (s === 'fm')    return 0;
            if (s === 'cover' || s === 'ck') return 1;
            if (s === 'toc')   return 2;
            if (/^\d+$/.test(s)) return 3 + parseInt(s, 10); // numeric: ascending
            if (s === 'bm' || s === 'ind') return 9999;
            return 5000;
        };

        let groupCount = 0;
        for (const [groupId, members] of groups) {
            if (members.length < 2) continue; // single file = not a group
            groupCount++;
            // Sort by priority to pick representative.
            members.sort((a, b) => suffixPriority(a.suffix) - suffixPriority(b.suffix));
            for (let i = 0; i < members.length; i++) {
                members[i].item.chapterGroupId    = groupId;
                members[i].item.chapterSuffix     = members[i].suffix;
                members[i].item.isChapterRepresentative = i === 0;
                members[i].item.chapterGroupSize  = members.length;
            }
        }
        return groupCount;
    }

    /** Toggle the "edited volume" flag for all items in a chapter group.
     *  When enabled, author propagation is suppressed in propagateChapterMetadata. */
    toggleChapterEditedVolume(groupId: string): void {
        const members = this.items.filter(i => i.chapterGroupId === groupId);
        if (members.length === 0) return;
        const next = !members[0].chapterIsEditedVolume;
        for (const m of members) m.chapterIsEditedVolume = next;
        this.saveCurrentSession();
    }

    /** After processAll completes, copy title/year from the representative
     *  to all other chapter-group members. Author propagated only when
     *  non-sentinel (monograph assumption; edited volumes need per-chapter). */
    propagateChapterMetadata(): void {
        const reps = new Map<string, BatchItem>();
        for (const item of this.items) {
            if (item.chapterGroupId && item.isChapterRepresentative) {
                reps.set(item.chapterGroupId, item);
            }
        }
        for (const item of this.items) {
            if (!item.chapterGroupId || item.isChapterRepresentative) continue;
            const rep = reps.get(item.chapterGroupId);
            if (!rep) continue;
            if (!item.suggestedTitle && rep.suggestedTitle)   item.suggestedTitle = rep.suggestedTitle;
            if (!item.suggestedYear  && rep.suggestedYear)    item.suggestedYear  = rep.suggestedYear;
            // Don't propagate author for edited volumes — each chapter has its own.
            if (!item.chapterIsEditedVolume &&
                !item.suggestedAuthor &&
                rep.suggestedAuthor &&
                !isUnknownSentinel(rep.suggestedAuthor)) {
                item.suggestedAuthor = rep.suggestedAuthor;
            }
            if (item.status === 'queued' || item.status === 'unfinished') {
                item.status = 'review';
            }
        }
    }
}

export const batchManager = new BatchManager();
