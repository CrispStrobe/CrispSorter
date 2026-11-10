// Browser integration uses explicit IPC/provider fixtures. Real native and model
// qualification is separate; these tests do not claim inference accuracy.
import { test, expect } from '@playwright/test';
import { fixtures } from './fixtures';


test.beforeEach(async ({ page }) => {
    await fixtures(page); await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
});

test('Inbox and Review show their own work and retain the quality presets', async ({ page }) => {
    await expect(page.getByRole('button', { name: 'Inbox', exact: true }).first()).toBeVisible();
    await expect(page.getByText('Pending scan.png', { exact: true }).first()).toBeVisible();
    await page.getByRole('button', { name: 'Review', exact: true }).first().click();
    await expect(page.getByText('Warranty.txt', { exact: true }).first()).toBeVisible();
    await expect(page.locator('.batch-container').getByText('Pending scan.png', { exact: true })).toHaveCount(0);
    await page.getByLabel('Processing preset', { exact: true }).selectOption('quality');
    await expect(page.getByLabel('Processing preset', { exact: true })).toHaveValue('quality');
});

test('mobile keeps the three workflow destinations and capability-gated tools', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const nav = page.locator('nav.mobile-nav');
    await expect(nav.getByRole('button', { name: 'Inbox', exact: true })).toBeVisible();
    await expect(nav.getByRole('button', { name: 'Review', exact: true })).toBeVisible();
    await expect(nav.getByRole('button', { name: 'Library', exact: true })).toBeVisible();
    await nav.getByRole('button', { name: 'Tools', exact: true }).click();
    await expect(page.locator('#mobile-tools-menu').getByRole('button', { name: 'OCR', exact: true })).toBeVisible();
    await page.screenshot({ path: 'test-results/mobile-workflow.png', fullPage: true });
});

for (const platform of ['ios', 'android']) {
    test(`${platform} mobile client hides unavailable native OCR while retaining navigation`, async ({ page }) => {
        await page.setViewportSize({ width: 390, height: 844 });
        await page.goto(`/?fixturePlatform=${platform}`);
        const nav = page.locator('nav.mobile-nav');
        await expect(nav.getByRole('button', { name: 'Inbox', exact: true })).toBeVisible({ timeout: 30000 });
        await expect(nav.getByRole('button', { name: 'Review', exact: true })).toBeVisible();
        await expect(nav.getByRole('button', { name: 'Library', exact: true })).toBeVisible();
        await nav.getByRole('button', { name: 'Tools', exact: true }).click();
        const tools = page.locator('#mobile-tools-menu');
        await expect(tools.getByRole('button', { name: 'OCR', exact: true })).toHaveCount(0);
        await tools.getByRole('button', { name: 'Translate', exact: true }).click();
        await expect(page.locator('#mobile-tools-menu')).not.toBeVisible();
        await nav.getByRole('button', { name: 'Chat', exact: true }).click();
        await expect(page.locator('deep-chat')).toBeVisible();
    });
}

test('document OCR action carries the selected Inbox file into the workbench', async ({ page }) => {
    await page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first().click();
    await page.getByRole('button', { name: 'OCR selected document', exact: true }).click();
    await expect(page.locator('.ocrwb')).toBeVisible();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'ocr_doc_open' && call.args.locationUri === '/fixtures/scan.png').length)).toBe(1);
});

test('script generation reports no file moves and preserves the batch', async ({ page }) => {
    await page.goto('/?fixtureSort=1');
    await page.getByRole('button', { name: 'Review', exact: true }).first().click();
    await expect(page.getByText('Warranty.txt', { exact: true }).first()).toBeVisible();
    await page.getByRole('button', { name: 'Sort Options', exact: true }).click();
    await page.getByRole('button', { name: 'Create Move Script', exact: true }).click();
    const preview = page.locator('.sort-preview');
    await expect(preview).toContainText('This creates a script only.');
    await expect(preview).toContainText('does not include extracted-text sidecars');
    await preview.getByRole('button', { name: 'Confirm and execute', exact: true }).click();
    const report = page.locator('.report-modal');
    await expect(report).toContainText('Script generated for 1 file(s).');
    await expect(report).toContainText('/fixtures/sorted/sort_files.sh');
    await expect(report).toContainText('No files have been moved or copied');
    await expect(report.getByRole('button')).toHaveCount(1);
    await report.getByRole('button', { name: 'Keep complete batch', exact: true }).click();
    await expect(page.getByText('Warranty.txt', { exact: true }).first()).toBeVisible();
    const calls = await page.evaluate(() => (window as any).__fixtureCalls);
    expect(calls.filter((c: any) => c.command === 'execute_batch').at(-1).args.payload.mode).toBe('script_move');
    expect(calls.some((c: any) => c.command === 'index_update_location_by_path')).toBe(false);
});

