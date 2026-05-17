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
    return {
        ...item,
        // Store a bounded preview in the items table row.
        // Full text is persisted separately via setExtractedText.
        extractedTextPreview: item.extractedText != null
            ? item.extractedText.slice(0, 500)
            : undefined,
    };
}

function rowToBatchItem(row: BatchItemRow): BatchItem {
    const { extractedTextPreview, ...rest } = row;
    // On load, hydrate extractedText from the preview so the UI can show
    // the "⚠ poor extraction" marker and the LLM consumer gets at least
    // the first 500 chars.  The full body is lazy-loaded via getExtractedText
    // when the LLM worker needs it (Slice 3 wires this).
    return {
        ...rest,
        extractedText: extractedTextPreview ?? undefined,
    };
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
