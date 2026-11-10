import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { BatchItem } from './types';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('./store', () => ({ getSetting: vi.fn() }));
vi.mock('./log', () => ({ flog: vi.fn() }));
import { replaceBatch } from './batchStore';

const file = (id: string): BatchItem => ({ id, originalPath: `/docs/${id}.pdf`, originalName: `${id}.pdf`,
    extension: 'pdf', size: 42, modifiedAt: 1, status: 'review', isAccepted: false });

describe('transactional batch replacement IPC', () => {
    beforeEach(() => { invoke.mockReset(); invoke.mockResolvedValue(undefined); });

    it('sends one transaction even beyond normal bulk chunk size, with complete supplied text', async () => {
        const files = Array.from({ length: 501 }, (_, index) => file(String(index)));
        files[0].extractedText = 'Full document text. '.repeat(50);
        files[1].extractedText = '';
        await replaceBatch(files);
        expect(invoke).toHaveBeenCalledTimes(1);
        const [command, args] = invoke.mock.calls[0];
        expect(command).toBe('batch_session_replace');
        expect(args.items).toHaveLength(501);
        expect(args.items[0].extractedText).toBe(files[0].extractedText);
        expect(args.items[0].item.extractedText).toBeUndefined();
        expect(args.items[0].item.extractedTextPreview).toHaveLength(500);
        expect(args.items[1].extractedText).toBe('');
        expect(args.items[2].extractedText).toBeUndefined();
    });

    it('propagates failure without falling back to clear/upsert commands', async () => {
        invoke.mockRejectedValue(new Error('Database full'));
        await expect(replaceBatch([file('new')])).rejects.toThrow('Database full');
        expect(invoke.mock.calls.map(call => call[0])).toEqual(['batch_session_replace']);
    });

    it('passes an explicitly confirmed empty replacement to the same transaction', async () => {
        await replaceBatch([]);
        expect(invoke).toHaveBeenCalledWith('batch_session_replace', { items: [] });
    });
});