test('sort report counts general failures', async ({ page }) => {
    await page.goto('/?fixtureSort=1');
    await page.evaluate(() => { (window as any).__fixtureSortError = true; });
    await page.getByRole('button', { name: 'Review', exact: true }).first().click();
    await expect(page.getByText('Warranty.txt', { exact: true }).first()).toBeVisible();
    await page.getByRole('button', { name: 'Sort', exact: true }).click();
    await page.locator('.sort-preview').getByRole('button', { name: 'Confirm and execute', exact: true }).click();
    const report = page.locator('.report-modal');
    await expect(report).toContainText('1 file(s) could not be sorted. See each item for details.');
    await report.getByRole('button', { name: 'Keep complete batch', exact: true }).click();
    // Failed items return to Inbox rather than remaining in Review.
    await page.getByRole('button', { name: 'Inbox', exact: true }).first().click();
    await expect(page.locator('.batch-container').getByRole('row').filter({ hasText: 'Warranty.txt' })).toBeVisible();
});

test('library chat retains real passages and opens the cited source', async ({ page }) => {
    await page.getByRole('button', { name: 'Chat', exact: true }).first().click();
    await page.locator('#evidence-scope').selectOption('library');
    await expect.poll(() => page.locator('deep-chat').evaluate((el: any) => typeof el.connect?.handler)).toBe('function');
    const input = page.locator('deep-chat').locator('[contenteditable="true"]');
    await input.fill('When does the cobalt warranty expire?');
    await input.press('Enter');
    await expect(page.locator('deep-chat')).toContainText('The cobalt warranty expires in November 2031.');
    await page.getByRole('button', { name: /\[S1\].*Warranty/ }).click();
    await expect(page.getByRole('complementary', { name: 'Source evidence' })).toContainText('November 2031');
    await expect(page.getByRole('complementary', { name: 'Source evidence' })).toContainText('Page number unavailable');
    await expect(page.locator('.evidence-viewer .tv-text')).toContainText('November 2031');
    const prompts = await page.evaluate(() => (window as any).__fixturePrompts);
    expect(JSON.stringify(prompts)).toContain('The cobalt warranty expires in November 2031.');
    expect(prompts[0].max_tokens).toBe(1024);
    await page.screenshot({ path: 'test-results/cited-library-chat.png', fullPage: true });
});

async function ask(page: Page, question: string) {
    await expect.poll(() => page.locator('deep-chat').evaluate((el: any) => typeof el.connect?.handler)).toBe('function');
    const input = page.locator('deep-chat').locator('[contenteditable="true"]');
    await input.fill(question); await input.press('Enter');
}

test('selected chat retrieves a middle passage from persisted text', async ({ page }) => {
    await page.getByRole('button', { name: 'Chat', exact: true }).first().click();
    await page.locator('.context-list .context-item').filter({ hasText: 'Warranty.txt' }).click();
    await ask(page, 'When does the cobalt warranty expire?');
    await expect(page.locator('deep-chat')).toContainText('November 2031');
    const prompts = await page.evaluate(() => (window as any).__fixturePrompts);
    expect(JSON.stringify(prompts)).toContain('The cobalt warranty expires in November 2031.');
    await expect(page.getByRole('button', { name: /\[S1\].*Warranty/ })).toBeVisible();
});

