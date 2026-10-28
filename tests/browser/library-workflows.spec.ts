// Browser integration uses explicit IPC/provider fixtures. Real native and model
// qualification is separate; these tests do not claim inference accuracy.
import { test, expect, type Page } from '@playwright/test';

async function fixtures(page: Page) {
    await page.addInitScript(() => {
        const root = window as any;
        const body = 'Unrelated opening. '.repeat(350) + 'The cobalt warranty expires in November 2031. ' + 'Unrelated appendix. '.repeat(350);
        const items = [
            { id: 'pending', originalPath: '/fixtures/scan.png', originalName: 'Pending scan.png', extension: 'png', size: 1024, modifiedAt: 1, status: 'queued', isAccepted: false },
            { id: 'warranty', originalPath: '/fixtures/warranty.txt', originalName: 'Warranty.txt', extension: 'txt', size: body.length, modifiedAt: 1, status: 'review', isAccepted: false, suggestedTitle: 'Warranty', suggestedAuthor: 'Acme' },
        ];
        const settings: Record<string, any> = {
            language: 'en', providers: [{ id: 'ollama', name: 'Ollama (Local)', baseUrl: 'http://localhost:11434/v1', apiKey: 'ollama', models: ['fixture-model'], selectedModel: 'fixture-model', isConfigured: true }],
            activeProviderId: 'ollama', localModels: ['fixture-model'], llmContextLimit: 4096,
            experimentalCrispEmbed: { endpoint: 'http://127.0.0.1:18080', moedern: false, decisions: true },
        };
        root.__fixtureCalls = [];
        root.__fixturePrompts = [];
        root.__fixtureItems = items;
        root.__fixtureEmptyEvidence = false;
        root.__fixtureRegionEmpty = false;
        const result = { doc_id: 'warranty', filename: 'Warranty.txt', location_uri: '/fixtures/warranty.txt', chunk_index: 4, score: 0.8, source_hash: 'a'.repeat(64), snippet: 'Cobalt warranty' };
        root.__fixtureResults = [result];
        root.__fixtureSortGate = false;
        let fields = [{ id: 'vendor-1', label: 'vendor', original_value: 'Acme Incorporated', normalized_value: 'Acme Incorporated', model: 'fixture-ner', confidence: 0.85, start_char: 12, end_char: 29, page: null, anchor_method: 'first_exact_match', review_status: 'suggested', revision: 0 }];
        let rid = 1;
        const callbacks = new Map<number, Function>();
        const requests = new Map<number, any>();
        const capabilities = {
            platform: 'linux', desktop: true, mobile: false, sidecars: false,
            dev_tools: false, aitoolkit: false, cloud_backup: false,
            local_llm: false, launch_local_servers: false,
            ocr: true, ocr_tiers: ['crispembed'], pdf_render: true, pdf_zpdf: true,
            asr: false, tts: false, tts_watermarked: false,
            translate_align: false, translate_nmt: false,
            drive_filen_native: false, drive_internxt_native: false, drive_subprocess: false,
            fuse: false, audio_glint: false, audio_ffmpeg: false, direct_print: false, share_sheet: false,
            flags: ['build:ocr', 'build:desktop'],
        };
        root.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
        root.__TAURI_INTERNALS__ = {
            metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
            transformCallback: (fn: Function) => { const id = rid++; callbacks.set(id, fn); return id; },
            unregisterCallback: (id: number) => callbacks.delete(id),
            convertFileSrc: () => 'data:image/svg+xml,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="500" height="700"><rect width="500" height="700" fill="white"/><text x="20" y="40">Warranty expires in November 2031.</text></svg>'),
            invoke: async (command: string, args: any = {}) => {
                root.__fixtureCalls.push({ command, args });
                if (command === 'plugin:store|load') return 1;
                if (command === 'plugin:store|get') return [settings[args.key], args.key in settings];
                if (command === 'plugin:store|set') { settings[args.key] = args.value; return; }
                if (command === 'plugin:dialog|open') return '/fixtures/scan.pdf';
                if (command === 'plugin:dialog|save') return '/fixtures/document-fields.csv';
                if (command === 'index_search') return root.__fixtureResults;
                if (command === 'index_query_documents') return { rows: [result], next_cursor: null };
                if (command === 'index_get_fields') return args.docId === 'warranty' ? fields : [];
                if (command === 'index_review_field') {
                    if (args.expectedRevision !== fields[0].revision) throw new Error('Stale revision');
                    fields = [{ ...fields[0], normalized_value: args.normalizedValue, review_status: args.reviewStatus, revision: fields[0].revision + 1 }];
                    return fields;
                }
                if (command === 'verify_sort_field_source') {
                    if (root.__fixtureSortGate) return new Promise(resolve => { root.__fixtureSortResolve = () => { root.__fixtureSortGate = false; resolve(null); }; });
                    return;
                }
                if (command === 'crispembed_classify_document') return { value: args.task === 'collection' ? 'Books' : 'Invoice', confidence: 0.6, model: 'fixture-systemone', input_excerpt: 'Library excerpt for classification.' };
                if (command === 'plugin:fs|write_text_file') return;
                if (command === 'plugin:event|listen') return rid++;
                if (command === 'plugin:path|join') return args.paths.join('/');
                if (command === 'plugin:path|resolve_directory') return '/fixtures';
                if (command === 'plugin:fs|read_file' || command === 'plugin:fs|read_text_file') return Array.from(new TextEncoder().encode(body));
                if (command === 'plugin:fs|stat') return { size: body.length, isFile: true, isDirectory: false, isSymlink: false };
                if (command === 'build_capabilities') return capabilities;
                if (command === 'intended_purpose_status') return { acknowledged: true, version: 1, statement: '', accepted_at_unix: 1 };
                if (command === 'batch_session_is_migrated') return true;
                if (command === 'batch_session_load') return items;
                if (command === 'batch_session_get_extracted_text') return args.itemId === 'warranty' ? body : '';
                if (command === 'batch_session_upsert_item') return;
                if (command === 'index_is_ready') return true;
                if (command === 'index_filename_only_count') return 7;
                if (command === 'index_stats') return { doc_count: 2, chunk_count: 4, total_rows: 4 };
                if (command === 'index_get_config') return { enabled: true, mode: 'Hybrid', backend_type: 'Local', use_vector: true };
                if (command === 'sync_status') return { pending_count: 0, last_push_ts: null, remote_online: false };
                if (command === 'index_retrieve_passages') return root.__fixtureEmptyEvidence ? [] : [{ document: { doc_id: 'warranty', filename: 'Warranty.txt', location_uri: '/fixtures/warranty.txt', chunk_index: 4, score: 0.8 }, passage: { text: 'The cobalt warranty expires in November 2031.', start_char: 4000, end_char: 4045, offset_unit: 'utf8_bytes' } }];
                if (command === 'ocr_doc_open') return { count: 2, pages: ['/fixtures/page1.png', '/fixtures/page2.png'] };
                if (command === 'ocr_page_regions') return { width: 500, height: 700, regions: args.region && root.__fixtureRegionEmpty ? [] : [{ text: args.region ? 'Corrected crop text.' : 'Warranty expires in November 2031.', x: 10, y: 20, w: 350, h: 40, confidence: args.pagePath.endsWith('page2.png') ? 0.9 : 0.2, char_conf: [] }] };
                if (command === 'plugin:http|fetch') {
                    const id = rid++;
                    const config = args.clientConfig;
                    let response = { data: [{ id: 'fixture-model' }], models: [{ name: 'fixture-model' }] } as any;
                    if (config.url.includes('/chat/completions')) {
                        const payload = JSON.parse(new TextDecoder().decode(new Uint8Array(config.data)));
                        root.__fixturePrompts.push(payload);
                        response = { choices: [{ message: { content: 'The cobalt warranty expires in November 2031. [S1]' } }] };
                    }
                    requests.set(id, { url: config.url, bytes: Array.from(new TextEncoder().encode(JSON.stringify(response))), read: false });
                    return id;
                }
                if (command === 'plugin:http|fetch_send') return { status: 200, statusText: 'OK', url: requests.get(args.rid).url, headers: [['content-type', 'application/json']], rid: args.rid };
                if (command === 'plugin:http|fetch_read_body') {
                    const request = requests.get(args.rid);
                    if (request.read) return [1];
                    request.read = true; return [...request.bytes, 0];
                }
                if (command.endsWith('_list') || command.endsWith('_list_sessions') || command === 'index_list_documents') return [];
                return null;
            },
        };
    });
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

