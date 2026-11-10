// These tests verify UI/IPC behavior. Native filtering has separate Rust fixtures.
import { test, expect, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function openSearch(page: Page) {
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toBeVisible();
}

async function lastSearch(page: Page) {
    return page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search').at(-1)?.args);
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
});

test('search sends metadata filters and restores distinct complete bookmarks', async ({ page }) => {
    await openSearch(page);
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await page.getByRole('button', { name: 'Search filters', exact: true }).click();
    const options = page.getByRole('region', { name: 'Search options' });
    // A wrapping label also contains the option text. Match the combobox's
    // accessible name, which is just the caption, rather than label textContent.
    const resultLimit = options.getByRole('combobox', { name: 'Maximum results', exact: true });
    await options.getByLabel('File type', { exact: true }).fill('.PDF, docx');
    await options.getByLabel('Language', { exact: true }).fill('de');
    await options.getByLabel('Year from', { exact: true }).fill('2020');
    await options.getByLabel('Year to', { exact: true }).fill('2025');
    await options.getByLabel('Folder', { exact: true }).fill('/papers');
    await options.getByLabel('Source URL contains', { exact: true }).fill('example.org');
    await options.getByLabel('Tag', { exact: true }).fill('reviewed');
    await options.getByLabel('Audio from (s)', { exact: true }).fill('0');
    await options.getByLabel('Audio to (s)', { exact: true }).fill('300');
    await options.getByLabel('Camera make', { exact: true }).fill('Canon');
    await options.getByLabel('Camera model', { exact: true }).fill('R5');
    await options.getByLabel('Typo tolerance', { exact: true }).check();
    await options.getByLabel('Synonyms', { exact: true }).check();
    await resultLimit.selectOption('50');
    await expect(resultLimit).toHaveValue('50');
    await page.getByRole('button', { name: 'Save this search', exact: true }).click();
    await expect(page.locator('.search-root').getByRole('status')).toContainText('Search saved with filters.');
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({
        query: 'warranty', mode: 'text', limit: 50, fuzzy: true, synonyms: true,
        searchFilters: { ext: ['docx', 'pdf'], language: 'de', year_min: 2020, year_max: 2025,
            parent_dir_prefix: '/papers', url_domain: 'example.org', audio_duration_min_seconds: 0,
            audio_duration_max_seconds: 300, image_camera_make: 'Canon', image_camera_model: 'R5' },
    });
    await page.getByRole('button', { name: 'Reset filters', exact: true }).click();
    await resultLimit.selectOption('20');
    await page.getByRole('button', { name: 'Save this search', exact: true }).click();
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await expect(page.locator('.saved-dropdown .saved-name')).toHaveCount(2);
    await page.locator('.saved-dropdown .saved-name').first().click();
    await expect(options.getByLabel('Language', { exact: true })).toHaveValue('de');
    await expect(options.getByLabel('Year from', { exact: true })).toHaveValue('2020');
    await expect(options.getByLabel('Synonyms', { exact: true })).toBeChecked();
    await expect(resultLimit).toHaveValue('50');
    await expect.poll(() => lastSearch(page)).toMatchObject({ limit: 50, tag: 'reviewed', searchFilters: { ext: ['docx', 'pdf'], language: 'de' } });
    await page.getByRole('button', { name: 'Search filters', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Remove Language', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Remove Language', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Remove Language', exact: true })).toHaveCount(0);
});

test('legacy bookmarks clear active filters instead of inheriting them', async ({ page }) => {
    await page.evaluate(async () => {
        await (window as any).__TAURI_INTERNALS__.invoke('plugin:store|set', {
            key: 'savedSearches', value: [{ name: 'Legacy warranty', query: 'warranty', mode: 'text', limit: 20, savedAt: 1 }],
        });
    });
    await openSearch(page);
    await page.getByRole('button', { name: 'Search filters', exact: true }).click();
    await page.getByLabel('Language', { exact: true }).fill('de');
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await page.locator('.saved-dropdown .saved-name').filter({ hasText: 'Legacy warranty' }).click();
    await expect(page.getByLabel('Language', { exact: true })).toHaveValue('');
    await expect.poll(() => lastSearch(page)).toMatchObject({ searchFilters: { language: null, ext: [] }, fuzzy: false, synonyms: false, docIdScope: null });
});

test('inverted ranges show recovery guidance without making a search request', async ({ page }) => {
    await openSearch(page);
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.getByRole('button', { name: 'Search filters', exact: true }).click();
    await page.getByLabel('Year from', { exact: true }).fill('2025');
    await page.getByLabel('Year to', { exact: true }).fill('2020');
    await page.locator('.search-btn').click();
    await expect(page.getByRole('alert')).toContainText('Year min must not be after year max.');
    expect(await lastSearch(page)).toBeUndefined();
    await page.getByRole('button', { name: 'Reset filters', exact: true }).click();
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ searchFilters: { year_min: null, year_max: null } });
});

