import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function openHistory(page: Page) {
    await page.keyboard.press('Control+k');
    const palette = page.getByRole('dialog', { name: 'Go to a tool or workflow' });
    const search = palette.getByRole('combobox');
    await search.fill('history');
    await search.press('Enter');
    await expect(page.getByRole('heading', { name: 'History', exact: true })).toBeVisible();
}

async function currentBatch(page: Page) {
    return page.evaluate(async () => {
        const managerPath = '/src/lib/batch/store.svelte.ts';
        const draftsPath = '/src/lib/batch/reviewDrafts.svelte.ts';
        const { batchManager } = await import(managerPath);
        const { reviewDrafts } = await import(draftsPath);
        return {
            ids: batchManager.items.map((item: any) => item.id),
            locked: batchManager.isUpdatingBatch,
            draftTitle: reviewDrafts.drafts.warranty?.title ?? null,
        };
    });
}

async function replacementCalls(page: Page) {
    return page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'batch_session_replace'));
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first()).toBeVisible({ timeout: 30000 });
    await expect.poll(() => page.evaluate(async () => {
        const path = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(path);
        return batchManager.sessionReady && !batchManager.isUpdatingBatch;
    })).toBe(true);
    await page.evaluate(() => {
        const root = window as any;
        const file = (id: string) => ({ id, originalPath: `/fixtures/${id}.txt`, originalName: `${id}.txt`,
            extension: 'txt', size: 42, modifiedAt: 1, status: 'queued', isAccepted: false });
        root.__historyImport = [{ ...file('imported'), extractedText: 'Complete imported text.' }];
        root.__historyLegacy = { legacy: { id: 'legacy', startTime: 1700000000000, status: 'paused',
            providerId: '', modelId: '', items: [{ ...file('legacy-file'), extractedText: 'Saved session text.' }] } };
        root.__historySaved = structuredClone(root.__historyLegacy);
        root.__historyReadFailures = 0;
        root.__historyReplaceFailures = 0;
        root.__historyHoldReplace = false;
        root.__historyTextFailure = false;
        root.__historyExports = [];
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}, options: any) => {
            if (command === 'plugin:store|get' && args.key === 'sessions') {
                root.__fixtureCalls.push({ command, args });
                if (root.__historyReadFailures-- > 0) throw new Error('Fixture history settings unavailable');
                return [structuredClone(root.__historySaved), true];
            }
            if (command === 'plugin:dialog|open') {
                root.__fixtureCalls.push({ command, args });
                return '/fixtures/batch.json';
            }
            if (command === 'plugin:fs|read_text_file') {
                root.__fixtureCalls.push({ command, args });
                return Array.from(new TextEncoder().encode(JSON.stringify(root.__historyImport)));
            }
            if (command === 'batch_session_replace') {
                root.__fixtureCalls.push({ command, args: structuredClone(args) });
                if (root.__historyHoldReplace) {
                    await new Promise<void>(resolve => { root.__historyRelease = () => { root.__historyHoldReplace = false; resolve(); }; });
                }
                if (root.__historyReplaceFailures-- > 0) throw new Error('Fixture database write failed');
                root.__historyPersisted = structuredClone(args.items);
                return;
            }
            if (command === 'batch_session_get_extracted_text') {
                root.__fixtureCalls.push({ command, args });
                if (root.__historyTextFailure) throw new Error('Fixture extracted text unavailable');
                return args.itemId === 'warranty' ? 'Full saved text for the warranty.' : null;
            }
            if (command === 'plugin:fs|write_text_file') {
                root.__fixtureCalls.push({ command });
                root.__historyExports.push(JSON.parse(new TextDecoder().decode(args)));
                return;
            }
            return originalInvoke(command, args, options);
        };
    });
});

