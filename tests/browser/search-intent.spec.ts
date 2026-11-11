// Query intent is explicit in UI/IPC; native contracts qualify actual retrieval.
import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function openSearch(page: Page) {
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await expect(page.getByRole('combobox', { name: 'Query interpretation', exact: true })).toBeVisible();
}

async function lastSearch(page: Page) {
    return page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search').at(-1)?.args);
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.addInitScript(() => {
        const root = window as any;
        const wrapped = new WeakSet<object>();
        const wrap = (bridge: any) => {
            if (!bridge || wrapped.has(bridge)) return bridge;
            wrapped.add(bridge);
            const original = bridge.invoke;
            bridge.invoke = async (command: string, args: any, options: any) => {
                if (command === 'sync_cb_v2_search' || command === 'sync_federated_search') {
                    root.__fixtureCalls.push({ command, args });
                    return command === 'sync_cb_v2_search'
                        ? { rows: [], used_text: true, used_vector: false, shards_queried: 1 }
                        : { hits: [], errors: {} };
                }
                const result = await original(command, args, options);
                return command === 'build_capabilities' ? { ...result, cloud_backup: true } : result;
            };
            return bridge;
        };
        // Fixture init scripts can run in either order; wrap before app startup.
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', {
            configurable: true,
            get: () => bridge,
            set: value => { bridge = wrap(value); },
        });
    });
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
});

test('sends natural language unchanged and marks results stale when interpretation changes', async ({ page }) => {
    await openSearch(page);
    const interpretation = page.getByRole('combobox', { name: 'Query interpretation', exact: true });
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await interpretation.selectOption('natural');
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('Does carbon dioxide increase crop yields?');
    await expect(page.locator('.search-help').filter({ hasText: 'Words and punctuation are treated as ordinary text.' })).toBeVisible();
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({
        query: 'Does carbon dioxide increase crop yields?', mode: 'text', searchFilters: { query_intent: 'natural' },
    });
    await expect(page.locator('.result-card')).toHaveCount(1);
    await interpretation.selectOption('advanced');
    await expect(page.getByText('Search options changed. Search again to update the results.', { exact: true })).toBeVisible();
    await expect(page.locator('.result-card')).toHaveCount(1);
    expect(await lastSearch(page)).toMatchObject({ searchFilters: { query_intent: 'natural' } });
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ searchFilters: { query_intent: 'advanced' } });
});

