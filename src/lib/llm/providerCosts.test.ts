import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ProviderRequests, type ProviderRequestConfig } from './providerRequests';
const mocks = vi.hoisted(() => ({ fetch: vi.fn() }));
vi.mock('@tauri-apps/plugin-http', () => ({ fetch: mocks.fetch }));
import { fetchProviderAccountReport, resolveProviderReportKey } from './providerCosts';

const config = (id = 'openai', baseUrl = `https://api.${id}.com/v1`): ProviderRequestConfig => ({
    id, baseUrl, apiKey: `@keyring/llm-provider:${id}`, selectedModel: 'chat',
});
const response = (body: unknown) => new Response(JSON.stringify(body), { status: 200 });
const now = new Date('2026-11-10T12:34:56Z');
beforeEach(() => vi.resetAllMocks());

describe('captured provider account reports', () => {
    it('uses the configured credit endpoint and captured credential', async () => {
        mocks.fetch.mockResolvedValue(response({ data: { total_credits: 25, total_usage: 9.5 } }));
        const provider = config('openrouter', 'https://credits.example/v1/');
        await expect(fetchProviderAccountReport(provider, 'captured'))
            .resolves.toBe('$15.50 left of $25.00 purchased ($9.50 used)');
        expect(mocks.fetch).toHaveBeenCalledWith('https://credits.example/v1/credits', { headers: { Authorization: 'Bearer captured' } });
        await expect(fetchProviderAccountReport({ ...provider, baseUrl: '' }, 'captured')).rejects.toThrow('Base URL');
        expect(mocks.fetch).toHaveBeenCalledTimes(1);
    });

    it('keeps a custom endpoint key away from the official cost API unless a separate admin key is saved', async () => {
        const provider = config('openai', 'https://compatible.example/v1');
        const resolve = vi.fn(async () => 'custom-endpoint-key');
        await expect(resolveProviderReportKey(provider, async () => null, resolve)).rejects.toThrow('separate provider admin key');
        expect(resolve).not.toHaveBeenCalled();
        const readAdmin = vi.fn(async () => 'official-admin-key');
        await expect(resolveProviderReportKey(provider, readAdmin, resolve)).resolves.toEqual({ key: 'official-admin-key', admin: true });
        expect(readAdmin).toHaveBeenCalledWith('llm-provider-admin:openai');
        expect(resolve).not.toHaveBeenCalled();
    });

    it('resolves an official endpoint ordinary key but does not hide a failed admin-store lookup', async () => {
        const resolve = vi.fn(async (reference: string) => `${reference}-resolved`);
        await expect(resolveProviderReportKey(config('anthropic'), async () => null, resolve))
            .resolves.toEqual({ key: '@keyring/llm-provider:anthropic-resolved', admin: false });
        resolve.mockClear();
        await expect(resolveProviderReportKey(config(), async () => { throw new Error('vault locked'); }, resolve)).rejects.toThrow('vault locked');
        expect(resolve).not.toHaveBeenCalled();
    });

    it.each(['openai', 'anthropic'])('aggregates every %s cost page with captured credentials and units', async id => {
        const entry = (value: number) => id === 'anthropic' ? { amount: String(value * 100) } : { amount: { value, currency: 'usd' } };
        mocks.fetch.mockResolvedValueOnce(response({ data: [{ results: [entry(1.25)] }], has_more: true, next_page: 'page/2' }))
            .mockResolvedValueOnce(response({ data: [{ results: [entry(2.5)] }], has_more: false }));
        await expect(fetchProviderAccountReport(config(id), 'captured-admin', undefined, now)).resolves.toContain('$3.75 spent this month');
        expect(mocks.fetch).toHaveBeenCalledTimes(2);
        expect(mocks.fetch.mock.calls[1][0]).toContain('&page=page%2F2');
        const headers = mocks.fetch.mock.calls[1][1].headers;
        expect(id === 'anthropic' ? headers['x-api-key'] : headers.Authorization)
            .toBe(id === 'anthropic' ? 'captured-admin' : 'Bearer captured-admin');
        expect(mocks.fetch.mock.calls[0][0]).toContain(id === 'anthropic' ? 'starting_at=2026-11-01T00:00:00Z' : 'start_time=1793491200');
    });

    it('refuses malformed, cyclic, and capped pagination instead of presenting partial totals', async () => {
        mocks.fetch.mockImplementation(async () => response({ data: [], has_more: true }));
        await expect(fetchProviderAccountReport(config(), 'admin')).rejects.toThrow('invalid page cursor');
        mocks.fetch.mockImplementation(async () => response({ data: [], has_more: true, next_page: 'same' }));
        await expect(fetchProviderAccountReport(config(), 'admin')).rejects.toThrow('invalid page cursor');
        mocks.fetch.mockReset();
        mocks.fetch.mockImplementation(async () => response({ data: [], has_more: true, next_page: String(mocks.fetch.mock.calls.length) }));
        await expect(fetchProviderAccountReport(config(), 'admin')).rejects.toThrow('no partial total');
        expect(mocks.fetch).toHaveBeenCalledTimes(20);
    });

    it('rejects invalid amounts instead of displaying zero or NaN as reported spend', async () => {
        for (const value of [null, undefined, '', 'not-a-number']) {
            mocks.fetch.mockResolvedValue(response({ data: [{ results: [{ amount: { value } }] }], has_more: false }));
            await expect(fetchProviderAccountReport(config(), 'admin')).rejects.toThrow('invalid cost amount');
        }
    });

    it('stops a paginated report when a credential replacement supersedes its request', async () => {
        let current = true;
        mocks.fetch.mockImplementation(async () => {
            current = false;
            return response({ data: [], has_more: true, next_page: 'next' });
        });
        await expect(fetchProviderAccountReport(config(), 'previous-admin', () => {
            if (!current) throw new Error('superseded');
        })).rejects.toThrow('superseded');
        expect(mocks.fetch).toHaveBeenCalledTimes(1);
    });

    it('does not dispatch after an admin lookup is invalidated while pending', async () => {
        const requests = new ProviderRequests(), provider = config();
        const request = requests.begin('balance', provider);
        let release!: (value: string) => void;
        const admin = new Promise<string>(resolve => { release = resolve; });
        const result = requests.run(request, () => provider,
            async () => (await resolveProviderReportKey(request.config, () => admin, async () => 'ordinary')).key,
            (captured, key) => fetchProviderAccountReport(captured, key));
        requests.invalidate(provider.id, 'balance');
        release('old-admin');
        await expect(result).resolves.toEqual({ current: false });
        expect(mocks.fetch).not.toHaveBeenCalled();
    });
});
