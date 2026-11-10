import { expect, test, type Locator } from '@playwright/test';
import { fixtures } from './fixtures';

async function expectNoHorizontalOverflow(container: Locator) {
    await expect.poll(() => container.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto('/?fixturePlatform=ios');
    await expect(page.locator('nav.mobile-nav')).toBeAttached({ timeout: 30000 });
    await expect(page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first()).toBeVisible();
});

test('phone rows expose filename, status and selection immediately and open usable metadata details', async ({ page }) => {
    const table = page.locator('.table-container');
    const row = page.locator('.dense-table tbody tr').filter({ has: page.getByRole('button', { name: 'Pending scan.png', exact: true }) });
    const filename = row.getByRole('button', { name: 'Pending scan.png', exact: true });
    const checkbox = row.getByRole('checkbox', { name: 'Select item', exact: true });
    await expect(filename).toBeInViewport({ ratio: 1 });
    await expect(checkbox).toBeInViewport({ ratio: 1 });
    await expect(row.locator('.status-badge')).toHaveText('Queued');
    await expect(row.locator('.status-badge')).toBeInViewport({ ratio: 1 });
    await expectNoHorizontalOverflow(table);
    expect(await table.evaluate(element => element.scrollLeft)).toBe(0);

    await checkbox.check();
    await expect(page.locator('.selection-toolbar')).toContainText('1 selected in this view');
    await expectNoHorizontalOverflow(page.locator('.selection-toolbar'));
    await checkbox.uncheck();
    await filename.click();
    const detail = page.locator('.detail-pane');
    await expect(detail.locator('.detail-filename')).toHaveText('Pending scan.png');
    await expect(detail.getByRole('button', { name: 'Close details', exact: true })).toBeInViewport({ ratio: 1 });
    await expectNoHorizontalOverflow(detail);
    const title = detail.getByRole('textbox', { name: 'Edit Title', exact: true });
    await title.scrollIntoViewIfNeeded();
    await expect(title).toBeInViewport({ ratio: 1 });
    await title.fill('Reviewed scan');
    await expect(page.locator('.draft-summary')).toHaveText('1 metadata draft');
    const author = detail.getByRole('textbox', { name: 'Edit Author', exact: true });
    await author.scrollIntoViewIfNeeded();
    // Native scrolling can leave a fractional border pixel at the clipping
    // edge (CI observed 0.19px). Keep the field inside its scroll pane to one
    // CSS pixel, then verify it can actually receive input and save the edit.
    await expect.poll(() => author.evaluate(element => {
        const field = element.getBoundingClientRect();
        const content = element.closest('.detail-content')!.getBoundingClientRect();
        const split = element.closest('.main-split')!.getBoundingClientRect();
        return Math.max(0,
            Math.max(content.top, split.top, 0) - field.top,
            field.bottom - Math.min(content.bottom, split.bottom, window.innerHeight),
            Math.max(content.left, split.left, 0) - field.left,
            field.right - Math.min(content.right, split.right, window.innerWidth));
    })).toBeLessThanOrEqual(1);
    await author.click();
    await expect(author).toBeFocused();
    await author.fill('Mobile Reviewer');
    await detail.getByRole('button', { name: 'Save', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls
        .filter((call: any) => call.command === 'batch_session_upsert_item' && call.args.item.id === 'pending')
        .map((call: any) => call.args.item))).toEqual([
        expect.objectContaining({ id: 'pending', suggestedTitle: 'Reviewed scan', suggestedAuthor: 'Mobile Reviewer', isAccepted: true }),
    ]);
    await expect(page.locator('.draft-summary')).not.toBeVisible();
    await detail.getByRole('button', { name: 'Close details', exact: true }).click();
    await expect(detail).not.toBeVisible();
    await expect(filename).toBeInViewport({ ratio: 1 });
    await expectNoHorizontalOverflow(table);
    await filename.click();
    await expect(title).toHaveValue('Reviewed scan');
    await expect(author).toHaveValue('Mobile Reviewer');
});

test('phone filters and bulk actions stay within reach and use English index states', async ({ page }) => {
    await expect(page.locator('.right-actions').getByRole('button', { name: 'Re-analyze', exact: true })).toBeInViewport({ ratio: 1 });
    await page.getByRole('button', { name: 'Show Filters', exact: true }).click();
    for (const selector of ['#ext-filter', '#status-filter', '#size-filter']) {
        await expect(page.locator(selector)).toBeInViewport({ ratio: 1 });
    }
    await page.locator('#ext-filter').selectOption('png');
    await page.getByRole('textbox', { name: 'Search items', exact: true }).fill('absent');
    await expect(page.getByText('No files match these filters', { exact: true })).toBeVisible();
    await page.locator('.active-batch-filters').getByRole('button', { name: 'Clear filters', exact: true }).click();
    await expect(page.locator('#ext-filter')).toHaveValue('all');
    await expect(page.locator('.batch-container table.dense-table .file-name')).toHaveText('Pending scan.png');
    await page.getByRole('button', { name: 'Show Filters', exact: true }).click();

    await page.getByRole('navigation', { name: 'Mobile navigation' }).getByRole('button', { name: 'Review', exact: true }).click();
    await expect(page.locator('.batch-container table.dense-table .file-name')).toHaveText('Warranty.txt');
    await page.getByRole('checkbox', { name: 'Select item', exact: true }).check();
    const selection = page.locator('.selection-toolbar');
    for (const name of ['Re-analyze', 'Confirm', 'Remove', 'Add to index']) {
        await expect(selection.getByRole('button', { name, exact: true })).toBeInViewport({ ratio: 1 });
    }
    await expectNoHorizontalOverflow(selection);
    await expectNoHorizontalOverflow(page.locator('.batch-container'));
    await page.evaluate(async () => {
        const root = window as any;
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        batchManager.items.find((item: any) => item.id === 'warranty').extractedText = 'The cobalt warranty expires in November 2031.';
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'index_ingest_batch') {
                root.__fixtureCalls.push({ command, args });
                return new Promise<void>(resolve => { root.__releaseMobileIndex = resolve; });
            }
            return originalInvoke(command, args);
        };
    });
    await selection.getByRole('button', { name: 'Add to index', exact: true }).click();
    const indexing = selection.getByRole('button', { name: 'Indexing…', exact: true });
    await expect(indexing).toBeDisabled();
    await expect(indexing).toBeInViewport({ ratio: 1 });
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseMobileIndex)).toBe('function');
    await page.evaluate(() => { (window as any).__releaseMobileIndex(); });
    await expect(selection.getByRole('button', { name: 'Add to index', exact: true })).toBeEnabled();
    await expect(page.locator('.toast-message')).toHaveText('Index: 1 added, 0 failed');
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_ingest_batch'))).toHaveLength(1);
});

test('phone identity remains available after hiding the filename column on a wide screen', async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 900 });
    const columns = page.getByRole('button', { name: 'Select Columns', exact: true });
    await columns.click();
    await page.locator('.col-selector').getByRole('checkbox', { name: 'File Name', exact: true }).uncheck();
    const filename = page.locator('.batch-container table.dense-table').getByRole('button', { name: 'Pending scan.png', exact: true });
    await expect(filename).not.toBeVisible();
    await columns.click();
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(filename).toBeInViewport({ ratio: 1 });
    await expectNoHorizontalOverflow(page.locator('.table-container'));
    await filename.click();
    await expect(page.locator('.detail-pane .detail-filename')).toHaveText('Pending scan.png');
});
