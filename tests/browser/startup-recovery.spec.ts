import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

interface StartupFaults {
    preferencesFail?: boolean;
    capabilities?: 'valid' | 'failed' | 'invalid';
    sessionFail?: boolean;
    language?: 'en' | 'de';
}

async function startWithFaults(page: Page, faults: StartupFaults) {
    await fixtures(page);
    await page.addInitScript(config => {
        const root = window as any;
        root.__startupFaults = { capabilities: 'valid', ...config, holdCapabilities: false, holdSession: false };
        const wrapped = new WeakSet<object>();
        const wrap = (bridge: any) => {
            if (!bridge || wrapped.has(bridge)) return bridge;
            wrapped.add(bridge);
            const originalInvoke = bridge.invoke;
            bridge.invoke = async (command: string, args: any = {}) => {
                const fault = root.__startupFaults;
                if (command === 'plugin:store|get' && args.key === 'experimentalCrispEmbed' && fault.preferencesFail) {
                    root.__fixtureCalls.push({ command, args });
                    throw new Error('Fixture experimental settings read failed');
                }
                if (command === 'build_capabilities' && fault.capabilities !== 'valid') {
                    root.__fixtureCalls.push({ command, args });
                    if (fault.capabilities === 'failed') throw new Error('Fixture capability check failed');
                    return { platform: 'linux', desktop: true, flags: ['build:ocr', 'build:ocr-workbench'] };
                }
                if (command === 'batch_session_load' && fault.sessionFail) {
                    root.__fixtureCalls.push({ command, args });
                    throw new Error('Fixture saved batch is temporarily unavailable');
                }
                const result = await originalInvoke(command, args);
                if (command === 'plugin:store|get' && args.key === 'language' && fault.language) return [fault.language, true];
                if (command === 'build_capabilities' && fault.holdCapabilities) {
                    return new Promise(resolve => { root.__releaseCapabilities = () => resolve(result); });
                }
                if (command === 'batch_session_load' && fault.holdSession) {
                    return new Promise(resolve => { root.__releaseSession = () => resolve(result); });
                }
                return result;
            };
            return bridge;
        };
        // Fixture init scripts can run in either order. Install the override
        // before app code without changing the shared IPC fixture.
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', {
            configurable: true,
            get: () => bridge,
            set: value => { bridge = wrap(value); },
        });
    }, faults);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
}

async function startupCalls(page: Page) {
    return page.evaluate(() => {
        const calls = (window as any).__fixtureCalls;
        return {
            capabilities: calls.filter((call: any) => call.command === 'build_capabilities').length,
            session: calls.filter((call: any) => call.command === 'batch_session_load').length,
            fileListener: calls.filter((call: any) => call.command === 'plugin:event|listen' && call.args.event === 'folder-watch:added').length,
            syncListener: calls.filter((call: any) => call.command === 'plugin:event|listen' && call.args.event === 'folder-watch:sync-pair-candidate').length,
            indexConfig: calls.filter((call: any) => call.command === 'index_set_config').length,
        };
    });
}

test('an optional settings failure preserves language, saved batch and later startup, and retries only preferences', async ({ page }) => {
    await startWithFaults(page, { preferencesFail: true, language: 'de' });
    const notice = page.getByRole('alert', { name: 'Einstellungen konnten nicht geladen werden', exact: true });
    await expect(notice).toBeVisible();
    await expect(page.locator('.batch-container .file-name')).toHaveText('Pending scan.png');
    await expect.poll(() => startupCalls(page)).toEqual({ capabilities: 1, session: 1, fileListener: 1, syncListener: 1, indexConfig: 1 });
    expect(await page.evaluate(async () => {
        const modulePath = '/src/lib/i18n.svelte.ts';
        return (await import(modulePath)).i18n.lang;
    })).toBe('de');

    await page.evaluate(() => { (window as any).__startupFaults.preferencesFail = false; });
    await notice.getByRole('button', { name: 'Erneut versuchen', exact: true }).click();
    await expect(notice).not.toBeVisible();
    expect(await startupCalls(page)).toEqual({ capabilities: 1, session: 1, fileListener: 1, syncListener: 1, indexConfig: 1 });
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'plugin:store|get' && call.args.key === 'experimentalCrispEmbed'))).toHaveLength(2);
});

