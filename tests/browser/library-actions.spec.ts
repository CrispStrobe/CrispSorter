// Result-action feedback and retry behavior use explicit IPC/clipboard fixtures.
import { test, expect, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function search(page: Page) {
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.locator('.search-btn').click();
    await expect(page.locator('.result-card').first()).toBeVisible();
}

async function actionCalls(page: Page, command: string) {
    return page.evaluate(command => (window as any).__libraryActionCalls.filter((call: any) => call.command === command), command);
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    // Keep these failure controls local to this suite instead of broadening the
    // shared fixture's behavior for unrelated workflows.
    await page.evaluate(() => {
        const root = window as any;
        root.__libraryActionCalls = [];
        root.__libraryActionFailures = {};
        root.__libraryActionGates = {};
        root.__libraryActionResolvers = {};
        root.__libraryClipboardWrites = [];
        root.__libraryClipboardAttempts = 0;
        root.__libraryClipboardFailures = 0;
        Object.defineProperty(navigator, 'clipboard', {
            configurable: true,
            value: {
                writeText: async (text: string) => {
                    root.__libraryClipboardAttempts += 1;
                    if (root.__libraryClipboardFailures > 0) {
                        root.__libraryClipboardFailures -= 1;
                        throw new Error('Fixture clipboard unavailable');
                    }
                    root.__libraryClipboardWrites.push(text);
                },
            },
        });
        const original = root.__TAURI_INTERNALS__.invoke;
        const commands = ['export_to_html', 'highlight_add', 'drive_capabilities', 'drive_share_link',
            'batch_session_upsert_item', 'batch_session_upsert_items_bulk'];
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
            if (commands.includes(command)) {
                root.__libraryActionCalls.push({ command, args });
                if (root.__libraryActionGates[command]) {
                    await new Promise<void>(resolve => {
                        root.__libraryActionResolvers[command] = () => {
                            delete root.__libraryActionGates[command];
                            delete root.__libraryActionResolvers[command];
                            resolve();
                        };
                    });
                }
                if (root.__libraryActionFailures[command] > 0) {
                    root.__libraryActionFailures[command] -= 1;
                    throw new Error(`Fixture ${command} unavailable`);
                }
                if (command === 'drive_capabilities') return { read: true, share_links: args.driveId !== 'private' };
                if (command === 'drive_share_link') return 'https://example.invalid/shared/warranty';
            }
            return original(command, args);
        };
    });
});

test('copy failure and action retry preserve results, preview and selected documents', async ({ page }) => {
    await search(page);
    const card = page.locator('.result-card');
    await card.getByTitle('Select for batch sorter', { exact: true }).click();
    await card.getByTitle('Vorschau (Preview)', { exact: true }).click();
    await page.evaluate(() => { (window as any).__libraryClipboardFailures = 1; });
    await card.getByTitle('Copy path', { exact: true }).click();

    const feedback = page.locator('.result-action-feedback');
    await expect(feedback).toHaveAttribute('role', 'alert');
    await expect(feedback).toContainText('Could not copy path: Fixture clipboard unavailable');
    await expect(card).toBeVisible();
    await expect(page.locator('.preview-title')).toContainText('Warranty.txt');
    await expect(card.getByTitle('Remove from batch selection', { exact: true })).toBeVisible();
    await expect(page.locator('.state-msg.error')).toHaveCount(0);

    await feedback.getByRole('button', { name: 'Retry action', exact: true }).click();
    await expect(feedback).toHaveAttribute('role', 'status');
    await expect(feedback).toContainText('Path copied to clipboard: /fixtures/warranty.txt');
    await expect(card).toBeVisible();
    await expect(page.locator('.preview-title')).toContainText('Warranty.txt');
    await expect(card.getByTitle('Remove from batch selection', { exact: true })).toBeVisible();
    expect(await page.evaluate(() => (window as any).__libraryClipboardWrites)).toEqual(['/fixtures/warranty.txt']);
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search').length)).toBe(1);

    await feedback.getByRole('button', { name: 'Dismiss notification', exact: true }).click();
    await expect(feedback).toHaveCount(0);
    await expect(card).toBeVisible();
});

