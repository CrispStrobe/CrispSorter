import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flog } from '../../log';
import { cancelledDocumentAction, createDocumentActions, requireCurrentDocument } from './actionRunner';

vi.mock('../../log', () => ({ flog: vi.fn() }));
beforeEach(() => { vi.mocked(flog).mockReset(); });

function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

const messages = () => vi.mocked(flog).mock.calls.map(([, message]) => message);
const terminals = () => messages().filter((message) => /phase=(completed|failed|cancelled)/.test(message));

describe('document action ownership and receipts', () => {
    it('blocks a second mutation until the first native action settles', async () => {
        const pending: number[] = [];
        const actions = createDocumentActions((count) => pending.push(count));
        const native = deferred<{ revision: number }>();
        const first = actions.run({ operation: 'pdf.edit' }, () => native.promise);
        const duplicate = vi.fn(async () => ({ revision: 2 }));
        expect(await actions.run({ operation: 'pdf.edit' }, duplicate)).toBeUndefined();
        expect(duplicate).not.toHaveBeenCalled();
        native.resolve({ revision: 1 });
        expect(await first).toEqual({ revision: 1 });
        expect(pending).toEqual([1, 0]);
        expect(terminals()).toHaveLength(1);
        expect(terminals()[0]).toContain('phase=completed');
    });

    it('suppresses old capability/read results while accepting the replacement file', async () => {
        const actions = createDocumentActions(() => {});
        const old = deferred<{ ext: string }>();
        let owner = 'first';
        const first = actions.run({ operation: 'document.capabilities', concurrent: true, isCurrent: () => owner === 'first' }, () => old.promise);
        owner = 'second';
        const next = await actions.run({ operation: 'document.capabilities', concurrent: true, isCurrent: () => owner === 'second' }, async () => ({ ext: 'pptx' }));
        old.resolve({ ext: 'doc' });
        expect(await first).toBeUndefined();
        expect(next).toEqual({ ext: 'pptx' });
        expect(terminals().map((message) => message.match(/phase=(\w+)/)?.[1])).toEqual(['completed', 'cancelled']);
    });

    it('keeps a completed output write successful after its panel is replaced', async () => {
        const actions = createDocumentActions(() => {});
        const write = deferred<string>();
        let current = true;
        const pending = actions.run({ operation: 'document.convert', writesOutput: true, isCurrent: () => current }, () => write.promise);
        current = false;
        write.resolve('/private/output.docx');
        expect(await pending).toBeUndefined();
        expect(terminals()).toHaveLength(1);
        expect(terminals()[0]).toContain('phase=completed');
        expect(terminals()[0]).toContain('saved=true');
        expect(messages().join('\n')).not.toContain('/private');
    });

    it('keeps real failures visible and reports their original error only to the owning UI', async () => {
        const actions = createDocumentActions(() => {});
        const fail = new Error('private document https://host.invalid/?password=secret');
        const onError = vi.fn();
        expect(await actions.run({ operation: 'pdf.save', onError }, async () => { throw fail; })).toBeUndefined();
        expect(onError).toHaveBeenCalledWith(fail);
        expect(vi.mocked(flog).mock.calls.at(-1)?.[0]).toBe('error');
        expect(terminals()[0]).toContain('phase=failed');
        expect(messages().join('\n')).not.toMatch(/private|host\.invalid|password|secret/);
    });

    it('does not put an obsolete failure into the replacement document banner', async () => {
        const actions = createDocumentActions(() => {});
        const read = deferred<string>();
        const onError = vi.fn();
        let current = true;
        const pending = actions.run({ operation: 'docx.check', isCurrent: () => current, onError }, () => read.promise);
        current = false;
        read.reject(new Error('Native checker failed'));
        expect(await pending).toBeUndefined();
        expect(onError).not.toHaveBeenCalled();
        expect(terminals()[0]).toContain('phase=failed');
    });

    it('cancels before dispatch when the input was replaced during a dialog', async () => {
        const actions = createDocumentActions(() => {});
        const invoke = vi.fn();
        let current = true;
        await actions.run({ operation: 'pdf.export', writesOutput: true, isCurrent: () => current }, async () => {
            await Promise.resolve();
            current = false;
            requireCurrentDocument(() => current);
            invoke();
        });
        expect(invoke).not.toHaveBeenCalled();
        expect(terminals()[0]).toContain('phase=cancelled');
        expect(terminals()[0]).not.toContain('saved=true');
    });

    it('waits for all in-flight owners before starting a document replacement', async () => {
        const actions = createDocumentActions(() => {});
        const first = deferred<void>(), second = deferred<void>();
        const a = actions.run({ operation: 'pdf.open', concurrent: true }, () => first.promise);
        const b = actions.run({ operation: 'document.capabilities', concurrent: true }, () => second.promise);
        const idle = vi.fn();
        const waiting = actions.whenIdle().then(idle);
        first.resolve();
        await a;
        expect(idle).not.toHaveBeenCalled();
        second.resolve();
        await Promise.all([b, waiting]);
        expect(idle).toHaveBeenCalledOnce();
        expect(actions.pending).toBe(0);
    });

    it('classifies explicit cancellation without calling the UI failure handler', async () => {
        const actions = createDocumentActions(() => {});
        const onError = vi.fn();
        await actions.run({ operation: 'pdf.open', onError }, async () => { throw cancelledDocumentAction(); });
        expect(onError).not.toHaveBeenCalled();
        expect(terminals()[0]).toContain('phase=cancelled');
    });

    it('adds receipt counts to the completion and survives a throwing receipt', async () => {
        const actions = createDocumentActions(() => {});
        const result = await actions.run({ operation: 'docx.footnotes', writesOutput: true,
            receipt: (n: number) => ({ count: n, total: 3 }) }, async () => 2);
        expect(result).toBe(2);
        expect(terminals()[0]).toContain('phase=completed');
        expect(terminals()[0]).toContain('count=2 total=3 saved=true');
        const kept = await actions.run({ operation: 'docx.check',
            receipt: () => { throw new Error('private detail'); } }, async () => 'ok');
        expect(kept).toBe('ok');
        expect(terminals()[1]).toContain('phase=completed');
        expect(messages().join('\n')).not.toContain('private detail');
    });
});
