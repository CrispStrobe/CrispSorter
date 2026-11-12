import { beforeEach, describe, expect, it, vi } from 'vitest';
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
import { loadTrashContext, trashWithReceipt, restoreTrashReceipt, recheckTrashReceipt, type TrashReceipt } from './trashReceipts';
const receipt: TrashReceipt = { id: '10000000-0000-4000-8000-000000000001', drive_id: 'drive-a', group_id: 'group-a', path: '/Docs/report.pdf', state: 'deleted' };

describe('durable duplicate trash IPC', () => {
    beforeEach(() => invoke.mockReset());
    it('loads backend receipts after restart and requires an explicit receipt capability', async () => {
        invoke.mockResolvedValueOnce([receipt]).mockResolvedValueOnce({ trash_receipts: true }).mockResolvedValueOnce({ reversible_trash: true });
        expect(await loadTrashContext('group-a', ['drive-a', 'drive-b', 'drive-a']))
            .toEqual({ receipts: [receipt], capabilities: { 'drive-a': true, 'drive-b': false } });
        expect(invoke).toHaveBeenNthCalledWith(1, 'drive_trash_receipts', { groupId: 'group-a' });
    });
    it('uses the receipt-producing delete boundary rather than ordinary delete', async () => {
        invoke.mockResolvedValueOnce(receipt);
        expect(await trashWithReceipt('drive-a', 'group-a', receipt.path)).toEqual(receipt);
        expect(invoke).toHaveBeenCalledExactlyOnceWith('drive_trash_with_receipt', { driveId: 'drive-a', groupId: 'group-a', path: receipt.path });
    });
    it('restores only by opaque receipt ID without accepting account or replacement paths', async () => {
        invoke.mockResolvedValueOnce({ ...receipt, state: 'restored' });
        await restoreTrashReceipt(receipt);
        expect(invoke).toHaveBeenCalledExactlyOnceWith('drive_restore_trash_receipt', { driveId: 'drive-a', receiptId: receipt.id });
    });
    it('blocks legacy path history and uncertain/replayed restores before IPC', async () => {
        await expect(restoreTrashReceipt({ ...receipt, id: receipt.path })).rejects.toThrow('durable object receipt');
        for (const state of ['delete_uncertain', 'restore_uncertain', 'restored', 'not_deleted'] as const) {
            await expect(restoreTrashReceipt({ ...receipt, state })).rejects.toThrow('Recheck');
        }
        expect(invoke).not.toHaveBeenCalled();
    });
    it('recovers uncertain outcomes through a distinct read-only reconciliation command', async () => {
        invoke.mockResolvedValueOnce(receipt);
        await recheckTrashReceipt({ ...receipt, state: 'delete_uncertain' });
        expect(invoke).toHaveBeenCalledExactlyOnceWith('drive_recheck_trash_receipt', { driveId: 'drive-a', receiptId: receipt.id });
    });
});