test('adding selected documents waits for persistence and retries without counting chunks as files', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any;
        const base = root.__fixtureResults[0];
        const local = { ...base, doc_id: 'new', filename: 'New.txt', location_uri: '/fixtures/new.txt', chunk_index: 0 };
        root.__fixtureResults = [local, { ...local, chunk_index: 1, score: 0.7 },
            { ...base, doc_id: 'cloud', filename: 'Cloud.txt', location_uri: 'crisp+drive://shared/Cloud.txt' }];
        root.__libraryActionFailures.batch_session_upsert_items_bulk = 1;
        root.__libraryActionGates.batch_session_upsert_items_bulk = true;
    });
    await search(page);
    const localCard = page.locator('.result-card').filter({ hasText: 'New.txt' });
    const cloudCard = page.locator('.result-card').filter({ hasText: 'Cloud.txt' });
    await localCard.getByTitle('Select for batch sorter', { exact: true }).click();
    await cloudCard.getByTitle('Select for batch sorter', { exact: true }).click();
    const addSelected = page.getByTitle('Add selected local results to batch sorter', { exact: true });
    await addSelected.click();
    const feedback = page.locator('.result-action-feedback');
    await page.waitForFunction(() => typeof (window as any).__libraryActionResolvers.batch_session_upsert_items_bulk === 'function');
    await expect(addSelected).toBeDisabled();
    await expect(feedback).toHaveCount(0);
    await expect(page.locator('.result-card')).toHaveCount(2);
    await page.evaluate(() => (window as any).__libraryActionResolvers.batch_session_upsert_items_bulk());
    await expect(feedback).toHaveAttribute('role', 'alert');
    await expect(feedback).toContainText('Fixture batch_session_upsert_items_bulk unavailable');
    await expect(page.locator('.result-card')).toHaveCount(2);
    await expect(page.getByTitle('Remove from batch selection', { exact: true })).toHaveCount(2);
    await expect(page.locator('.state-msg.error')).toHaveCount(0);

    await feedback.getByRole('button', { name: 'Retry action', exact: true }).click();
    await expect(feedback).toHaveAttribute('role', 'status');
    await expect(feedback).toContainText('Added to the sorting inbox: 1');
    await expect(feedback).toContainText('Already in the sorting inbox: 0');
    await expect(feedback).toContainText('Cloud documents skipped: 1');
    const calls = await actionCalls(page, 'batch_session_upsert_items_bulk');
    expect(calls).toHaveLength(2);
    for (const call of calls) expect(call.args.items.map((item: any) => item.originalPath)).toEqual(['/fixtures/new.txt']);
    await expect(page.getByTitle('Remove from batch selection', { exact: true })).toHaveCount(2);

    // Repeating the action is truthful about already-present documents.
    await page.getByTitle('Add selected local results to batch sorter', { exact: true }).click();
    await expect(feedback).toContainText('Added to the sorting inbox: 0');
    await expect(feedback).toContainText('Already in the sorting inbox: 1');
    expect(await actionCalls(page, 'batch_session_upsert_items_bulk')).toHaveLength(2);
});

test('excerpt export and reading-list failures have visible action recovery', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any;
        root.__libraryActionFailures.export_to_html = 1;
        root.__libraryActionFailures.highlight_add = 1;
    });
    await search(page);
    const card = page.locator('.result-card');
    const feedback = page.locator('.result-action-feedback');
    await card.getByTitle('Export search excerpt as HTML', { exact: true }).click();
    await expect(feedback).toHaveAttribute('role', 'alert');
    await expect(feedback).toContainText('Could not export search excerpt');
    await expect(card).toBeVisible();
    await feedback.getByRole('button', { name: 'Retry action', exact: true }).click();
    await expect(feedback).toHaveAttribute('role', 'status');
    await expect(feedback).toContainText('Search excerpt exported: /fixtures/document-fields.csv');
    expect(await actionCalls(page, 'export_to_html')).toHaveLength(2);

    await card.getByTitle('Add to reading list', { exact: true }).click();
    await expect(feedback).toHaveAttribute('role', 'alert');
    await expect(feedback).toContainText('Could not add excerpt to the reading list');
    await feedback.getByRole('button', { name: 'Retry action', exact: true }).click();
    await expect(feedback).toHaveAttribute('role', 'status');
    await expect(feedback).toContainText('Excerpt added to the reading list: Warranty.txt');
    await expect(card).toBeVisible();
    expect(await actionCalls(page, 'highlight_add')).toHaveLength(2);
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search').length)).toBe(1);
});

test('share clipboard retry reuses the provider link and respects unsupported sharing', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any;
        const base = root.__fixtureResults[0];
        root.__fixtureResults = [
            { ...base, doc_id: 'shared', filename: 'Shared.txt', location_uri: 'crisp+drive://shared/Shared.txt' },
            { ...base, doc_id: 'private', filename: 'Private.txt', location_uri: 'crisp+drive://private/Private.txt' },
        ];
        root.__libraryClipboardFailures = 1;
    });
    await search(page);
    const sharedCard = page.locator('.result-card').filter({ hasText: 'Shared.txt' });
    const privateCard = page.locator('.result-card').filter({ hasText: 'Private.txt' });
    const feedback = page.locator('.result-action-feedback');
    await sharedCard.getByTitle('Share link kopieren', { exact: true }).click();
    await expect(feedback).toContainText('Fixture clipboard unavailable');
    await expect(page.locator('.result-card')).toHaveCount(2);
    await feedback.getByRole('button', { name: 'Retry action', exact: true }).click();
    await expect(feedback).toHaveAttribute('role', 'status');
    await expect(feedback).toContainText('Share link copied to clipboard.');
    expect(await actionCalls(page, 'drive_share_link')).toHaveLength(1);
    expect(await page.evaluate(() => (window as any).__libraryClipboardWrites)).toEqual(['https://example.invalid/shared/warranty']);

    await privateCard.getByTitle('Share link kopieren', { exact: true }).click();
    await expect(feedback).toHaveAttribute('role', 'alert');
    await expect(feedback).toContainText('does not advertise public share links');
    await expect(feedback.getByRole('button', { name: 'Retry action', exact: true })).toHaveCount(0);
    await expect(privateCard.getByTitle('Public sharing is not supported', { exact: true })).toBeDisabled();
    expect(await actionCalls(page, 'drive_share_link')).toHaveLength(1);
    await expect(page.locator('.result-card')).toHaveCount(2);
});
