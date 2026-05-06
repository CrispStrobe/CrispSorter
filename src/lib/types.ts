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
    /** Per-stage outcome for the bottom-of-row 3-pip indicator
     *  (M | T | L = Metadata / Text / LLM).
     *
     *  metadataReadStatus tracks the explicit "read embedded
     *  document metadata" step (PDF /Info dict, DOCX core.xml,
     *  EPUB OPF, image EXIF). Undefined = not yet attempted;
     *  'ok' / 'failed' = result; 'na' = the file format has no
     *  metadata convention to read (.txt, .md, .html). */
    metadataReadStatus?: 'ok' | 'failed' | 'na';

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