test('empty library evidence answers explicitly without calling a model', async ({ page }) => {
    await page.evaluate(() => { (window as any).__fixtureEmptyEvidence = true; });
    await page.getByRole('button', { name: 'Chat', exact: true }).first().click();
    await page.locator('#evidence-scope').selectOption('library');
    await ask(page, 'Explain the missing nebula contract.');
    await expect(page.locator('deep-chat')).toContainText('could not find supporting passages');
    expect(await page.evaluate(() => (window as any).__fixturePrompts.length)).toBe(0);
});

test('OCR retries only weak pages and preserves edits through crop failure and undo', async ({ page }) => {
    await page.getByRole('button', { name: 'OCR', exact: true }).first().click();
    const workbench = page.locator('.ocrwb');
    await workbench.getByRole('button', { name: 'Open document…', exact: true }).first().click();
    await workbench.getByLabel('OCR task preset').selectOption('quick');
    await expect(workbench.getByLabel('CPU pixel budget', { exact: true })).toBeDisabled();
    await workbench.getByLabel('OCR task preset').selectOption('multilingual');
    await workbench.getByLabel('CPU pixel budget', { exact: true }).check();
    await workbench.getByRole('button', { name: 'Run OCR (all pages)', exact: true }).click();
    await expect(workbench.getByRole('button', { name: 'Retry weak pages (1)', exact: true })).toBeEnabled();
    await page.evaluate(() => { (window as any).__fixtureCalls = []; });
    await workbench.getByRole('button', { name: 'Retry weak pages (1)', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((c: any) => c.command === 'ocr_page_regions').length)).toBe(1);
    expect(await page.evaluate(() => (window as any).__fixtureCalls.find((c: any) => c.command === 'ocr_page_regions').args)).toMatchObject({ pagePath: '/fixtures/page1.png', preset: 'multilingual', cpuPixelBudget: true });
    const text = workbench.locator('textarea').first();
    await text.fill('My proofread warranty text.');
    await page.evaluate(() => { (window as any).__fixtureRegionEmpty = true; });
    await workbench.getByRole('button', { name: 'Retry selected region', exact: true }).click();
    await expect(workbench).toContainText('previous text was retained');
    await expect(text).toHaveValue('My proofread warranty text.');
    await page.evaluate(() => { (window as any).__fixtureRegionEmpty = false; });
    await workbench.getByRole('button', { name: 'Retry selected region', exact: true }).click();
    await expect(workbench).toContainText('My proofread warranty text.');
    await expect(workbench).toContainText('Corrected crop text.');
    await workbench.getByRole('button', { name: 'Apply retry', exact: true }).click();
    await expect(text).toHaveValue('Corrected crop text.');
    await workbench.getByRole('button', { name: 'Undo OCR', exact: true }).click();
    await expect(text).toHaveValue('My proofread warranty text.');
    await workbench.getByLabel('Proofread', { exact: true }).check();
    await expect(workbench.getByRole('button', { name: 'Retry weak pages (0)', exact: true })).toBeDisabled();
});

test('field review preserves evidence, finds exact values, exports and stages sorting without moves', async ({ page }) => {
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.locator('.query-input').fill('warranty');
    await page.locator('.search-btn').click();
    await page.getByTitle('Vorschau (Preview)', { exact: true }).click();
    await page.locator('.fields-panel summary').click();
    const fields = page.locator('.fields-panel');
    await fields.getByLabel('Normalized value').fill('ACME');
    await fields.getByRole('button', { name: 'Approve value', exact: true }).click();
    await expect(fields).toContainText('corrected');
    await fields.getByLabel('Normalized value').fill('UNREVIEWED');
    await expect(fields.getByRole('button', { name: 'Use approved fields for sorting', exact: true })).toBeDisabled();
    await fields.getByRole('button', { name: 'Export CSV', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((c: any) => c.command === 'plugin:fs|write_text_file').length)).toBe(1);
    const savedExport = await page.evaluate(() => new TextDecoder().decode((window as any).__fixtureCalls.find((c: any) => c.command === 'plugin:fs|write_text_file').args));
    expect(savedExport).toContain('ACME');
    expect(savedExport).not.toContain('UNREVIEWED');
    await fields.getByLabel('Normalized value').fill('ACME');
    await expect(fields.locator('blockquote')).toHaveText('Acme Incorporated');
    await fields.getByRole('button', { name: 'Find matching documents', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((c: any) => c.command === 'index_query_documents' && c.args.filter.tags?.includes('field:vendor:ACME')).length)).toBe(1);
    await fields.getByRole('button', { name: 'Export CSV', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((c: any) => c.command === 'plugin:fs|write_text_file').length)).toBe(2);
    await fields.getByRole('button', { name: 'Use approved fields for sorting', exact: true }).click();
    await expect(fields).toContainText('Approved fields saved for sorting');
    const calls = await page.evaluate(() => (window as any).__fixtureCalls);
    expect(calls.some((c: any) => c.command === 'verify_sort_field_source')).toBe(true);
    expect(calls.some((c: any) => /^(move_file|rename_file|apply_moves)$/.test(c.command))).toBe(false);
});


