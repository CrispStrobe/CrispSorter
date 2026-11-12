// OCR asks before downloading models: the workbench shows the shared model
// dialog, tracks the installer's progress events, and recognizes only after
// the models are installed. Native download/verification is covered by the
// Rust installer tests; this spec drives the UI contract with IPC fixtures.
import { test, expect, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

const MODEL = { name: 'tesseract-deu', purpose: 'text recognition', engine: 'tesseract', size_bytes: 2_000_000,
    license: 'apache-2.0', requires_consent: false, consent_accepted: false, downloadable: true, installed: false,
    managed: true, note: null };

async function modelFixtures(page: Page) {
    await fixtures(page);
    await page.addInitScript((model) => {
        const root = window as any;
        root.__modelsInstalled = false;
        root.__modelInstalls = [];
        root.__modelCancels = [];
        root.__listeners = [];
        root.__callbacks = new Map<number, Function>();
        root.__emit = (event: string, payload: unknown) => {
            for (const listener of root.__listeners.filter((l: any) => l.event === event)) {
                root.__callbacks.get(listener.handler)?.({ event, id: listener.id, payload });
            }
        };
        const plan = () => {
            const models = [{ ...model, name: 'dbnet-det', purpose: 'text detection', installed: true },
                { ...model, installed: root.__modelsInstalled }];
            const missing = models.filter(m => !m.installed);
            return { engine: 'tesseract', models, missing: missing.map(m => m.name),
                missing_bytes: missing.reduce((sum, m) => sum + m.size_bytes, 0), free_bytes: 80e9, offline: false };
        };
        const wrapped = new WeakSet<object>();
        let nextCallback = 100000;
        const wrap = (value: any) => {
            if (!value || wrapped.has(value)) return value;
            wrapped.add(value);
            const invoke = value.invoke.bind(value);
            const transform = value.transformCallback.bind(value);
            value.transformCallback = (fn: Function) => {
                const id = transform(fn);
                const key = typeof id === 'number' ? id : nextCallback++;
                root.__callbacks.set(key, fn);
                return key;
            };
            value.invoke = async (command: string, args: any = {}, options?: any) => {
                if (command === 'plugin:event|listen') {
                    const id = root.__listeners.length + 1;
                    root.__listeners.push({ event: args.event, handler: args.handler, id });
                    return id;
                }
                if (command === 'ocr_models_required') return plan();
                if (command === 'ocr_models_install') {
                    root.__modelInstalls.push(args);
                    return new Promise((resolve, reject) => {
                        root.__finishInstall = () => { root.__modelsInstalled = true; resolve(['/models/tesseract-deu-q8_0.gguf']); };
                        root.__failInstall = (message: string) => reject(new Error(message));
                    });
                }
                if (command === 'ocr_models_cancel') {
                    root.__modelCancels.push(args.requestId);
                    root.__emit('ocr-model-download', { request_id: args.requestId, model: 'tesseract-deu', filename: 'tesseract-deu-q8_0.gguf',
                        index: 0, count: 1, downloaded: 0, total: null, phase: 'cancelled', message: null });
                    root.__failInstall?.('OCR_MODEL_DOWNLOAD_CANCELLED');
                    return true;
                }
                return invoke(command, args, options);
            };
            return value;
        };
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', {
            configurable: true,
            get: () => bridge,
            set: value => { bridge = wrap(value); },
        });
    }, MODEL);
}

