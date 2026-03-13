import { type BatchItem, type BatchStatus, type BatchSession } from '../types';
// import { extractText } from '../extractors'; // MOCKED for debug
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
    
    constructor() {}

    async createNewSession() {
        this.currentSessionId = crypto.randomUUID();
        this.items = [];
        await this.saveCurrentSession();
    }

    addItem(path: string, name: string) {
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
        if (this.isProcessing) return;
        this.isProcessing = true;

        const providers = await getSetting('providers', []);
        const activeProvider = (providers as any[]).find(p => p.id === 'ollama') || providers[0];
        const modelId = activeProvider?.models?.[0];

        for (const item of this.items) {
            if (item.status !== 'queued' && item.status !== 'error') continue;
            
            try {
                item.status = 'extracting';
                // Mocking extraction for debug
                item.extractedText = "Sample extracted text for " + item.originalName;

                if (this.isMetadataExtractionEnabled && activeProvider && modelId) {
                    item.status = 'analyzing';
                    const prompt = `Extract metadata...`;
                    const response = await llmClient.query(activeProvider.id, modelId, prompt, activeProvider.apiKey);
                    
                    try {
                        const metadata = JSON.parse(response.replace(/```json|```/g, '').trim());
                        item.suggestedTitle = metadata.title || 'Unknown Title';
                        item.suggestedAuthor = metadata.author || 'Unknown Author';
                        item.suggestedYear = metadata.year || '';
                        
                        const safeTitle = (item.suggestedTitle as string).replace(/[\\/:*?"<>|]/g, '');
                        const safeAuthor = (item.suggestedAuthor as string).replace(/[\\/:*?"<>|]/g, '');
                        const baseDir = item.originalPath.substring(0, item.originalPath.lastIndexOf('/'));
                        
                        item.targetPath = `${baseDir}/Sorted/${safeAuthor}/${item.suggestedYear ? item.suggestedYear + ' - ' : ''}${safeTitle}.${item.extension}`;
                        item.status = 'review';
                    } catch (e) {
                        item.status = 'error';
                        item.errorMessage = 'Failed to parse LLM JSON';
                    }
                } else {
                    item.status = 'review';
                }
            } catch (error: any) {
                item.status = 'error';
                item.errorMessage = error.message;
            }
            await this.saveCurrentSession();
        }
        this.isProcessing = false;
        await this.saveCurrentSession();
    }

    async executeBatch() {
        const toMove = this.items.filter(i => i.isAccepted && i.targetPath && (i.status === 'review' || i.status === 'error'));
        if (toMove.length === 0) return;

        toMove.forEach(i => i.status = 'moving');
        const moves = toMove.map(i => ({ source: i.originalPath, destination: i.targetPath! }));

        try {
            const results: string[] = await invoke('move_files', { moves });
            results.forEach((res, index) => {
                const item = toMove[index];
                if (res.startsWith('Success:')) item.status = 'done';
                else { item.status = 'error'; item.errorMessage = res; }
            });
        } catch (error: any) {
            toMove.forEach(i => { i.status = 'error'; i.errorMessage = error.message; });
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