test('capability failures stay visible and gated through malformed retries until a valid response arrives', async ({ page }) => {
    await startWithFaults(page, { capabilities: 'failed' });
    const notice = page.getByRole('alert', { name: 'Available tools could not be checked', exact: true });
    const retry = notice.getByRole('button', { name: /^Retry(?:ing…)?$/ });
    const addFiles = page.locator('.batch-container').getByRole('button', { name: 'Add files', exact: true });
    const ocr = page.locator('.main-nav').getByRole('button', { name: 'OCR', exact: true });
    await expect(notice).toBeVisible();
    await expect(addFiles).toBeDisabled();
    await expect(ocr).toHaveCount(0);
    await expect(page.locator('.batch-container .file-name')).toHaveText('Pending scan.png');
    await expect.poll(() => startupCalls(page)).toEqual({ capabilities: 1, session: 1, fileListener: 1, syncListener: 1, indexConfig: 1 });

    await page.evaluate(() => { (window as any).__startupFaults.capabilities = 'invalid'; });
    await retry.click();
    await expect(notice).toContainText('Invalid app capabilities response.');
    await expect(retry).toBeEnabled();
    await expect(addFiles).toBeDisabled();
    await expect(ocr).toHaveCount(0);

    await page.evaluate(() => {
        (window as any).__startupFaults.capabilities = 'valid';
        (window as any).__startupFaults.holdCapabilities = true;
    });
    await retry.click();
    await expect(retry).toBeDisabled();
    await expect(retry).toHaveAttribute('aria-busy', 'true');
    await expect(addFiles).toBeDisabled();
    await expect(ocr).toHaveCount(0);
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseCapabilities)).toBe('function');
    await page.evaluate(() => { (window as any).__releaseCapabilities(); });
    await expect(notice).not.toBeVisible();
    await expect(addFiles).toBeEnabled();
    await expect(ocr).toBeVisible();
    expect(await startupCalls(page)).toEqual({ capabilities: 3, session: 1, fileListener: 1, syncListener: 1, indexConfig: 1 });
});

test('failed batch restore blocks additions and Retry preserves current edits without repeating startup', async ({ page }) => {
    await startWithFaults(page, { sessionFail: true });
    const notice = page.getByRole('alert', { name: 'The previous batch could not be restored', exact: true });
    const retry = notice.getByRole('button', { name: /^Retry(?:ing…)?$/ });
    await expect(notice).toBeVisible();
    await expect.poll(() => startupCalls(page)).toEqual({ capabilities: 1, session: 1, fileListener: 1, syncListener: 1, indexConfig: 1 });
    const blockedAddition = await page.evaluate(async () => {
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        try { await batchManager.addItem('/fixtures/new-work.txt', 'New work.txt', 120); return ''; }
        catch (error) { return String(error); }
    });
    expect(blockedAddition).toMatch(/restor/i);
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => ['batch_session_upsert_item', 'batch_session_upsert_items_bulk'].includes(call.command)))).toHaveLength(0);
    await page.evaluate(async () => {
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        // Preserve current objects if a shell remount or another existing
        // workspace has already populated the shared in-memory batch.
        batchManager.items = [
            { id: 'pending', originalPath: '/fixtures/scan.png', originalName: 'Renamed scan.png', extension: 'png',
                size: 1024, modifiedAt: 1, status: 'queued', isAccepted: false, suggestedTitle: 'Current correction' },
            { id: 'current', originalPath: '/fixtures/current.txt', originalName: 'Current work.txt', extension: 'txt',
                size: 120, modifiedAt: 1, status: 'queued', isAccepted: false },
        ];
        (window as any).__startupFaults.sessionFail = false;
        (window as any).__startupFaults.holdSession = true;
    });
    await expect(page.locator('.batch-container .file-name')).toHaveText(['Current work.txt', 'Renamed scan.png']);
    await retry.click();
    await expect(retry).toBeDisabled();
    await expect(retry).toHaveAttribute('aria-busy', 'true');
    await expect(page.locator('.batch-container .file-name')).toHaveText(['Current work.txt', 'Renamed scan.png']);
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseSession)).toBe('function');
    await page.evaluate(() => { (window as any).__releaseSession(); });
    await expect(notice).not.toBeVisible();
    await expect(page.locator('.batch-container .file-name')).toHaveText(['Current work.txt', 'Renamed scan.png']);
    expect(await page.evaluate(async () => {
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        return batchManager.items.map((item: any) => ({ id: item.id, title: item.suggestedTitle }));
    })).toEqual(expect.arrayContaining([
        { id: 'pending', title: 'Current correction' }, { id: 'warranty', title: 'Warranty' }, { id: 'current', title: undefined },
    ]));
    expect(await startupCalls(page)).toEqual({ capabilities: 1, session: 2, fileListener: 1, syncListener: 1, indexConfig: 1 });
});