async function startRecognition(page: Page) {
    await page.locator('nav.main-nav').getByRole('button', { name: 'OCR', exact: true }).click();
    const workspace = page.getByRole('region', { name: 'OCR document workspace', exact: true });
    const commands = workspace.locator('.command-bar');
    await commands.getByRole('button', { name: 'Add documents', exact: true }).click();
    await expect(workspace.locator('.page-card')).toHaveCount(2);
    await commands.getByRole('button', { name: 'Recognize page', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Download OCR model?' });
    await expect(dialog).toBeVisible();
    return { workspace, dialog };
}

function recognitions(page: Page) {
    return page.evaluate(() => (window as any).__fixtureCalls.filter((c: any) => c.command === 'ocr_workspace_recognize').length);
}

function requestId(page: Page): Promise<string> {
    return page.evaluate(() => (window as any).__modelInstalls.at(-1)?.requestId);
}

test.beforeEach(async ({ page }) => {
    await modelFixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
});

test('recognition waits for missing models, shows download progress, then recognizes', async ({ page }) => {
    const { workspace, dialog } = await startRecognition(page);
    await expect(dialog).toContainText('Recognizing this page needs a model that is not on this computer yet.');
    await expect(dialog.locator('[data-model="tesseract-deu"]')).toContainText('2 MB');
    await expect(dialog.locator('[data-model="dbnet-det"]')).toHaveCount(0);
    expect(await recognitions(page)).toBe(0);

    await dialog.getByRole('button', { name: 'Download 2 MB' }).click();
    await expect.poll(() => requestId(page)).toBeTruthy();
    const id = await requestId(page);
    expect(await page.evaluate(() => (window as any).__modelInstalls[0].names)).toEqual(['tesseract-deu']);

    await page.evaluate((request) => (window as any).__emit('ocr-model-download', { request_id: request, model: 'tesseract-deu',
        filename: 'tesseract-deu-q8_0.gguf', index: 0, count: 1, downloaded: 1_000_000, total: 2_000_000, phase: 'downloading', message: null }), id);
    await expect(dialog.getByRole('progressbar', { name: 'tesseract-deu download' })).toHaveAttribute('aria-valuenow', '50');
    await expect(dialog.getByRole('progressbar', { name: 'Overall download' })).toHaveAttribute('aria-valuenow', '50');
    await expect(dialog).toContainText('1 MB of 2 MB');
    // Another request's events do not move this dialog.
    await page.evaluate(() => (window as any).__emit('ocr-model-download', { request_id: 'other', model: 'tesseract-deu',
        filename: 'x', index: 0, count: 1, downloaded: 2_000_000, total: 2_000_000, phase: 'complete', message: null }));
    await expect(dialog.getByRole('progressbar', { name: 'tesseract-deu download' })).toHaveAttribute('aria-valuenow', '50');

    await page.evaluate((request) => {
        (window as any).__emit('ocr-model-download', { request_id: request, model: 'tesseract-deu', filename: 'tesseract-deu-q8_0.gguf',
            index: 0, count: 1, downloaded: 2_000_000, total: 2_000_000, phase: 'complete', message: null });
        (window as any).__finishInstall();
    }, id);
    await expect(dialog).toHaveCount(0);
    await expect(workspace.locator('.queue-bar')).toContainText('1 / 1 pages');
    expect(await recognitions(page)).toBe(1);
    await workspace.getByRole('tab', { name: 'Recognize', exact: true }).click();
    await expect(workspace.locator('[data-model-chip]')).toHaveText('Models ready');
});

test('cancelling the download stops before recognition and keeps the batch resumable', async ({ page }) => {
    const { workspace, dialog } = await startRecognition(page);
    await dialog.getByRole('button', { name: 'Download 2 MB' }).click();
    await expect.poll(() => requestId(page)).toBeTruthy();
    const id = await requestId(page);
    await page.evaluate((request) => (window as any).__emit('ocr-model-download', { request_id: request, model: 'tesseract-deu',
        filename: 'tesseract-deu-q8_0.gguf', index: 0, count: 1, downloaded: 400_000, total: 2_000_000, phase: 'downloading', message: null }), id);
    await expect(dialog.getByRole('progressbar', { name: 'Overall download' })).toHaveAttribute('aria-valuenow', '20');

    await dialog.getByRole('button', { name: 'Cancel download' }).click();
    await expect(dialog).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).__modelCancels)).toEqual([id]);
    expect(await recognitions(page)).toBe(0);
    await workspace.getByRole('tab', { name: 'Recognize', exact: true }).click();
    await expect(workspace.locator('[data-model-chip]')).toHaveText('1 model missing (2 MB)');
    await expect(page.getByRole('region', { name: 'Saved recognition batch' })).toContainText('1 unfinished');
});

test('declining keeps the page unrecognized and asks again on the next explicit run', async ({ page }) => {
    const { dialog } = await startRecognition(page);
    await dialog.getByRole('button', { name: 'Not now' }).click();
    await expect(dialog).toHaveCount(0);
    expect(await recognitions(page)).toBe(0);
    await page.getByRole('region', { name: 'Saved recognition batch' }).getByRole('button', { name: 'Resume saved batch' }).click();
    await expect(page.getByRole('dialog', { name: 'Download OCR model?' })).toBeVisible();
});
