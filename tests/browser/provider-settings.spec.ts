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
        root.__providerSavedSnapshots = [];
        root.__providerSecretValues = {};
        root.__providerSecretWriteFailure = false;
        root.__providerHoldSecretWrite = false;
        const responses = new Map<number, { url: string; bytes: number[]; read: boolean }>();
        let nextId = 100000;
        const wrapped = new WeakSet<object>();
        const wrap = (bridge: any) => {
            if (!bridge || wrapped.has(bridge)) return bridge;
            wrapped.add(bridge);
            const original = bridge.invoke;
            bridge.invoke = async (command: string, args: any = {}) => {
                if (command === 'plugin:store|get' && args.key in settings) return [settings[args.key], true];
                if (command === 'plugin:store|set' && args.key in settings) {
                    if (args.key === 'providers') root.__providerSavedSnapshots.push(structuredClone(args.value));
                    settings[args.key] = args.value; return;
                }
                if (command === 'secrets_bulk_set') {
                    if (root.__providerHoldSecretWrite) {
                        root.__providerHoldSecretWrite = false;
                        await new Promise<void>(resolve => { root.__providerReleases['secret-write'] = resolve; });
                    }
                    if (root.__providerSecretWriteFailure) return [];
                    for (const [account, value] of args.items) root.__providerSecretValues[account] = value;
                    return args.items.map(([account]: [string, string]) => account);
                }
                if (command === 'secret_get' && args.account.startsWith('llm-provider-admin:')) {
                    return root.__providerSecretValues[args.account] ?? null;
                }
                if (['secret_set', 'secret_delete'].includes(command) && args.account.startsWith('llm-provider-admin:')) {
                    if (root.__providerHoldSecretWrite) {
                        root.__providerHoldSecretWrite = false;
                        await new Promise<void>(resolve => { root.__providerReleases['secret-write'] = resolve; });
                    }
                    if (root.__providerSecretWriteFailure) throw new Error('The fixture secret store is locked');
                    if (command === 'secret_set') root.__providerSecretValues[args.account] = args.value;
                    else delete root.__providerSecretValues[args.account];
                    return;
                }
                if (command === 'secret_get' && args.account.startsWith('llm-provider:')) {
                    const id = args.account.slice('llm-provider:'.length);
                    if (root.__providerHoldSecret === id) {
                        root.__providerHoldSecret = '';
                        await new Promise<void>(resolve => { root.__providerReleases[`secret:${id}`] = resolve; });
                    }
                    return root.__providerSecretValues[args.account] ?? `${id}-secret`;
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
    const providers = page.locator('.settings-container .provider-details');
    if (!(await providers.evaluate(element => (element as HTMLDetailsElement).open))) {
        await providers.locator('summary').click();
    }
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

test('Index apply reports a settings persistence failure before native initialization and permits retry', async ({ page }) => {
    await providerFixture(page);
    const messages: string[] = [];
    page.on('console', message => { if (message.text().includes('settings.')) messages.push(message.text()); });
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__failIndexSettings = true;
        root.__indexInitializationCalls = 0;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any, options: any) => {
            if (command === 'bg_ingest_set_ocr_pipeline' && root.__failIndexSettings) throw new Error('Fixture OCR settings persistence failed');
            if (command === 'index_set_config' || command === 'index_init') root.__indexInitializationCalls += 1;
            return original(command, args, options);
        };
    });
    await page.locator('.settings-container').getByRole('button', { name: 'Search Index', exact: true }).click();
    const apply = page.getByRole('button', { name: 'Apply & Initialize', exact: true });
    await apply.click();
    await expect(page.locator('.settings-container')).toContainText('Fixture OCR settings persistence failed');
    await expect(apply).toBeEnabled();
    expect(await page.evaluate(() => (window as any).__indexInitializationCalls)).toBe(0);
    expect(messages.some(message => message.includes('settings.ocr phase=failed'))).toBe(true);
    expect(messages.join('\n')).not.toMatch(/Fixture OCR|secret|\.example/);
});

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
    // fetchModels returns ids sorted; the replacement endpoint lists groq-changed before groq-chat.
    await expect(page.locator('#model-select-input-groq option')).toHaveText(['-- Active Model --', 'groq-changed', 'groq-chat']);
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

