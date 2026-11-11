import { beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
    fetch: vi.fn(), resolveSecret: vi.fn(), consent: vi.fn(),
}));
vi.mock('@tauri-apps/plugin-http', () => ({ fetch: mocks.fetch }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('./webllm', () => ({ queryWebLLM: vi.fn(), getWebLLMLoadedModel: vi.fn() }));
vi.mock('./ort', () => ({ queryORT: vi.fn(), getORTLoadedModel: vi.fn() }));
vi.mock('../log', () => ({ flog: vi.fn() }));
vi.mock('../secrets', () => ({ resolveSecret: mocks.resolveSecret }));
vi.mock('./thirdPartyConsent', () => ({ ensureThirdPartyConsent: mocks.consent }));

import { LLMClient } from './client';

beforeEach(() => {
    vi.resetAllMocks();
    mocks.resolveSecret.mockImplementation(async key => key ?? '');
    mocks.consent.mockResolvedValue(undefined);
    mocks.fetch.mockImplementation(async () => new Response(JSON.stringify({
        choices: [{ message: { content: 'OK' } }], data: [{ id: 'model' }],
    }), { status: 200, headers: { 'Content-Type': 'application/json' } }));
});

describe('provider connection request routing', () => {
    it('tests the captured configured endpoint and uses the same endpoint for consent', async () => {
        const client = new LLMClient();
        await expect(client.query('openai', 'custom-model', 'test', 'captured-key', 0.3,
            undefined, undefined, 'https://configured.example/v1/')).resolves.toBe('OK');
        expect(mocks.consent).toHaveBeenCalledWith('openai', 'https://configured.example/v1');
        const [url, options] = mocks.fetch.mock.calls[0];
        expect(url).toBe('https://configured.example/v1/chat/completions');
        expect(options.headers.Authorization).toBe('Bearer captured-key');
        expect(JSON.parse(options.body).model).toBe('custom-model');
    });

    it('preserves the default endpoint and saved-key behavior for existing callers', async () => {
        const client = new LLMClient({ openai: 'saved-key' });
        await client.query('openai', 'model', 'test');
        expect(mocks.fetch.mock.calls[0][0]).toBe('https://api.openai.com/v1/chat/completions');
        expect(mocks.fetch.mock.calls[0][1].headers.Authorization).toBe('Bearer saved-key');
    });

    it('does not replace an explicitly cleared key with an older cached credential', async () => {
        const client = new LLMClient({ openai: 'old-key' });
        await client.query('openai', 'model', 'test', '');
        expect(mocks.resolveSecret).toHaveBeenCalledWith('');
        expect(mocks.fetch.mock.calls[0][1].headers.Authorization).toBeUndefined();
    });

    it('keeps an explicit empty key empty when refreshing a configured model endpoint', async () => {
        const client = new LLMClient({ openai: 'old-key' });
        await expect(client.fetchModels('openai', '', 'https://configured.example/v1')).resolves.toEqual(['model']);
        expect(mocks.fetch.mock.calls[0][0]).toBe('https://configured.example/v1/models');
        expect(mocks.fetch.mock.calls[0][1].headers.Authorization).toBeUndefined();
    });

    it('does not silently test the default server when the configured endpoint is empty', async () => {
        const client = new LLMClient();
        await expect(client.query('openai', 'model', 'test', 'key', 0.3, undefined, undefined, ''))
            .rejects.toThrow('Base URL');
        expect(mocks.fetch).not.toHaveBeenCalled();
    });

    it('does not send a model-list credential to a default endpoint after the endpoint is cleared', async () => {
        const client = new LLMClient();
        await expect(client.fetchModels('openai', 'key', '')).rejects.toThrow('Base URL');
        expect(mocks.fetch).not.toHaveBeenCalled();
    });

    it('normalizes a saved Gemini endpoint for both model discovery and the captured connection test', async () => {
        const client = new LLMClient();
        const legacy = 'https://generativelanguage.googleapis.com/v1beta/';
        await client.fetchModels('google', 'google-key', legacy);
        await client.query('google', 'gemini-chat', 'test', 'google-key', 0.3, undefined, undefined, legacy);
        expect(mocks.fetch.mock.calls.map(call => call[0])).toEqual([
            'https://generativelanguage.googleapis.com/v1beta/openai/models',
            'https://generativelanguage.googleapis.com/v1beta/openai/chat/completions',
        ]);
        expect(mocks.consent).toHaveBeenCalledWith('google', 'https://generativelanguage.googleapis.com/v1beta/openai');
    });

    it('preserves Anthropic model pagination and its headers at the captured endpoint', async () => {
        mocks.fetch.mockResolvedValueOnce(new Response(JSON.stringify({ data: [{ id: 'first' }], has_more: true, last_id: 'first/page' })))
            .mockResolvedValueOnce(new Response(JSON.stringify({ data: [{ id: 'second' }], has_more: false })));
        const client = new LLMClient();
        await expect(client.fetchModels('anthropic', 'captured-key', 'https://anthropic-proxy.example/v1/')).resolves.toEqual(['first', 'second']);
        expect(mocks.fetch.mock.calls[1][0]).toBe('https://anthropic-proxy.example/v1/models?limit=1000&after_id=first%2Fpage');
        for (const [, options] of mocks.fetch.mock.calls) {
            expect(options.headers).toEqual({ 'x-api-key': 'captured-key', 'anthropic-version': '2023-06-01' });
        }
    });

    it('refuses incomplete Anthropic model pagination and an explicitly cleared key', async () => {
        const client = new LLMClient({ anthropic: 'older-cached-key' });
        await expect(client.fetchModels('anthropic', '', 'https://api.anthropic.com/v1')).rejects.toThrow('No API key');
        expect(mocks.fetch).not.toHaveBeenCalled();
        mocks.fetch.mockImplementation(async () => new Response(JSON.stringify({ data: [{ id: 'one' }], has_more: true, last_id: 'same' })));
        await expect(client.fetchModels('anthropic', 'current-key')).rejects.toThrow('incomplete model list');
        expect(mocks.fetch).toHaveBeenCalledTimes(2);
    });
});
