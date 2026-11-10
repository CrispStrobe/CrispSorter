// Browser interaction/IPC contracts; real text-index matching has native tests.
import { test, expect, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function openSearch(page: Page) {
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toBeVisible();
}
async function search(page: Page, query = 'warranty') {
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill(query);
    await page.locator('.search-btn').click();
    await expect(page.locator('.result-card')).toHaveCount(3);
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    await page.evaluate(() => {
        const root = window as any;
        root.__fixtureResults = [
            { doc_id: 'z', chunk_index: 2, score: .9, filename: 'Zeta.txt', title: 'Zeta report', year: 2020, location_uri: '/z.txt', source_hash: 'z'.repeat(64), snippet: 'warranty & <source>', tags: ['reviewed'] },
            { doc_id: 'a', chunk_index: 0, score: .5, filename: 'Alpha.txt', title: 'Alpha report', year: 2026, location_uri: '/a.txt', source_hash: 'a'.repeat(64), snippet: 'warranty annual report', tags: ['reviewed'] },
            { doc_id: 'u', chunk_index: 1, score: .4, filename: 'Unknown.txt', title: 'No year', location_uri: '/u.txt', snippet: 'Warranty reference', tags: ['unreviewed'] },
        ];
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any, options: any) => {
            if (command === 'index_vocabulary') {
                root.__vocabularyRequests = [...(root.__vocabularyRequests || []), args];
                return [{ term: 'annual', doc_freq: 3 }, { term: 'and', doc_freq: 8 }];
            }
            if (command === 'index_read_passage') {
                root.__passageRequests = [...(root.__passageRequests || []), args];
                return { text: `Passage for ${args.docId}. warranty first. Warranty second. <img src=x onerror=alert(1)>`, start_char: 200, end_char: 300, offset_unit: 'utf8_bytes', truncated: false };
            }
            if (command === 'plugin:fs|write_file') {
                root.__writtenReports = [...(root.__writtenReports || []), new TextDecoder().decode(new Uint8Array(args))];
                return;
            }
            return original(command, args, options);
        };
    });
    await openSearch(page);
});

test('sorts the returned window with unknown years last and a compact view', async ({ page }) => {
    await search(page);
    const cards = page.locator('.result-card');
    await expect(cards.first()).toHaveAttribute('data-document-id', 'z');
    await page.getByRole('combobox', { name: 'Sort results', exact: true }).selectOption('year-desc');
    await expect(cards.first()).toHaveAttribute('data-document-id', 'a');
    await expect(cards.last()).toHaveAttribute('data-document-id', 'u');
    await page.getByRole('combobox', { name: 'Sort results', exact: true }).selectOption('year-asc');
    await expect(cards.first()).toHaveAttribute('data-document-id', 'z');
    await expect(cards.last()).toHaveAttribute('data-document-id', 'u');
    await page.getByLabel('Compact results', { exact: true }).check();
    await expect(page.locator('.results-area')).toHaveClass(/compact-results/);
    await expect(page.locator('.search-window-note')).toContainText('returned window');
});

test('exports selected results with the executed query after the search box changes', async ({ page }) => {
    await search(page);
    await page.locator('.result-card[data-document-id="a"]').getByTitle('Select for batch sorter', { exact: true }).click();
    await page.getByRole('combobox', { name: 'Report format', exact: true }).selectOption('json');
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('edited but not searched');
    await page.getByRole('button', { name: 'Export selected (1)', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__writtenReports?.length || 0)).toBe(1);
    const report = await page.evaluate(() => JSON.parse((window as any).__writtenReports[0]));
    expect(report.recipe.query).toBe('warranty');
    expect(report.executedQuery).toBe('warranty');
    expect(report.scope).toBe('selected');
    expect(report.coverage.completeMatchSet).toBe(false);
    expect(report.documents.map((document: any) => document.docId)).toEqual(['a']);
    expect(report.documents[0].sourceHash).toBe('a'.repeat(64));
});