test('Save All persists key references and preserves edits made while migration is pending', async ({ page }) => {
    await providerFixture(page);
    await chooseProvider(page, 'Groq');
    await expect(page.locator('#model-select-input-groq option')).toHaveText(['-- Active Model --', 'groq-chat', 'groq-updated']);
    await expect(page.getByRole('button', { name: 'Refresh models', exact: true })).toBeEnabled();
    const save = page.locator('.settings-container .header').getByRole('button', { name: 'Save All', exact: true });
    await expect(save).toBeEnabled();
    await page.locator('#api-key-input-groq').fill('fixture-submitted-key');
    await page.evaluate(() => { (window as any).__providerHoldSecretWrite = true; });
    await save.click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__providerReleases['secret-write'])).toBe('function');
    await page.locator('#api-key-input-groq').fill('fixture-new-unsaved-key');
    await page.locator('#base-url-input-groq').fill('https://groq-changed.example/v1');
    await release(page, 'secret-write');
    await expect(page.locator('#api-key-input-groq')).toHaveValue('fixture-new-unsaved-key');
    await expect(page.locator('#base-url-input-groq')).toHaveValue('https://groq-changed.example/v1');
    await expect(page.getByRole('status').filter({ hasText: 'New edits are still unsaved' })).toBeVisible();
    const stored = await page.evaluate(() => (window as any).__providerSavedSnapshots.at(-1).find((provider: any) => provider.id === 'groq'));
    expect(stored.apiKey).toBe('@keyring/llm-provider:groq');
    expect(stored.baseUrl).toBe('https://groq.example/v1');
    await save.click();
    await expect(page.locator('#api-key-input-groq')).toHaveValue('@keyring/llm-provider:groq');
    expect(await page.evaluate(() => (window as any).__providerSecretValues['llm-provider:groq'])).toBe('fixture-new-unsaved-key');
    const snapshots = JSON.stringify(await page.evaluate(() => (window as any).__providerSavedSnapshots));
    expect(snapshots).not.toContain('fixture-submitted-key');
    expect(snapshots).not.toContain('fixture-new-unsaved-key');
});

test('a failed key migration keeps the editable key and writes no plaintext settings before retry', async ({ page }) => {
    await providerFixture(page);
    await chooseProvider(page, 'Groq');
    await expect(page.locator('#model-select-input-groq option')).toHaveText(['-- Active Model --', 'groq-chat', 'groq-updated']);
    await expect(page.getByRole('button', { name: 'Refresh models', exact: true })).toBeEnabled();
    const save = page.locator('.settings-container .header').getByRole('button', { name: 'Save All', exact: true });
    await expect(save).toBeEnabled();
    const writes = await page.evaluate(() => (window as any).__providerSavedSnapshots.length);
    await page.evaluate(() => { (window as any).__providerSecretWriteFailure = true; });
    await page.locator('#api-key-input-groq').fill('fixture-retry-key');
    await save.click();
    await expect(page.getByRole('alert').filter({ hasText: 'Secret storage did not save' })).toBeVisible();
    await expect(page.locator('#api-key-input-groq')).toHaveValue('fixture-retry-key');
    expect(await page.evaluate(() => (window as any).__providerSavedSnapshots.length)).toBe(writes);
    await page.evaluate(() => { (window as any).__providerSecretWriteFailure = false; });
    await save.click();
    await expect(page.locator('#api-key-input-groq')).toHaveValue('@keyring/llm-provider:groq');
    expect(JSON.stringify(await page.evaluate(() => (window as any).__providerSavedSnapshots))).not.toContain('fixture-retry-key');
});

test('a pending admin-key save keeps its provider and preserves a newer draft after switching panels', async ({ page }) => {
    await providerFixture(page);
    await chooseProvider(page, 'OpenAI');
    const input = page.locator('#cost-key-openai');
    await input.fill('fixture-submitted-admin');
    await page.evaluate(() => { (window as any).__providerHoldSecretWrite = true; });
    await page.locator('.cost-key-row').getByRole('button', { name: 'Save', exact: true }).click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__providerReleases['secret-write'])).toBe('function');
    await input.fill('fixture-new-unsaved-admin');
    await chooseProvider(page, 'Groq');
    await release(page, 'secret-write');
    await chooseProvider(page, 'OpenAI');
    await expect(input).toHaveValue('fixture-new-unsaved-admin');
    await expect(page.getByRole('status').filter({ hasText: 'Admin key saved in the secret store. New edits are still unsaved.' })).toBeVisible();
    const vault = await page.evaluate(() => (window as any).__providerSecretValues);
    expect(vault['llm-provider-admin:openai']).toBe('fixture-submitted-admin');
    expect(vault['llm-provider-admin:groq']).toBeUndefined();
    const stored = JSON.stringify(await page.evaluate(() => (window as any).__providerSavedSnapshots));
    expect(stored).not.toContain('fixture-submitted-admin');
    expect(stored).not.toContain('fixture-new-unsaved-admin');
});
