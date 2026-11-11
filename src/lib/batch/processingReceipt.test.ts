import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flog } from '../log';
import { beginOperation } from '../operationLog';
import { finishProcessingCheckpoint } from './processingReceipt';

vi.mock('../log', () => ({ flog: vi.fn() }));
beforeEach(() => { vi.mocked(flog).mockReset(); });
const messages = () => vi.mocked(flog).mock.calls.map(([, message]) => message);

describe('batch final checkpoint receipts', () => {
    it('keeps ownership until the final write settles before reporting cancellation', async () => {
        let resolve!: () => void;
        const persist = vi.fn(() => new Promise<void>(done => { resolve = done; }));
        const release = vi.fn();
        const pending = finishProcessingCheckpoint({ persist, release,
            operation: beginOperation('batch.processing'), cancelled: true, counts: { completed: 3 } });
        expect(release).not.toHaveBeenCalled();
        expect(messages().some(message => /phase=(completed|cancelled)/.test(message))).toBe(false);
        resolve(); await pending;
        expect(release).toHaveBeenCalledOnce();
        expect(messages().at(-1)).toContain('phase=cancelled');
        expect(messages().at(-1)).toContain('saved=true');
    });

    it('propagates checkpoint failure and unlocks controls without a successful terminal log', async () => {
        const failure = new Error('Private database detail'); const release = vi.fn();
        await expect(finishProcessingCheckpoint({ persist: async () => { throw failure; }, release,
            operation: beginOperation('batch.processing'), cancelled: false, counts: {} })).rejects.toBe(failure);
        expect(release).toHaveBeenCalledOnce();
        expect(messages().at(-1)).toContain('phase=failed');
        expect(messages().at(-1)).toContain('saved=false');
        expect(messages().join('\n')).not.toContain('Private database detail');
        expect(messages().some(message => message.includes('phase=completed'))).toBe(false);
    });

    it('preserves the original processing error if persistence also fails, while logging the unsaved result', async () => {
        const original = new DOMException('Stopped processing', 'AbortError');
        const release = vi.fn();
        const run = async () => {
            try { throw original; }
            finally { await finishProcessingCheckpoint({
                persist: async () => { throw new Error('Checkpoint failed'); }, release,
                operation: beginOperation('batch.processing'), cancelled: true,
                failure: { error: original }, counts: { completed: 2 },
            }); }
        };
        await expect(run()).rejects.toBe(original);
        expect(release).toHaveBeenCalledOnce();
        expect(vi.mocked(flog).mock.calls.at(-1)?.[0]).toBe('error');
        expect(messages().at(-1)).toContain('saved=false');
    });
});
