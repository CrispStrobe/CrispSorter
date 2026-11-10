import { expect, test } from '@playwright/test';
import { fixtures } from './fixtures';

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    await expect.poll(() => page.evaluate(async () => {
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        return batchManager.sessionReady;
    })).toBe(true);
});

test('concurrent additions wait for storage, deduplicate, and keep the batch locked until committed', async ({ page }) => {
    await page.evaluate(async () => {
        const root = window as any;
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        batchManager.items = [];
        root.__storageWrites = [];
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'batch_session_upsert_items_bulk') {
                root.__storageWrites.push(args.items);
                return new Promise<void>(resolve => { root.__releaseStorage = resolve; });
            }
            return originalInvoke(command, args);
        };
        root.__storageFinished = Promise.all([
            batchManager.addItem('/fixtures/new.pdf', 'new.pdf', 10),
            batchManager.addItem('/fixtures/new.pdf', 'new.pdf', 10),
        ]).then(values => { root.__storageResults = values; });
    });
    await expect.poll(() => page.evaluate(() => (window as any).__storageWrites.length)).toBe(1);
    expect(await page.evaluate(async () => {
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        await batchManager.clear();
        return { count: batchManager.items.length, busy: batchManager.isUpdatingBatch };
    })).toEqual({ count: 0, busy: true });
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'batch_session_clear'))).toHaveLength(0);

    await page.evaluate(async () => {
        const root = window as any;
        root.__releaseStorage();
        await root.__storageFinished;
    });
    expect(await page.evaluate(() => (window as any).__storageResults)).toEqual([true, false]);
    expect(await page.evaluate(async () => {
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        return { paths: batchManager.items.map((item: any) => item.originalPath), busy: batchManager.isUpdatingBatch };
    })).toEqual({ paths: ['/fixtures/new.pdf'], busy: false });
    expect(await page.evaluate(() => (window as any).__storageWrites.length)).toBe(1);
});

test('a partial large import retains committed files and retries only the missing files', async ({ page }) => {
    const result = await page.evaluate(async () => {
        const root = window as any;
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        batchManager.items = [];
        const persisted = new Map<string, string>();
        let writes = 0;
        let failSecondWrite = true;
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'batch_session_upsert_items_bulk') {
                writes++;
                if (writes === 2 && failSecondWrite) throw new Error('Fixture storage is full');
                for (const item of args.items) persisted.set(item.id, item.originalPath);
                return;
            }
            return originalInvoke(command, args);
        };
        const entries = Array.from({ length: 501 }, (_, index) => ({
            path: `/fixtures/import-${index}.pdf`, name: `import-${index}.pdf`, size: 10,
        }));
        let failure: { message: string; addedCount: number } | null = null;
        try { await batchManager.addItemsBulk(entries); }
        catch (error: any) { failure = { message: error.message, addedCount: error.addedCount }; }
        const afterFailure = {
            visible: batchManager.items.length, persisted: persisted.size,
            busy: batchManager.isUpdatingBatch,
        };
        failSecondWrite = false;
        const retryCount = await batchManager.addItemsBulk(entries);
        return {
            failure, afterFailure, retryCount,
            visible: batchManager.items.length, persisted: persisted.size,
            uniquePaths: new Set(batchManager.items.map((item: any) => item.originalPath)).size,
            busy: batchManager.isUpdatingBatch,
        };
    });
    expect(result.failure).toEqual({ message: '500 of 501 files were saved. Fixture storage is full', addedCount: 500 });
    expect(result.afterFailure).toEqual({ visible: 500, persisted: 500, busy: false });
    expect(result).toMatchObject({ retryCount: 1, visible: 501, persisted: 501, uniquePaths: 501, busy: false });
});

test('a file arriving during another batch write is queued instead of lost', async ({ page }) => {
    await page.evaluate(async () => {
        const root = window as any;
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        batchManager.items = [];
        batchManager.isUpdatingBatch = true;
        root.__arrivalSettled = false;
        root.__arrival = batchManager.addItem('/fixtures/watched.pdf', 'watched.pdf', 20)
            .then((added: boolean) => { root.__arrivalSettled = true; return added; });
    });
    expect(await page.evaluate(() => (window as any).__arrivalSettled)).toBe(false);
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'batch_session_upsert_items_bulk'))).toHaveLength(0);
    const result = await page.evaluate(async () => {
        const root = window as any;
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        batchManager.isUpdatingBatch = false;
        const added = await root.__arrival;
        return { added, paths: batchManager.items.map((item: any) => item.originalPath) };
    });
    expect(result).toEqual({ added: true, paths: ['/fixtures/watched.pdf'] });
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'batch_session_upsert_items_bulk'))).toHaveLength(1);
});
