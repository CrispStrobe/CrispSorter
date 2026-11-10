// Interaction contracts for bounded stored text; native tests qualify actual data.
import { test, expect } from '@playwright/test';
import { fixtures } from './fixtures';

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    await page.evaluate(() => {
        const root = window as any;
        root.__fixtureResults = [{ doc_id: 'book', chunk_index: 2, score: 1, filename: 'Book.txt', title: 'Book',
            location_uri: '/library/Book.txt', source_hash: 'old-fingerprint', snippet: 'war warranty War WAR' }];
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any, options: any) => {
            if (command === 'index_read_passage') {
                root.__inspectedPassages = [...(root.__inspectedPassages || []), args.chunkIndex];
                return { text: args.chunkIndex === 2 ? 'war warranty War WAR <source>' : 'Second stored passage: war again.',
                    start_char: 200, end_char: 300, offset_unit: 'utf8_bytes', truncated: false,
                    chunk_index: args.chunkIndex, passage_number: args.chunkIndex === 2 ? 1 : 2, chunk_count: 2,
                    previous_chunk_index: args.chunkIndex === 2 ? null : 2, next_chunk_index: args.chunkIndex === 2 ? 9 : null,
                    source_hash: 'current-fingerprint', indexed_at: 1000 };
            }
            return original(command, args, options);
        };
    });
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
});

test('builds a literal ordered proximity query and keeps incomplete conditions visible', async ({ page }) => {
    await page.getByText('Query help, builder, and indexed words', { exact: true }).click();
    const builder = page.getByRole('region', { name: 'Query builder' });
    await builder.getByLabel('First word or phrase', { exact: true }).fill('net income');
    await expect(builder.getByRole('button', { name: 'Use this query' })).toBeDisabled();
    await expect(builder.getByRole('alert')).toContainText('Enter both');
    await builder.getByLabel('Second word or phrase', { exact: true }).fill('growth');
    await builder.getByLabel('Maximum intervening words', { exact: true }).fill('0');
    await builder.getByLabel('In this order', { exact: true }).check();
    await builder.getByLabel('Text field', { exact: true }).selectOption('body');
    await builder.getByRole('button', { name: 'Use this query' }).click();
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toHaveValue('body:(("net income" pre/0 "growth"))');
});

test('navigates all stored chunks, keeps find options and reports refreshed source evidence', async ({ page }, testInfo) => {
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('war');
    await page.locator('.search-btn').click();
    await page.getByTitle('Inspect indexed passage', { exact: true }).click();
    const passage = page.getByRole('region', { name: 'Indexed passage', exact: true });
    await expect(passage).toContainText('Passage 1 / 2');
    await expect(passage).toContainText('The index changed after this search.');
    await expect(passage.locator('[title="current-fingerprint"]')).toBeVisible();
    await passage.getByRole('textbox', { name: 'Find in passage' }).fill('war');
    await expect(passage.locator('mark')).toHaveCount(4);
    await passage.getByLabel('Whole words', { exact: true }).check();
    await expect(passage.locator('mark')).toHaveCount(3);
    await passage.getByLabel('Match case', { exact: true }).check();
    await expect(passage.locator('mark')).toHaveCount(1);
    const next = passage.getByRole('button', { name: 'Next stored passage', exact: true });
    await next.focus();
    await next.press('Enter');
    await expect(passage).toContainText('Passage 2 / 2');
    await expect(passage).toContainText('Second stored passage');
    await expect(passage.getByRole('textbox', { name: 'Find in passage' })).toHaveValue('war');
    await expect(passage.getByLabel('Match case', { exact: true })).toBeChecked();
    await expect(passage.getByRole('button', { name: 'Previous stored passage', exact: true })).toBeFocused();
    expect(await page.evaluate(() => (window as any).__inspectedPassages)).toEqual([2, 9]);
    await page.screenshot({ path: testInfo.outputPath('passage-navigation.png'), fullPage: true });
});

test('does not imply a truncated occurrence count when exactly one thousand matches exist', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any, options: any) => command === 'index_read_passage'
            ? { text: 'war '.repeat(1000), start_char: 0, end_char: 4000, offset_unit: 'utf8_bytes', truncated: false }
            : original(command, args, options);
    });
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('war');
    await page.locator('.search-btn').click();
    await page.getByTitle('Inspect indexed passage', { exact: true }).click();
    const navigation = page.getByRole('group', { name: 'Text occurrence navigation', exact: true });
    await expect(navigation).toContainText('1 / 1000');
    await expect(navigation).not.toContainText('1000+');
});