test('confirmation, failed replacement and retry preserve the current batch and drafts until commit', async ({ page }) => {
    await page.evaluate(async () => {
        const managerPath = '/src/lib/batch/store.svelte.ts';
        const draftsPath = '/src/lib/batch/reviewDrafts.svelte.ts';
        const { batchManager } = await import(managerPath);
        const { reviewDrafts } = await import(draftsPath);
        await reviewDrafts.load();
        reviewDrafts.set(batchManager.items.find((item: any) => item.id === 'warranty'),
            { title: 'Unapplied warranty edit', author: 'Acme', year: '' });
        await reviewDrafts.flush();
    });
    await openHistory(page);
    const history = page.locator('.history-container');
    const importButton = history.getByRole('button', { name: 'Import', exact: true });
    await importButton.click();
    const dialog = page.getByRole('dialog', { name: 'Replace current batch?' });
    await expect(dialog).toContainText('Replace 2 files in the current batch with 1 file');
    await expect(dialog).toContainText('1 unapplied metadata draft');
    await expect(dialog).toContainText('batch.json');
    await expect(dialog.getByRole('button', { name: 'Cancel', exact: true })).toBeFocused();
    await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
    await expect(importButton).toBeFocused();
    expect(await replacementCalls(page)).toHaveLength(0);
    expect(await currentBatch(page)).toEqual({ ids: ['pending', 'warranty'], locked: false, draftTitle: 'Unapplied warranty edit' });

    await page.evaluate(() => { const root = window as any; root.__historyHoldReplace = true; root.__historyReplaceFailures = 1; });
    await importButton.click();
    await dialog.getByRole('button', { name: 'Replace batch', exact: true }).click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__historyRelease)).toBe('function');
    await expect(importButton).toBeDisabled();
    await expect(history.getByRole('button', { name: 'Export', exact: true })).toBeDisabled();
    expect(await currentBatch(page)).toEqual({ ids: ['pending', 'warranty'], locked: true, draftTitle: 'Unapplied warranty edit' });
    await page.evaluate(() => (window as any).__historyRelease());
    const error = history.getByRole('alert');
    await expect(error).toContainText('Your current batch and its metadata drafts are preserved');
    await expect(error).toContainText('Fixture database write failed');
    expect(await currentBatch(page)).toEqual({ ids: ['pending', 'warranty'], locked: false, draftTitle: 'Unapplied warranty edit' });
    await error.getByRole('button', { name: 'Retry', exact: true }).click();
    await expect(dialog).toContainText('Replace 2 files in the current batch with 1 file');
    await dialog.getByRole('button', { name: 'Replace batch', exact: true }).click();
    await expect(page.locator('.batch-container table.dense-table').getByRole('button', { name: 'imported.txt', exact: true })).toBeVisible();
    expect(await currentBatch(page)).toEqual({ ids: ['imported'], locked: false, draftTitle: null });
    const calls = await replacementCalls(page);
    expect(calls).toHaveLength(2);
    expect(calls[1].args.items[0]).toMatchObject({ item: { id: 'imported' }, extractedText: 'Complete imported text.' });
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'batch_session_clear'))).toHaveLength(0);
});

