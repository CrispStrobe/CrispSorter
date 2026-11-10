import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function providerFixture(page: Page) {
    await fixtures(page);
    await page.addInitScript(() => {
        const root = window as any;
        const settings: Record<string, unknown> = {
            providers: ['groq', 'openai'].map(id => ({
                id, name: id === 'groq' ? 'Groq' : 'OpenAI',
                baseUrl: `https://${id}.example/v1`, apiKey: `@keyring/llm-provider:${id}`,
                models: [`${id}-chat`], selectedModel: `${id}-chat`, isConfigured: true,
            })),
            activeProviderId: 'groq', thirdPartyAiConsent: ['groq', 'openai'],
        };
        root.__providerCalls = [];
        root.__providerReleases = {};
        root.__providerHoldSecret = '';
        root.__providerHoldUrl = '';
        const responses = new Map<number, { url: string; bytes: number[]; read: boolean }>();
        let nextId = 100000;
        const wrapped = new WeakSet<object>();
        const wrap = (bridge: any) => {
            if (!bridge || wrapped.has(bridge)) return bridge;
            wrapped.add(bridge);
            const original = bridge.invoke;
            bridge.invoke = async (command: string, args: any = {}) => {
                if (command === 'plugin:store|get' && args.key in settings) return [settings[args.key], true];
                if (command === 'plugin:store|set' && args.key in settings) { settings[args.key] = args.value; return; }
                if (command === 'secret_get' && args.account.startsWith('llm-provider:')) {
                    const id = args.account.slice('llm-provider:'.length);
                    if (root.__providerHoldSecret === id) {
                        root.__providerHoldSecret = '';
                        await new Promise<void>(resolve => { root.__providerReleases[`secret:${id}`] = resolve; });
                    }
                    return `${id}-secret`;
                }
                if (command === 'plugin:http|fetch' && /^https:\/\/(groq|openai)(-changed)?\.example\//.test(args.clientConfig.url)) {
                    const config = args.clientConfig;
                    const provider = config.url.includes('groq') ? 'groq' : 'openai';
                    root.__providerCalls.push({ url: config.url, headers: config.headers });
                    if (root.__providerHoldUrl === config.url) {
                        await new Promise<void>(resolve => { root.__providerReleases[config.url] = resolve; });
                    }
                    const response = config.url.endsWith('/models')
                        ? { data: [{ id: `${provider}-chat` }, { id: `${provider}-${config.url.includes('-changed') ? 'changed' : 'updated'}` }] }
                        : { choices: [{ message: { content: `${provider}-response` } }] };
                    const id = nextId++;
                    responses.set(id, { url: config.url, bytes: Array.from(new TextEncoder().encode(JSON.stringify(response))), read: false });
                    return id;
                }
                if (command === 'plugin:http|fetch_send' && responses.has(args.rid)) {
                    return { status: 200, statusText: 'OK', url: responses.get(args.rid)!.url, headers: [['content-type', 'application/json']], rid: args.rid };
                }
                if (command === 'plugin:http|fetch_read_body' && responses.has(args.rid)) {
                    const response = responses.get(args.rid)!;
                    if (response.read) return [1];
                    response.read = true;
                    return [...response.bytes, 0];
                }
                const result = await original(command, args);
                if (command === 'plugin:event|listen' && args.event === 'index://download-progress') root.__providerSettingsReady = true;
                return result;
            };
            return bridge;
        };
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', {
            configurable: true, get: () => bridge, set: value => { bridge = wrap(value); },
        });
    });
    await page.goto('/');
    await expect(page.locator('.batch-container .file-name')).toHaveText('Pending scan.png');
    await page.locator('.main-nav').getByRole('button', { name: 'Settings', exact: true }).click();
    await expect(page.locator('.settings-container')).toBeVisible();
    await expect.poll(() => page.evaluate(() => (window as any).__providerSettingsReady)).toBe(true);
}

async function chooseProvider(page: Page, name: 'Groq' | 'OpenAI') {
    await page.locator('.settings-container .provider-list').getByRole('button', { name, exact: true }).click();
    await expect(page.locator('.settings-container').getByRole('heading', { name, exact: true })).toBeVisible();
}