test('failed report writing keeps results and selection and offers a working retry', async ({ page }) => {
    await search(page);
    await page.getByRole('button', { name: 'Select displayed', exact: true }).click();
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        let fail = true;
        root.__TAURI_INTERNALS__.invoke = (command: string, ...args: any[]) => {
            if (command === 'plugin:fs|write_file' && fail) { fail = false; return Promise.reject(new Error('Fixture disk full')); }
            return original(command, ...args);
        };
    });
    await page.getByRole('button', { name: 'Export selected (3)', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('Fixture disk full');
    await expect(page.locator('.result-card')).toHaveCount(3);
    await expect(page.getByRole('button', { name: 'Export selected (3)', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Retry action', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__writtenReports?.length || 0)).toBe(1);
});

test('inspects stored passage text safely and navigates occurrences and documents', async ({ page }) => {
    await search(page);
    await page.locator('.result-card[data-document-id="z"]').getByTitle('Inspect indexed passage', { exact: true }).click();
    const passage = page.getByRole('region', { name: 'Indexed passage', exact: true });
    await expect(passage).toContainText('Passage for z');
    await expect(passage.locator('img')).toHaveCount(0);
    await expect(passage.locator('mark')).toHaveCount(2);
    await expect(passage.locator('mark.current')).toHaveText('warranty');
    await passage.getByRole('button', { name: 'Next occurrence', exact: true }).click();
    await expect(passage.locator('mark.current')).toHaveText('Warranty');
    await passage.getByRole('textbox', { name: 'Find in passage', exact: true }).fill('second');
    await expect(passage.locator('mark')).toHaveCount(1);
    await page.getByRole('button', { name: 'Next document', exact: true }).click();
    await expect(passage).toContainText('Passage for a');
    await expect(passage.getByRole('textbox', { name: 'Find in passage', exact: true })).toHaveValue('');
});

test('builds a field phrase query and inserts reserved indexed words as literal terms', async ({ page }) => {
    await page.getByText('Query help, builder, and indexed words', { exact: true }).click();
    const builder = page.getByRole('region', { name: 'Query builder', exact: true });
    await builder.getByLabel('Exact phrase', { exact: true }).fill('annual report');
    await builder.getByLabel('Without these words', { exact: true }).fill('draft');
    await builder.getByRole('combobox', { name: 'Text field', exact: true }).selectOption('title');
    await builder.getByRole('button', { name: 'Use this query', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toHaveValue('title:("annual report" AND NOT "draft")');
    const vocabulary = page.getByRole('region', { name: 'Indexed words', exact: true });
    await vocabulary.getByRole('textbox', { name: 'Word prefix', exact: true }).fill('an');
    await vocabulary.getByRole('button', { name: 'Browse words', exact: true }).click();
    await vocabulary.getByRole('button', { name: 'Add term and', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toHaveValue('(title:("annual report" AND NOT "draft")) AND "and"');
    expect(await page.evaluate(() => (window as any).__vocabularyRequests.at(-1))).toEqual({ prefix: 'an', limit: 50 });
});

test('changing the indexed-word prefix discards an older pending response', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, ...args: any[]) => command === 'index_vocabulary'
            ? new Promise(resolve => { root.__finishOldVocabulary = () => resolve([{ term: 'old', doc_freq: 1 }]); })
            : original(command, ...args);
    });
    await page.getByText('Query help, builder, and indexed words', { exact: true }).click();
    const vocabulary = page.getByRole('region', { name: 'Indexed words', exact: true });
    await vocabulary.getByRole('textbox', { name: 'Word prefix', exact: true }).fill('old');
    await vocabulary.getByRole('button', { name: 'Browse words', exact: true }).click();
    await vocabulary.getByRole('textbox', { name: 'Word prefix', exact: true }).fill('new');
    await page.evaluate(() => (window as any).__finishOldVocabulary());
    await expect(vocabulary.getByRole('button', { name: 'Add term old', exact: true })).toHaveCount(0);
});

test('phone results retain reachable report controls and indexed-passage close navigation', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await search(page);
    await page.getByRole('button', { name: 'Select displayed', exact: true }).click();
    const report = page.getByRole('button', { name: 'Export selected (3)', exact: true });
    await report.scrollIntoViewIfNeeded();
    const box = await report.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(390);
    const inspect = page.locator('.result-card[data-document-id="z"]').getByTitle('Inspect indexed passage', { exact: true });
    await inspect.click();
    await expect(page.getByRole('region', { name: 'Indexed passage' })).toContainText('Passage for z');
    await expect(page.getByRole('button', { name: 'Close preview', exact: true })).toBeFocused();
    await page.getByRole('button', { name: 'Close preview', exact: true }).click();
    await expect(inspect).toBeFocused();
});
