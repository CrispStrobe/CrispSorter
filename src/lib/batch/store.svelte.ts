import { type BatchItem, type BatchStatus, type BatchSession } from '../types';
import { extractText } from '../extractors';
import { llmClient } from '../llm/client';
import { getSetting, saveSetting, getSetting as getFromStore } from '../store';
import { readFile, writeFile, mkdir } from '@tauri-apps/plugin-fs';
import { invoke } from '@tauri-apps/api/core';
import { save, open } from '@tauri-apps/plugin-dialog';

export function getDefaultPrompt(format: 'xml' | 'json', language: string): string {
    const isDE = language === 'de';
    if (format === 'json') {
        if (isDE) {
            return `Du bist ein Metadaten-Extraktionsassistent. Extrahiere bibliografische Metadaten aus dem bereitgestellten Dokumenttext und Dateinamen.\n\nAUSGABE-REGELN:\n- Gib NUR ein gültiges JSON-Objekt zurück. Kein Markdown, keine Erklärungen.\n- Alle Werte müssen Strings sein. Nutze null für fehlende Felder.\n- TITLE: Vollständiger Dokumenttitel aus dem Inhalt (nicht Dateiname).\n- AUTHOR: Format "Nachname Vorname". Alle akademischen Titel entfernen (Dr., Prof., PhD usw.).\n- YEAR: 4-stelliges Erscheinungsjahr. null wenn unbekannt.\n- LANGUAGE: 2-buchstabiger ISO 639-1 Code (z.B. "de", "en"). null wenn unklar.\n\nBEISPIEL-AUSGABE:\n{"title":"Titel des Dokuments","author":"Müller Hans","year":"2023","language":"de"}`;
        }
        return `You are a metadata extraction assistant. Extract bibliographic metadata from the provided document text and filename.\n\nOUTPUT RULES:\n- Return a valid JSON object ONLY. No markdown, no explanation, no surrounding text.\n- All values must be strings. Use null for missing fields.\n- TITLE: Full document title from content (not the filename).\n- AUTHOR: "Lastname Firstname" format. Strip all titles (Dr., Prof., PhD, etc.).\n- YEAR: 4-digit publication year. Use null if unknown.\n- LANGUAGE: 2-letter ISO 639-1 code (e.g. "en", "de"). Use null if uncertain.\n\nEXAMPLE OUTPUT:\n{"title":"Artificial Intelligence in Healthcare","author":"Smith John","year":"2023","language":"en"}`;
    } else {
        if (isDE) {
            return `Du bist ein Metadaten-Extraktionsassistent. Extrahiere bibliografische Metadaten aus dem bereitgestellten Dokumenttext und Dateinamen.\n\nAUSGABE-REGELN:\n- Gib NUR XML-Tags zurück. Kein Markdown, keine Erklärungen, kein weiterer Text.\n- TITLE: Vollständiger Dokumenttitel aus dem Inhalt.\n- AUTHOR: Format "Nachname Vorname". Alle akademischen Titel entfernen (Dr., Prof., PhD usw.).\n- YEAR: Nur 4-stellige Jahreszahl. "UnknownYear" wenn unbekannt.\n- LANGUAGE: 2-buchstabiger ISO 639-1 Code. "ul" wenn unklar.\n\nBEISPIEL-AUSGABE:\n<TITLE>Titel des Dokuments</TITLE>\n<YEAR>2023</YEAR>\n<AUTHOR>Müller Hans</AUTHOR>\n<LANGUAGE>de</LANGUAGE>`;
        }
        return `You are a metadata extraction assistant. Extract bibliographic metadata from the provided document text and filename.\n\nOUTPUT RULES:\n- Return XML tags ONLY. No markdown, no explanation, no extra text.\n- TITLE: Full document title from content.\n- AUTHOR: "Lastname Firstname" format. Strip all titles (Dr., Prof., PhD, etc.).\n- YEAR: 4-digit year only. Use "UnknownYear" if missing.\n- LANGUAGE: 2-letter ISO 639-1 code. Use "ul" if uncertain.\n\nEXAMPLE OUTPUT:\n<TITLE>Artificial Intelligence in Healthcare</TITLE>\n<YEAR>2023</YEAR>\n<AUTHOR>Smith John</AUTHOR>\n<LANGUAGE>en</LANGUAGE>`;
    }
}

