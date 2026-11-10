import { expect, test, type Locator, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

const longFilename = 'Warranty coverage and service terms for international business equipment.txt';

async function openSearch(page: Page, mobile = false) {
    if (mobile) await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(mobile ? '/?fixturePlatform=ios' : '/');
    const navigation = page.locator(mobile ? 'nav.mobile-nav' : 'nav.main-nav');
    await expect(navigation).toBeVisible({ timeout: 30000 });
    await page.evaluate(({ mobile, longFilename }) => {
        const root = window as any;
        const base = { ...root.__fixtureResults[0], filename: mobile ? longFilename : 'Warranty.txt' };
        root.__fixtureResults = [
            base,
            { ...base, chunk_index: 9, score: 0.6, snippet: 'Another matching warranty passage.' },
            { ...base, doc_id: 'cloud', filename: 'Cloud warranty.txt', chunk_index: 0, score: 0.5,
                location_uri: 'crisp+drive://shared/Cloud%20warranty.txt', url: 'https://example.invalid/warranty' },
        ];
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
            if (command === 'plugin:fs|read_text_file') return Array.from(new TextEncoder().encode('Warranty coverage remains available through November 2031.'));
            const result = await original(command, args);
            if (command === 'index_get_fields' && mobile) {
                return result.map((field: any) => ({ ...field,
                    original_value: `Vendor-${'A'.repeat(120)}`,
                    model: `fixture-model-${'x'.repeat(90)}`,
                }));
            }
            return result;
        };
    }, { mobile, longFilename });
    await navigation.getByRole('button', { name: 'Library', exact: true }).click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.locator('.search-btn').click();
    await expect(page.locator('.result-card')).toHaveCount(2);
}

async function expectNoHorizontalOverflow(element: Locator) {
    await expect.poll(() => element.evaluate(node => node.scrollWidth - node.clientWidth)).toBeLessThanOrEqual(1);
}

async function expectWithinWidth(element: Locator, container: Locator) {
    const bounds = await container.boundingBox();
    if (!bounds) throw new Error('The Library container is not visible');
    await expect.poll(() => element.evaluate((node, bounds) => {
        const rect = node.getBoundingClientRect();
        return Math.max(0, bounds.x - rect.left, rect.right - bounds.x - bounds.width,
            -rect.left, rect.right - window.innerWidth);
    }, bounds)).toBeLessThanOrEqual(1);
}

test.beforeEach(async ({ page }) => { await fixtures(page); });

test('matching passages use a native expansion button while result actions keep independent keyboard behavior', async ({ page }) => {
    await openSearch(page);
    const card = page.locator('.result-card').first();
    const expand = card.getByRole('button', { name: '2 matching passages for Warranty.txt', exact: true });
    const passages = card.locator('.chunk-list');
    await expect(card.locator('.result-header')).not.toHaveAttribute('role');
    await expect(expand).toHaveAttribute('aria-expanded', 'false');
    await expect(passages).toHaveAttribute('id', (await expand.getAttribute('aria-controls'))!);
    await expect(passages).toBeHidden();

    await expand.focus();
    await page.keyboard.press('Enter');
    await expect(expand).toHaveAttribute('aria-expanded', 'true');
    await expect(passages).toBeVisible();
    await expect(expand).toBeFocused();
    await page.keyboard.press('Space');
    await expect(expand).toHaveAttribute('aria-expanded', 'false');
    await expect(passages).toBeHidden();

    const selection = card.getByTitle('Select for batch sorter', { exact: true });
    await selection.focus();
    await page.keyboard.press('Enter');
    await expect(card.getByTitle('Remove from batch selection', { exact: true })).toHaveAttribute('aria-pressed', 'true');
    await expect(expand).toHaveAttribute('aria-expanded', 'false');
    await page.keyboard.press('Space');
    await expect(selection).toHaveAttribute('aria-pressed', 'false');
    await expect(passages).toBeHidden();

    const preview = card.getByTitle('Vorschau (Preview)', { exact: true });
    await preview.focus();
    await page.keyboard.press('Enter');
    await expect(page.getByRole('complementary', { name: 'Document preview' })).toBeVisible();
    await expect(expand).toHaveAttribute('aria-expanded', 'false');
    await page.keyboard.press('Space');
    await expect(page.locator('.preview-pane')).toHaveCount(0);
    await expect(preview).toBeFocused();
    await expect(passages).toBeHidden();
    await expect(page.locator('.result-card').last().locator('.expand-result')).toHaveCount(0);
});

