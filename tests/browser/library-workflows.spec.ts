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
        };
        root.__fixtureCalls = [];
        root.__fixturePrompts = [];
        root.__fixtureItems = items;
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
                if (command === 'index_retrieve_passages') return [{ document: { doc_id: 'warranty', filename: 'Warranty.txt', location_uri: '/fixtures/warranty.txt', chunk_index: 4, score: 0.8 }, passage: { text: 'The cobalt warranty expires in November 2031.', start_char: 4000, end_char: 4045, offset_unit: 'utf8_bytes' } }];
                if (command === 'ocr_doc_open') return { count: 2, pages: ['/fixtures/page1.png', '/fixtures/page2.png'] };
                if (command === 'ocr_page_regions') return { width: 500, height: 700, regions: [{ text: 'Warranty expires in November 2031.', x: 10, y: 20, w: 350, h: 40, confidence: 0.2, char_conf: [] }] };
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
    await page.screenshot({ path: 'test-results/cited-library-chat.png', fullPage: true });
});
