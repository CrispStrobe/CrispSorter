export type BatchStatus = 'queued' | 'extracting' | 'analyzing' | 'review' | 'ready' | 'moving' | 'done' | 'error';

export interface BatchItem {
    id: string;
    originalPath: string;
    originalName: string;
    extension: string;
    status: BatchStatus;
    errorMessage?: string;
    extractedText?: string;
    
    // LLM Suggestions
    suggestedTitle?: string;
    suggestedAuthor?: string;
    suggestedYear?: string;
    targetPath?: string;
    
    // User edits/acceptance
    isAccepted: boolean;
    isIgnored: boolean;
}

export interface BatchSession {
    id: string;
    startTime: number;
    items: BatchItem[];
    status: 'active' | 'paused' | 'completed';
    providerId: string;
    modelId: string;
}