export interface ProcessOverrides {
    providerId?: string;
    modelId?: string;
    maxChars?: number;
    authorSort?: boolean;
}

export class BatchManager {
    items = $state<BatchItem[]>([]);
    isProcessing = $state(false);
    stopRequested = $state(false);
    isMetadataExtractionEnabled = $state(true);
    currentSessionId = $state<string | null>(null);
    
    // UI Filters
    searchQuery = $state('');
    filterExtension = $state('all');
    filterStatus = $state('all');
    filterMinSize = $state(0);

    // Track paths synchronously to prevent rapid drop duplicates
    private processingPaths = new Set<string>();

    filteredItems = $derived(
        this.items.filter(item => {
            const matchesSearch = item.originalName.toLowerCase().includes(this.searchQuery.toLowerCase()) ||
                                 (item.suggestedTitle?.toLowerCase().includes(this.searchQuery.toLowerCase()) ?? false) ||
                                 (item.suggestedAuthor?.toLowerCase().includes(this.searchQuery.toLowerCase()) ?? false);
            
            const matchesExt = this.filterExtension === 'all' || item.extension === this.filterExtension;
            const matchesStatus = this.filterStatus === 'all' || item.status === this.filterStatus;
            const matchesSize = item.size >= this.filterMinSize * 1024;

            return matchesSearch && matchesExt && matchesStatus && matchesSize;
        })
    );

    constructor() {
        console.log("[BatchManager] Initialized");
    }

    async createNewSession() {
        console.log("[BatchManager] Creating new session");
        this.currentSessionId = crypto.randomUUID();
        this.items = [];
        this.processingPaths.clear();
        await this.saveCurrentSession();
    }

    async addItem(path: string, name: string, size = 0, modifiedAt = Date.now()) {
        if (this.processingPaths.has(path) || this.items.some(i => i.originalPath === path)) {
            console.log(`[BatchManager] Synchronous duplicate block for: ${name}`);
            return;
        }

        this.processingPaths.add(path);
        console.log(`[BatchManager] Adding item: ${name} at ${path} (${size} bytes)`);

        const id = crypto.randomUUID();
        const extension = name.split('.').pop()?.toLowerCase() || '';

        this.items.push({
            id,
            originalPath: path,
            originalName: name,
            extension,
            size,
            modifiedAt,
            status: 'queued',
            isAccepted: false,
            isIgnored: false
        });

        await this.saveCurrentSession();
    }

    async reprocessItems(ids: string[], overrides?: ProcessOverrides) {
        ids.forEach(id => {
            const item = this.items.find(i => i.id === id);
            if (item) item.status = 'queued';
        });
        await this.processAll(overrides, new Set(ids));
    }

    async reextractItems(ids: string[]) {
        ids.forEach(id => {
            const item = this.items.find(i => i.id === id);
            if (item) {
                item.status = 'queued';
                item.extractedText = undefined;
            }
        });
        await this.processAll(undefined, new Set(ids));
    }

    async removeItems(ids: string[]) {
        const idSet = new Set(ids);
        this.items = this.items.filter(item => {
            if (idSet.has(item.id)) {
                this.processingPaths.delete(item.originalPath);
                return false;
            }
            return true;
        });
        await this.saveCurrentSession();
    }

    async setAcceptedItems(ids: string[], isAccepted: boolean) {
        const idSet = new Set(ids);
        this.items.forEach(item => {
            if (idSet.has(item.id)) {
                item.isAccepted = isAccepted;
            }
        });
        await this.saveCurrentSession();
    }

