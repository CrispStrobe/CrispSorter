// Browser integration uses explicit IPC/provider fixtures. Real native and model
// qualification is separate; these tests do not claim inference accuracy.
import { test, expect, type Locator, type Page } from '@playwright/test';
import { fixtures } from './fixtures';
import type { OcrProject } from '../../src/lib/ocr/model';

async function ocrCalls(page: Page, command = 'ocr_workspace_recognize') {
    return page.evaluate(name => (window as any).__fixtureCalls.filter((call: any) => call.command === name), command);
}

async function savedOcrProject(page: Page): Promise<OcrProject | null> {
    return page.evaluate(async () => {
        const saved = await (window as any).__TAURI_INTERNALS__.invoke('ocr_workspace_load', { projectId: null });
        return saved?.project ?? null;
    });
}

async function recognizeFirstZone(page: Page, workspace: Locator) {
    const drawingTool = workspace.getByLabel('Region drawing tool', { exact: true });
    await drawingTool.selectOption('recognize');
    const canvas = workspace.locator('svg[aria-label="Page regions"]');
    await canvas.scrollIntoViewIfNeeded();
    const bounds = await canvas.boundingBox();
    if (!bounds) throw new Error('The OCR page canvas is not visible');
    // Coordinates are relative to the rendered 500 × 700 page. Cover the
    // first text region at (10, 20, 350, 40), as a user drawing on the image.
    await page.mouse.move(bounds.x + bounds.width * 0.01, bounds.y + bounds.height * 0.01);
    await page.mouse.down();
    await page.mouse.move(bounds.x + bounds.width * 0.8, bounds.y + bounds.height * 0.12, { steps: 5 });
    await page.mouse.up();
    await expect(drawingTool).toHaveValue('select');
}


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