test('phone result titles and local and cloud actions fit without horizontal scrolling', async ({ page }, testInfo) => {
    await openSearch(page, true);
    const root = page.locator('.search-root');
    await expectNoHorizontalOverflow(root);
    await expectNoHorizontalOverflow(page.locator('.query-bar'));
    await expectNoHorizontalOverflow(page.locator('.results-area'));
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toBeInViewport({ ratio: 1 });

    for (const card of await page.locator('.result-card').all()) {
        await expectNoHorizontalOverflow(card);
        await expectNoHorizontalOverflow(card.locator('.result-right'));
        expect((await card.locator('.result-title').boundingBox())!.width).toBeGreaterThan(160);
        for (const action of await card.locator('.result-right button').all()) {
            // Center deliberately: Chromium's minimum-scroll operation can
            // leave a fractional edge against the inner scrollport boundary.
            await action.evaluate(element => element.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }));
            await expect(action).toBeInViewport({ ratio: 1 });
            await expectWithinWidth(action, root);
        }
    }

    const cloud = page.locator('.result-card').last();
    await cloud.getByTitle('Add to reading list', { exact: true }).click();
    await expect(page.locator('.result-action-feedback')).toContainText('Excerpt added to the reading list: Cloud warranty.txt');
    await expectNoHorizontalOverflow(root);
    await page.getByRole('button', { name: 'Dismiss notification', exact: true }).click();
    await page.locator('.results-area').evaluate(element => { element.scrollTop = 0; });
    const screenshot = testInfo.outputPath('phone-library-results.png');
    await page.screenshot({ path: screenshot, fullPage: true });
    await testInfo.attach('Phone Library results', { path: screenshot, contentType: 'image/png' });
});

test('phone preview fills the results area and returns to the selected expanded document', async ({ page }, testInfo) => {
    await openSearch(page, true);
    const root = page.locator('.search-root');
    const card = page.locator('.result-card').first();
    const expand = card.getByRole('button', { name: `2 matching passages for ${longFilename}`, exact: true });
    await expand.click();
    await card.getByTitle('Select for batch sorter', { exact: true }).click();
    const previewTrigger = card.getByTitle('Vorschau (Preview)', { exact: true });
    await previewTrigger.click();

    const preview = page.getByRole('complementary', { name: 'Document preview' });
    const close = preview.getByRole('button', { name: 'Close preview', exact: true });
    await expect(preview).toBeVisible();
    await expect(page.locator('.results-area')).toBeHidden();
    await expect(close).toBeFocused();
    await expect(close).toBeInViewport({ ratio: 1 });
    await expectWithinWidth(preview, root);
    await expectNoHorizontalOverflow(preview);
    await expectNoHorizontalOverflow(preview.locator('.preview-body'));
    expect((await preview.boundingBox())!.width).toBeGreaterThan(300);

    const fields = preview.locator('.fields-panel');
    await fields.locator('summary').click();
    const value = fields.getByRole('textbox', { name: 'Normalized value', exact: true });
    await value.scrollIntoViewIfNeeded();
    await value.click();
    await expect(value).toBeFocused();
    await expectWithinWidth(value, preview);
    await expectNoHorizontalOverflow(fields);
    const approve = fields.getByRole('button', { name: 'Approve value', exact: true });
    await approve.scrollIntoViewIfNeeded();
    await expect(approve).toBeInViewport({ ratio: 1 });
    await expectWithinWidth(approve, preview);
    await expect(close).toBeInViewport({ ratio: 1 });

    const screenshot = testInfo.outputPath('phone-library-preview.png');
    await page.screenshot({ path: screenshot, fullPage: true });
    await testInfo.attach('Phone Library preview', { path: screenshot, contentType: 'image/png' });
    await fields.locator('summary').click();
    const viewer = preview.locator('.document-viewer');
    expect((await viewer.boundingBox())!.height).toBeGreaterThanOrEqual(240);
    await viewer.getByRole('button', { name: 'Edit', exact: true }).click();
    await expect(viewer.getByRole('textbox', { name: 'Text editor', exact: true })).toHaveValue('Warranty coverage remains available through November 2031.');
    await viewer.getByRole('button', { name: 'Cancel', exact: true }).click();

    await close.click();
    await expect(preview).toHaveCount(0);
    await expect(previewTrigger).toBeFocused();
    await expect(previewTrigger).toBeInViewport({ ratio: 1 });
    await expect(expand).toHaveAttribute('aria-expanded', 'true');
    await expect(card.getByTitle('Remove from batch selection', { exact: true })).toHaveAttribute('aria-pressed', 'true');
    await expectNoHorizontalOverflow(page.locator('.results-area'));

    // A new query must reveal its results instead of leaving the previous
    // document preview covering the narrow-screen result area.
    await previewTrigger.click();
    await expect(preview).toBeVisible();
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('coverage');
    await page.locator('.search-btn').click();
    await expect(preview).toHaveCount(0);
    await expect(page.locator('.results-area')).toBeVisible();
    await expect(page.locator('.result-card')).toHaveCount(2);
});