test('saves distinct query interpretations and restores the complete selected search', async ({ page }) => {
    await openSearch(page);
    const interpretation = page.getByRole('combobox', { name: 'Query interpretation', exact: true });
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('carbon AND dioxide');
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await interpretation.selectOption('natural');
    await page.getByRole('button', { name: 'Save this search', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Save this search', exact: true })).toBeEnabled();
    await interpretation.selectOption('advanced');
    await page.getByRole('button', { name: 'Save this search', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Save this search', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await expect(page.locator('.saved-dropdown .saved-name')).toHaveCount(2);
    await page.locator('.saved-dropdown .saved-name').first().click();
    await expect(interpretation).toHaveValue('natural');
    await expect.poll(() => lastSearch(page)).toMatchObject({
        query: 'carbon AND dioxide', mode: 'text', searchFilters: { query_intent: 'natural' },
    });
});

test('keeps legacy bookmarks strict and query-builder syntax explicitly advanced', async ({ page }) => {
    await page.evaluate(async () => {
        await (window as any).__TAURI_INTERNALS__.invoke('plugin:store|set', {
            key: 'savedSearches', value: [{ version: 3, name: 'Legacy expression', query: 'carbon AND dioxide', mode: 'text', limit: 20, savedAt: 1 }],
        });
    });
    await openSearch(page);
    const interpretation = page.getByRole('combobox', { name: 'Query interpretation', exact: true });
    await interpretation.selectOption('natural');
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await page.locator('.saved-dropdown .saved-name').filter({ hasText: 'Legacy expression' }).click();
    await expect(interpretation).toHaveValue('advanced');
    await expect.poll(() => lastSearch(page)).toMatchObject({ searchFilters: { query_intent: 'advanced' } });
    await interpretation.selectOption('natural');
    await page.getByText('Query help, builder, and indexed words', { exact: true }).click();
    const builder = page.getByRole('region', { name: 'Query builder', exact: true });
    await builder.getByLabel('Exact phrase', { exact: true }).fill('carbon dioxide');
    await builder.getByRole('button', { name: 'Use this query', exact: true }).click();
    await expect(interpretation).toHaveValue('advanced');
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toHaveValue('"carbon dioxide"');
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: '"carbon dioxide"', searchFilters: { query_intent: 'advanced' } });
});

test('disables lexical interpretation in Semantic mode and restores its selection on return', async ({ page }) => {
    await openSearch(page);
    const interpretation = page.getByRole('combobox', { name: 'Query interpretation', exact: true });
    await interpretation.selectOption('natural');
    await page.getByRole('button', { name: 'Semantic', exact: true }).click();
    await expect(interpretation).toBeDisabled();
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await expect(interpretation).toBeEnabled();
    await expect(interpretation).toHaveValue('natural');
});

test('makes the All sources guard actionable from Semantic while preserving selected-index semantic search', async ({ page }) => {
    await openSearch(page);
    const interpretation = page.getByRole('combobox', { name: 'Query interpretation', exact: true });
    await interpretation.selectOption('natural');
    await page.getByRole('button', { name: 'Semantic', exact: true }).click();
    await expect(interpretation).toBeDisabled();
    await expect(interpretation).toHaveValue('natural');
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('carbon dioxide');
    const allSources = page.getByTitle('Search all backends (local + cloud-backup + CrispLens) with RRF merge', { exact: true });
    const federationCalls = () => page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'sync_federated_search'));
    await allSources.click();
    await expect(page.locator('.fed-results .error')).toContainText('For All sources (Alle), first select Full text or Hybrid, then choose Advanced syntax. Use Search for semantic retrieval in the selected index.');
    expect(await federationCalls()).toEqual([]);
    await expect(allSources).toBeEnabled();
    await expect(interpretation).toBeDisabled();

    await expect(page.locator('.search-btn')).toBeEnabled();
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({
        query: 'carbon dioxide', mode: 'vector', searchFilters: { query_intent: 'natural' },
    });
    await expect(page.locator('.result-card')).toHaveCount(1);
    expect(await federationCalls()).toEqual([]);

    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await expect(interpretation).toBeEnabled();
    await interpretation.selectOption('advanced');
    await allSources.click();
    await expect.poll(federationCalls).toHaveLength(1);
    expect((await federationCalls())[0].args).toMatchObject({ q: 'carbon dioxide' });
    await expect(page.locator('.fed-results .error')).toHaveCount(0);
    await expect(allSources).toBeEnabled();
});

for (const mode of ['Full text', 'Hybrid']) {
    test('rejects unsupported natural Cloud and All sources requests in ' + mode + ' and recovers in Advanced syntax', async ({ page }) => {
        await openSearch(page);
        await page.getByRole('button', { name: mode, exact: true }).click();
        const interpretation = page.getByRole('combobox', { name: 'Query interpretation', exact: true });
        await interpretation.selectOption('natural');
        await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('carbon AND dioxide');
        const cloud = page.getByTitle('Search the cloud-backup VPS over HTTPS', { exact: true });
        const allSources = page.getByTitle('Search all backends (local + cloud-backup + CrispLens) with RRF merge', { exact: true });
        const searchCalls = () => page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) =>
            call.command === 'index_search' || call.command === 'sync_cb_v2_search' || call.command === 'sync_federated_search').map((call: any) => call.command));
        await cloud.click();
        await expect(page.locator('.remote-results .error')).toHaveText('Use Search with the selected index for Natural language. Choose Advanced syntax for Cloud search.');
        await expect(cloud).toBeEnabled();
        await allSources.click();
        await expect(page.locator('.fed-results .error')).toContainText('Use Search with the selected index for Natural language. Choose Advanced syntax for All sources.');
        await expect(allSources).toBeEnabled();
        expect(await searchCalls()).toEqual([]);

        await interpretation.selectOption('advanced');
        await cloud.click();
        await expect.poll(searchCalls).toEqual(['sync_cb_v2_search']);
        await expect(page.locator('.remote-results .error')).toHaveCount(0);
        await expect(cloud).toBeEnabled();
        await allSources.click();
        await expect.poll(searchCalls).toEqual(['sync_cb_v2_search', 'sync_federated_search']);
        await expect(page.locator('.fed-results .error')).toHaveCount(0);
        await expect(allSources).toBeEnabled();
    });
}
