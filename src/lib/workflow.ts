import { writable } from 'svelte/store';
import type { BatchItem } from './types';
export type WorkflowStage = 'inbox' | 'review';
export type ProcessingPreset = 'fast' | 'balanced' | 'quality' | 'advanced';
export function inWorkflow(item: Pick<BatchItem, 'status'>, stage: WorkflowStage): boolean {
    return stage === 'review' ? ['review', 'ready', 'moving'].includes(item.status)
        : ['queued', 'unfinished', 'extracting', 'analyzing', 'error'].includes(item.status);
}
export function processingOverrides(preset: ProcessingPreset) {
    if (preset === 'advanced') return undefined;
    return { maxChars: { fast: 3500, balanced: 8000, quality: 14000 }[preset], extractionWorkers: 1, llmWorkers: 1 };
}
export type DocumentTool = 'ocr' | 'translate' | 'pdf';
export const documentToolRequest = writable<{ tool: DocumentTool; locationUri: string; filename: string } | null>(null);
export function availableDocumentTools(filename: string, capabilities = { ocr: true, pdf_render: true }): DocumentTool[] {
    const ext = filename.split('.').pop()?.toLowerCase();
    if (ext === 'docx') return ['translate'];
    if (ext === 'pdf') return capabilities.ocr && capabilities.pdf_render ? ['ocr', 'pdf'] : ['pdf'];
    if (ext && ['png', 'jpg', 'jpeg', 'tif', 'tiff', 'bmp', 'webp'].includes(ext)) return capabilities.ocr ? ['ocr'] : [];
    return [];
}
