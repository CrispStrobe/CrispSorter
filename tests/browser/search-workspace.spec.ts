// UI state and IPC contracts. Retrieval correctness is covered by native suites.
import { test, expect, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function search(page: Page, query = 'warranty') {
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill(query);
    await page.locator('.search-btn').click();
    await expect(page.locator('.result-card')).toHaveCount(3);
}

async function lastSearch(page: Page) {
    return page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search').at(-1)?.args);
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    await page.evaluate(() => {
        const root = window as any;
        root.__fixtureResults = [
            { doc_id: 'z', chunk_index: 2, score: .9, title: 'Zeta report', author: 'A. Researcher', year: 2020, filename: 'Zeta.txt', location_uri: '/research/zeta.txt', source_hash: 'a'.repeat(64), snippet: 'warranty reviewed source', tags: ['reviewed'] },
            { doc_id: 'a', chunk_index: 0, score: .7, title: 'Alpha report', year: 2026, filename: 'Alpha.txt', location_uri: '/research/alpha.txt', snippet: 'warranty annual source', tags: ['reviewed'] },
            { doc_id: 'u', chunk_index: 1, score: .5, title: 'Unknown report', filename: 'Unknown.txt', location_uri: '/archive/unknown.txt', snippet: 'warranty reference', tags: ['unreviewed'] },
        ];
        Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
            writeText: async (text: string) => { root.__copiedText = text; },
        } });
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'index_read_passage') return Promise.resolve({ text: `Stored warranty passage for ${args.docId}.`, start_char: 0, end_char: 40, offset_unit: 'utf8_bytes', truncated: false });
            return original(command, args, options);
        };
    });
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toBeVisible();
});

test('bookmarks restore sort, compact layout, report format, display tags and preview preference', async ({ page }) => {
    await search(page);
    await page.getByRole('combobox', { name: 'Sort results', exact: true }).selectOption('year-desc');
    await page.getByLabel('Compact results', { exact: true }).check();
    await page.getByRole('combobox', { name: 'Report format', exact: true }).selectOption('json');
    await page.getByRole('button', { name: 'Tags in returned results', exact: true }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^reviewed/ }).click();
    await page.locator('.result-card[data-document-id="a"]').getByTitle('Inspect indexed passage', { exact: true }).click();
    await page.getByRole('button', { name: 'Close preview', exact: true }).click();
    await page.getByRole('button', { name: 'Save this search', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Save this search', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Clear display filters', exact: true }).click();
    await page.getByRole('combobox', { name: 'Sort results', exact: true }).selectOption('filename');
    await page.getByLabel('Compact results', { exact: true }).uncheck();
    await page.getByRole('combobox', { name: 'Report format', exact: true }).selectOption('csv');
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await page.locator('.saved-dropdown .saved-name').first().click();
    await expect(page.locator('.result-card')).toHaveCount(2);
    await expect(page.locator('.result-card').first()).toHaveAttribute('data-document-id', 'a');
    await expect(page.getByLabel('Compact results', { exact: true })).toBeChecked();
    await expect(page.getByRole('combobox', { name: 'Report format', exact: true })).toHaveValue('json');
    await expect(page.getByRole('combobox', { name: 'Sort results', exact: true })).toHaveValue('year-desc');
    await expect(page.getByRole('button', { name: 'Remove displayed tag reviewed', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Preview Alpha report', exact: true }).click();
    await expect(page.getByRole('region', { name: 'Indexed passage', exact: true })).toContainText('Stored warranty passage for a.');
    expect(await page.evaluate(async () => (await (window as any).__TAURI_INTERNALS__.invoke('plugin:store|get', { key: 'savedSearches' }))[0][0].view))
        .toEqual({ sort: 'year-desc', compact: true, reportFormat: 'json', preview: 'indexed', displayTags: ['reviewed'], tagCloud: true });
});

test('keyboard traverses results and preview and restores focus to the current document', async ({ page }) => {
    await search(page);
    const input = page.getByRole('textbox', { name: 'Search indexed documents' });
    await input.focus();
    await input.press('ArrowDown');
    await expect(page.getByRole('button', { name: 'Preview Zeta report', exact: true })).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(page.getByRole('button', { name: 'Preview Alpha report', exact: true })).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(page.getByRole('button', { name: 'Close preview', exact: true })).toBeFocused();
    await page.keyboard.press('Alt+ArrowDown');
    await expect(page.locator('.preview-title')).toHaveText('Unknown report');
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: 'Preview Unknown report', exact: true })).toBeFocused();
    await expect(page.locator('.preview-pane')).toHaveCount(0);
    await page.keyboard.press('Home');
    await expect(page.getByRole('button', { name: 'Preview Zeta report', exact: true })).toBeFocused();
    await page.keyboard.press('End');
    await expect(page.getByRole('button', { name: 'Preview Unknown report', exact: true })).toBeFocused();
    await page.keyboard.press('Control+k');
    await expect(input).toBeFocused();
});

