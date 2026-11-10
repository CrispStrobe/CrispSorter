import type { BatchItem, BatchSession, BatchStatus } from '../types';
import { batchSourceKey } from '../mobileProvider';

const statuses = new Set<BatchStatus>(['queued', 'unfinished', 'extracting', 'analyzing', 'review', 'ready', 'moving', 'done', 'error']);
const interrupted = new Set<BatchStatus>(['extracting', 'analyzing', 'moving']);
const strings = ['errorMessage', 'statusDetail', 'extractedText', 'detectedLanguage', 'audioCodec',
    'suggestedTitle', 'suggestedAuthor', 'suggestedYear', 'targetPath', 'duplicateGroupId',
    'chapterGroupId', 'chapterSuffix', 'sha256'] as const;
const booleans = ['isIgnored', 'isDuplicatePrimary', 'isChapterRepresentative', 'chapterIsEditedVolume', 'skipHistoryCheck'] as const;
const numbers = ['audioDurationSeconds', 'audioSampleRateHz', 'audioChannels', 'audioBitrateKbps', 'chapterGroupSize'] as const;
const integers = new Set<string>(['audioSampleRateHz', 'audioChannels', 'chapterGroupSize']);

function record(value: unknown): value is Record<string, unknown> {
    return !!value && typeof value === 'object' && !Array.isArray(value);
}

function requiredString(value: unknown): value is string {
    return typeof value === 'string' && value.trim().length > 0 && !value.includes('\0');
}

function natural(value: unknown): value is number {
    return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

/** Validate the complete snapshot before any destructive operation. Unknown fields are omitted. */
export function validateBatchItems(value: unknown): BatchItem[] {
    if (!Array.isArray(value)) throw new Error('Expected a JSON array of batch files.');
    const ids = new Set<string>();
    const sources = new Set<string>();
    return value.map((entry, index) => {
        const fail = (field: string): never => { throw new Error(`File ${index + 1}: invalid ${field}.`); };
        if (!record(entry)) return fail('file record');
        for (const field of ['id', 'originalPath', 'originalName'] as const) {
            if (!requiredString(entry[field])) fail(field);
        }
        if (['__proto__', 'constructor', 'prototype'].includes(entry.id as string)) fail('id');
        if (typeof entry.extension !== 'string') fail('extension');
        if (!natural(entry.size)) fail('size');
        if (!natural(entry.modifiedAt)) fail('modifiedAt');
        if (!statuses.has(entry.status as BatchStatus)) fail('status');
        if (typeof entry.isAccepted !== 'boolean') fail('isAccepted');
        const id = entry.id as string;
        const source = batchSourceKey(entry.originalPath as string);
        if (ids.has(id)) fail('duplicate id');
        if (sources.has(source)) fail('duplicate source path');
        ids.add(id);
        sources.add(source);
        const item: BatchItem = {
            id, originalPath: entry.originalPath as string, originalName: entry.originalName as string,
            extension: entry.extension as string, size: entry.size as number, modifiedAt: entry.modifiedAt as number,
            status: entry.status as BatchStatus, isAccepted: entry.isAccepted as boolean,
        };
        for (const field of strings) {
            if (entry[field] == null) continue;
            if (typeof entry[field] !== 'string') fail(field);
            item[field] = entry[field] as string;
        }
        for (const field of booleans) {
            if (entry[field] == null) continue;
            if (typeof entry[field] !== 'boolean') fail(field);
            item[field] = entry[field] as boolean;
        }
        for (const field of numbers) {
            if (entry[field] == null) continue;
            const number = entry[field];
            if (typeof number !== 'number' || !Number.isFinite(number) || number < 0 ||
                (integers.has(field) && !Number.isSafeInteger(number))) fail(field);
            item[field] = number as number;
        }
        if (entry.metadataReadStatus != null) {
            if (!['ok', 'failed', 'na'].includes(entry.metadataReadStatus as string)) fail('metadataReadStatus');
            item.metadataReadStatus = entry.metadataReadStatus as BatchItem['metadataReadStatus'];
        }
        if (entry.historyMatch != null) {
            if (!record(entry.historyMatch) || !natural(entry.historyMatch.processedAt)) fail('historyMatch');
            const history = entry.historyMatch as Record<string, unknown>;
            item.historyMatch = { processedAt: history.processedAt as number };
            for (const field of ['suggestedTitle', 'suggestedAuthor', 'suggestedYear', 'targetPath'] as const) {
                if (history[field] == null) continue;
                if (typeof history[field] !== 'string') fail(`historyMatch.${field}`);
                item.historyMatch[field] = history[field] as string;
            }
        }
        // A saved snapshot cannot have a running worker. Make interrupted work explicit.
        if (interrupted.has(item.status)) {
            item.status = 'unfinished';
            item.isAccepted = false;
            delete item.statusDetail;
        }
        return item;
    });
}

/** Legacy History stores session objects in one settings record. */
export function validateSessions(value: unknown): BatchSession[] {
    if (!record(value)) throw new Error('Saved history is not a session record.');
    return Object.entries(value).map(([key, entry]) => {
        if (!record(entry) || !requiredString(entry.id) || key !== entry.id ||
            !natural(entry.startTime) || !Number.isFinite(new Date(entry.startTime).getTime()) ||
            !['active', 'paused', 'completed'].includes(entry.status as string)) {
            throw new Error(`Saved session ${key}: invalid session details.`);
        }
        return {
            id: entry.id, startTime: entry.startTime, status: entry.status as BatchSession['status'],
            providerId: typeof entry.providerId === 'string' ? entry.providerId : '',
            modelId: typeof entry.modelId === 'string' ? entry.modelId : '',
            items: validateBatchItems(entry.items),
        };
    }).sort((a, b) => b.startTime - a.startTime);
}

export interface BatchReplacementSummary {
    incomingCount: number;
    currentCount: number;
    currentDraftCount?: number;
    source: string;
}

export type ConfirmBatchReplacement = (summary: BatchReplacementSummary) => Promise<boolean>;
