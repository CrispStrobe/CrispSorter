import { test, expect } from '@playwright/test';
import { fixtures } from './fixtures';

test('opens existing full-text indexes and preserves the active index on failure', async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await page.evaluate(() => {
        const root = window as any;
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__nextIndexDirectory = '/fixtures/meeting-index';
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'plugin:dialog|open') return Promise.resolve(root.__nextIndexDirectory);
            if (command === 'index_init') {
                root.__fixtureCalls.push({ command, args });
                return args.dataDir.endsWith('invalid') ? Promise.reject('existing_index_not_found') : Promise.resolve();
            }
            return original(command, args, options);
        };
    });

    await page.locator('nav.main-nav').getByRole('button', { name: 'Settings', exact: true }).click();
    await page.getByRole('button', { name: 'Search Index', exact: true }).click();
    const manager = page.getByRole('region', { name: 'Index manager' });
    await manager.getByRole('button', { name: 'Add existing index' }).click();
    await expect(manager.getByRole('button', { name: /meeting-index/ })).toBeVisible();
    await expect(manager.getByRole('button', { name: 'Search in Library' })).toBeEnabled();
    await expect(page.getByText('Vector search may download an embedding model', { exact: false })).toHaveCount(0);

    await page.evaluate(() => (window as any).__nextIndexDirectory = '/fixtures/invalid');
    await manager.getByRole('button', { name: 'Add existing index' }).click();
    await expect(page.getByRole('alert')).toContainText('does not contain a CrispSorter index');
    await expect(manager.getByRole('button', { name: 'Search in Library' })).toBeEnabled();
    await expect(manager.getByRole('button', { name: /invalid/ })).toHaveCount(0);

    await page.evaluate(() => (window as any).__nextIndexDirectory = '/fixtures/second-index');
    await manager.getByRole('button', { name: 'Add existing index' }).click();
    await expect(manager.getByRole('button', { name: /second-index/ })).toBeVisible();
    await manager.getByRole('button', { name: /meeting-index/ }).click();
    await manager.getByRole('button', { name: 'Open index' }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls
        .filter((call: any) => call.command === 'index_init').at(-1)?.args)).toMatchObject({
        dataDir: '/fixtures/meeting-index', directoryMode: 'existing',
        config: { mode: 'text_only', use_vector: false, backend_type: 'local' },
    });
    await manager.getByRole('button', { name: 'Search in Library' }).click();
    const search = page.getByRole('textbox', { name: 'Search indexed documents' });
    await search.fill('warranty');
    await search.press('Enter');
    await expect(page.locator('.result-card').first()).toBeVisible();

    await page.getByRole('button', { name: 'Open index manager · meeting-index', exact: true }).click();
    await expect(manager).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Search Index', exact: true })).toBeFocused();
    await manager.getByRole('button', { name: /second-index/ }).click();
    await manager.getByRole('button', { name: 'Open index' }).click();
    await manager.getByRole('button', { name: 'Search in Library' }).click();
    await expect(page.getByRole('button', { name: 'Open index manager · second-index', exact: true })).toBeVisible();
    await expect(search).toHaveValue('warranty');
    await expect(search).toBeFocused();
    await expect(page.locator('.result-card')).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search').length)).toBe(1);
});

test('keeps reporting and document tools collapsed until opened', async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await page.evaluate(() => {
        const root = window as any;
        const invoke = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: unknown, options: unknown) =>
            command === 'index_get_config'
                ? Promise.resolve({ enabled: true, backend_type: 'local', use_vector: false })
                : invoke(command, args, options);
    });
    const search = page.getByRole('textbox', { name: 'Search indexed documents' });
    await page.locator('nav.main-nav').waitFor();
    await page.keyboard.press('Control+Shift+f');
    await search.fill('warranty');
    await search.press('Enter');
    const card = page.locator('.result-card').first();
    await expect(card).toBeVisible();
    await expect(card.locator('.chunk-preview')).toHaveCount(1);
    await expect(card.locator('.document-details')).not.toHaveAttribute('open', '');
    await expect(page.getByRole('button', { name: 'Count all full-text matches' })).toHaveCount(0);
    await page.getByText('All matches and reports', { exact: true }).click();
    await expect(page.getByRole('button', { name: 'Count all full-text matches' })).toBeVisible();
});

test('opens the index manager directly from German Library search', async ({ page }) => {
    await fixtures(page);
    await page.goto('/?fixtureLanguage=de');
    await page.locator('nav.main-nav').getByRole('button', { name: 'Bibliothek', exact: true }).waitFor();
    await page.keyboard.press('Control+Shift+f');
    await page.getByRole('button', { name: 'Indexverwaltung öffnen', exact: true }).click();
    await expect(page.getByRole('region', { name: 'Indexverwaltung', exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Such-Index', exact: true })).toBeFocused();
});