test('document OCR action carries the selected Inbox file into the workbench', async ({ page }) => {
    await page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first().click();
    await page.getByRole('button', { name: 'OCR selected document', exact: true }).click();
    await expect(page.locator('.ocrwb')).toBeVisible();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'ocr_doc_open' && call.args.locationUri === '/fixtures/scan.png').length)).toBe(1);
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
    await workbench.getByLabel('OCR task preset').selectOption('multilingual');
    await workbench.getByRole('button', { name: 'Run OCR (all pages)', exact: true }).click();
    await expect(workbench.getByRole('button', { name: 'Retry weak pages (1)', exact: true })).toBeEnabled();
    await page.evaluate(() => { (window as any).__fixtureCalls = []; });
    await workbench.getByRole('button', { name: 'Retry weak pages (1)', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((c: any) => c.command === 'ocr_page_regions').length)).toBe(1);
    expect(await page.evaluate(() => (window as any).__fixtureCalls.find((c: any) => c.command === 'ocr_page_regions').args)).toMatchObject({ pagePath: '/fixtures/page1.png', preset: 'multilingual' });
    const text = workbench.locator('textarea').first();
    await text.fill('My proofread warranty text.');
    await page.evaluate(() => { (window as any).__fixtureRegionEmpty = true; });
    await workbench.getByRole('button', { name: 'Retry selected region', exact: true }).click();
    await expect(workbench).toContainText('previous text was retained');
    await expect(text).toHaveValue('My proofread warranty text.');
    await page.evaluate(() => { (window as any).__fixtureRegionEmpty = false; });
    await workbench.getByRole('button', { name: 'Retry selected region', exact: true }).click();
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
    const savedExport = await page.evaluate(() => (window as any).__fixtureCalls.find((c: any) => c.command === 'plugin:fs|write_text_file'));
    expect(JSON.stringify(savedExport.args)).toContain('ACME');
    expect(JSON.stringify(savedExport.args)).not.toContain('UNREVIEWED');
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
    await panel.getByLabel('Question', { exact: true }).selectOption('collection');
    await expect(panel.locator('p strong')).toHaveCount(0);
    await panel.getByLabel('Choices, separated by commas', { exact: true }).fill('Books, Other');
    await panel.getByRole('button', { name: 'Suggest from document excerpt', exact: true }).click();
    await expect(panel.locator('p strong')).toHaveText('Books');
    await panel.getByLabel('Choices, separated by commas', { exact: true }).fill('Private, Work');
    await expect(panel.locator('p strong')).toHaveCount(0);
    await expect(panel.getByRole('button', { name: 'Approve as a sorting field', exact: true })).toHaveCount(0);
});
