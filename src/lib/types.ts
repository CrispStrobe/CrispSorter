export type BatchStatus = 'queued' | 'unfinished' | 'extracting' | 'analyzing' | 'review' | 'ready' | 'moving' | 'done' | 'error';

export interface BatchItem {
    id: string;
    originalPath: string;
    originalName: string;
    extension: string;
    size: number;
    modifiedAt: number;
    status: BatchStatus;
    errorMessage?: string;
    statusDetail?: string;
    extractedText?: string;
    
    // LLM Suggestions
    suggestedTitle?: string;
    suggestedAuthor?: string;
    suggestedYear?: string;
    targetPath?: string;
    
    // User edits/acceptance
    isAccepted: boolean;
    isIgnored?: boolean;
}

export interface Metadata {
    title?: string;
    author?: string;
    year?: string;
}

export interface BatchSession {
    id: string;
    startTime: number;
    items: BatchItem[];
    status: 'active' | 'paused' | 'completed';
    providerId: string;
    modelId: string;
}
