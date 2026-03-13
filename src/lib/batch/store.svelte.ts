import { type BatchItem, type BatchStatus, type BatchSession } from '../types';
import { extractText } from '../extractors'; 
import { llmClient } from '../llm/client';
import { getSetting, saveSetting, getSetting as getFromStore } from '../store';
import { readFile, writeFile } from '@tauri-apps/plugin-fs';
import { invoke } from '@tauri-apps/api/core';
import { save, open } from '@tauri-apps/plugin-dialog';

export class BatchManager {
    items = $state<BatchItem[]>([]);
    isProcessing = $state(false);
    isMetadataExtractionEnabled = $state(true);
    currentSessionId = $state<string | null>(null);
    
    constructor() {
        console.log("[BatchManager] Initialized");
    }

    async createNewSession() {
        console.log("[BatchManager] Creating new session");
        this.currentSessionId = crypto.randomUUID();
        this.items = [];
        await this.saveCurrentSession();
    }

    addItem(path: string, name: string) {
        console.log(`[BatchManager] Adding item: ${name} at ${path}`);
        const id = crypto.randomUUID();
        const extension = name.split('.').pop()?.toLowerCase() || '';
        this.items.push({
            id,
            originalPath: path,
            originalName: name,
            extension,
            status: 'queued',
            isAccepted: false,
            isIgnored: false
        });
        this.saveCurrentSession();
    }