test('document OCR action imports the selected Inbox file once and retains its workspace across tabs', async ({ page }) => {
    const navigation = page.locator('nav.main-nav');
    const inboxFile = page.locator('.batch-container table.dense-table .file-name').filter({ hasText: /^Pending scan\.png$/ });
    await inboxFile.click();
    await page.getByRole('button', { name: 'OCR selected document', exact: true }).click();
    const workspace = page.getByRole('region', { name: 'OCR document workspace', exact: true });
    await expect(workspace).toBeVisible();
    await expect(workspace.locator('.page-card')).toHaveCount(1);
    await expect(workspace.getByRole('button', { name: 'Page 1: scan.png · 1', exact: true })).toBeVisible();
    await expect.poll(() => savedOcrProject(page)).toMatchObject({
        documents: [expect.objectContaining({ sourceUri: '/fixtures/scan.png', name: 'scan.png' })],
        pages: [expect.objectContaining({ imagePath: '/fixtures/scan.png', status: 'pending' })],
    });
    const projectId = (await savedOcrProject(page))!.id;

    await navigation.getByRole('button', { name: 'Inbox', exact: true }).click();
    await navigation.getByRole('button', { name: 'OCR', exact: true }).click();
    await expect(workspace.locator('.page-card')).toHaveCount(1);
    // Sending the same source again also selects its existing page, rather
    // than importing another copy or replacing the current project.
    await navigation.getByRole('button', { name: 'Inbox', exact: true }).click();
    await inboxFile.click();
    await page.getByRole('button', { name: 'OCR selected document', exact: true }).click();
    await expect(workspace.locator('.page-card')).toHaveCount(1);
    expect((await savedOcrProject(page))!.id).toBe(projectId);
    expect((await ocrCalls(page, 'ocr_workspace_import')).map((call: any) => call.args.locationUri)).toEqual(['/fixtures/scan.png']);
    expect(await ocrCalls(page)).toHaveLength(0);
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

test('OCR scopes page processing and preserves reviewed corrections through failed and empty zone retries and Undo', async ({ page }) => {
    const navigation = page.locator('nav.main-nav');
    await navigation.getByRole('button', { name: 'OCR', exact: true }).click();
    const workspace = page.getByRole('region', { name: 'OCR document workspace', exact: true });
    const commands = workspace.locator('.command-bar');
    await commands.getByRole('button', { name: 'Add documents', exact: true }).click();
    await expect(workspace.locator('.page-card')).toHaveCount(2);
    await workspace.getByRole('tab', { name: 'Recognize', exact: true }).click();
    await workspace.getByLabel('Recognition engine').selectOption('tesseract');
    await expect(workspace.getByRole('checkbox', { name: /Low-memory CPU profile/ })).toHaveCount(0);
    await workspace.getByLabel('Recognition engine').selectOption('qwen2vl');
    await workspace.getByRole('checkbox', { name: /Low-memory CPU profile/ }).check();
    await workspace.getByLabel('Processing profile').selectOption('accurate');
    await workspace.getByLabel('Document language').selectOption('deu');
    await commands.getByRole('button', { name: 'Recognize page', exact: true }).click();
    await expect(workspace.locator('.queue-bar')).toContainText('1 / 1 pages');
    expect((await ocrCalls(page)).map((call: any) => call.args.pagePath)).toEqual(['/fixtures/page1.png']);
    expect((await ocrCalls(page))[0].args).toMatchObject({
        pagePath: '/fixtures/page1.png', engine: 'qwen2vl', language: 'deu', cpuPixelBudget: true,
        pageWidthPoints: 120, pageHeightPoints: 168, zone: null,
        config: { layout: true, min_confidence: 0.85 },
    });
    await workspace.getByRole('tab', { name: 'Review', exact: true }).click();
    await expect(workspace.locator('.review-heading')).toContainText('1 need attention');
    const text = workspace.getByRole('textbox', { name: 'Text for region 1', exact: true });
    await text.fill('My proofread warranty text.');
    await workspace.getByRole('button', { name: 'Approve page', exact: true }).click();
    await expect(workspace.getByRole('button', { name: 'Page approved', exact: true })).toBeVisible();

    // The remaining page fails once. Retry failed must not recognize the
    // already reviewed first page or overwrite its manual corrections.
    await page.evaluate(() => { (window as any).__fixtureOcrPageFailures['/fixtures/page2.png'] = 1; });
    await commands.getByRole('button', { name: 'Process pending', exact: true }).click();
    await expect(workspace.locator('.queue-bar')).toContainText('1 failed');
    await expect(workspace.locator('.page-card').nth(1)).toContainText('Failed');
    await expect(text).toHaveValue('My proofread warranty text.');
    await workspace.getByRole('button', { name: 'Retry failed', exact: true }).click();
    await expect(workspace.locator('.queue-bar')).toContainText('Batch complete');
    await expect(workspace.getByRole('button', { name: 'Retry failed', exact: true })).toHaveCount(0);
    expect((await ocrCalls(page)).map((call: any) => call.args.pagePath)).toEqual([
        '/fixtures/page1.png', '/fixtures/page2.png', '/fixtures/page2.png',
    ]);
    await expect.poll(() => savedOcrProject(page)).toMatchObject({ pages: [
        expect.objectContaining({ approved: true, regions: [expect.objectContaining({ text: 'My proofread warranty text.', review: 'approved' })] }),
        expect.objectContaining({ status: 'recognized', regions: [expect.objectContaining({ text: 'Archive reference 42.' })] }),
    ] });

    await page.evaluate(() => { (window as any).__fixtureOcrZoneOutcome = 'error'; });
    await recognizeFirstZone(page, workspace);
    await expect(workspace.getByRole('alert')).toContainText('Fixture zone recognition failed');
    await expect(text).toHaveValue('My proofread warranty text.');
    await expect(workspace.getByRole('button', { name: 'Page approved', exact: true })).toBeVisible();

    const beforeEmptyRetry = await savedOcrProject(page);
    await page.evaluate(() => { (window as any).__fixtureOcrZoneOutcome = 'empty'; });
    await recognizeFirstZone(page, workspace);
    await expect(workspace.locator('.workspace-status')).toContainText('No text was recognized in the selected zone. The existing text was preserved.');
    await expect(text).toHaveValue('My proofread warranty text.');
    await expect(workspace.getByRole('button', { name: 'Page approved', exact: true })).toBeVisible();
    expect(await savedOcrProject(page)).toEqual(beforeEmptyRetry);

    await page.evaluate(() => { (window as any).__fixtureOcrZoneOutcome = 'success'; });
    await recognizeFirstZone(page, workspace);
    await expect(text).toHaveValue('Corrected crop text.');
    await expect(workspace.getByRole('button', { name: 'Approve page', exact: true })).toBeVisible();
    const zoneCalls = (await ocrCalls(page)).filter((call: any) => call.args.zone !== null);
    expect(zoneCalls).toHaveLength(3);
    for (const { args } of zoneCalls) {
        expect(args).toMatchObject({ pagePath: '/fixtures/page1.png', engine: 'qwen2vl', language: 'deu', cpuPixelBudget: true });
        expect(args.zone.x).toBeLessThanOrEqual(10);
        expect(args.zone.y).toBeLessThanOrEqual(20);
        expect(args.zone.x + args.zone.w).toBeGreaterThanOrEqual(360);
        expect(args.zone.y + args.zone.h).toBeGreaterThanOrEqual(60);
    }
    await workspace.getByRole('button', { name: 'Undo', exact: true }).click();
    await expect(text).toHaveValue('My proofread warranty text.');
    await expect(workspace.getByRole('button', { name: 'Page approved', exact: true })).toBeVisible();
    await expect.poll(() => savedOcrProject(page)).toMatchObject({ pages: [
        expect.objectContaining({ approved: true, regions: [expect.objectContaining({
            text: 'My proofread warranty text.', originalText: 'Warranty expires in November 2031.', review: 'approved',
        })] }),
        expect.objectContaining({ status: 'recognized', regions: [expect.objectContaining({ text: 'Archive reference 42.' })] }),
    ] });
    await navigation.getByRole('button', { name: 'Inbox', exact: true }).click();
    await navigation.getByRole('button', { name: 'OCR', exact: true }).click();
    await expect(text).toHaveValue('My proofread warranty text.');
    await expect(workspace.getByRole('button', { name: 'Page approved', exact: true })).toBeVisible();
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