test('clearing a pending search prevents stale results from reappearing', async ({ page }) => {
    await openSearch(page);
    await page.evaluate(() => {
        const root = window as any;
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: unknown) => command === 'index_search'
            ? new Promise(resolve => { root.__resolvePendingSearch = () => resolve(root.__fixtureResults); })
            : command === 'sync_skeleton_search'
                ? new Promise(resolve => { root.__resolvePendingHints = () => resolve({ authors: [{ name: 'Old author', doc_count: 1 }], parent_dirs: [] }); })
                : original(command, args);
    });
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.locator('.search-btn').click();
    await expect(page.locator('.search-root').getByRole('status')).toContainText('Searching…');
    await page.getByRole('button', { name: 'Clear search', exact: true }).click();
    await page.evaluate(() => { (window as any).__resolvePendingSearch(); (window as any).__resolvePendingHints(); });
    await expect(page.getByText('Find something in your library', { exact: true })).toBeVisible();
    await expect(page.locator('.result-card')).toHaveCount(0);
    await expect(page.locator('.skeleton-hints')).toHaveCount(0);
});

test('reranker failure leaves the original results visible with a notice', async ({ page }) => {
    await page.evaluate(async () => {
        const root = window as any;
        await root.__TAURI_INTERNALS__.invoke('plugin:store|set', {
            key: 'experimentalCrispEmbed', value: { endpoint: 'http://127.0.0.1:18080', moedern: true, decisions: true },
        });
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: unknown) => {
            if (command === 'crispembed_rerank_passages') return Promise.reject(new Error('Fixture service offline'));
            return original(command, args);
        };
    });
    await openSearch(page);
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.locator('.search-btn').click();
    await expect(page.locator('.search-root').getByRole('status')).toContainText('showing the original search order');
    await expect(page.locator('.result-card').filter({ hasText: 'Warranty.txt' })).toBeVisible();
});

test('bookmark persistence finishes before accepting the next save', async ({ page }) => {
    await openSearch(page);
    await page.evaluate(() => {
        const root = window as any;
        const original = root.__TAURI_INTERNALS__.invoke;
        let held = false;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any) => {
            if (command === 'plugin:store|set' && args.key === 'savedSearches' && !held) {
                held = true;
                return new Promise(resolve => { root.__releaseBookmarkSave = async () => resolve(await original(command, args)); });
            }
            return original(command, args);
        };
    });
    const input = page.getByRole('textbox', { name: 'Search indexed documents' });
    const save = page.getByRole('button', { name: 'Save this search', exact: true });
    await input.fill('first query');
    await save.click();
    await input.fill('second query');
    await expect(save).toBeDisabled();
    await page.evaluate(() => (window as any).__releaseBookmarkSave());
    await expect(save).toBeEnabled();
    await save.click();
    await expect(save).toBeEnabled();
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await expect(page.locator('.saved-dropdown .saved-name')).toHaveCount(2);
    await expect(page.locator('.saved-dropdown')).toContainText('first query');
    await expect(page.locator('.saved-dropdown')).toContainText('second query');
});

test('clearing image search prevents its late response from replacing the empty state', async ({ page }) => {
    await openSearch(page);
    await page.evaluate(() => {
        const root = window as any;
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: unknown) => command === 'index_search_by_image'
            ? new Promise(resolve => { root.__resolveImageSearch = () => resolve(root.__fixtureResults); })
            : original(command, args);
    });
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.getByTitle('Search by image similarity (ViT + omni embeddings)', { exact: true }).click();
    await expect(page.locator('.search-root').getByRole('status')).toContainText('Searching…');
    await page.getByRole('button', { name: 'Clear search', exact: true }).click();
    await page.evaluate(() => (window as any).__resolveImageSearch());
    await expect(page.getByText('Find something in your library', { exact: true })).toBeVisible();
    await expect(page.locator('.result-card')).toHaveCount(0);
});

test('initial bookmark loading preserves existing searches before allowing a new save', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any;
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any) => {
            if (command === 'plugin:store|get' && args.key === 'savedSearches') {
                return new Promise(resolve => {
                    root.__finishBookmarkLoad = () => resolve([[{ name: 'Existing', query: 'old query', mode: 'text', limit: 20, savedAt: 1 }], true]);
                });
            }
            return original(command, args);
        };
    });
    await openSearch(page);
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('new query');
    const save = page.getByRole('button', { name: 'Save this search', exact: true });
    await expect(save).toBeDisabled();
    await page.evaluate(() => (window as any).__finishBookmarkLoad());
    await expect(save).toBeEnabled();
    await save.click();
    await expect(save).toBeEnabled();
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await expect(page.locator('.saved-dropdown .saved-name')).toHaveCount(2);
    await expect(page.locator('.saved-dropdown')).toContainText('Existing');
    await expect(page.locator('.saved-dropdown')).toContainText('new query');
});