async function release(page: Page, key: string) {
    await expect.poll(() => page.evaluate(k => typeof (window as any).__providerReleases[k], key)).toBe('function');
    await page.evaluate(k => {
        const root = window as any;
        root.__providerHoldSecret = '';
        root.__providerHoldUrl = '';
        root.__providerReleases[k]();
        delete root.__providerReleases[k];
    }, key);
}

test('switching provider panels during secret resolution preserves credential routing and model lists', async ({ page }) => {
    await providerFixture(page);
    await page.evaluate(() => { (window as any).__providerHoldSecret = 'groq'; });
    await chooseProvider(page, 'Groq');
    await expect.poll(() => page.evaluate(() => typeof (window as any).__providerReleases['secret:groq'])).toBe('function');
    await chooseProvider(page, 'OpenAI');
    await expect(page.locator('#model-select-input-openai option')).toHaveText(['-- Active Model --', 'openai-chat', 'openai-updated']);
    await release(page, 'secret:groq');
    await expect.poll(() => page.evaluate(() => (window as any).__providerCalls.map((call: any) => call.url)))
        .toContain('https://groq.example/v1/models');
    await expect(page.locator('#model-select-input-openai option')).toHaveText(['-- Active Model --', 'openai-chat', 'openai-updated']);
    await chooseProvider(page, 'Groq');
    await expect(page.locator('#model-select-input-groq option')).toHaveText(['-- Active Model --', 'groq-chat', 'groq-updated']);
    const calls = await page.evaluate(() => (window as any).__providerCalls);
    for (const call of calls) {
        const recipient = call.url.includes('groq') ? 'groq' : 'openai';
        expect(new Map<string, string>(call.headers).get('authorization')).toBe(`Bearer ${recipient}-secret`);
    }
});

test('an edited endpoint discards its pending model list and can refresh the replacement endpoint', async ({ page }) => {
    await providerFixture(page);
    await page.evaluate(() => { (window as any).__providerHoldUrl = 'https://groq.example/v1/models'; });
    await chooseProvider(page, 'Groq');
    await expect.poll(() => page.evaluate(() => typeof (window as any).__providerReleases['https://groq.example/v1/models'])).toBe('function');
    await page.locator('#base-url-input-groq').fill('https://groq-changed.example/v1');
    await release(page, 'https://groq.example/v1/models');
    await expect(page.locator('#model-select-input-groq option')).toHaveText(['-- Active Model --', 'groq-chat']);
    await page.getByRole('button', { name: 'Refresh models', exact: true }).click();
    await expect(page.locator('#model-select-input-groq option')).toHaveText(['-- Active Model --', 'groq-chat', 'groq-changed']);
    await expect.poll(() => page.evaluate(() => (window as any).__providerCalls.map((call: any) => call.url)))
        .toContain('https://groq-changed.example/v1/models');
});

test('parallel connection tests keep responses and secrets with their configured provider', async ({ page }) => {
    await providerFixture(page);
    await chooseProvider(page, 'Groq');
    await expect(page.locator('#model-select-input-groq option')).toHaveText(['-- Active Model --', 'groq-chat', 'groq-updated']);
    await expect(page.getByRole('button', { name: 'Refresh models', exact: true })).toBeEnabled();
    await page.evaluate(() => { (window as any).__providerHoldSecret = 'groq'; });
    await page.getByRole('button', { name: 'Test', exact: true }).click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__providerReleases['secret:groq'])).toBe('function');
    await chooseProvider(page, 'OpenAI');
    await expect(page.getByRole('button', { name: 'Test', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Test', exact: true }).click();
    await expect(page.locator('.test-result-box')).toContainText('openai-response');
    await release(page, 'secret:groq');
    await expect.poll(() => page.evaluate(() => (window as any).__providerCalls.map((call: any) => call.url)))
        .toContain('https://groq.example/v1/chat/completions');
    await expect(page.locator('.test-result-box')).toContainText('openai-response');
    await chooseProvider(page, 'Groq');
    await expect(page.locator('.test-result-box')).toContainText('groq-response');
    const calls = await page.evaluate(() => (window as any).__providerCalls.filter((call: any) => call.url.endsWith('/chat/completions')));
    expect(calls).toHaveLength(2);
    for (const call of calls) {
        const recipient = call.url.includes('groq') ? 'groq' : 'openai';
        expect(new Map<string, string>(call.headers).get('authorization')).toBe(`Bearer ${recipient}-secret`);
    }
});
