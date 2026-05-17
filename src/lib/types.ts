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
    /** ISO 639-1 source language detected during extraction —
     *  populated for audio/video items by the whisper LID head
     *  inside `audio_extract_text`.  `undefined` for documents
     *  (their language detection happens elsewhere) and for
     *  audio items where whisper couldn't classify confidently. */
    detectedLanguage?: string;

    /** P13.6 Step 3b — L2 audio metadata, pre-filled by the
     *  `audio_metadata` Tauri command before the full ASR run.
     *  Populated for items whose extension is in
     *  AUDIO_EXTENSIONS; undefined for documents.  Optional
     *  per-field because symphonia doesn't always expose every
     *  datapoint (VBR mp3 with no n_frames, container-truncated
     *  m4a, …) — the UI renders "—" / hides for missing fields. */
    audioDurationSeconds?: number;
    audioCodec?: string;
    audioSampleRateHz?: number;
    audioChannels?: number;
    audioBitrateKbps?: number;
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

    // P15a — content-dedup
    /** First 8 hex chars of the SHA-256 shared by all files in the same
     *  content-identical group. Set by detectAndMarkDuplicates(). */
    duplicateGroupId?: string;
    /** true = keep this one; false = skip on sort (non-primary duplicate). */
    isDuplicatePrimary?: boolean;

    // P15b — book-chapter grouping
    /** The ISBN-13 or shared filename prefix that identifies the book. */
    chapterGroupId?: string;
    /** The chapter suffix token: "fm", "001", "bm", etc. */
    chapterSuffix?: string;
    /** Only the representative file (fm > 001 > …) gets the full LLM pass. */
    isChapterRepresentative?: boolean;
    /** Total number of files in this chapter group. */
    chapterGroupSize?: number;
    /** When true, author propagation is suppressed for this group —
     *  chapters are from different authors (edited volume). */
    chapterIsEditedVolume?: boolean;

    // Processed-history dedup (SHA-256 matched against processed_history table)
    /** SHA-256 hex of the file content, computed on add. Used to detect
     *  files already sorted in a previous batch so they can skip extraction. */
    sha256?: string;
    /** Populated when the file was found in the processed_history table.
     *  Drives the "already sorted" badge and the skip-extraction fast-path. */
    historyMatch?: {
        suggestedTitle?: string;
        suggestedAuthor?: string;
        suggestedYear?: string;
        targetPath?: string;
        processedAt: number;
    };
    /** Set to true by the "re-process" action so the extraction worker
     *  ignores the historyMatch and runs a fresh extraction pass. */
    skipHistoryCheck?: boolean;
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
