// TypeScript wrapper around the SQLite batch session Tauri commands.
// Replaces saveSetting('lastSession', …) / getSetting('lastSession') calls.
// See handover-prompts/session-prompt-batch-sqlite-persistence.md for the
// full design context.

import { invoke } from '@tauri-apps/api/core';
import { getSetting } from './store';
import type { BatchItem } from './types';

// ── Internal row type ─────────────────────────────────────────────────────────

// Extends BatchItem with the extra column that lives in the SQLite row but
// not on the in-memory BatchItem.  Used only inside this module.
interface BatchItemRow extends BatchItem {
    extractedTextPreview?: string;
}

// ── Conversion helpers ────────────────────────────────────────────────────────

function batchItemToRow(item: BatchItem): BatchItemRow {
    // Explicitly exclude extractedText from the IPC payload — it can be
    // megabytes per item and Rust ignores it (stored separately via
    // setExtractedText / extracted_texts table).  Spreading it caused
    // upsertItemsBulk(135 items) to serialize 100MB+ over the IPC bridge,
    // blocking the JS event loop and freezing the UI.
    const { extractedText: _, ...rest } = item;
    return {
        ...rest,
        extractedTextPreview: item.extractedText != null
            ? item.extractedText.slice(0, 500)
            : undefined,
    };
}

function rowToBatchItem(row: BatchItemRow): BatchItem {
    // Drop the preview column — extractedText is intentionally not restored here.
    // The extraction worker lazy-loads the full text from extracted_texts via
    // getExtractedText() before deciding to re-extract, so resumed items get
    // their full body back without re-running extraction.
    const { extractedTextPreview: _, ...rest } = row;
    return rest as BatchItem;
}

// ── Public API ────────────────────────────────────────────────────────────────

/** Load all items for the current session, ordered by insertion order. */
export async function loadBatch(): Promise<BatchItem[]> {
    const rows = await invoke<BatchItemRow[]>('batch_session_load');
    return rows.map(rowToBatchItem);
}

/** Upsert a single item (insert or update all mutable columns). */
export async function upsertItem(item: BatchItem): Promise<void> {
    return invoke('batch_session_upsert_item', { item: batchItemToRow(item) });
}

/** Upsert many items in a single SQLite transaction.
 *  Use for bulk status updates (resetToQueued, setAcceptedItems, etc.) to
 *  avoid N separate IPC round-trips. */
export async function upsertItemsBulk(items: BatchItem[]): Promise<void> {
    return invoke('batch_session_upsert_items_bulk', { items: items.map(batchItemToRow) });
}

/** Delete items by their ids. */
export async function deleteItems(ids: string[]): Promise<void> {
    return invoke('batch_session_delete_items', { ids });
}

/** Delete all items for the current session. */
export async function clearBatch(): Promise<void> {
    return invoke('batch_session_clear');
}

/** Persist the full extracted text for an item.
 *  Kept out of the item row so status updates don't carry MB of text over IPC. */
export async function setExtractedText(itemId: string, text: string): Promise<void> {
    return invoke('batch_session_set_extracted_text', { itemId, text });
}

/** Retrieve the full extracted text for an item, or `null` if not stored. */
export async function getExtractedText(itemId: string): Promise<string | null> {
    return invoke('batch_session_get_extracted_text', { itemId });
}

// ── Processed-history dedup ───────────────────────────────────────────────────

export interface ProcessedHistoryRow {
    sha256: string;
    filename: string;
    sizeBytes: number;
    suggestedTitle?: string;
    suggestedAuthor?: string;
    suggestedYear?: string;
    targetPath?: string;
    processedAt: number;
}

/** Record a file that has been fully sorted (status = 'done') so future
 *  batches can skip extraction for the same content.  Fire-and-forget safe. */
export async function recordProcessed(row: ProcessedHistoryRow): Promise<void> {
    return invoke('batch_session_record_processed', { row });
}

/** Look up a previous run by SHA-256.  Returns null when unseen. */
export async function lookupHistory(sha256: string): Promise<ProcessedHistoryRow | null> {
    return invoke('batch_session_lookup_history', { sha256 });
}

/** Total number of distinct hashes stored in the processed history. */
export async function historyCount(): Promise<number> {
    return invoke('batch_session_history_count');
}

// ── One-shot JSON → SQLite migration ─────────────────────────────────────────

/** Migrate the legacy `lastSession` JSON blob from tauri-plugin-store into
 *  the SQLite batch session store.
 *
 *  Idempotent: a sentinel row (`id = 'json_migration_done'`) in the
 *  `batch_sessions` table prevents double-migration on subsequent launches.
 *
 *  The JSON entry is intentionally NOT deleted — it stays as a backup for
 *  one release cycle (remove in the release after Slice 5 lands).
 *
 *  Returns the number of items migrated, or 0 when already done / nothing
 *  to migrate. */
export async function migrateFromJson(): Promise<number> {
    const alreadyDone = await invoke<boolean>('batch_session_is_migrated');
    if (alreadyDone) return 0;

    const last = await getSetting('lastSession');
    const items: BatchItem[] | null =
        last != null && typeof last === 'object' && Array.isArray((last as any).items)
            ? (last as any).items
            : null;

    if (items && items.length > 0) {
        await invoke('batch_session_upsert_items_bulk', {
            items: items.map(batchItemToRow),
        });
    }

    await invoke('batch_session_mark_migrated');
    return items?.length ?? 0;
}
