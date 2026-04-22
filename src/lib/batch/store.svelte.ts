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
import { extractText } from '../extractors';
import { flog } from '../log';

export interface ProcessOverrides {
    providerId?: string;
    modelId?: string;
    maxChars?: number;
    authorSort?: boolean;
    enforceOcr?: boolean;
    extractionOnly?: boolean;
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
            isAccepted: true,
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

    stopAll() {
        this.stopRequested = true;
        this.extractionAbort?.abort();
        this.llmAbort?.abort();
        flog('info', 'Stop requested by user — aborting extraction and LLM');
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

        flog('info', `processAll config: format=${parsingFormat} lang=${language} provider=${activeProviderId} model=${modelId} maxChars=${llmMaxChars}`);

        // Two-phase: EXTRACT all → ANALYZE all.
        // This decouples text extraction from LLM analysis so a stalled LLM
        // queue never blocks extraction of remaining items.
        const needsProcessing = (item: BatchItem) =>
            (item.status === 'queued' || item.status === 'error' || item.status === 'unfinished') &&
            (!onlyIds || onlyIds.has(item.id));

        const PAGE_WATCHDOG_MS = 30_000; // abort if no page progress for 30 s
        const RUST_EXTRACT_TIMEOUT_MS = 5 * 60 * 1000;

        try {
            // ── Phase 1: extraction ─────────────────────────────────────────
            for (const item of this.items) {
                if (!needsProcessing(item) || item.extractedText) continue;
                if (this.stopRequested) { flog('info', 'Stop requested, halting extraction phase'); break; }

                item.status = 'extracting';
                const forceOCR = overrides?.enforceOcr ?? false;

                try {
                    if (item.originalName.toLowerCase().endsWith('.pdf') && pdfBackend === 'rust' && !forceOCR) {
                        flog('info', `Rust extraction: ${item.originalName}`);
                        const text = await Promise.race([
                            invoke<string>('extract_pdf_native', { path: item.originalPath }),
                            new Promise<never>((_, reject) =>
                                setTimeout(() => reject(new Error('EXTRACT_TIMEOUT')), RUST_EXTRACT_TIMEOUT_MS)
                            )
                        ]);
                        item.extractedText = text;
                    } else {
                        flog('info', `JS extraction: ${item.originalName} (forceOCR=${forceOCR})`);
                        this.extractionAbort = new AbortController();
                        // Per-page watchdog: abort if no page progress for PAGE_WATCHDOG_MS
                        let watchdogId: ReturnType<typeof setTimeout> | null = null;
                        const resetWatchdog = () => {
                            if (watchdogId) clearTimeout(watchdogId);
                            watchdogId = setTimeout(() => {
                                this.extractionAbort?.abort(new Error('EXTRACT_PAGE_TIMEOUT'));
                                flog('warn', `Page watchdog fired (${PAGE_WATCHDOG_MS / 1000}s no progress): ${item.originalName}`);
                            }, PAGE_WATCHDOG_MS);
                        };
                        resetWatchdog(); // start watchdog immediately

                        const fileData = await readFile(item.originalPath);
                        if (this.stopRequested) {
                            if (watchdogId) clearTimeout(watchdogId);
                            this.extractionAbort = null;
                            item.status = 'queued';
                            break;
                        }
                        let extraction;
                        try {
                            extraction = await extractText(
                                { name: item.originalName, arrayBuffer: fileData.buffer },
                                {
                                    forceOCR,
                                    signal: this.extractionAbort.signal,
                                    maxPages: extractionMaxPages || undefined,
                                    onProgress: (page, total) => {
                                        item.statusDetail = `${page}/${total} pages`;
                                        resetWatchdog();
                                    }
                                }
                            );
                        } finally {
                            if (watchdogId) clearTimeout(watchdogId);
                            this.extractionAbort = null;
                        }
                        if (this.stopRequested) { item.status = 'queued'; break; }
                        item.extractedText = extraction.text;
                    }
                    item.statusDetail = (item.extractedText?.trim().length ?? 0) < 100 ? '⚠ poor extraction' : undefined;
                    // Park at 'queued' with text so phase 2 picks it up (unless extraction-only)
                    item.status = overrides?.extractionOnly ? 'review' : 'queued';
                    if (overrides?.extractionOnly) await this.calculateTargetPath(item);
                    flog('info', `Extracted: ${item.originalName} — ${item.extractedText?.length ?? 0} chars`);
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
                }
                await this.saveCurrentSession();
            }

            if (overrides?.extractionOnly) return; // done after phase 1

            // ── Phase 2: LLM analysis ───────────────────────────────────────
            if (!this.isMetadataExtractionEnabled) return;

            for (const item of this.items) {
                // Analyze items that have text but no metadata yet, or were previously queued/unfinished
                const needsAnalysis = (item.status === 'queued' || item.status === 'unfinished') &&
                    item.extractedText &&
                    (!onlyIds || onlyIds.has(item.id));
                if (!needsAnalysis) continue;
                if (this.stopRequested || this.llmAbort?.signal.aborted) {
                    flog('info', 'Stop requested, halting analysis phase');
                    break;
                }

                item.status = 'analyzing';
                try {
                    const textSample = item.extractedText!.substring(0, llmMaxChars);
                    const prompt = `${basePrompt}\n\nFilename: "${item.originalName}"\n\nDocument snippet:\n${textSample}`;

                    flog('info', `LLM analyze: ${item.originalName}`);
                    const response = await llmClient.query(activeProvider.id, modelId, prompt, activeProvider.apiKey, 0.3, this.llmAbort?.signal);
                    if (this.stopRequested || this.llmAbort?.signal.aborted) { item.status = 'queued'; break; }
                    const metadata = this.parseLLMResponse(response, parsingFormat);

                    item.suggestedTitle = metadata.title || 'Unknown Title';
                    item.suggestedAuthor = metadata.author || 'Unknown Author';
                    item.suggestedYear = metadata.year || 'Unknown Year';
                    flog('info', `Analyzed: ${item.originalName} → "${item.suggestedTitle}" / ${item.suggestedAuthor}`);

                    if (authorSortEnabled && item.suggestedAuthor && item.suggestedAuthor !== 'Unknown Author') {
                        const sortPrompt = `Reformat author to "Lastname Firstname": "${item.suggestedAuthor}". Output ONLY <AUTHOR> tags.`;
                        const sortRes = await llmClient.query(activeProvider.id, modelId, sortPrompt, activeProvider.apiKey, 0.3, this.llmAbort?.signal);
                        if (this.stopRequested || this.llmAbort?.signal.aborted) { item.status = 'queued'; break; }
                        const match = sortRes.match(/<AUTHOR>(.*?)<\/AUTHOR>/i);
                        if (match) item.suggestedAuthor = match[1].trim();
                    }

                    item.status = 'review';
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
                }
                await this.saveCurrentSession();
            }
        } finally {
            this.isProcessing = false;
            this.stopRequested = false;
            this.llmAbort = null;
            flog('info', 'processAll finished');
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
                return item;
            });
        }
    }

    async executeBatch(mode: string = 'move') {
        // Include 'review' AND 'error' items so previously-failed items can be retried
        const accepted = this.items.filter(i => i.isAccepted && i.targetPath && (i.status === 'review' || i.status === 'error'));
        console.log(`[BatchManager] executeBatch(${mode}): ${accepted.length} accepted out of ${this.items.length} total`);
        this.items.forEach(i => console.log(`  item: ${i.originalName} status=${i.status} accepted=${i.isAccepted} target=${i.targetPath ? '✓' : 'MISSING'}`));
        if (accepted.length === 0) return null;

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
}

export const batchManager = new BatchManager();