test('switching previews during sorting keeps the approved document identity', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any;
        root.__fixtureResults[0].location_uri = '/fixtures/new-warranty.txt';
        root.__fixtureResults.push({ ...root.__fixtureResults[0], doc_id: 'other', filename: 'Other.txt', location_uri: '/fixtures/other.txt', source_hash: 'b'.repeat(64) });
    });
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.locator('.query-input').fill('warranty');
    await page.locator('.search-btn').click();
    await page.getByTitle('Vorschau (Preview)', { exact: true }).first().click();
    await page.locator('.fields-panel summary').click();
    const fields = page.locator('.fields-panel');
    await fields.getByRole('button', { name: 'Approve value', exact: true }).click();
    await expect(fields).toContainText('accepted');
    await page.evaluate(() => { (window as any).__fixtureSortGate = true; });
    await fields.getByRole('button', { name: 'Use approved fields for sorting', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((c: any) => c.command === 'verify_sort_field_source').length)).toBe(1);
    await page.getByTitle('Vorschau (Preview)', { exact: true }).nth(1).click();
    await expect(page.locator('.preview-title')).toContainText('Other.txt');
    await page.evaluate(() => (window as any).__fixtureSortResolve());
    await expect.poll(() => page.evaluate(() =>
        (window as any).__fixtureCalls.some((c: any) =>
            c.command === 'batch_session_upsert_item' &&
            c.args.item.originalPath === '/fixtures/new-warranty.txt' &&
            c.args.item.originalName === 'Warranty.txt')
    )).toBe(true);
    await expect(fields).not.toContainText('Approved fields saved for sorting');
    await page.getByRole('button', { name: 'Inbox', exact: true }).first().click();
    await expect(page.locator('.batch-container').getByText('Warranty.txt', { exact: true }).first()).toBeVisible();
    await expect(page.locator('.batch-container').getByText('Other.txt', { exact: true })).toHaveCount(0);
});

test('restored optional decisions clear when their question or choices change', async ({ page }) => {
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.locator('.query-input').fill('warranty');
    await page.locator('.search-btn').click();
    await page.getByTitle('Vorschau (Preview)', { exact: true }).first().click();
    const panel = page.locator('.decision-panel');
    await panel.locator('summary').click();
    await panel.getByRole('button', { name: 'Suggest from document excerpt', exact: true }).click();
    await expect(panel.locator('p strong')).toHaveText('Invoice');
    await panel.getByRole('combobox', { name: 'Question', exact: true }).selectOption('collection');
    await expect(panel.locator('p strong')).toHaveCount(0);
    await panel.getByLabel('Choices, separated by commas', { exact: true }).fill('Books, Other');
    await panel.getByRole('button', { name: 'Suggest from document excerpt', exact: true }).click();
    await expect(panel.locator('p strong')).toHaveText('Books');
    await panel.getByLabel('Choices, separated by commas', { exact: true }).fill('Private, Work');
    await expect(panel.locator('p strong')).toHaveCount(0);
    await expect(panel.getByRole('button', { name: 'Approve as a sorting field', exact: true })).toHaveCount(0);
});
