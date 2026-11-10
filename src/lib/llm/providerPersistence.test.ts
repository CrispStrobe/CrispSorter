import { describe, expect, it, vi } from 'vitest';
import { prepareProviderSecrets, ProviderSaveQueue, reconcileProviderSecrets } from './providerPersistence';

function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason: unknown) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

const provider = (id = 'alpha', apiKey = 'fixture-secret') => ({
    id, apiKey, baseUrl: `https://${id}.example/v1`, selectedModel: 'saved-model', models: ['saved-model'],
});

describe('provider credential persistence', () => {
    it('prepares only key references for settings after every requested key is stored', async () => {
        const submitted = [provider(), provider('beta', '@keyring/llm-provider:beta')];
        const store = vi.fn(async () => ['llm-provider:alpha']);
        const result = await prepareProviderSecrets(submitted, store);
        expect(store).toHaveBeenCalledWith([['llm-provider:alpha', 'fixture-secret']]);
        expect(result.providers.map(item => item.apiKey)).toEqual(['@keyring/llm-provider:alpha', '@keyring/llm-provider:beta']);
        expect(result.migrated).toBe(1);
        expect(submitted[0].apiKey).toBe('fixture-secret');
        expect(JSON.stringify(result.providers)).not.toContain('fixture-secret');
    });

    it('refuses partial secret-store success without returning a plaintext fallback', async () => {
        const submitted = [provider(), provider('beta', 'second-fixture-secret')];
        let settings: unknown = null;
        const attempt = async () => {
            const result = await prepareProviderSecrets(submitted, async () => ['llm-provider:alpha']);
            settings = result.providers;
        };
        await expect(attempt()).rejects.toThrow('Secret storage did not save llm-provider:beta');
        expect(settings).toBeNull();
        expect(submitted[1].apiKey).toBe('second-fixture-secret');
        const error = await prepareProviderSecrets(submitted, async () => []).catch(error => String(error));
        expect(error).not.toContain('fixture-secret');
    });

    it('does not migrate empty, local placeholder, or already stored values', async () => {
        const keys = ['', 'ollama', 'no-key', 'local', '@keyring/llm-provider:alpha'];
        const store = vi.fn(async () => []);
        const result = await prepareProviderSecrets(keys.map((key, i) => provider(String(i), key)), store);
        expect(store).not.toHaveBeenCalled();
        expect(result.providers.map(item => item.apiKey)).toEqual(keys);
    });

    it('captures the submission before an awaited vault write', async () => {
        const gate = deferred<string[]>();
        const submitted = [provider()];
        const result = prepareProviderSecrets(submitted, () => gate.promise);
        submitted[0].apiKey = 'new-unsaved-key';
        submitted[0].baseUrl = 'https://new-unsaved.example';
        gate.resolve(['llm-provider:alpha']);
        const prepared = await result;
        expect(prepared.providers[0].apiKey).toBe('@keyring/llm-provider:alpha');
        expect(prepared.providers[0].baseUrl).toBe('https://alpha.example/v1');
    });

    it('reconciles a saved key reference without reverting newer endpoint and model edits', () => {
        const submitted = [provider()];
        const stored = [{ ...submitted[0], apiKey: '@keyring/llm-provider:alpha' }];
        const current = [{ ...submitted[0], baseUrl: 'https://edited.example', selectedModel: 'new-model' }];
        expect(reconcileProviderSecrets(current, submitted, stored)).toEqual([
            { ...current[0], apiKey: '@keyring/llm-provider:alpha' },
        ]);
    });

    it('retains newer raw key drafts and never restores a removed provider', () => {
        const submitted = [provider(), provider('beta')];
        const stored = submitted.map(item => ({ ...item, apiKey: `@keyring/llm-provider:${item.id}` }));
        const current = [{ ...submitted[0], apiKey: 'new-unsaved-key' }];
        expect(reconcileProviderSecrets(current, submitted, stored)).toEqual(current);
    });

    it('serializes secret, settings, and runtime updates in submission order', async () => {
        const queue = new ProviderSaveQueue();
        const gate = deferred<void>();
        const events: string[] = [];
        const first = queue.run(async () => { events.push('first vault'); await gate.promise; events.push('first settings'); events.push('first runtime'); });
        const second = queue.run(async () => { events.push('second vault'); events.push('second settings'); events.push('second runtime'); });
        await Promise.resolve();
        expect(events).toEqual(['first vault']);
        gate.resolve();
        await Promise.all([first, second]);
        expect(events).toEqual(['first vault', 'first settings', 'first runtime', 'second vault', 'second settings', 'second runtime']);
    });

    it('allows retry after a failed save without swallowing the original error', async () => {
        const queue = new ProviderSaveQueue();
        const first = queue.run(async () => { throw new Error('vault locked'); });
        const retry = queue.run(async () => 'saved');
        await expect(first).rejects.toThrow('vault locked');
        await expect(retry).resolves.toBe('saved');
    });
});
