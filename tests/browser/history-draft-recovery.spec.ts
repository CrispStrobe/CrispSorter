import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function batchState(page: Page) {
    return page.evaluate(async () => {
        const managerPath = '/src/lib/batch/store.svelte.ts';
        const draftsPath = '/src/lib/batch/reviewDrafts.svelte.ts';
        const { batchManager } = await import(managerPath);
        const { reviewDrafts } = await import(draftsPath);
        return {
            ids: batchManager.items.map((item: any) => item.id),
            ready: batchManager.sessionReady,
            locked: batchManager.isUpdatingBatch,
            draftsReady: reviewDrafts.ready,
            draftTitle: reviewDrafts.drafts.warranty?.title ?? null,
        };
    });
}

test('replacement waits for unread metadata drafts and Retry confirms their actual count', async ({ page }) => {
    await fixtures(page);
    await page.addInitScript(() => {
        const root = window as any;
        root.__historyDraftFaults = { failRead: true, holdRead: false };
        root.__historyDraftReads = 0;
        root.__historyDraftWrites = [];
        root.__historyDraftReplacements = [];
        root.__historySavedDrafts = {
            warranty: {
                originalPath: '/fixtures/warranty.txt',
                title: 'Unapplied warranty correction', author: 'Acme', year: '',
                base: { title: 'Warranty', author: 'Acme', year: '' },
            },
        };
        const imported = [{ id: 'incoming', originalPath: '/fixtures/incoming.txt', originalName: 'incoming.txt',
            extension: 'txt', size: 42, modifiedAt: 1, status: 'queued', isAccepted: false }];
        const wrapped = new WeakSet<object>();
        const wrap = (bridge: any) => {
            if (!bridge || wrapped.has(bridge)) return bridge;
            wrapped.add(bridge);
            const originalInvoke = bridge.invoke;
            bridge.invoke = async (command: string, args: any = {}) => {
                if (command === 'plugin:store|get' && args.key === 'batchMetadataDrafts') {
                    root.__fixtureCalls.push({ command, args });
                    root.__historyDraftReads++;
                    if (root.__historyDraftFaults.failRead) throw new Error('Fixture saved metadata drafts are unreadable');
                    if (root.__historyDraftFaults.holdRead) {
                        await new Promise<void>(resolve => { root.__releaseHistoryDraftRead = resolve; });
                    }
                    return [structuredClone(root.__historySavedDrafts), true];
                }
                if (command === 'plugin:store|set' && args.key === 'batchMetadataDrafts') {
                    root.__historyDraftWrites.push(structuredClone(args.value));
                    root.__historySavedDrafts = structuredClone(args.value);
                }
                if (command === 'plugin:dialog|open') {
                    root.__fixtureCalls.push({ command, args });
                    return '/fixtures/incoming-batch.json';
                }
                if (command === 'plugin:fs|read_text_file') {
                    root.__fixtureCalls.push({ command, args });
                    return Array.from(new TextEncoder().encode(JSON.stringify(imported)));
                }
                if (command === 'batch_session_replace') {
                    root.__fixtureCalls.push({ command, args });
                    root.__historyDraftReplacements.push(structuredClone(args));
                    throw new Error('Unexpected replacement before explicit confirmation');
                }
                return originalInvoke(command, args);
            };
            return bridge;
        };
        // Init scripts may run in either order. Wrap the fixture before app
        // startup so this exercises a real failed load, not mutated readiness.
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', {
            configurable: true,
            get: () => bridge,
            set: value => { bridge = wrap(value); },
        });
    });
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    const unreadState = { ids: ['pending', 'warranty'], ready: true, locked: false, draftsReady: false, draftTitle: null };
    await expect.poll(() => batchState(page)).toEqual(unreadState);
    await expect(page.locator('.draft-storage-error')).toContainText('Saved metadata drafts could not be loaded.');
    const startupReads = await page.evaluate(() => (window as any).__historyDraftReads);

    await page.keyboard.press('Control+k');
    const palette = page.getByRole('dialog', { name: 'Go to a tool or workflow' });
    await palette.getByRole('combobox').fill('history');
    await palette.getByRole('combobox').press('Enter');
    const history = page.locator('.history-container');
    await expect(history.getByRole('heading', { name: 'History', exact: true })).toBeVisible();
    const importButton = history.getByRole('button', { name: 'Import', exact: true });
    await importButton.click();
    const error = history.getByRole('alert');
    await expect(error).toContainText('Saved metadata drafts could not be loaded');
    await expect(error).toContainText('Your current batch and its metadata drafts are preserved');
    const dialog = page.getByRole('dialog', { name: 'Replace current batch?' });
    await expect(dialog).not.toBeVisible();
    expect(await batchState(page)).toEqual(unreadState);
    expect(await page.evaluate(() => (window as any).__historyDraftReads)).toBeGreaterThan(startupReads);
    expect(await page.evaluate(() => (window as any).__historyDraftReplacements)).toEqual([]);
    expect(await page.evaluate(() => (window as any).__historyDraftWrites)).toEqual([]);

    await page.evaluate(() => {
        const fault = (window as any).__historyDraftFaults;
        fault.failRead = false;
        fault.holdRead = true;
    });
    const retry = error.getByRole('button', { name: 'Retry', exact: true });
    await retry.click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseHistoryDraftRead)).toBe('function');
    await expect(importButton).toBeDisabled();
    await expect(history.getByRole('button', { name: 'Export', exact: true })).toBeDisabled();
    await expect(dialog).not.toBeVisible();
    expect(await batchState(page)).toEqual({ ...unreadState, locked: true });
    expect(await page.evaluate(() => (window as any).__historyDraftReplacements)).toEqual([]);

    await page.evaluate(() => {
        const root = window as any;
        root.__historyDraftFaults.holdRead = false;
        root.__releaseHistoryDraftRead();
    });
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText('Replace 2 files in the current batch with 1 file');
    await expect(dialog).toContainText('1 unapplied metadata draft');
    await expect(dialog).toContainText('incoming-batch.json');
    await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
    await expect(dialog).not.toBeVisible();
    await expect(importButton).toBeEnabled();
    await expect.poll(() => batchState(page)).toEqual({
        ...unreadState, draftsReady: true, draftTitle: 'Unapplied warranty correction',
    });
    expect(await page.evaluate(() => (window as any).__historyDraftReplacements)).toEqual([]);
    expect(await page.evaluate(() => (window as any).__historyDraftWrites)).toEqual([]);
    expect(await page.evaluate(() => (window as any).__historySavedDrafts.warranty.title)).toBe('Unapplied warranty correction');
});
