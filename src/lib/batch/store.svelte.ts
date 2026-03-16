import { get, writable } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { readFile } from '@tauri-apps/plugin-fs';
import { documentDir, downloadDir, join, resolve } from '@tauri-apps/api/path';
import { getSetting, saveSetting } from '../store';
import { llmClient } from '../llm/client';
import { getWebLLMLoadedModel } from '../llm/webllm';
import { getORTLoadedModel } from '../llm/ort';
import type { BatchItem, Metadata } from '../types';
import { extractText } from '../extractors';

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
            isAccepted: true
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
    }

    async reextractItems(ids: string[], enforceOcr: boolean = false) {
        ids.forEach(id => {
            const item = this.items.find(i => i.id === id);
            if (item) {
                item.extractedText = undefined;
                item.status = 'queued';
            }
        });
        await this.processAll({ enforceOcr, extractionOnly: true }, new Set(ids));
    }

    async reprocessItems(ids: string[], overrides?: ProcessOverrides) {
        ids.forEach(id => {
            const item = this.items.find(i => i.id === id);
            if (item) {
                item.status = 'queued';
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
        console.log("[BatchManager] Starting processAll loop", overrides ? `(overrides: ${JSON.stringify(overrides)})` : '', onlyIds ? `(onlyIds: ${onlyIds.size})` : '');
        if (this.isProcessing) return;
        this.isProcessing = true;
        this.stopRequested = false;

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

        console.log(`[BatchManager] processAll config: format=${parsingFormat}, language=${language}, provider=${activeProviderId}, model=${modelId}, maxChars=${llmMaxChars}, authorSort=${authorSortEnabled}`);

        try {
            for (const item of this.items) {
                if (item.status !== 'queued' && item.status !== 'error') continue;
                if (onlyIds && !onlyIds.has(item.id)) continue;
                if (this.stopRequested) {
                    console.log(`[BatchManager] Stop requested, halting.`);
                    break;
                }

                try {
                    if (!item.extractedText) {
                        item.status = 'extracting';
                        const forceOCR = overrides?.enforceOcr ?? false;
                        
                        if (item.originalName.toLowerCase().endsWith('.pdf') && pdfBackend === 'rust' && !forceOCR) {
                            console.log(`[BatchManager] Using Rust-Native extraction for ${item.originalName}`);
                            const text = await invoke('extract_pdf_native', { path: item.originalPath });
                            item.extractedText = text as string;
                        } else {
                            console.log(`[BatchManager] Using JS-Native extraction for ${item.originalName} (forceOCR=${forceOCR})`);
                            const fileData = await readFile(item.originalPath);
                            const extraction = await extractText({ name: item.originalName, arrayBuffer: fileData.buffer }, { forceOCR });
                            item.extractedText = extraction.text;
                        }
                        console.log(`[BatchManager] Extracted: ${item.originalName} — ${item.extractedText?.length} chars`);
                    }

                    // Metadata Analysis
                    if (this.isMetadataExtractionEnabled && !overrides?.extractionOnly) {
                        item.status = 'analyzing';
                        const textSample = item.extractedText?.substring(0, llmMaxChars) || '';
                        const prompt = `${basePrompt}\n\nFilename: "${item.originalName}"\n\nDocument snippet:\n${textSample}`;
                        
                        const response = await llmClient.query(activeProvider.id, modelId, prompt, activeProvider.apiKey);
                        const metadata = this.parseLLMResponse(response, parsingFormat);
                        
                        item.suggestedTitle = metadata.title || 'Unknown Title';
                        item.suggestedAuthor = metadata.author || 'Unknown Author';
                        item.suggestedYear = metadata.year || 'Unknown Year';

                        if (authorSortEnabled && item.suggestedAuthor && item.suggestedAuthor !== 'Unknown Author') {
                            const sortPrompt = `Reformat author to "Lastname Firstname": "${item.suggestedAuthor}". Output ONLY <AUTHOR> tags.`;
                            const sortRes = await llmClient.query(activeProvider.id, modelId, sortPrompt, activeProvider.apiKey);
                            const match = sortRes.match(/<AUTHOR>(.*?)<\/AUTHOR>/i);
                            if (match) item.suggestedAuthor = match[1].trim();
                        }
                    }

                    item.status = 'review';
                    await this.calculateTargetPath(item);
                } catch (e: any) {
                    item.status = 'error';
                    item.errorMessage = e.message || String(e);
                    console.error(`[BatchManager] Error processing ${item.originalName}:`, e);
                }
                await this.saveCurrentSession();
            }
        } finally {
            this.isProcessing = false;
            this.stopRequested = false;
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
            this.items = (last as any).items;
        }
    }

    async executeBatch(mode: string = 'move') {
        // Include 'review' AND 'error' items so previously-failed items can be retried
        const accepted = this.items.filter(i => i.isAccepted && i.targetPath && (i.status === 'review' || i.status === 'error'));
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
                            copiedFallback++;
                        } else {
                            item.errorMessage = undefined;
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