test('refinement includes only displayed documents and leaves a removable explicit scope', async ({ page }) => {
    await search(page);
    await page.getByRole('button', { name: 'Tags in returned results', exact: true }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^reviewed/ }).click();
    await expect(page.locator('.result-card')).toHaveCount(2);
    await page.getByRole('button', { name: 'Search within displayed documents', exact: true }).click();
    const input = page.getByRole('textbox', { name: 'Search indexed documents' });
    await expect(input).toBeFocused();
    await expect(input).toHaveValue('');
    await expect(page.getByRole('button', { name: 'Remove document scope', exact: true })).toContainText('2');
    await input.fill('renewal');
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'renewal', docIdScope: ['a', 'z'] });
    await page.getByRole('button', { name: 'Remove document scope', exact: true }).click();
    await expect(page.getByText('Search options changed. Search again to update the results.', { exact: true })).toBeVisible();
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'renewal', docIdScope: null });
});

test('display filter chips explain an empty returned window and can recover individual tags', async ({ page }) => {
    await search(page);
    await page.getByRole('button', { name: 'Tags in returned results', exact: true }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^reviewed/ }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^unreviewed/ }).click();
    await expect(page.locator('.result-card')).toHaveCount(0);
    await expect(page.getByText('No document in this returned window has all selected tags.', { exact: true })).toBeVisible();
    await expect(page.locator('.search-window-note')).toContainText('this returned window');
    await page.getByRole('button', { name: 'Remove displayed tag reviewed', exact: true }).click();
    await expect(page.locator('.result-card')).toHaveCount(1);
    await expect(page.locator('.result-card')).toHaveAttribute('data-document-id', 'u');
    await expect(page.getByText('Search options changed. Search again to update the results.', { exact: true })).toHaveCount(0);
});

test('citation and source path retain indexed provenance and the executed query', async ({ page }) => {
    await search(page);
    const card = page.locator('.result-card[data-document-id="z"]');
    await expect(card.locator('.result-path')).toHaveText('/research/zeta.txt');
    await expect(card.locator('.result-origin')).toHaveText('Indexed passage 3');
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('unexecuted edit');
    await card.getByTitle('Copy citation', { exact: true }).click();
    const citation = await page.evaluate(() => (window as any).__copiedText);
    expect(citation).toContain('Zeta report\nA. Researcher · 2020');
    expect(citation).toContain('Source: /research/zeta.txt');
    expect(citation).toContain('Document: z\nIndexed passage: 3');
    expect(citation).toContain(`Source SHA-256: ${'a'.repeat(64)}`);
    expect(citation).toContain('Search: warranty');
    expect(citation).not.toContain('unexecuted edit');
    await card.getByTitle('Copy path', { exact: true }).click();
    expect(await page.evaluate(() => (window as any).__copiedText)).toBe('/research/zeta.txt');
});

test('a new or cleared search closes an obsolete desktop preview', async ({ page }) => {
    await search(page);
    await page.locator('.result-card[data-document-id="z"]').getByTitle('Inspect indexed passage', { exact: true }).click();
    await expect(page.locator('.preview-pane')).toBeVisible();
    await search(page, 'renewal');
    await expect(page.locator('.preview-pane')).toHaveCount(0);
    await page.locator('.result-card[data-document-id="a"]').getByTitle('Inspect indexed passage', { exact: true }).click();
    await page.getByRole('button', { name: 'Clear search', exact: true }).click();
    await expect(page.locator('.preview-pane')).toHaveCount(0);
    await expect(page.locator('.result-card')).toHaveCount(0);
});

test('loading a larger returned window keeps current presentation and exact request options', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any;
        const template = root.__fixtureResults[0];
        root.__fixtureResults = Array.from({ length: 20 }, (_, i) => ({ ...template, doc_id: `doc-${i}`, title: `Report ${i}`, filename: `report-${i}.txt` }));
    });
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.locator('.search-btn').click();
    await expect(page.locator('.result-card')).toHaveCount(20);
    await page.getByRole('combobox', { name: 'Sort results', exact: true }).selectOption('filename');
    await page.getByLabel('Compact results', { exact: true }).check();
    const more = page.getByRole('button', { name: 'Load more results', exact: true });
    await expect(more).toBeEnabled();
    await more.click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'warranty', limit: 50 });
    await expect(page.getByRole('combobox', { name: 'Sort results', exact: true })).toHaveValue('filename');
    await expect(page.getByLabel('Compact results', { exact: true })).toBeChecked();
    await expect(page.locator('.search-window-note')).toContainText('Requested limit 50');
});
