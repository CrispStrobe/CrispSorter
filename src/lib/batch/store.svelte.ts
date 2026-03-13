import { type BatchItem, type BatchStatus, type BatchSession } from '../types';
import { extractText } from '../extractors'; 
import { llmClient } from '../llm/client';
import { getSetting, saveSetting, getSetting as getFromStore } from '../store';
import { readFile, writeFile, mkdir, stat } from '@tauri-apps/plugin-fs';
import { invoke } from '@tauri-apps/api/core';
import { save, open } from '@tauri-apps/plugin-dialog';

export class BatchManager {
    items = $state<BatchItem[]>([]);
    isProcessing = $state(false);
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

    async addItem(path: string, name: string) {
        if (this.processingPaths.has(path) || this.items.some(i => i.originalPath === path)) {
            console.log(`[BatchManager] Synchronous duplicate block for: ${name}`);
            return;
        }

        this.processingPaths.add(path);
        console.log(`[BatchManager] Adding item: ${name} at ${path}`);
        
        const id = crypto.randomUUID();
        const extension = name.split('.').pop()?.toLowerCase() || '';
        
        const newItem: BatchItem = {
            id,
            originalPath: path,
            originalName: name,
            extension,
            size: 0,
            modifiedAt: Date.now(),
            status: 'queued',
            isAccepted: false,
            isIgnored: false
        };
        
        this.items.push(newItem);

        try {
            const s = await stat(path);
            newItem.size = s.size;
            newItem.modifiedAt = s.mtime?.getTime() || Date.now();
        } catch(e) {
            console.warn(`[BatchManager] Failed to stat file ${path}:`, e);
        }

        await this.saveCurrentSession();
    }

    async processAll() {
        console.log("[BatchManager] Starting processAll loop");
        if (this.isProcessing) return;
        this.isProcessing = true;

        const providers = await getSetting('providers', []);
        const activeProviderId = await getSetting('activeProviderId', 'ollama');
        const activeProvider = (providers as any[]).find(p => p.id === activeProviderId) || providers[0];
        
        let modelId = activeProvider?.selectedModel || activeProvider?.models?.[0];
        
        if (['mistralrs', 'llamacpp'].includes(activeProviderId)) {
            const localModels = await getSetting('localModels', []) as any[];
            const activeLocal = localModels.find(m => m.isActive && m.isDownloaded);
            modelId = activeLocal?.path || modelId;
        }

        const globalExportPath = await getSetting('exportPath', '');
        const globalSaveTxt = await getSetting('saveTxt', true);
        const llmMaxChars = await getSetting('llmMaxChars', 5000);
        const parsingFormat = await getSetting('parsingFormat', 'xml') as 'xml' | 'json';
        const authorSortEnabled = await getSetting('authorSortEnabled', false);

        // Define prompts based on format
        const xmlPrompt = 'Extract metadata from this document. Respond ONLY in this exact format:\n<TITLE>...</TITLE>\n<YEAR>YYYY</YEAR>\n<AUTHOR>Lastname Firstname</AUTHOR>\n<LANGUAGE>ISO</LANGUAGE>';
        const jsonPrompt = 'Extract metadata from this document text. Return JSON ONLY. { "title": "...", "author": "...", "year": "...", "language": "..." }.';
        
        const basePrompt = await getSetting('llmPrompt', parsingFormat === 'xml' ? xmlPrompt : jsonPrompt);

        for (const item of this.items) {
            if (item.status !== 'queued' && item.status !== 'error') continue;
            
            try {
                item.status = 'extracting';
                const fileData = await readFile(item.originalPath);
                const extraction = await extractText({ name: item.originalName, arrayBuffer: fileData.buffer });
                item.extractedText = extraction.text;

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
                    
                    const response = await llmClient.query(activeProvider.id, modelId, prompt, activeProvider.apiKey);
                    
                    const metadata = this.parseLLMResponse(response, parsingFormat);
                    item.suggestedTitle = metadata.title || 'Unknown Title';
                    item.suggestedAuthor = metadata.author || 'Unknown Author';
                    item.suggestedYear = metadata.year || 'Unknown Year';

                    if (authorSortEnabled && item.suggestedAuthor && item.suggestedAuthor !== 'Unknown Author') {
                        const sortPrompt = `Convert author name to "Lastname Firstname" format. Use <AUTHOR> tags. Name: "${item.suggestedAuthor}"`;
                        const sortRes = await llmClient.query(activeProvider.id, modelId, sortPrompt, activeProvider.apiKey);
                        const match = sortRes.match(/<AUTHOR>(.*?)<\/AUTHOR>/i);
                        if (match) item.suggestedAuthor = match[1].trim();
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
            }
            await this.saveCurrentSession();
        }
        this.isProcessing = false;
        await this.saveCurrentSession();
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

    private parseLLMResponse(response: string, format: 'xml' | 'json'): { title?: string, author?: string, year?: string } {
        if (format === 'json') {
            try {
                const cleanJson = response.replace(/```json|```/g, '').trim();
                const data = JSON.parse(cleanJson);
                return { title: data.title, author: data.author, year: data.year };
            } catch (e) {
                // Fallback to XML parsing if JSON fails even in JSON mode
                console.warn("[BatchManager] JSON parsing failed, falling back to XML tags");
            }
        }

        const cleaned = this.fixMalformedXmlTags(response);
        
        const extractTag = (tag: string) => {
            const regex = new RegExp(`<${tag}[^>]*>(.*?)<\/${tag}>`, 'i');
            const match = cleaned.match(regex);
            return match ? match[1].trim() : undefined;
        };

        let title = extractTag('TITLE');
        const author = extractTag('AUTHOR');
        const year = extractTag('YEAR');

        // Handle alternates
        if (!title) {
            title = extractTag('PUBLICATION TITLE') || extractTag('PUBLICATIONTITLE');
        }

        return { title, author, year };
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
        await this.saveCurrentSession();
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
