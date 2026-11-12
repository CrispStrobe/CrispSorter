import { invoke } from '@tauri-apps/api/core';
import type { DriveCapabilities } from './browser';

export type TrashReceipt = {
    id: string;
    drive_id: string;
    group_id: string;
    path: string;
    state: 'deleted' | 'restored' | 'not_deleted' | 'delete_uncertain' | 'restore_uncertain';
};

export async function loadTrashContext(groupId: string, driveIds: string[]) {
    const receipts = await invoke<TrashReceipt[]>('drive_trash_receipts', { groupId });
    const ids = [...new Set([...driveIds, ...receipts.map((receipt) => receipt.drive_id)])];
    const capabilities = Object.fromEntries(await Promise.all(ids.map(async (driveId) => {
        try {
            const caps = await invoke<DriveCapabilities>('drive_capabilities', { driveId });
            return [driveId, caps.trash_receipts === true] as const;
        } catch { return [driveId, false] as const; }
    })));
    return { receipts, capabilities };
}

export function trashWithReceipt(driveId: string, groupId: string, path: string): Promise<TrashReceipt> {
    return invoke('drive_trash_with_receipt', { driveId, groupId, path });
}

function requireReceipt(receipt: TrashReceipt) {
    if (!/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(receipt.id) || !receipt.drive_id) {
        throw new Error('A durable object receipt is required; path-only history cannot restore an item.');
    }
}

export async function restoreTrashReceipt(receipt: TrashReceipt): Promise<TrashReceipt> {
    requireReceipt(receipt);
    if (receipt.state !== 'deleted') throw new Error('Recheck uncertain outcomes before restoring; mutations are never replayed automatically.');
    return invoke('drive_restore_trash_receipt', { driveId: receipt.drive_id, receiptId: receipt.id });
}

export async function recheckTrashReceipt(receipt: TrashReceipt): Promise<TrashReceipt> {
    requireReceipt(receipt);
    return invoke('drive_recheck_trash_receipt', { driveId: receipt.drive_id, receiptId: receipt.id });
}