    stopAll() {
        if (this.isProcessing) {
            this.stopRequested = true;
            console.log('[BatchManager] Stop requested by user');
        }
    }

    async processAll(overrides?: ProcessOverrides, onlyIds?: Set<string>) {
        console.log("[BatchManager] Starting processAll loop", overrides ? `(overrides: ${JSON.stringify(overrides)})` : '', onlyIds ? `(onlyIds: ${onlyIds.size})` : '');
        if (this.isProcessing) return;
        this.isProcessing = true;

        const providers = await getSetting('providers', []);
        const activeProviderId = overrides?.providerId || await getSetting('activeProviderId', 'ollama');
        const activeProvider = (providers as any[]).find(p => p.id === activeProviderId) || providers[0];

        let modelId = overrides?.modelId || activeProvider?.selectedModel || activeProvider?.models?.[0];

        if (!overrides?.modelId && ['mistralrs', 'llamacpp'].includes(activeProviderId)) {
            const localModels = await getSetting('localModels', []) as any[];
            const activeLocal = localModels.find(m => m.isActive && m.isDownloaded);
            modelId = activeLocal?.path || modelId;
        }

        const globalExportPath = await getSetting('exportPath', '');
        const globalSaveTxt = await getSetting('saveTxt', true);
        const llmMaxChars = overrides?.maxChars ?? await getSetting('llmMaxChars', 5000);
        const parsingFormat = await getSetting('parsingFormat', 'xml') as 'xml' | 'json';
        const authorSortEnabled = overrides?.authorSort ?? await getSetting('authorSortEnabled', false);
        const pdfBackend = await getSetting('pdfBackend', 'js');
        const language = await getSetting('language', 'en') as string;

        const defaultPrompt = getDefaultPrompt(parsingFormat, language);
        const basePrompt = await getSetting('llmPrompt', defaultPrompt);

        console.log(`[BatchManager] processAll config: format=${parsingFormat}, language=${language}, provider=${activeProviderId}, model=${modelId}, maxChars=${llmMaxChars}, authorSort=${authorSortEnabled}`);
        console.log(`[BatchManager] Items to process: ${this.items.filter(i => i.status === 'queued' || i.status === 'error').map(i => i.originalName).join(', ') || 'none'}`);

        try {
        for (const item of this.items) {
            if (item.status !== 'queued' && item.status !== 'error') continue;
            if (onlyIds && !onlyIds.has(item.id)) continue;
            if (this.stopRequested) {
                console.log(`[BatchManager] Stop requested, halting before: ${item.originalName}`);
                break;
            }

            try {
                // Skip re-extraction if text already exists (reprocessItems preserves it; reextractItems clears it)
                if (!item.extractedText) {
                    item.status = 'extracting';
                    if (item.originalName.toLowerCase().endsWith('.pdf') && pdfBackend === 'rust') {
                        const text = await invoke('extract_pdf_native', { path: item.originalPath });
                        item.extractedText = text as string;
                    } else {
                        const fileData = await readFile(item.originalPath);
                        const extraction = await extractText({ name: item.originalName, arrayBuffer: fileData.buffer });
                        item.extractedText = extraction.text;
                    }
                    console.log(`[BatchManager] Extracted: ${item.originalName} — ${item.extractedText.length} chars (LLM will use first ${llmMaxChars})`);
                } else {
                    console.log(`[BatchManager] Skipping extraction for ${item.originalName}, using cached ${item.extractedText.length} chars`);
                }

                if (this.isMetadataExtractionEnabled) {
                    if (!activeProvider || !modelId) {
                        throw new Error(`AI Provider or Model not selected in Settings.`);
                    }
                    if (!activeProvider.apiKey && !['ollama', 'mistralrs'].includes(activeProvider.id)) {
                        throw new Error(`API Key for ${activeProvider.name} is missing.`);
                    }

                    item.status = 'analyzing';
                    const textSample = item.extractedText.substring(0, llmMaxChars);
                    const prompt = `${basePrompt}\n\nFilename: "${item.originalName}"\n\nDocument snippet:\n${textSample}`;
                    
                    console.log(`[BatchManager] Querying AI for: ${item.originalName}`);
                    console.log(`[BatchManager] PROMPT (Shortened): ${prompt.substring(0, 1500)}...`);
                    
                    const response = await llmClient.query(activeProvider.id, modelId, prompt, activeProvider.apiKey);
                    
                    console.log(`[BatchManager] AI RESPONSE: ${response}`);
                    
                    const metadata = this.parseLLMResponse(response, parsingFormat);
                    item.suggestedTitle = metadata.title || 'Unknown Title';
                    item.suggestedAuthor = metadata.author || 'Unknown Author';
                    item.suggestedYear = metadata.year || 'Unknown Year';

                    if (authorSortEnabled && item.suggestedAuthor && item.suggestedAuthor !== 'Unknown Author') {
                        const sortPrompt = `Reformat the following author name to "Lastname Firstname" order (surname first, then given name). Strip ALL academic titles and honorifics (Dr., Prof., PhD, Dipl., Ing., M.D., M.A., etc.). Output ONLY the reformatted name inside <AUTHOR> tags — no explanation, no extra text.\nInput: "${item.suggestedAuthor}"\n<AUTHOR>`;
                        console.log(`[BatchManager] Author sort prompt: ${sortPrompt}`);
                        const sortRes = await llmClient.query(activeProvider.id, modelId, sortPrompt, activeProvider.apiKey);
                        console.log(`[BatchManager] Author sort response: ${sortRes}`);
                        const match = sortRes.match(/<AUTHOR>(.*?)<\/AUTHOR>/i) || sortRes.match(/^([^\n<]+)/);
                        if (match) item.suggestedAuthor = this.cleanAuthorName(match[1].trim()) ?? item.suggestedAuthor;
                    }
                    
                    const safeTitle = (item.suggestedTitle as string).replace(/[\\/:*?"<>|]/g, '');
                    const safeAuthor = (item.suggestedAuthor as string).replace(/[\\/:*?"<>|]/g, '');
                    const lastSlash = item.originalPath.lastIndexOf('/');
                    const baseDir = globalExportPath || (lastSlash !== -1 ? item.originalPath.substring(0, lastSlash) : '.');
                    
                    item.targetPath = `${baseDir}/Sorted/${safeAuthor}/${item.suggestedYear !== 'Unknown Year' ? item.suggestedYear + ' - ' : ''}${safeTitle}.${item.extension}`;
                    item.status = 'review';
                } else {
                    const lastSlash = item.originalPath.lastIndexOf('/');
                    const baseDir = globalExportPath || (lastSlash !== -1 ? item.originalPath.substring(0, lastSlash) : '.');
                    item.targetPath = `${baseDir}/Extracted/${item.originalName}.txt`;
                    item.status = 'review';
                }
            } catch (error: any) {
                item.status = 'error';
                item.errorMessage = error.message || String(error);
                console.error(`[BatchManager] Error processing ${item.originalName}:`, error);
            }
            await this.saveCurrentSession();
        }
        } finally {
            this.isProcessing = false;
            this.stopRequested = false;
            await this.saveCurrentSession();
        }
    }

    private cleanAuthorName(author: string | undefined): string | undefined {
        if (!author) return undefined;
        // Strip academic titles that low-capacity models include despite the prompt
        const cleaned = author
            .replace(/\b(Prof\.\s*Dr\.-Ing\.|Prof\.\s*Dr\.|Dr\.\s*Prof\.|PD\s+Dr\.|Dipl\.-[A-Za-zäöü]+\.\s*|Prof\.|Dr\.|PhD\.?|M\.D\.?|M\.A\.?|B\.A\.?|B\.Sc\.?|M\.Sc\.?|Mag\.|Ing\.)\s*/gi, '')
            .replace(/\s+/g, ' ')
            .trim();
        if (cleaned && cleaned !== author) {
            console.log(`[BatchManager] cleanAuthorName: "${author}" → "${cleaned}"`);
        }
        return cleaned || author;
    }

    private fixMalformedXmlTags(text: string): string {
        // Fix tags with equals signs: <TAG=value</TAG> -> <TAG>value</TAG>
        let fixed = text.replace(/<(TITLE|AUTHOR|YEAR|LANGUAGE)[=\s]+([^>]*?)<\/(\1)>/gi, (match, tag, val) => {
            let content = val.trim();
            if ((content.startsWith('"') && content.endsWith('"')) || (content.startsWith("'") && content.endsWith("'"))) {
                content = content.slice(1, -1);
            }
            return `<${tag}>${content}</${tag}>`;
        });
        // Fix unclosed malformed tags: <TAG=value> -> <TAG>value</TAG>
        fixed = fixed.replace(/<(TITLE|AUTHOR|YEAR|LANGUAGE)[=\s]+([^<>]+?)(?=\s*(?:<|$))/gi, '<$1>$2</$1>');
        return fixed;
    }

    private tryParseJson(response: string): { title?: string, author?: string, year?: string } | null {
        // Step 1: Strip markdown code fences
        let clean = response.replace(/```json\s*/gi, '').replace(/```\s*/g, '').trim();
        console.log(`[BatchManager] JSON Step 1 (stripped fences): "${clean.substring(0, 200)}"`);

        // Step 2: Strip JS-style comments
        const preComment = clean;
        clean = clean.replace(/\/\/[^\n]*/g, '').replace(/\/\*[\s\S]*?\*\//g, '');
        if (clean.length !== preComment.length) {
            console.log(`[BatchManager] JSON Step 2 (stripped comments): removed ${preComment.length - clean.length} chars`);
        }

        // Step 3: Isolate outermost { ... }
        const start = clean.indexOf('{');
        const end = clean.lastIndexOf('}');
        if (start === -1 || end === -1 || end <= start) {
            console.warn(`[BatchManager] JSON Step 3 FAILED: no valid { } pair found (start=${start}, end=${end})`);
            return null;
        }
        clean = clean.substring(start, end + 1);
        console.log(`[BatchManager] JSON Step 3 (isolated braces): "${clean}"`);

        // Step 4: Try strict parse
        try {
            const data = JSON.parse(clean);
            console.log(`[BatchManager] JSON Step 4 (parse success):`, data);
            const year = data.year != null && data.year !== 'null' ? String(data.year) : undefined;
            return { title: data.title || undefined, author: this.cleanAuthorName(data.author || undefined), year };
        } catch (e) {
            console.warn(`[BatchManager] JSON Step 4 (parse error):`, e);
        }

        // Step 5: Fuzzy regex fallback
        console.log(`[BatchManager] JSON Step 5 (fuzzy regex fallback)...`);
        const titleMatch = response.match(/"title"\s*:\s*"([^"]*?)"/i);
        const authorMatch = response.match(/"author"\s*:\s*"([^"]*?)"/i);
        const yearMatch = response.match(/"year"\s*:\s*"(\d{4})"/i) || response.match(/"year"\s*:\s*(\d{4})/i);
        console.log(`[BatchManager] JSON Step 5 results: title=${!!titleMatch}, author=${!!authorMatch}, year=${!!yearMatch}`);

        if (titleMatch || authorMatch || yearMatch) {
            return { title: titleMatch?.[1], author: this.cleanAuthorName(authorMatch?.[1]), year: yearMatch?.[1] };
        }
        return null;
    }

    private parseXml(response: string): { title?: string, author?: string, year?: string } {
        const cleaned = this.fixMalformedXmlTags(response);
        console.log(`[BatchManager] XML parsing input (first 300): "${cleaned.substring(0, 300)}"`);

        const extractTag = (tag: string) => {
            const regex = new RegExp(`<${tag}[^>]*>([\\s\\S]*?)<\\/${tag}>`, 'i');
            const match = cleaned.match(regex);
            const val = match ? match[1].trim() : undefined;
            console.log(`[BatchManager] XML <${tag}>: ${val !== undefined ? `"${val}"` : 'NOT FOUND'}`);
            return val;
        };

        const title = extractTag('TITLE') || extractTag('PUBLICATION TITLE') || extractTag('PUBLICATIONTITLE');
        const author = this.cleanAuthorName(extractTag('AUTHOR'));
        const year = extractTag('YEAR');
        return { title, author, year };
    }

    private parseLLMResponse(response: string, format: 'xml' | 'json'): { title?: string, author?: string, year?: string } {
        console.log(`[BatchManager] parseLLMResponse: preferred format=${format.toUpperCase()}, response length=${response.length}`);
        console.log(`[BatchManager] Raw response preview: "${response.substring(0, 400)}"`);

        // Auto-detect JSON: try JSON if format is json OR response looks like JSON
        const stripped = response.replace(/```json\s*/gi, '').replace(/```\s*/g, '').trim();
        const looksLikeJson = stripped.startsWith('{') || response.includes('```json');
        console.log(`[BatchManager] Detection: format=${format}, looksLikeJson=${looksLikeJson}`);

        if (format === 'json' || looksLikeJson) {
            console.log(`[BatchManager] Attempting JSON parsing...`);
            const result = this.tryParseJson(response);
            if (result) {
                console.log(`[BatchManager] JSON parsing succeeded:`, result);
                return result;
            }
            console.warn(`[BatchManager] JSON parsing failed (format=${format}, looksLikeJson=${looksLikeJson}), falling back to XML`);
        }

        console.log(`[BatchManager] Attempting XML tag parsing...`);
        return this.parseXml(response);
    }

    async executeBatch() {
        const toMove = this.items.filter(i => i.isAccepted && i.targetPath && (i.status === 'review' || i.status === 'error'));
        if (toMove.length === 0) return;

        const globalSaveTxt = await getSetting('saveTxt', true);
        toMove.forEach(i => i.status = 'moving');

        for (const item of toMove) {
            try {
                if (globalSaveTxt && item.extractedText) {
                    const txtPath = item.targetPath!.replace(/\.[^.]+$/, '.txt');
                    const lastSlash = txtPath.lastIndexOf('/');
                    const parentDir = lastSlash !== -1 ? txtPath.substring(0, lastSlash) : '.';
                    try { await mkdir(parentDir, { recursive: true }); } catch(e) {}
                    const encoder = new TextEncoder();
                    await writeFile(txtPath, encoder.encode(item.extractedText));
                }

                const results: string[] = await invoke('move_files', { 
                    moves: [{ source: item.originalPath, destination: item.targetPath! }] 
                });

                if (results[0].startsWith('Success:')) item.status = 'done';
                else { item.status = 'error'; item.errorMessage = results[0]; }
            } catch (error: any) {
                item.status = 'error';
                item.errorMessage = error.message;
            }
        }

        // Remove successfully moved items from the list
        const doneIds = new Set(toMove.filter(i => i.status === 'done').map(i => i.id));
        if (doneIds.size > 0) {
            this.items = this.items.filter(item => {
                if (doneIds.has(item.id)) {
                    this.processingPaths.delete(item.originalPath);
                    return false;
                }
                return true;
            });
            console.log(`[BatchManager] Removed ${doneIds.size} done items from batch`);
        }
        await this.saveCurrentSession();
    }

    async getDuplicateGroups(checkContent = false): Promise<Array<{ size: number; items: BatchItem[] }>> {
        // Group by size, skip tiny files
        const bySize = new Map<number, BatchItem[]>();
        for (const item of this.items) {
            if (item.size < 100) continue;
            if (!bySize.has(item.size)) bySize.set(item.size, []);
            bySize.get(item.size)!.push(item);
        }

        const sizeGroups = [...bySize.entries()]
            .filter(([, items]) => items.length > 1)
            .map(([size, items]) => ({ size, items }));

        if (!checkContent) return sizeGroups;

        // Content hash (SHA-256) — only for same-size candidates
        const result: Array<{ size: number; items: BatchItem[] }> = [];
        for (const group of sizeGroups) {
            const hashMap = new Map<string, BatchItem[]>();
            for (const item of group.items) {
                try {
                    const data = await readFile(item.originalPath);
                    const hashBuf = await crypto.subtle.digest('SHA-256', data);
                    const hash = Array.from(new Uint8Array(hashBuf)).map(b => b.toString(16).padStart(2, '0')).join('');
                    if (!hashMap.has(hash)) hashMap.set(hash, []);
                    hashMap.get(hash)!.push(item);
                } catch (e) {
                    console.warn(`[BatchManager] Could not hash ${item.originalName}:`, e);
                }
            }
            for (const [, items] of hashMap) {
                if (items.length > 1) result.push({ size: group.size, items });
            }
        }
        return result;
    }

    async saveCurrentSession() {
        if (!this.currentSessionId) this.currentSessionId = crypto.randomUUID();
        const session: BatchSession = {
            id: this.currentSessionId,
            startTime: Date.now(),
            items: $state.snapshot(this.items),
            status: this.isProcessing ? 'active' : 'paused',
            providerId: 'ollama',
            modelId: ''
        };
        const sessions = await getFromStore('sessions', {}) as Record<string, BatchSession>;
        sessions[this.currentSessionId] = session;
        await saveSetting('sessions', sessions);
    }

    async loadSession(id: string) {
        const sessions = await getFromStore('sessions', {}) as Record<string, BatchSession>;
        const session = sessions[id];
        if (session) {
            this.currentSessionId = session.id;
            this.items = session.items;
            this.processingPaths = new Set(this.items.map(i => i.originalPath));
        }
    }

    async resumeLastSession() {
        const sessions = await getFromStore('sessions', {}) as Record<string, BatchSession>;
        const lastSession = Object.values(sessions).sort((a, b) => b.startTime - a.startTime)[0];
        if (lastSession) await this.loadSession(lastSession.id);
        else await this.createNewSession();
    }

    async exportBatch() {
        const sessionData = JSON.stringify({ items: $state.snapshot(this.items), isMetadataExtractionEnabled: this.isMetadataExtractionEnabled }, null, 2);
        const filePath = await save({ filters: [{ name: 'CrispSorter Batch', extensions: ['json'] }], defaultPath: `batch_export_${new Date().toISOString().split('T')[0]}.json` });
        if (filePath) {
            const encoder = new TextEncoder();
            await writeFile(filePath, encoder.encode(sessionData));
            alert('Batch exported successfully!');
        }
    }

    async importBatch() {
        const filePath = await open({ multiple: false, filters: [{ name: 'CrispSorter Batch', extensions: ['json'] }] });
        if (typeof filePath === 'string') {
            const fileData = await readFile(filePath);
            const decoder = new TextDecoder();
            const data = JSON.parse(decoder.decode(fileData));
            await this.createNewSession();
            this.items = data.items || [];
            this.isMetadataExtractionEnabled = data.isMetadataExtractionEnabled ?? true;
            this.processingPaths = new Set(this.items.map(i => i.originalPath));
            await saveSetting('isMetadataExtractionEnabled', this.isMetadataExtractionEnabled);
            await saveSetting('parsingFormat', data.parsingFormat || 'xml');
            await this.saveCurrentSession();
            alert('Batch imported successfully!');
        }
    }

    clear() {
        this.items = [];
        this.processingPaths.clear();
        this.saveCurrentSession();
    }
}

export const batchManager = new BatchManager();
