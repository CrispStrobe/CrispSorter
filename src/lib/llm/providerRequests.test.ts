import { describe, expect, it, vi } from 'vitest';
import {
    ProviderRequests, providerConfigurationMatches, providerErrorMessage,
    type ProviderRequestConfig,
} from './providerRequests';

function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

function provider(id: string): ProviderRequestConfig {
    return { id, baseUrl: `https://${id}.example/v1`, apiKey: `@keyring/${id}`, selectedModel: `${id}-chat` };
}

describe('provider requests across asynchronous boundaries', () => {
    it('keeps a delayed secret and model request with its original provider after a panel switch', async () => {
        const requests = new ProviderRequests();
        const providers = [provider('alpha'), provider('beta')];
        let selected = providers[0];
        const request = requests.begin('models', selected);
        const secret = deferred<string>();
        const resolveKey = vi.fn(() => secret.promise);
        const fetchModels = vi.fn(async (config: ProviderRequestConfig, key: string) => {
            expect(config).toEqual(providers[0]);
            expect(key).toBe('alpha-secret');
            return ['alpha-result'];
        });
        const result = requests.run(request,
            () => providers.find(p => p.id === request.config.id), resolveKey, fetchModels);
        selected = providers[1];
        secret.resolve('alpha-secret');
        await expect(result).resolves.toEqual({ current: true, value: ['alpha-result'] });
        expect(resolveKey).toHaveBeenCalledWith('@keyring/alpha');
        expect(fetchModels).toHaveBeenCalledOnce();
        expect(selected.id).toBe('beta');
        expect(request.config).not.toBe(providers[0]);
        expect(Object.isFrozen(request.config)).toBe(true);
    });

    it.each(['baseUrl', 'apiKey', 'selectedModel'] as const)(
        'does not dispatch a connection test after %s changes while the secret is resolving', async field => {
            const requests = new ProviderRequests();
            let current = provider('alpha');
            const request = requests.begin('connection', current);
            const secret = deferred<string>();
            const send = vi.fn(async () => 'OK');
            const result = requests.run(request, () => current, () => secret.promise, send);
            current = { ...current, [field]: 'changed' };
            secret.resolve('old-secret');
            await expect(result).resolves.toEqual({ current: false });
            expect(send).not.toHaveBeenCalled();
        },
    );

    it('discards a model response after its endpoint changes, without overwriting the new configuration', async () => {
        const requests = new ProviderRequests();
        let current = provider('alpha');
        const request = requests.begin('models', current);
        const response = deferred<string[]>();
        const send = vi.fn(() => response.promise);
        const result = requests.run(request, () => current, async () => 'key', send);
        await Promise.resolve();
        expect(send).toHaveBeenCalledOnce();
        current = { ...current, baseUrl: 'https://replacement.example/v1' };
        response.resolve(['old-model']);
        await expect(result).resolves.toEqual({ current: false });
        expect(current.baseUrl).toBe('https://replacement.example/v1');
    });

    it('keeps the latest same-provider response when requests finish out of order', async () => {
        const requests = new ProviderRequests();
        const current = provider('alpha');
        const old = requests.begin('models', current);
        const oldResponse = deferred<string[]>();
        const first = requests.run(old, () => current, async () => 'key', () => oldResponse.promise);
        await Promise.resolve();
        const newest = requests.begin('models', current);
        const second = requests.run(newest, () => current, async () => 'key', async () => ['new-model']);
        await expect(second).resolves.toEqual({ current: true, value: ['new-model'] });
        oldResponse.resolve(['old-model']);
        await expect(first).resolves.toEqual({ current: false });
        expect(requests.isCurrent(old, current)).toBe(false);
        expect(requests.isCurrent(newest, current)).toBe(true);
    });

    it('allows independent providers and operations without clearing each other', async () => {
        const requests = new ProviderRequests();
        const alpha = provider('alpha');
        const beta = provider('beta');
        const a = requests.begin('models', alpha);
        const b = requests.begin('models', beta);
        const test = requests.begin('connection', alpha);
        expect(requests.isCurrent(a, alpha)).toBe(true);
        expect(requests.isCurrent(b, beta)).toBe(true);
        expect(requests.isCurrent(test, alpha)).toBe(true);
        expect(requests.isCurrent(a, beta)).toBe(false);
    });

    it('does not invalidate a model list when the user chooses a model during refresh', async () => {
        const requests = new ProviderRequests();
        let current = provider('alpha');
        const request = requests.begin('models', current);
        const response = deferred<string[]>();
        const result = requests.run(request, () => current, async () => 'key', () => response.promise);
        await Promise.resolve();
        current = { ...current, selectedModel: 'chosen-by-user' };
        response.resolve(['another-model']);
        await expect(result).resolves.toEqual({ current: true, value: ['another-model'] });
        expect(current.selectedModel).toBe('chosen-by-user');
        expect(providerConfigurationMatches(request.config, current, true)).toBe(false);
    });

    it('does not dispatch after the provider is removed or the Settings component is disposed', async () => {
        for (const disposition of ['removed', 'disposed']) {
            const requests = new ProviderRequests();
            let current: ProviderRequestConfig | undefined = provider('alpha');
            const request = requests.begin('balance', current);
            const secret = deferred<string>();
            const send = vi.fn(async () => 'balance');
            const result = requests.run(request, () => current, () => secret.promise, send);
            if (disposition === 'removed') current = undefined;
            else requests.dispose();
            secret.resolve('key');
            await expect(result).resolves.toEqual({ current: false });
            expect(send).not.toHaveBeenCalled();
        }
    });

    it('makes an older error ineligible to replace the latest successful result', async () => {
        const requests = new ProviderRequests();
        const current = provider('alpha');
        const request = requests.begin('connection', current);
        const response = deferred<string>();
        const result = requests.run(request, () => current, async () => 'key', () => response.promise);
        await Promise.resolve();
        const latest = requests.begin('connection', current);
        await expect(requests.run(latest, () => current, async () => 'key', async () => 'OK'))
            .resolves.toEqual({ current: true, value: 'OK' });
        response.reject(new Error('old failure'));
        await expect(result).rejects.toThrow('old failure');
        expect(requests.isCurrent(request, current)).toBe(false);
    });

    it('keeps a request invalid after an edit returns to the same credential reference', async () => {
        const requests = new ProviderRequests();
        const current = provider('alpha');
        const request = requests.begin('connection', current);
        const secret = deferred<string>();
        const send = vi.fn(async () => 'OK');
        const result = requests.run(request, () => current, () => secret.promise, send);
        // Saving a newly entered key restores the same keyring sentinel. The
        // input edit still cancels the request that resolved the previous key.
        requests.invalidate(current.id);
        secret.resolve('previous-secret');
        await expect(result).resolves.toEqual({ current: false });
        expect(send).not.toHaveBeenCalled();
    });
});

describe('provider error feedback', () => {
    it('handles native string and record errors without assuming message is a string', () => {
        expect(providerErrorMessage('  refused  ', 'fallback')).toBe('refused');
        expect(providerErrorMessage({ message: ' expired ' }, 'fallback')).toBe('expired');
        expect(providerErrorMessage({ message: 429 }, 'fallback')).toBe('429');
        expect(providerErrorMessage(new Error('offline'), 'fallback')).toBe('offline');
    });

    it('gives useful fallback text for empty or unserializable errors', () => {
        const circular: Record<string, unknown> = {};
        circular.self = circular;
        for (const error of [null, undefined, {}, '', circular]) {
            expect(providerErrorMessage(error, 'Please retry')).toBe('Please retry');
        }
    });
});
