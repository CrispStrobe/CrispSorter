import { expect, test } from '@playwright/test';
import { fixtures } from './fixtures';

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached();
    await expect(page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first()).toBeVisible();
    await page.evaluate(async () => {
        const root = window as any;
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        root.__duplicateHashes = {
            '/fixtures/original.pdf': 'a'.repeat(64),
            '/fixtures/copy.pdf': 'a'.repeat(64),
            '/fixtures/unrelated.pdf': 'b'.repeat(64),
        };
        root.__duplicateConfirm = true;
        root.__duplicateDeleteError = false;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (['file_sha256', 'delete_files', 'plugin:dialog|message'].includes(command)) {
                root.__fixtureCalls.push({ command, args });
            }
            if (command === 'file_sha256') {
                const hash = root.__duplicateHashes[args.path];
                if (!hash) throw new Error('Permission denied');
                return hash;
            }
            if (command === 'delete_files') {
                return [root.__duplicateDeleteError
                    ? `Error deleting ${args.paths[0]}: Permission denied`
                    : `Deleted: ${args.paths[0]}`];
            }
            if (command === 'plugin:dialog|message') return root.__duplicateConfirm ? 'Permanently delete' : 'Cancel';
            return originalInvoke(command, args);
        };
        // Seed the same shared batch store the UI uses; all filesystem work
        // still crosses the explicitly mocked native IPC boundary above.
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        batchManager.items = ['original', 'copy', 'unrelated'].map(id => ({
            id, originalPath: `/fixtures/${id}.pdf`, originalName: `${id}.pdf`, extension: 'pdf',
            size: 1024, modifiedAt: 1, status: 'review', isAccepted: false,
        }));
        root.__duplicateBatchIds = () => batchManager.items.map((item: any) => item.id);
    });
    await page.getByRole('button', { name: 'Review', exact: true }).first().click();
});

test('duplicate review uses verified content and distinguishes same-size files', async ({ page }) => {
    await page.getByRole('button', { name: 'Duplicates', exact: true }).click();
    const panel = page.getByRole('region', { name: 'Identical files', exact: true });
    await expect(panel.locator('.dupe-group')).toHaveCount(1);
    await expect(panel.locator('.dupe-name')).toHaveText(['original.pdf', 'copy.pdf']);
    await expect(panel).toContainText('SHA-256');
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'delete_files'))).toHaveLength(0);
});

test('unreadable candidates are reported and cannot become false duplicates', async ({ page }) => {
    await page.evaluate(() => { (window as any).__duplicateHashes['/fixtures/copy.pdf'] = null; });
    await page.getByRole('button', { name: 'Duplicates', exact: true }).click();
    const panel = page.getByRole('region', { name: 'Identical files', exact: true });
    await expect(panel).toContainText('No identical files found');
    await panel.locator('summary').click();
    await expect(panel).toContainText('copy.pdf: Error: Permission denied');
    await expect(panel.getByRole('button', { name: 'Delete other copies', exact: true })).toHaveCount(0);
});

test('permanent duplicate deletion needs explicit confirmation and keeps the chosen original', async ({ page }) => {
    await page.getByRole('button', { name: 'Duplicates', exact: true }).click();
    const panel = page.getByRole('region', { name: 'Identical files', exact: true });
    await expect(panel.locator('.dupe-group')).toHaveCount(1);
    await page.evaluate(() => { (window as any).__duplicateConfirm = false; });
    await panel.getByRole('button', { name: 'Delete other copies', exact: true }).first().click();
    await expect(panel.getByRole('button', { name: 'Delete other copies', exact: true }).first()).toBeEnabled();
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'delete_files'))).toHaveLength(0);

    await page.evaluate(() => { (window as any).__duplicateConfirm = true; });
    await panel.getByRole('button', { name: 'Delete other copies', exact: true }).first().click();
    await expect(panel).toContainText('1 copy/copies deleted. The selected file was kept.');
    expect(await page.evaluate(() => (window as any).__duplicateBatchIds())).toEqual(['original', 'unrelated']);
    const calls = await page.evaluate(() => (window as any).__fixtureCalls);
    const deletion = calls.find((call: any) => call.command === 'delete_files');
    expect(deletion.args.paths).toEqual(['/fixtures/copy.pdf']);
    expect(calls.find((call: any) => call.command === 'plugin:dialog|message').args.message)
        .toContain('Keep this file:\n/fixtures/original.pdf');
});

test('failed native deletions retain batch rows and offer a retry', async ({ page }) => {
    await page.evaluate(() => { (window as any).__duplicateDeleteError = true; });
    await page.getByRole('button', { name: 'Duplicates', exact: true }).click();
    const panel = page.getByRole('region', { name: 'Identical files', exact: true });
    await expect(panel.locator('.dupe-group')).toHaveCount(1);
    await panel.getByRole('button', { name: 'Delete other copies', exact: true }).first().click();
    await expect(panel.getByRole('alert')).toContainText('Files that could not be deleted remain in the batch');
    await expect(panel.getByRole('button', { name: 'Check again', exact: true })).toBeEnabled();
    expect(await page.evaluate(() => (window as any).__duplicateBatchIds())).toEqual(['original', 'copy', 'unrelated']);
});

test('files changed since duplicate review are rechecked before any deletion', async ({ page }) => {
    await page.getByRole('button', { name: 'Duplicates', exact: true }).click();
    const panel = page.getByRole('region', { name: 'Identical files', exact: true });
    await expect(panel.locator('.dupe-group')).toHaveCount(1);
    await page.evaluate(() => { (window as any).__duplicateHashes['/fixtures/copy.pdf'] = 'c'.repeat(64); });
    await panel.getByRole('button', { name: 'Delete other copies', exact: true }).first().click();
    await expect(panel.getByRole('alert')).toContainText('File contents changed since verification');
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'delete_files'))).toHaveLength(0);
    expect(await page.evaluate(() => (window as any).__duplicateBatchIds())).toEqual(['original', 'copy', 'unrelated']);
});
