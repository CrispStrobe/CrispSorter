import { type BatchItem, type BatchStatus, type BatchSession } from '../types';
import { extractText } from '../extractors'; 
import { llmClient } from '../llm/client';
import { getSetting, saveSetting, getSetting as getFromStore } from '../store';
import { readFile, writeFile, mkdir } from '@tauri-apps/plugin-fs';
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

        const providers = await getSetting('providers', []);
        const activeProvider = (providers as any[]).find(p => p.id === 'ollama') || providers[0];
        const modelId = activeProvider?.models?.[0];
        
        const globalExportPath = await getSetting('exportPath', '');
        const globalSaveTxt = await getSetting('saveTxt', true);

        for (const item of this.items) {
            if (item.status !== 'queued' && item.status !== 'error') continue;
            
            try {
                // 1. Extraction
                item.status = 'extracting';
                const fileData = await readFile(item.originalPath);
                const extraction = await extractText({ name: item.originalName, arrayBuffer: fileData.buffer });
                item.extractedText = extraction.text;

                // 2. LLM Analysis
                if (this.isMetadataExtractionEnabled && activeProvider && modelId) {
                    item.status = 'analyzing';
                    const prompt = `Extract metadata from this document text. Return JSON ONLY. { "title": "...", "author": "...", "year": "..." }. Use the first few pages of text: ${item.extractedText.substring(0, 4000)}`;
                    
                    const response = await llmClient.query(activeProvider.id, modelId, prompt, activeProvider.apiKey);
                    
                    try {
                        const metadata = JSON.parse(response.replace(/```json|```/g, '').trim());
                        item.suggestedTitle = metadata.title || 'Unknown Title';
                        item.suggestedAuthor = metadata.author || 'Unknown Author';
                        item.suggestedYear = metadata.year || '';
                        
                        const safeTitle = (item.suggestedTitle as string).replace(/[\\/:*?"<>|]/g, '');
                        const safeAuthor = (item.suggestedAuthor as string).replace(/[\\/:*?"<>|]/g, '');
                        
                        const baseDir = globalExportPath || item.originalPath.substring(0, item.originalPath.lastIndexOf('/'));
                        item.targetPath = `${baseDir}/Sorted/${safeAuthor}/${item.suggestedYear ? item.suggestedYear + ' - ' : ''}${safeTitle}.${item.extension}`;
                        
                        item.status = 'review';
                    } catch (e) {
                        item.status = 'error';
                        item.errorMessage = 'Failed to parse LLM JSON';
                    }
                } else {
                    // No LLM mode - just set default target path if export path exists
                    const baseDir = globalExportPath || item.originalPath.substring(0, item.originalPath.lastIndexOf('/'));
                    item.targetPath = `${baseDir}/Extracted/${item.originalName}.txt`;
                    item.status = 'review';
                }

                // Auto-save TXT if enabled and we are not in AI mode (or even in AI mode if desired)
                if (globalSaveTxt && item.extractedText) {
                    // We'll do this during execution to be cleaner, but could do here too.
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

    async executeBatch() {
        const toMove = this.items.filter(i => i.isAccepted && i.targetPath && (i.status === 'review' || i.status === 'error'));
        if (toMove.length === 0) return;

        const globalSaveTxt = await getSetting('saveTxt', true);
        toMove.forEach(i => i.status = 'moving');

        for (const item of toMove) {
            try {
                // 1. Save TXT if enabled
                if (globalSaveTxt && item.extractedText) {
                    const txtPath = item.targetPath!.replace(/\.[^.]+$/, '.txt');
                    const parentDir = txtPath.substring(0, txtPath.lastIndexOf('/'));
                    try { await mkdir(parentDir, { recursive: true }); } catch(e) {}
                    const encoder = new TextEncoder();
                    await writeFile(txtPath, encoder.encode(item.extractedText));
                }

                // 2. Perform Move/Rename via Rust
                const results: string[] = await invoke('move_files', { 
                    moves: [{ source: item.originalPath, destination: item.targetPath! }] 
                });

                if (results[0].startsWith('Success:')) {
                    item.status = 'done';
                } else {
                    item.status = 'error';
                    item.errorMessage = results[0];
                }
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
        }
    }

    async resumeLastSession() {
        const sessions = await getFromStore('sessions', {}) as Record<string, BatchSession>;
        const lastSession = Object.values(sessions).sort((a, b) => b.startTime - a.startTime)[0];
        if (lastSession) await this.loadSession(lastSession.id);
        else await this.createNewSession();
    }

    async exportBatch() {
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
            alert('Batch exported successfully!');
        }
    }

    async importBatch() {
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
            alert('Batch imported successfully!');
        }
    }

    clear() {
        this.items = [];
        this.saveCurrentSession();
    }
}

export const batchManager = new BatchManager();