test('malformed imports fail before confirmation and an empty replacement still requires consent', async ({ page }) => {
    await page.evaluate(() => { (window as any).__historyImport[0].isAccepted = 'yes'; });
    await openHistory(page);
    const history = page.locator('.history-container');
    await history.getByRole('button', { name: 'Import', exact: true }).click();
    await expect(history.getByRole('alert')).toContainText('invalid isAccepted');
    await expect(page.getByRole('dialog', { name: 'Replace current batch?' })).not.toBeVisible();
    expect(await replacementCalls(page)).toHaveLength(0);
    expect((await currentBatch(page)).ids).toEqual(['pending', 'warranty']);
    await page.evaluate(() => { (window as any).__historyImport = []; });
    await history.getByRole('alert').getByRole('button', { name: 'Retry', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Replace current batch?' });
    await expect(dialog).toContainText('Replace 2 files in the current batch with 0 files');
    await page.keyboard.press('Escape');
    await expect(dialog).not.toBeVisible();
    expect(await replacementCalls(page)).toHaveLength(0);
    await expect.poll(() => currentBatch(page)).toEqual({ ids: ['pending', 'warranty'], locked: false, draftTitle: null });
});

test('history read failure and a missing saved session are recoverable without navigating or replacing files', async ({ page }) => {
    await page.evaluate(() => { (window as any).__historyReadFailures = 1; });
    await openHistory(page);
    const history = page.locator('.history-container');
    await expect(history.getByRole('alert')).toContainText('History could not be loaded');
    await expect(history.getByText('No history found.', { exact: true })).not.toBeVisible();
    await history.getByRole('alert').getByRole('button', { name: 'Retry', exact: true }).click();
    await expect(history.getByRole('article')).toHaveCount(1);
    await expect(history.getByRole('button', { name: /^Delete saved session:/ })).toBeEnabled();
    await page.evaluate(() => { (window as any).__historySaved = {}; });
    await history.getByRole('button', { name: 'Resume', exact: true }).click();
    await expect(history.getByRole('alert')).toContainText('no longer available');
    await expect(history.getByRole('heading', { name: 'History', exact: true })).toBeVisible();
    expect(await replacementCalls(page)).toHaveLength(0);
    expect((await currentBatch(page)).ids).toEqual(['pending', 'warranty']);
    await page.evaluate(() => { (window as any).__historySaved = structuredClone((window as any).__historyLegacy); });
    await history.getByRole('alert').getByRole('button', { name: 'Retry', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Replace current batch?' });
    await expect(dialog).toContainText('Replace 2 files in the current batch with 1 file');
    await dialog.getByRole('button', { name: 'Replace batch', exact: true }).click();
    await expect(page.locator('.batch-container table.dense-table').getByRole('button', { name: 'legacy-file.txt', exact: true })).toBeVisible();
    expect((await replacementCalls(page))[0].args.items[0].extractedText).toBe('Saved session text.');
});

test('export includes lazy extracted text and a failed text read does not write an incomplete file', async ({ page }) => {
    await openHistory(page);
    const history = page.locator('.history-container');
    await page.evaluate(() => { (window as any).__historyTextFailure = true; });
    await history.getByRole('button', { name: 'Export', exact: true }).click();
    await expect(history.getByRole('alert')).toContainText('The export did not finish');
    expect(await page.evaluate(() => (window as any).__historyExports)).toEqual([]);
    await page.evaluate(() => { (window as any).__historyTextFailure = false; });
    await history.getByRole('alert').getByRole('button', { name: 'Retry', exact: true }).click();
    await expect(history.getByRole('status')).toContainText('The batch was exported with its saved extracted text.');
    const exports = await page.evaluate(() => (window as any).__historyExports);
    expect(exports).toHaveLength(1);
    expect(exports[0].map((item: any) => item.id)).toEqual(['pending', 'warranty']);
    expect(exports[0].find((item: any) => item.id === 'warranty').extractedText).toBe('Full saved text for the warranty.');
    expect(await replacementCalls(page)).toHaveLength(0);
});

test('phone History controls and replacement confirmation fit and keep a predictable keyboard cycle', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await openHistory(page);
    const history = page.locator('.history-container');
    await expect(history.getByRole('article')).toHaveCount(1);
    await expect.poll(() => history.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
    for (const name of ['Import', 'Export', 'Refresh history', 'Resume']) {
        await expect(history.getByRole('button', { name, exact: true })).toBeInViewport({ ratio: 1 });
    }
    await history.getByRole('button', { name: 'Import', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Replace current batch?' });
    const cancel = dialog.getByRole('button', { name: 'Cancel', exact: true });
    const replace = dialog.getByRole('button', { name: 'Replace batch', exact: true });
    await expect(dialog).toBeInViewport({ ratio: 1 });
    await expect(cancel).toBeInViewport({ ratio: 1 });
    await expect(replace).toBeInViewport({ ratio: 1 });
    await expect(cancel).toBeFocused();
    await cancel.press('Shift+Tab');
    await expect(replace).toBeFocused();
    await replace.press('Tab');
    await expect(cancel).toBeFocused();
    await cancel.press('Escape');
    await expect(dialog).not.toBeVisible();
    await expect(history.getByRole('button', { name: 'Import', exact: true })).toBeFocused();
    expect(await replacementCalls(page)).toHaveLength(0);
});