    async processAll() {
        console.log("[BatchManager] Starting processAll loop");
        if (this.isProcessing) {
            console.warn("[BatchManager] Already processing, skipping");
            return;
        }
        this.isProcessing = true;

        console.log("[BatchManager] Loading providers for LLM analysis");
        const providers = await getSetting('providers', []);
        const activeProvider = (providers as any[]).find(p => p.id === 'ollama') || providers[0];
        const modelId = activeProvider?.models?.[0];
        
        console.log(`[BatchManager] Active Provider: ${activeProvider?.name || 'None'}, Model: ${modelId || 'None'}`);

        for (const item of this.items) {
            if (item.status !== 'queued' && item.status !== 'error') {
                console.log(`[BatchManager] Skipping ${item.originalName} (Status: ${item.status})`);
                continue;
            }
            
            console.log(`[BatchManager] >>> Processing ${item.originalName}`);
            try {
                // 1. Extraction
                item.status = 'extracting';
                console.log(`[BatchManager] [${item.originalName}] Step 1: Reading file from disk...`);
                const fileData = await readFile(item.originalPath);
                console.log(`[BatchManager] [${item.originalName}] Step 1: File read success, length: ${fileData.length} bytes`);
                
                console.log(`[BatchManager] [${item.originalName}] Step 2: Calling extractText...`);
                const extraction = await extractText({ name: item.originalName, arrayBuffer: fileData.buffer });
                item.extractedText = extraction.text;
                console.log(`[BatchManager] [${item.originalName}] Step 2: Extraction success, text length: ${item.extractedText.length}`);

                // 2. LLM Analysis
                if (this.isMetadataExtractionEnabled && activeProvider && modelId) {
                    item.status = 'analyzing';
                    console.log(`[BatchManager] [${item.originalName}] Step 3: Sending to LLM (${activeProvider.id})...`);
                    const prompt = `Extract metadata from this document text. Return JSON ONLY. { "title": "...", "author": "...", "year": "..." }. Use the first few pages of text: ${item.extractedText.substring(0, 4000)}`;
                    
                    const response = await llmClient.query(activeProvider.id, modelId, prompt, activeProvider.apiKey);
                    console.log(`[BatchManager] [${item.originalName}] Step 3: LLM Response received: ${response.substring(0, 100)}...`);
                    
                    try {
                        const metadata = JSON.parse(response.replace(/```json|```/g, '').trim());
                        item.suggestedTitle = metadata.title || 'Unknown Title';
                        item.suggestedAuthor = metadata.author || 'Unknown Author';
                        item.suggestedYear = metadata.year || '';
                        
                        const safeTitle = (item.suggestedTitle as string).replace(/[\\/:*?"<>|]/g, '');
                        const safeAuthor = (item.suggestedAuthor as string).replace(/[\\/:*?"<>|]/g, '');
                        const lastSlash = item.originalPath.lastIndexOf('/');
                        const baseDir = lastSlash !== -1 ? item.originalPath.substring(0, lastSlash) : '.';
                        
                        item.targetPath = `${baseDir}/Sorted/${safeAuthor}/${item.suggestedYear ? item.suggestedYear + ' - ' : ''}${safeTitle}.${item.extension}`;
                        console.log(`[BatchManager] [${item.originalName}] Step 4: Target Path generated: ${item.targetPath}`);
                        item.status = 'review';
                    } catch (e) {
                        console.error(`[BatchManager] [${item.originalName}] Metadata Parse Error:`, e);
                        item.status = 'error';
                        item.errorMessage = 'Failed to parse LLM JSON';
                    }
                } else {
                    console.log(`[BatchManager] [${item.originalName}] AI Sort disabled or no provider, skipping to review`);
                    item.status = 'review';
                }
            } catch (error: any) {
                console.error(`[BatchManager] [${item.originalName}] CRITICAL ERROR:`, error);
                item.status = 'error';
                item.errorMessage = error.message || String(error);
            }
            await this.saveCurrentSession();
        }
        this.isProcessing = false;
        console.log("[BatchManager] Processing loop finished");
        await this.saveCurrentSession();
    }

    async executeBatch() {
        console.log("[BatchManager] Executing move batch...");
        const toMove = this.items.filter(i => i.isAccepted && i.targetPath && (i.status === 'review' || i.status === 'error'));
        if (toMove.length === 0) {
            console.warn("[BatchManager] No items accepted for moving");
            return;
        }

        toMove.forEach(i => i.status = 'moving');
        const moves = toMove.map(i => ({ source: i.originalPath, destination: i.targetPath! }));
        console.log(`[BatchManager] Requesting Rust to move ${moves.length} files`);

        try {
            const results: string[] = await invoke('move_files', { moves });
            console.log("[BatchManager] Rust move results:", results);
            
            results.forEach((res, index) => {
                const item = toMove[index];
                if (res.startsWith('Success:')) {
                    item.status = 'done';
                } else {
                    item.status = 'error';
                    item.errorMessage = res;
                }
            });
        } catch (error: any) {
            console.error("[BatchManager] Rust move execution failed:", error);
            toMove.forEach(i => {
                i.status = 'error';
                i.errorMessage = error.message;
            });
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
        console.log(`[BatchManager] Loading session: ${id}`);
        const sessions = await getFromStore('sessions', {}) as Record<string, BatchSession>;
        const session = sessions[id];
        if (session) {
            this.currentSessionId = session.id;
            this.items = session.items;
            console.log(`[BatchManager] Session loaded with ${this.items.length} items`);
        }
    }

    async resumeLastSession() {
        const sessions = await getFromStore('sessions', {}) as Record<string, BatchSession>;
        const lastSession = Object.values(sessions).sort((a, b) => b.startTime - a.startTime)[0];
        if (lastSession) {
            console.log(`[BatchManager] Resuming last session: ${lastSession.id}`);
            await this.loadSession(lastSession.id);
        } else {
            console.log("[BatchManager] No previous session found, creating new");
            await this.createNewSession();
        }
    }

    async exportBatch() {
        console.log("[BatchManager] Exporting batch...");
        const sessionData = JSON.stringify({
            items: $state.snapshot(this.items),
            isMetadataExtractionEnabled: this.isMetadataExtractionEnabled
        }, null, 2);

        const filePath = await save({
            filters: [{ name: 'CrispSorter Batch', extensions: ['json'] }],
            defaultPath: `batch_export_${new Date().toISOString().split('T')[0]}.json`
        });

        if (filePath) {
            const encoder = new TextEncoder();
            await writeFile(filePath, encoder.encode(sessionData));
            console.log(`[BatchManager] Exported to ${filePath}`);
            alert('Batch exported successfully!');
        }
    }

    async importBatch() {
        console.log("[BatchManager] Importing batch...");
        const filePath = await open({
            multiple: false,
            filters: [{ name: 'CrispSorter Batch', extensions: ['json'] }]
        });

        if (typeof filePath === 'string') {
            const fileData = await readFile(filePath);
            const decoder = new TextDecoder();
            const data = JSON.parse(decoder.decode(fileData));
            
            await this.createNewSession();
            this.items = data.items || [];
            this.isMetadataExtractionEnabled = data.isMetadataExtractionEnabled ?? true;
            await this.saveCurrentSession();
            console.log(`[BatchManager] Imported ${this.items.length} items from ${filePath}`);
            alert('Batch imported successfully!');
        }
    }

    clear() {
        console.log("[BatchManager] Clearing all items");
        this.items = [];
        this.saveCurrentSession();
    }
}

export const batchManager = new BatchManager();
