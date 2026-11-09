#!/usr/bin/env node
/** Real-browser workflow verification against the production Svelte component. */
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const outputArg = process.argv.find(arg => arg.startsWith('--output='));
const output = path.resolve(outputArg?.slice('--output='.length) ?? path.join(here, 'artifacts'));
const portArg = process.argv.find(arg => arg.startsWith('--port='));
const server = await createServer({
    configFile: false,
    root: here,
    plugins: [svelte({ configFile: false })],
    resolve: { alias: { $lib: path.join(repo, 'src/lib') } },
    server: { host: '127.0.0.1', port: Number(portArg?.slice('--port='.length) ?? 0), fs: { allow: [repo] } },
    clearScreen: false,
    logLevel: 'warn',
});
await server.listen();
const address = server.httpServer.address();
const url = `http://127.0.0.1:${typeof address === 'object' && address ? address.port : 0}`;

if (process.argv.includes('--serve')) {
    console.log(`OCR workspace fixture: ${url}`);
    console.log('Scenarios: ?scenario=review, queue-failure, queue-control, batch-recovery, extraction-delay, project-recovery, prepare-unchanged, mobile-models, mobile-storage, import-control, export-control');
    for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => { await server.close(); process.exit(0); });
} else {
    await mkdir(output, { recursive: true });
    // Preserve the host's short temporary path: Chromium's Unix socket name
    // exceeds platform limits when TMPDIR points into a deep artifact directory.
    // Restricted environments can opt into an explicitly writable, short path.
    if (process.env.OCR_SMOKE_TMPDIR) {
        const browserTemp = path.resolve(process.env.OCR_SMOKE_TMPDIR);
        await mkdir(browserTemp, { recursive: true });
        process.env.TMPDIR = browserTemp;
    }
    let playwright;
    if (process.env.PLAYWRIGHT_MODULE_PATH) {
        playwright = require(path.resolve(process.env.PLAYWRIGHT_MODULE_PATH));
    } else {
        try { playwright = require('playwright'); }
        catch {
            const runtimeModules = process.env.CODEX_PRIMARY_RUNTIME_NODE_MODULES;
            if (!runtimeModules) throw new Error('Install Playwright or set PLAYWRIGHT_MODULE_PATH to its package directory before running this browser smoke harness.');
            playwright = require(path.join(runtimeModules, 'playwright'));
        }
    }
    const { chromium } = playwright;
    const browserPath = process.env.OCR_SMOKE_CHROMIUM ?? [chromium.executablePath(), '/usr/bin/chromium', '/usr/bin/chromium-browser'].find(existsSync);
    const results = [];
    const browserErrors = [];
    const textExportEvidence = [];
    let phoneCornerEvidence = null;
    let browser;
    try {
        browser = await chromium.launch({ headless: true, ...(browserPath ? { executablePath: browserPath } : {}) });
        async function fixture(scenario, size = { width: 1440, height: 1000 }) {
            const page = await browser.newPage({ viewport: size, deviceScaleFactor: 1 });
            page.setDefaultTimeout(10_000);
            page.on('pageerror', error => browserErrors.push({ scenario, message: error.message }));
            page.on('dialog', dialog => void dialog.dismiss());
            await page.goto(`${url}/?scenario=${scenario}`);
            await page.waitForFunction(expectedName => {
                const input = document.querySelector('[aria-label="Project name"]');
                return input?.value === expectedName && !input.disabled;
            }, scenario === 'mobile-models' ? 'New mobile scan' : 'Northstar · quarterly statement');
            return page;
        }
        async function test(name, scenario, action) {
            const start = performance.now();
            let page;
            try {
                page = await fixture(scenario);
                await action(page);
                results.push({ name, status: 'passed', duration_ms: Math.round(performance.now() - start) });
                console.log(`PASS ${name}`);
            } catch (error) {
                if (page) await page.screenshot({ path: path.join(output, `failure-${results.length + 1}.png`), fullPage: true }).catch(() => {});
                results.push({ name, status: 'failed', duration_ms: Math.round(performance.now() - start), error: error.stack ?? String(error) });
                console.error(`FAIL ${name}: ${error.message}`);
            } finally { await page?.close({ runBeforeUnload: false }); }
        }

        function canvasFitState(details = false) {
            const viewport = document.querySelector('.canvas-shell .viewport');
            const paper = viewport?.querySelector('.paper');
            if (!viewport || !paper) return false;
            const style = getComputedStyle(viewport);
            const left = parseFloat(style.paddingLeft), right = parseFloat(style.paddingRight);
            const top = parseFloat(style.paddingTop), bottom = parseFloat(style.paddingBottom);
            const width = viewport.clientWidth - left - right;
            const height = viewport.clientHeight - top - bottom;
            const view = viewport.getBoundingClientRect(), image = paper.getBoundingClientRect();
            const state = {
                contentWidth: width, contentHeight: height, paperWidth: image.width, paperHeight: image.height,
                inside: image.left >= view.left + left - 1 && image.right <= view.left + viewport.clientWidth - right + 1 &&
                    image.top >= view.top + top - 1 && image.bottom <= view.top + viewport.clientHeight - bottom + 1,
                fillsAxis: Math.abs(image.width - width) <= 1 || Math.abs(image.height - height) <= 1,
                noScroll: viewport.scrollWidth <= viewport.clientWidth + 1 && viewport.scrollHeight <= viewport.clientHeight + 1,
            };
            return details ? state : width > 0 && height > 0 && state.inside && state.fillsAxis && state.noScroll;
        }
        async function assertFitPage(page, label) {
            await page.getByRole('button', { name: 'Fit page', exact: true }).click();
            try { await page.waitForFunction(canvasFitState, false); }
            catch { assert.fail(`${label} Fit page did not fill and remain inside its viewport: ${JSON.stringify(await page.evaluate(canvasFitState, true))}`); }
        }

        await test('recover pages, correct text, undo, redo, and revoke page approval', 'review', async page => {
            assert.equal(await page.getByRole('button', { name: /^Page \d:/ }).count(), 3);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'load').length), 1);
            const text = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            assert.equal(await text.inputValue(), 'Inv0ice NS-2026-1042');
            await text.fill('Invoice NS-2026-1042');
            await text.blur();
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.equal(await text.inputValue(), 'Inv0ice NS-2026-1042');
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            assert.equal(await text.inputValue(), 'Invoice NS-2026-1042');
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('button', { name: 'Page approved', exact: true }).waitFor();
            await text.fill('Invoice NS-2026-1042 · reviewed');
            await text.blur();
            await page.getByRole('button', { name: 'Approve page', exact: true }).waitFor();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].regions[2].text === 'Invoice NS-2026-1042 · reviewed');
            const saved = await page.evaluate(() => window.ocrSmoke.lastSaved);
            assert.equal(saved.pages[0].regions[2].originalText, 'Inv0ice NS-2026-1042');
            assert.equal(saved.pages[0].approved, false);
            assert.ok(await page.evaluate(() => window.ocrSmoke.saves.every(save => save.revision > 7)));
        });

        await test('a document sent from another tab is imported once and selected', `review&open=${encodeURIComponent('/fixtures/sent-from-library.png')}`, async page => {
            await page.getByRole('button', { name: /^Page 4:/ }).waitFor();
            await page.waitForFunction(() => [...document.querySelectorAll('[aria-current="page"]')]
                .some(el => /^Page 4:/.test(el.getAttribute('aria-label') ?? el.textContent?.trim() ?? '')));
            const imports = await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'importDocument'));
            assert.equal(imports.length, 1);
            assert.equal(imports[0].data.locationUri, '/fixtures/sent-from-library.png');
            assert.equal(await page.getByRole('button', { name: /^Page \d:/ }).count(), 4);
        });

        await test('a document already in the project is selected instead of imported again', `review&open=${encodeURIComponent('/fixtures/northstar-statement.pdf')}`, async page => {
            await page.getByText('is already in this project').waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'importDocument').length), 0);
            assert.equal(await page.getByRole('button', { name: /^Page \d:/ }).count(), 3);
        });

        await test('next issue navigates from uncertain text to an unrecognized page', 'review', async page => {
            await page.getByRole('button', { name: 'Next issue', exact: true }).click();
            assert.equal(await page.locator('.region-card.selected textarea').inputValue(), 'Inv0ice NS-2026-1042');
            await page.getByRole('button', { name: 'Next issue', exact: true }).click();
            await page.getByRole('heading', { name: 'Ready to recognize', exact: true }).waitFor();
            assert.equal(await page.getByRole('button', { name: /^Page 2:/ }).getAttribute('aria-current'), 'page');
            await page.keyboard.press('F8');
            assert.equal(await page.getByRole('button', { name: /^Page 1:/ }).getAttribute('aria-current'), 'page');
        });

        await test('unchanged preparation preserves corrections and approval while showing its warning', 'prepare-unchanged', async page => {
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            const corrected = 'Invoice NS-2026-1042 · confirmed';
            const text = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await text.fill(corrected);
            await text.blur();
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('button', { name: 'Page approved', exact: true }).waitFor();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[2].approved === true &&
                window.ocrSmoke.lastSaved.pages[2].regions[2].text.endsWith(' · confirmed'));
            const before = await page.evaluate(() => window.ocrSmoke.lastSaved.pages[2]);

            await page.getByRole('tab', { name: 'Recognize', exact: true }).click();
            await page.getByRole('button', { name: 'Apply preparation to this page', exact: true }).click();
            await page.locator('.status-message').filter({ hasText: 'Cleanup unavailable; original pixels retained.' }).waitFor({ state: 'visible' });
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'prepare').length), 1);
            await page.getByRole('tab', { name: 'Review', exact: true }).click();
            assert.equal(await text.inputValue(), corrected);
            await page.getByRole('button', { name: 'Page approved', exact: true }).waitFor();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.getByText('Project saved', { exact: true }).waitFor();
            const after = await page.evaluate(() => window.ocrSmoke.lastSaved.pages[2]);
            assert.equal(after.status, 'recognized');
            assert.equal(after.approved, true);
            assert.equal(after.imagePath, before.imagePath);
            assert.deepEqual([after.width, after.height, after.widthPoints, after.heightPoints],
                [before.width, before.height, before.widthPoints, before.heightPoints]);
            assert.deepEqual(after.regions, before.regions);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
        });

        await test('the low-memory CPU profile is offered only for Qwen2 VL and reaches recognition', 'review', async page => {
            await page.getByRole('button', { name: /^Page 2:/ }).click();
            await page.getByRole('tab', { name: 'Recognize', exact: true }).click();
            const toggle = page.getByRole('checkbox', { name: /Low-memory CPU profile/ });
            assert.equal(await toggle.count(), 0);
            const engine = page.getByRole('combobox', { name: 'Recognition engine' });
            await engine.selectOption('qwen2vl');
            await toggle.check();
            await page.getByRole('button', { name: 'Recognize page', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.calls.some(call => call.method === 'recognize'));
            let calls = await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'recognize').map(call => call.data));
            assert.equal(calls.at(-1).engine, 'qwen2vl');
            assert.equal(calls.at(-1).cpuPixelBudget, true);
            // Switching away hides the option and stops sending it, even though
            // the saved preference stays on for when Qwen2 VL is chosen again.
            await engine.selectOption('tesseract');
            assert.equal(await toggle.count(), 0);
            await page.getByRole('button', { name: 'Recognize page', exact: true }).click();
            await page.waitForFunction(n => window.ocrSmoke.calls.filter(call => call.method === 'recognize').length > n, calls.length);
            calls = await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'recognize').map(call => call.data));
            assert.equal(calls.at(-1).engine, 'tesseract');
            assert.equal(calls.at(-1).cpuPixelBudget, false);
        });

        await test('export preflight blocks incomplete pages and exports explicit approved scope', 'review', async page => {
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByText(/recognition is pending/).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.exports.length), 0);
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            // Implicit labels wrap the select, so their text includes options.
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('text');
            await page.getByRole('button', { name: 'Save Plain text', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const request = await page.evaluate(() => window.ocrSmoke.exports[0]);
            assert.equal(request.pages.length, 1);
            assert.equal(request.pages[0].image_path, '/fixtures/northstar-page-3.png');
            assert.equal(request.pages[0].regions[2].text, 'Invoice NS-2026-1042');
            assert.equal(request.overwrite, false);
            assert.deepEqual(request.sourceUris, ['/fixtures/northstar-statement.pdf']);

            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('pdf');
            await page.getByRole('combobox', { name: /^Page sizes/ }).selectOption('dpi');
            await page.getByRole('spinbutton', { name: /^Scan resolution \(DPI\)/ }).fill('150');
            await page.getByRole('button', { name: 'Save Searchable PDF', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 2);
            const pdf = await page.evaluate(() => window.ocrSmoke.exports[1]);
            assert.equal(pdf.format, 'pdf');
            assert.equal(pdf.pdfa, false);
            assert.equal(pdf.pages.length, 1);
            assert.equal(pdf.pages[0].image_path, request.pages[0].image_path);
            assert.deepEqual([pdf.pages[0].width, pdf.pages[0].height], [1000, 1400]);
            assert.equal(pdf.pages[0].page_width_points, pdf.pages[0].width * 72 / 150);
            assert.equal(pdf.pages[0].page_height_points, pdf.pages[0].height * 72 / 150);
            assert.deepEqual(pdf.pages[0].regions, request.pages[0].regions);
        });

        await test('archival export keeps approved page geometry and sends an explicit PDF/A request', 'export-archive', async page => {
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('pdfa');
            await page.getByRole('combobox', { name: /^Page sizes/ }).selectOption('dpi');
            await page.getByRole('spinbutton', { name: /^Scan resolution \(DPI\)/ }).fill('150');
            await page.getByRole('button', { name: 'Save Archival PDF/A-2b', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const request = await page.evaluate(() => window.ocrSmoke.exports[0]);
            assert.equal(request.format, 'pdf');
            assert.equal(request.pdfa, true);
            assert.ok(request.outputPath.endsWith('.pdf'));
            assert.equal(request.pages.length, 1);
            assert.equal(request.pages[0].image_path, '/fixtures/northstar-page-3.png');
            assert.equal(request.pages[0].page_width_points, 480);
            assert.equal(request.pages[0].page_height_points, 672);
            assert.equal(request.pages[0].regions[2].text, 'Invoice NS-2026-1042');
            assert.equal(request.overwrite, false);
        });

        await test('unsupported archival output is disabled with its native capability reason', 'review', async page => {
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            const unavailable = page.locator('select option[value="pdfa"]');
            assert.equal(await unavailable.count(), 1);
            // Playwright 1.62.1 follows the enclosing label for this query and
            // retargets OPTION to its enabled SELECT. Retain that observation,
            // then verify the actual native option state and keyboard behavior.
            // https://github.com/microsoft/playwright/blob/v1.62.1/packages/injected/src/injectedScript.ts
            const evidence = {
                browser: browser.version(),
                playwright_is_disabled: await unavailable.isDisabled(),
                native: await unavailable.evaluate(option => {
                    if (!(option instanceof HTMLOptionElement)) throw new Error('Expected a native OPTION');
                    const select = option.closest('select');
                    return {
                        option_disabled: option.disabled,
                        matches_disabled: option.matches(':disabled'),
                        select_disabled: select?.disabled,
                        label_targets_select: option.closest('label')?.control === select,
                        value: option.value,
                    };
                }),
                keyboard: [],
            };
            const receipt = path.join(output, 'archival-option-state.json');
            await writeFile(receipt, JSON.stringify(evidence, null, 2));
            assert.equal(evidence.native.option_disabled, true);
            assert.equal(evidence.native.matches_disabled, true);
            assert.equal(evidence.native.select_disabled, false);
            assert.equal(evidence.native.value, 'pdfa');
            const reason = page.getByText('PDF/A-2b requires the archival-capable native renderer in this build.', { exact: true });
            await reason.waitFor();
            evidence.native_reason = await reason.innerText();
            const format = page.getByRole('combobox', { name: /^Output format/ });
            await format.selectOption('docx');
            await format.focus();
            // First prove native keyboard navigation works, then prove it
            // cannot enter the unavailable option after the last supported one.
            for (const [key, expected] of [['ArrowUp', 'alto'], ['ArrowDown', 'docx'], ['ArrowDown', 'docx']]) {
                await page.keyboard.press(key);
                const selected = await format.inputValue();
                evidence.keyboard.push({ key, expected, selected });
                await writeFile(receipt, JSON.stringify(evidence, null, 2));
                assert.equal(selected, expected);
            }
            evidence.export_calls = await page.evaluate(() => window.ocrSmoke.exports.length);
            await writeFile(receipt, JSON.stringify(evidence, null, 2));
            assert.equal(evidence.export_calls, 0);
        });

        const rtlReviewedText = '  مَرْحَبًا ABC 123\t\n\nשָׁלוֹם é 𞤀𞤢  ';
        const bidiUnavailable = 'PDF export for right-to-left text or directional formatting is unavailable in this build. Choose a text format to preserve the reviewed reading order.';
        async function recordTextExport(evidence) {
            textExportEvidence.push(evidence);
            await writeFile(path.join(output, 'text-alternatives.json'), JSON.stringify(textExportEvidence, null, 2));
        }
        async function selectedExport(page, mobile = false) {
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            if (mobile) {
                await page.setViewportSize({ width: 390, height: 844 });
                await page.getByRole('navigation', { name: 'Workspace views', exact: true }).getByRole('button', { name: 'Text & tools', exact: true }).click();
            }
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
        }
        await test('RTL PDF preflight offers lossless text alternatives before any destination picker', 'export-rtl', async page => {
            await selectedExport(page);
            const selector = page.getByRole('combobox', { name: /^Output format/ });
            const requests = [];
            for (const [index, [format, name]] of [['text', 'Plain text'], ['html', 'HTML document'], ['docx', 'Word document']].entries()) {
                await selector.selectOption(index === 1 ? 'pdfa' : 'pdf');
                await page.getByText(bidiUnavailable, { exact: true }).waitFor();
                assert.equal(await page.getByText(/^Ready to export/).count(), 0);
                assert.equal(await page.getByRole('button', { name: /^Save (Searchable PDF|Archival PDF\/A-2b)$/ }).isDisabled(), true);
                assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'pickExport').length), index);
                await page.getByRole('button', { name: `Use ${name}`, exact: true }).click();
                assert.equal(await selector.inputValue(), format);
                await page.getByRole('button', { name: `Save ${name}`, exact: true }).click();
                await page.waitForFunction(count => window.ocrSmoke.exports.length === count, index + 1);
                const request = await page.evaluate(() => window.ocrSmoke.exports.at(-1));
                requests.push(request);
                assert.equal(request.format, format);
                assert.equal(request.pdfa, false);
                assert.equal(request.pages.length, 1);
                assert.equal(request.pages[0].regions[2].text, rtlReviewedText);
                assert.equal(request.pages[0].regions[2].original_text, rtlReviewedText);
                assert.equal(request.pages[0].approved, true);
                assert.deepEqual(request.sourceUris, ['/fixtures/northstar-statement.pdf']);
                if (index) assert.deepEqual(request.pages, requests[0].pages);
            }
            await selector.selectOption('pdf');
            await page.getByRole('button', { name: /^Page 1:/ }).click();
            await page.getByRole('checkbox', { name: /^Require page approval/ }).uncheck();
            assert.equal(await page.getByText(bidiUnavailable, { exact: true }).count(), 0);
            assert.equal(await page.getByRole('button', { name: 'Save Searchable PDF', exact: true }).isDisabled(), false);
            await recordTextExport({ scenario: 'desktop-rtl', picker_calls: 3, requests, latin_scope_available: true });
        });

        await test('phone can choose Word for reviewed RTL text without changing its source or selection', 'export-rtl-mobile', async page => {
            await selectedExport(page, true);
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('pdfa');
            await page.getByText(bidiUnavailable, { exact: true }).waitFor();
            const targets = [];
            for (const name of ['Plain text', 'HTML document', 'Word document']) {
                const button = page.getByRole('button', { name: `Use ${name}`, exact: true });
                await button.scrollIntoViewIfNeeded();
                const bounds = await button.boundingBox();
                assert.ok(bounds && bounds.height >= 44 && bounds.x >= 0 && bounds.x + bounds.width <= 391);
                targets.push({ name, bounds });
            }
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'pickExport').length), 0);
            await page.screenshot({ path: path.join(output, 'ocr-phone-rtl-alternatives.png'), fullPage: true });
            await page.getByRole('button', { name: 'Use Word document', exact: true }).click();
            await page.getByRole('button', { name: 'Save Word document', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const request = await page.evaluate(() => window.ocrSmoke.exports[0]);
            assert.equal(request.format, 'docx');
            assert.equal(request.pages[0].regions[2].text, rtlReviewedText);
            assert.equal(request.pages[0].image_path, '/fixtures/northstar-page-3.png');
            assert.deepEqual([request.pages[0].width, request.pages[0].height], [1000, 1400]);
            assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 2));
            await recordTextExport({ scenario: 'mobile-rtl-word', targets, request });
        });

        await test('older phone builds offer only their available RTL text alternatives', 'export-rtl-limited', async page => {
            await selectedExport(page, true);
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('pdf');
            await page.getByText(bidiUnavailable, { exact: true }).waitFor();
            assert.equal(await page.getByRole('button', { name: 'Use Word document', exact: true }).count(), 0);
            assert.equal(await page.locator('select option[value="docx"]').count(), 0);
            await page.getByRole('button', { name: 'Use HTML document', exact: true }).click();
            await page.getByRole('button', { name: 'Save HTML document', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const request = await page.evaluate(() => window.ocrSmoke.exports[0]);
            assert.equal(request.format, 'html');
            assert.equal(request.pages[0].regions[2].text, rtlReviewedText);
            await recordTextExport({ scenario: 'limited-mobile-rtl-html', request });
        });

        await test('supplementary PDF text retains export with a visible reader search limitation', 'export-unicode', async page => {
            await selectedExport(page);
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('pdf');
            const advisory = page.getByText(/Some PDF readers cannot find supplementary characters/);
            await advisory.waitFor();
            assert.equal(await page.getByRole('button', { name: 'Save Searchable PDF', exact: true }).isDisabled(), false);
            await page.getByRole('button', { name: 'Save Searchable PDF', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const request = await page.evaluate(() => window.ocrSmoke.exports[0]);
            assert.equal(request.pages[0].regions[2].text, 'Reviewed café 中文 😀 é');
            await recordTextExport({ scenario: 'supplementary-pdf', advisory: await advisory.innerText(), request });
        });

        await test('batch errors stay per page and retry only failed work', 'queue-failure', async page => {
            await page.getByRole('button', { name: 'Process pending', exact: true }).click();
            await page.getByText('Batch complete', { exact: true }).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 3);
            assert.equal(await page.evaluate(() => window.ocrSmoke.maxNative), 1);
            assert.match(await page.getByRole('button', { name: /^Page 2:/ }).innerText(), /Failed/);
            assert.match(await page.getByRole('button', { name: /^Page 3:/ }).innerText(), /Recognized/);
            await page.evaluate(() => { window.ocrSmoke.failPages = []; });
            await page.getByRole('button', { name: 'Retry failed', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.recognitions.length === 4 && window.ocrSmoke.activeNative === 0);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.recognitions.slice(3)), ['/fixtures/northstar-page-2.png']);
            await page.getByText('Batch complete', { exact: true }).waitFor();
            assert.doesNotMatch(await page.getByRole('button', { name: /^Page 2:/ }).innerText(), /Failed/);
        });

        await test('pause, resume and stop preserve finished and pending pages without concurrent inference', 'queue-control', async page => {
            await page.getByRole('button', { name: 'Process pending', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.activeNative === 1);
            await page.getByRole('button', { name: 'Pause', exact: true }).click();
            await page.evaluate(() => window.ocrSmoke.releaseNext());
            await page.getByText('Queue paused', { exact: true }).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 1);
            await page.getByRole('button', { name: 'Resume', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.recognitions.length === 2 && window.ocrSmoke.activeNative === 1);
            await page.getByRole('button', { name: 'Stop', exact: true }).click();
            await page.evaluate(() => window.ocrSmoke.releaseNext());
            await page.getByText('Queue stopped', { exact: true }).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 2);
            assert.equal(await page.evaluate(() => window.ocrSmoke.maxNative), 1);
            assert.match(await page.getByRole('button', { name: /^Page 3:/ }).innerText(), /Pending/);
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[1].status === 'recognized');
            const saved = await page.evaluate(() => window.ocrSmoke.lastSaved);
            assert.deepEqual(saved.pages.map(item => item.status), ['recognized', 'recognized', 'pending']);
        });

        await test('an interrupted batch resumes its saved selection and settings without rerunning completed pages', 'batch-recovery', async page => {
            await page.getByRole('button', { name: 'Resume saved batch', exact: true }).click();
            await page.getByText('Batch complete', { exact: true }).waitFor();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.batch?.phase === 'completed');
            const data = await page.evaluate(() => ({
                recognitions: window.ocrSmoke.calls.filter(call => call.method === 'recognize').map(call => call.data),
                project: window.ocrSmoke.lastSaved,
            }));
            assert.equal(data.recognitions.length, 1);
            assert.equal(data.recognitions[0].pagePath, '/fixtures/northstar-page-2.png');
            assert.equal(data.recognitions[0].engine, 'tesseract');
            assert.equal(data.recognitions[0].language, 'deu');
            assert.deepEqual(data.recognitions[0].config, { source_type: 'photo' });
            assert.deepEqual(data.project.batch.jobs.map(job => job.state), ['completed', 'completed']);
            assert.deepEqual(data.project.pages.map(item => item.status), ['recognized', 'recognized', 'pending']);
        });

        await test('a failed result save stops the batch and retry preserves completed recognition', 'queue-control', async page => {
            await page.getByRole('button', { name: 'Process pending', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.activeNative === 1 && window.ocrSmoke.lastSaved?.batch?.jobs[0]?.state === 'running');
            await page.evaluate(() => { window.ocrSmoke.failSaves = true; window.ocrSmoke.releaseNext(); });
            await page.getByText('Queue stopped', { exact: true }).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 1);
            assert.match(await page.getByRole('button', { name: /^Page 1:/ }).innerText(), /Recognized/);
            assert.match(await page.getByRole('button', { name: /^Page 2:/ }).innerText(), /Pending/);
            await page.evaluate(() => { window.ocrSmoke.failSaves = false; window.ocrSmoke.holdRecognition = false; });
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.batch?.jobs[0]?.state === 'completed');
            await page.getByRole('button', { name: 'Resume saved batch', exact: true }).click();
            await page.getByText('Batch complete', { exact: true }).waitFor();
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.recognitions), [
                '/fixtures/northstar-page-1.png', '/fixtures/northstar-page-2.png', '/fixtures/northstar-page-3.png',
            ]);
            assert.equal(await page.evaluate(() => window.ocrSmoke.maxNative), 1);
        });

        await test('finishing a batch keeps project changes locked until the final checkpoint is durable', 'queue-control', async page => {
            await page.getByRole('button', { name: 'Process pending', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.activeNative === 1);
            await page.evaluate(() => {
                window.ocrSmoke.holdFinalSave = true;
                window.ocrSmoke.holdRecognition = false;
                window.ocrSmoke.releaseNext();
            });
            await page.waitForFunction(() => window.ocrSmoke.recognitions.length === 3 && window.ocrSmoke.pendingSaves === 1);
            await page.getByText('Batch complete', { exact: true }).waitFor();
            for (const name of ['Process pending', 'New', 'Projects', 'Undo']) {
                assert.equal(await page.getByRole('button', { name, exact: true }).isEnabled(), false, `${name} must wait for the final checkpoint`);
            }
            await page.evaluate(() => { window.ocrSmoke.holdFinalSave = false; window.ocrSmoke.releaseSave(); });
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.batch?.phase === 'completed' && !document.querySelector('[aria-label="Project name"]').disabled);
            assert.equal(await page.getByRole('button', { name: 'New', exact: true }).isEnabled(), true);
            assert.equal(await page.evaluate(() => window.ocrSmoke.maxNative), 1);
        });

        await test('stopping an already paused queue saves its stopped state without another native call', 'queue-control', async page => {
            await page.getByRole('button', { name: 'Process pending', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.activeNative === 1);
            await page.getByRole('button', { name: 'Pause', exact: true }).click();
            await page.evaluate(() => window.ocrSmoke.releaseNext());
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.batch?.phase === 'paused');
            await page.getByRole('button', { name: 'Stop', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.batch?.phase === 'stopped');
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 1);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.lastSaved.pages.map(item => item.status)), ['recognized', 'pending', 'pending']);
            assert.equal(await page.getByRole('button', { name: 'Resume saved batch', exact: true }).isEnabled(), true);
        });

        await test('table extraction renders cell text and strips active HTML', 'review', async page => {
            await page.getByRole('tab', { name: 'Extract', exact: true }).click();
            await page.getByRole('button', { name: 'Table', exact: true }).click();
            await page.getByRole('button', { name: 'Extract table', exact: true }).click();
            const cell = page.getByRole('textbox', { name: 'Row 2, column 1', exact: true });
            await cell.waitFor();
            assert.equal(await cell.inputValue(), 'Secure cell');
            assert.equal(await page.evaluate(() => window.__ocrInjected === true), false);
            const table = cell.locator('xpath=ancestor::table');
            assert.equal(await table.locator('img,script,a,iframe').count(), 0);
            assert.equal(await table.getByRole('textbox', { name: 'Row 2, column 2', exact: true }).inputValue(), 'EUR 1,250.00');
        });

        await test('delayed extraction follows its source page and undo restores visible and exported cells', 'extraction-delay', async page => {
            await page.getByRole('tab', { name: 'Extract', exact: true }).click();
            await page.getByRole('button', { name: 'Table', exact: true }).click();
            await page.getByRole('button', { name: 'Extract table', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.pendingExtractions === 1);
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            assert.equal(await page.evaluate(() => window.ocrSmoke.releaseExtraction()), true);
            await page.getByText(/rows × 2 columns extracted · saved to/).waitFor();
            assert.equal(await page.getByRole('textbox', { name: 'Row 2, column 1', exact: true }).count(), 0);
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].provenance.extraction?.tableRows?.length === 2);
            const saved = await page.evaluate(() => window.ocrSmoke.lastSaved);
            assert.equal(saved.pages[0].provenance.extraction.sourceImagePath, saved.pages[0].imagePath);
            assert.equal(saved.pages[2].provenance.extraction, undefined);

            await page.getByRole('button', { name: /^Page 1:/ }).click();
            const cell = page.getByRole('textbox', { name: 'Row 2, column 1', exact: true });
            await cell.waitFor();
            assert.equal(await cell.inputValue(), 'Secure cell');
            await cell.fill('Reviewed material');
            await cell.blur();
            await page.getByRole('button', { name: 'Save table as CSV', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.writes.length === 1);
            assert.match(await page.evaluate(() => window.ocrSmoke.writes[0].text), /Reviewed material/);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            await page.waitForFunction(() => document.querySelector('[aria-label="Row 2, column 1"]')?.value === 'Secure cell');
            await page.getByRole('button', { name: 'Save table as CSV', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.writes.length === 2);
            assert.equal(await page.evaluate(() => window.ocrSmoke.writes[1].text), '"Item","Value"\r\n"Secure cell","EUR 1,250.00"');
        });

        await test('an invalid saved project leaves the current project and autosaver usable', 'project-recovery', async page => {
            await page.getByRole('button', { name: 'Projects', exact: true }).click();
            await page.getByRole('region', { name: 'Saved projects', exact: true }).getByRole('button', { name: /Invalid archived project/ }).click();
            await page.getByText('Unsupported OCR project version: 99.', { exact: true }).waitFor();
            assert.equal(await page.getByRole('textbox', { name: 'Project name', exact: true }).inputValue(), 'Northstar · quarterly statement');
            await page.getByRole('button', { name: 'Close projects', exact: true }).click();
            const text = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await text.fill('Invoice corrected after invalid restore');
            await text.blur();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].regions[2].text === 'Invoice corrected after invalid restore');
            assert.equal(await page.evaluate(() => window.ocrSmoke.lastSaved.id), 'ocr-fixture-project-recovery');
            assert.equal(await page.evaluate(() => window.ocrSmoke.saves.at(-1).revision), 8);
        });

        await test('reopening the current project flushes pending edits without rollback or revision conflicts', 'project-recovery', async page => {
            await page.getByRole('button', { name: 'Projects', exact: true }).click();
            await page.evaluate(() => { window.ocrSmoke.holdSaves = true; });
            const text = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await text.fill('Invoice with an unsaved correction');
            await text.blur();
            await page.getByRole('region', { name: 'Saved projects', exact: true }).getByRole('button', { name: /Northstar · quarterly statement/ }).click();
            await page.waitForFunction(() => window.ocrSmoke.pendingSaves === 1);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'load').length), 1);
            await page.evaluate(() => { window.ocrSmoke.holdSaves = false; window.ocrSmoke.releaseSave(); });
            await page.getByText('Current project saved', { exact: true }).waitFor();
            assert.equal(await text.inputValue(), 'Invoice with an unsaved correction');
            assert.equal(await page.evaluate(() => window.ocrSmoke.saves.at(-1).revision), 8);
            await text.fill('Invoice after reopening the same project');
            await text.blur();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].regions[2].text === 'Invoice after reopening the same project');
            assert.equal(await page.evaluate(() => window.ocrSmoke.saves.at(-1).revision), 9);
        });

        await test('a confirmation blocks navigation shortcuts and removes only its captured page', 'review', async page => {
            await page.getByRole('button', { name: 'Remove page', exact: true }).click();
            const dialog = page.getByRole('dialog', { name: 'Remove this page?', exact: true });
            await dialog.waitFor();
            await page.keyboard.press('Alt+ArrowRight');
            await page.keyboard.press('F8');
            assert.match(await page.locator('button[aria-current="page"]').getAttribute('aria-label'), /^Page 1:/);
            await dialog.getByRole('button', { name: 'Remove page', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages.length === 2);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.lastSaved.pages.map(item => item.imagePath)), ['/fixtures/northstar-page-2.png', '/fixtures/northstar-page-3.png']);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.equal(await page.getByRole('button', { name: /^Page \d:/ }).count(), 3);
        });

        await test('changing the search query cannot replace text at a stale match offset', 'review', async page => {
            await page.getByRole('button', { name: 'Find and replace', exact: true }).click();
            const query = page.getByRole('textbox', { name: 'Find text', exact: true });
            await query.fill('Inv0ice');
            await page.getByRole('textbox', { name: 'Replacement text', exact: true }).fill('Receipt');
            await page.getByRole('button', { name: 'Next match', exact: true }).click();
            assert.equal(await page.locator('.region-card.selected textarea').inputValue(), 'Inv0ice NS-2026-1042');
            await query.fill('Total due');
            await page.getByRole('button', { name: 'Replace', exact: true }).click();
            assert.equal(await page.getByRole('textbox', { name: 'Text for region 3', exact: true }).inputValue(), 'Inv0ice NS-2026-1042');
            assert.equal(await page.locator('.region-card.selected textarea').inputValue(), 'Total due EUR 1,250.00');
            await page.getByRole('button', { name: 'Replace', exact: true }).click();
            // Successful replacement advances to the next page's matching text.
            await page.getByRole('button', { name: /^Page 1:/ }).click();
            assert.equal(await page.getByRole('textbox', { name: 'Text for region 6', exact: true }).inputValue(), 'Receipt EUR 1,250.00');
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].regions[5].text === 'Receipt EUR 1,250.00');
            assert.equal(await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0].regions[2].text), 'Inv0ice NS-2026-1042');
        });

        await test('mobile storage cleanup is explicit, recovers from failure, and keeps models and reviewed pages', 'mobile-storage', async page => {
            const correction = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await correction.fill('Invoice NS-2026-1042 · storage checkpoint');
            await correction.blur();
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].approved === true &&
                window.ocrSmoke.lastSaved.pages[0].regions[2].text.endsWith(' · storage checkpoint'));
            const projectBefore = await page.evaluate(() => window.ocrSmoke.lastSaved);
            await page.setViewportSize({ width: 390, height: 844 });
            await page.getByRole('button', { name: 'OCR models', exact: true }).click();
            const toggle = page.getByRole('button', { name: 'Manage model storage', exact: true });
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'modelStorage').length), 0);
            await toggle.click();
            const storage = page.getByRole('region', { name: 'OCR model storage', exact: true });
            await storage.getByText('16.0 MiB downloaded · 4.0 MiB can be cleaned up', { exact: true }).waitFor();
            await storage.getByText('Unrecognized fixture metadata was kept for inspection.', { exact: true }).waitFor();
            assert.equal(await storage.getByRole('button', { name: 'Remove dbnet-det', exact: true }).isEnabled(), false);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'cleanModelStorage').length), 0);
            const clean = storage.getByRole('button', { name: 'Clean unused downloads', exact: true });
            await page.evaluate(() => { window.ocrSmoke.failModelStorage = true; });
            await clean.click();
            const error = page.getByText('Fixture storage is busy in another application. Retry cleanup.', { exact: true });
            await error.waitFor();
            assert.equal(await clean.isEnabled(), true);
            assert.equal(await page.evaluate(() => window.ocrSmoke.modelStorageReport.bytes), 16 * 1024 ** 2);
            await page.evaluate(() => { window.ocrSmoke.failModelStorage = false; window.ocrSmoke.holdModelStorage = true; });
            await clean.click();
            await page.waitForFunction(() => window.ocrSmoke.pendingModelStorage === 1);
            assert.equal(await storage.getAttribute('aria-busy'), 'true');
            assert.equal(await clean.isEnabled(), false);
            assert.equal(await toggle.isEnabled(), false);
            assert.equal(await storage.getByRole('button', { name: 'Refresh model storage', exact: true }).isEnabled(), false);
            assert.equal(await storage.getByRole('button', { name: 'Remove tesseract-eng', exact: true }).isEnabled(), false);
            assert.equal(await page.evaluate(() => window.ocrSmoke.releaseModelStorage()), true);
            await page.getByText('Freed 4.0 MiB of unused OCR downloads.', { exact: true }).waitFor();
            await error.waitFor({ state: 'hidden' });
            await storage.getByText('12.0 MiB downloaded · 0.0 MiB can be cleaned up', { exact: true }).waitFor();
            assert.equal(await clean.isEnabled(), false);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.modelStorageReport.models.map(model => model.name)), ['dbnet-det', 'tesseract-eng']);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.lastSaved), projectBefore);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'removeMobileModel').length), 0);
            await storage.scrollIntoViewIfNeeded();
            await page.screenshot({ path: path.join(output, 'ocr-phone-model-storage.png'), fullPage: true });
        });

        await test('model removal requires confirmation and the displayed exact generation, preserving cancellation and active models', 'mobile-storage', async page => {
            await page.setViewportSize({ width: 390, height: 844 });
            await page.getByRole('button', { name: 'OCR models', exact: true }).click();
            await page.getByRole('button', { name: 'Manage model storage', exact: true }).click();
            const storage = page.getByRole('region', { name: 'OCR model storage', exact: true });
            const remove = storage.getByRole('button', { name: 'Remove tesseract-eng', exact: true });
            await remove.waitFor();
            assert.equal(await storage.getByRole('button', { name: 'Remove dbnet-det', exact: true }).isEnabled(), false);
            await remove.click();
            const dialog = page.getByRole('dialog', { name: 'Remove downloaded model?', exact: true });
            await dialog.waitFor();
            await page.keyboard.press('Escape');
            await dialog.waitFor({ state: 'hidden' });
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'removeMobileModel').length), 0);
            await remove.click();
            await dialog.getByRole('button', { name: 'Keep model', exact: true }).click();
            await dialog.waitFor({ state: 'hidden' });
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'removeMobileModel').length), 0);
            await remove.click();
            await page.evaluate(() => { window.ocrSmoke.modelStorageReport.models[1].version = '33333333-3333-4333-8333-333333333333'; });
            await page.screenshot({ path: path.join(output, 'ocr-phone-model-removal-confirmation.png'), fullPage: true });
            await dialog.getByRole('button', { name: 'Remove model', exact: true }).click();
            await page.getByText('This model changed after the storage view was opened. Refresh storage before removing it.', { exact: true }).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.modelStorageReport.models.length), 2);
            const stale = await page.evaluate(() => window.ocrSmoke.calls.find(call => call.method === 'removeMobileModel').data);
            assert.deepEqual(stale, { name: 'tesseract-eng', expectedVersion: '22222222-2222-4222-8222-222222222222' });
            await storage.getByRole('button', { name: 'Refresh model storage', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.calls.filter(call => call.method === 'modelStorage').length === 2);
            await page.waitForFunction(() => document.querySelector('[aria-label="OCR model storage"]')?.getAttribute('aria-busy') === 'false');
            await remove.click();
            await dialog.getByRole('button', { name: 'Remove model', exact: true }).click();
            await page.getByText('Removed tesseract-eng and freed 8.0 MiB. Install it again before using this model for OCR.', { exact: true }).waitFor();
            await remove.waitFor({ state: 'hidden' });
            const calls = await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'removeMobileModel').map(call => call.data));
            assert.deepEqual(calls, [stale, { name: 'tesseract-eng', expectedVersion: '33333333-3333-4333-8333-333333333333' }]);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.modelStorageReport.models.map(model => model.name)), ['dbnet-det']);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'cleanModelStorage').length), 0);
            assert.equal(await page.evaluate(() => window.ocrSmoke.saves.length), 0, 'Storage removal must not mutate the OCR project.');
        });

        await test('mobile model setup is explicit, reports progress, locks work, and recovers after failure', 'mobile-models', async page => {
            await page.setViewportSize({ width: 390, height: 844 });
            const install = page.getByRole('button', { name: 'Install basic OCR models', exact: true });
            await install.waitFor();
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.modelInstalls), []);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.installedModels), []);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
            await page.getByRole('button', { name: 'OCR models', exact: true }).click();
            await page.waitForFunction(() => document.activeElement?.textContent?.includes('Install basic OCR models'));
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.modelInstalls), []);
            await page.evaluate(() => { window.ocrSmoke.failModelInstall = true; });
            await install.click();
            await page.waitForFunction(() => window.ocrSmoke.pendingModelInstalls === 1);
            for (const name of ['New', 'Projects', 'Add documents', 'OCR models']) {
                const buttons = await page.getByRole('button', { name, exact: true }).all();
                assert.ok(buttons.length > 0);
                for (const button of buttons) assert.equal(await button.isEnabled(), false, name + ' must wait for installation');
            }
            assert.equal(await page.evaluate(() => window.ocrSmoke.emitModelProgress({
                name: 'dbnet-det', filename: 'dbnet-det.gguf', phase: 'downloading', downloaded: 1048576, total: 2097152,
            })), true);
            await page.getByRole('region', { name: 'On-device OCR models', exact: true })
                .getByRole('status').filter({ hasText: 'dbnet-det.gguf: 1.0 / 2.0 MiB' }).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.releaseModelInstall()), true);
            await page.getByText('Fixture model download interrupted. Retry the installation.', { exact: true }).waitFor();
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.installedModels), []);
            assert.equal(await install.isEnabled(), true);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
            await page.evaluate(() => { window.ocrSmoke.failModelInstall = false; });
            await page.getByRole('button', { name: 'Add documents', exact: true }).first().click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages.length === 1);
            await page.getByRole('button', { name: 'OCR models', exact: true }).click();
            assert.equal(await page.getByRole('combobox', { name: 'Recognition engine', exact: true }).inputValue(), 'tesseract');
            assert.equal(await page.getByRole('combobox', { name: 'Document language', exact: true }).inputValue(), 'eng');
            await install.click();
            await page.waitForFunction(() => window.ocrSmoke.pendingModelInstalls === 1);
            for (const name of ['Recognize page', 'Process pending', 'New']) {
                assert.equal(await page.getByRole('button', { name, exact: true }).isEnabled(), false, name + ' must wait for installation');
            }
            await page.evaluate(() => window.ocrSmoke.emitModelProgress({
                name: 'tesseract-eng', filename: 'tesseract-eng.gguf', phase: 'downloading', downloaded: 1048576, total: 2097152,
            }));
            const status = page.getByRole('region', { name: 'On-device OCR models', exact: true }).getByRole('status');
            await status.filter({ hasText: 'tesseract-eng.gguf: 1.0 / 2.0 MiB' }).waitFor();
            await status.scrollIntoViewIfNeeded();
            await page.screenshot({ path: path.join(output, 'ocr-phone-model-setup.png'), fullPage: true });
            assert.equal(await page.evaluate(() => window.ocrSmoke.releaseModelInstall()), true);
            await page.getByText(/Basic OCR models are installed \(dbnet-det, tesseract-eng\)/).waitFor();
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.modelInstalls), ['eng', 'eng']);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.installedModels), ['dbnet-det', 'tesseract-eng']);
            await page.getByRole('button', { name: 'Recognize page', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.batch?.phase === 'completed');
            const request = await page.evaluate(() => window.ocrSmoke.calls.find(call => call.method === 'recognize').data);
            assert.equal(request.engine, 'tesseract');
            assert.equal(request.language, 'eng');
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 1);
            assert.equal(await page.evaluate(() => window.ocrSmoke.modelInstalls.length), 2);
        });

        await test('phone import progress stays cancellable and saves only completed documents', 'import-control', async page => {
            await page.setViewportSize({ width: 390, height: 844 });
            await page.getByRole('button', { name: 'Add documents', exact: true }).first().click();
            await page.waitForFunction(() => window.ocrSmoke.pendingImports === 1);
            await page.evaluate(() => window.ocrSmoke.emitImportProgress({ request_id: 'fixture-import', phase: 'rendering', completed: 1, total: 2 }));
            await page.getByText('Rendering pages: 1 of 2', { exact: true }).waitFor();
            const cancel = page.getByRole('button', { name: 'Cancel import', exact: true });
            const bounds = await cancel.boundingBox();
            assert.ok(bounds && bounds.height >= 44 && bounds.x >= 0 && bounds.x + bounds.width <= 390 && bounds.y + bounds.height <= 844, 'Phone cancel target must stay fully reachable');
            assert.equal(await page.getByRole('button', { name: 'Add documents', exact: true }).first().isDisabled(), true);
            await page.screenshot({ path: path.join(output, 'ocr-phone-import-progress.png'), fullPage: true });
            await page.evaluate(() => window.ocrSmoke.releaseImport());
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages.length === 5 && window.ocrSmoke.pendingImports === 1);
            await cancel.click();
            const stopping = page.getByRole('button', { name: 'Stopping…', exact: true });
            await stopping.waitFor(); assert.equal(await stopping.isDisabled(), true);
            assert.equal(await page.evaluate(() => window.ocrSmoke.cancelledImports), 1);
            await page.evaluate(() => window.ocrSmoke.emitImportProgress({ request_id: 'fixture-import', phase: 'rendering', completed: 2, total: 2 }));
            await page.getByText('Stopping import after the current step…', { exact: true }).waitFor();
            assert.equal(await page.getByRole('button', { name: 'Add documents', exact: true }).first().isDisabled(), true);
            await page.evaluate(() => window.ocrSmoke.releaseImport());
            await page.getByText('2 pages added. Import cancelled; completed documents are saved.', { exact: true }).waitFor();
            const final = await page.evaluate(() => ({ project: window.ocrSmoke.lastSaved, calls: window.ocrSmoke.calls.filter(call => call.method === 'importDocument'), max: window.ocrSmoke.maxNative }));
            assert.equal(final.project.pages.length, 5); assert.equal(final.calls.length, 2); assert.equal(final.max, 1);
            assert.equal(final.project.documents.at(-1).sourceUri, '/fixtures/first.pdf');
            assert.equal(final.project.documents.at(-1).archiveUri, '/fixtures/archive/first.pdf');
            assert.equal(await cancel.count(), 0);
        });

        await test('a failed import checkpoint stops further documents and Save retries the retained pages', 'import-control', async page => {
            await page.getByRole('button', { name: 'Add documents', exact: true }).first().click();
            await page.waitForFunction(() => window.ocrSmoke.pendingImports === 1);
            await page.evaluate(() => { window.ocrSmoke.failSaves = true; window.ocrSmoke.releaseImport(); });
            await page.getByText('2 pages added. Import stopped because saving failed. Use Save to retry; keep this workspace open.', { exact: true }).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'importDocument').length), 1);
            assert.equal(await page.evaluate(() => window.ocrSmoke.pendingImports), 0);
            assert.equal(await page.evaluate(() => window.ocrSmoke.activeNative), 0);
            assert.equal(await page.getByRole('button', { name: 'Cancel import', exact: true }).count(), 0);
            await page.getByText('Save failed', { exact: true }).waitFor();
            await page.evaluate(() => { window.ocrSmoke.failSaves = false; });
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages.length === 5);
            const saved = await page.evaluate(() => window.ocrSmoke.lastSaved);
            assert.equal(saved.documents.at(-1).sourceUri, '/fixtures/first.pdf');
            assert.equal(saved.documents.at(-1).archiveUri, '/fixtures/archive/first.pdf');
            assert.equal(saved.pages.filter(item => item.documentId === saved.documents.at(-1).id).length, 2);
            await page.getByText('All changes saved', { exact: true }).waitFor();
        });

        await test('phone export progress waits for cancellation and a retry preserves the committed result', 'export-control', async page => {
            await page.setViewportSize({ width: 390, height: 844 });
            const nav = page.getByRole('navigation', { name: 'Workspace views', exact: true });
            await nav.getByRole('button', { name: /^Pages\b/ }).click();
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            await nav.getByRole('button', { name: 'Text & tools', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            const save = page.getByRole('button', { name: 'Save Searchable PDF', exact: true });
            await save.click();
            await page.waitForFunction(() => window.ocrSmoke.pendingExports === 1);
            await page.evaluate(() => window.ocrSmoke.emitExportProgress({ request_id: 'fixture-export', phase: 'writing', bytes: 2621440 }));
            await page.getByText('Writing export · 2.5 MiB', { exact: true }).waitFor();
            const cancel = page.getByRole('button', { name: 'Cancel export', exact: true });
            const bounds = await cancel.boundingBox();
            assert.ok(bounds && bounds.height >= 44 && bounds.x >= 0 && bounds.x + bounds.width <= 390 && bounds.y + bounds.height <= 844, 'Phone export cancel target must stay fully reachable');
            assert.equal(await save.isDisabled(), true);
            assert.equal(await page.getByRole('combobox', { name: /^Output format/ }).isDisabled(), true);
            assert.equal(await page.getByRole('button', { name: 'Add documents', exact: true }).first().isDisabled(), true);
            await page.screenshot({ path: path.join(output, 'ocr-phone-export-progress.png'), fullPage: true });
            await cancel.click();
            const stopping = page.getByRole('button', { name: 'Stopping…', exact: true });
            await stopping.waitFor(); assert.equal(await stopping.isDisabled(), true);
            assert.deepEqual(await page.evaluate(() => [window.ocrSmoke.cancelledExports, window.ocrSmoke.activeNative, window.ocrSmoke.completedExports]), [1, 1, 0]);
            await page.evaluate(() => window.ocrSmoke.emitExportProgress({ request_id: 'fixture-export', phase: 'writing', bytes: 3145728 }));
            await page.getByText('Stopping export after the current step…', { exact: true }).waitFor();
            assert.equal(await save.isDisabled(), true);
            await page.evaluate(() => window.ocrSmoke.releaseExport());
            await page.getByText('Export cancelled', { exact: true }).waitFor();
            assert.equal(await cancel.count(), 0);
            assert.equal(await save.isEnabled(), true);
            assert.deepEqual(await page.evaluate(() => [window.ocrSmoke.activeNative, window.ocrSmoke.completedExports]), [0, 0]);

            await save.click();
            await page.waitForFunction(() => window.ocrSmoke.pendingExports === 1);
            assert.equal(await cancel.isEnabled(), true);
            await page.evaluate(() => window.ocrSmoke.emitExportProgress({ request_id: 'fixture-export', phase: 'committing', bytes: 4194304 }));
            await page.getByText('Finishing export · 4.0 MiB', { exact: true }).waitFor();
            assert.equal(await cancel.isDisabled(), true);
            assert.equal(await cancel.getAttribute('title'), 'The export is finishing');
            await page.evaluate(() => window.ocrSmoke.releaseExport());
            await page.getByText(/^Exported 1 pages ·/).waitFor();
            assert.equal(await cancel.count(), 0);
            assert.equal(await save.isEnabled(), true);
            const state = await page.evaluate(() => ({ completed: window.ocrSmoke.completedExports, cancelled: window.ocrSmoke.cancelledExports, max: window.ocrSmoke.maxNative, requests: window.ocrSmoke.exports }));
            assert.deepEqual([state.completed, state.cancelled, state.max], [1, 1, 1]);
            assert.equal(state.requests.length, 2);
            assert.deepEqual(state.requests[1], state.requests[0]);
            assert.equal(state.requests[1].pages[0].image_path, '/fixtures/northstar-page-3.png');
        });

        await test('phone export failures unlock retry and destination transfer belongs to the system dialog', 'export-control', async page => {
            await page.setViewportSize({ width: 390, height: 844 });
            const nav = page.getByRole('navigation', { name: 'Workspace views', exact: true });
            await nav.getByRole('button', { name: /^Pages\b/ }).click();
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            await nav.getByRole('button', { name: 'Text & tools', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            const save = page.getByRole('button', { name: 'Save Searchable PDF', exact: true });
            const cancel = page.getByRole('button', { name: 'Cancel export', exact: true });
            await page.evaluate(() => { window.ocrSmoke.failExport = true; });
            await save.click();
            await page.waitForFunction(() => window.ocrSmoke.pendingExports === 1);
            await page.evaluate(() => window.ocrSmoke.releaseExport());
            await page.getByText('Fixture export could not finish writing.', { exact: true }).waitFor();
            assert.equal(await cancel.count(), 0);
            assert.equal(await save.isEnabled(), true);
            assert.equal(await page.evaluate(() => window.ocrSmoke.completedExports), 0);
            await page.evaluate(() => { window.ocrSmoke.failExport = false; });
            await save.click();
            await page.waitForFunction(() => window.ocrSmoke.pendingExports === 1);
            await page.evaluate(() => window.ocrSmoke.emitExportProgress({ request_id: 'fixture-export', phase: 'destination', bytes: 6291456 }));
            await page.getByText('Saving to a destination · 6.0 MiB', { exact: true }).waitFor();
            assert.equal(await cancel.isDisabled(), true);
            assert.equal(await cancel.getAttribute('title'), 'Use the system save dialog to finish or cancel');
            assert.equal(await save.isDisabled(), true);
            await page.evaluate(() => window.ocrSmoke.releaseExport());
            await page.getByText(/^Exported 1 pages ·/).waitFor();
            assert.equal(await cancel.count(), 0);
            assert.equal(await save.isEnabled(), true);
            assert.deepEqual(await page.evaluate(() => [window.ocrSmoke.completedExports, window.ocrSmoke.cancelledExports, window.ocrSmoke.activeNative]), [1, 0, 0]);
        });

        await test('camera import validates the file and opens a preserved photo without starting recognition', 'review', async page => {
            await page.setViewportSize({ width: 390, height: 844 });
            const camera = page.getByLabel('Camera photo file', { exact: true });
            assert.equal(await camera.getAttribute('capture'), 'environment');
            // Wry's Android file chooser only offers the external camera for image/* plus capture.
            assert.ok((await camera.getAttribute('accept')).split(',').map(type => type.trim()).includes('image/*'));
            await camera.setInputFiles({ name: 'inspection.HEIC', mimeType: 'image/heic', buffer: Buffer.from('unsupported fixture') });
            await page.getByText(/Export this photo as JPEG or PNG first/).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'importImage').length), 0);
            const photo = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZ8AAAAASUVORK5CYII=', 'base64');
            await camera.setInputFiles({ name: 'inspection.png', mimeType: 'image/png', buffer: photo });
            await page.getByRole('region', { name: 'Photo preparation', exact: true }).waitFor();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages.length === 4);
            const saved = await page.evaluate(() => window.ocrSmoke.lastSaved);
            const imported = saved.pages[3];
            assert.equal(imported.imagePath, '/fixtures/captured-photo.png');
            assert.equal(imported.originalImagePath, imported.imagePath);
            assert.equal(imported.status, 'pending');
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.find(call => call.method === 'importImage').data.byteLength), photo.length);
            const nav = page.getByRole('navigation', { name: 'Workspace views', exact: true });
            assert.equal(await nav.getByRole('button', { name: 'Page image', exact: true }).getAttribute('aria-pressed'), 'true');
            await page.getByRole('button', { name: 'Check photo quality', exact: true }).click();
            await page.getByText('Suggested correction: -2.7°', { exact: true }).waitFor();
            await page.getByRole('navigation', { name: 'Photo tools', exact: true }).getByRole('button', { name: 'Lighting', exact: true }).click();
            assert.equal(await page.getByRole('spinbutton', { name: /^Tilt correction \(degrees\)/ }).inputValue(), '-2.7');
            await page.getByRole('navigation', { name: 'Photo tools', exact: true }).getByRole('button', { name: 'Check', exact: true }).click();
            await page.screenshot({ path: path.join(output, 'ocr-phone-photo-preparation.png'), fullPage: true });
        });

        await test('phone corner editing fits a usable page, accepts touch drags, and changes pixels only on Apply', 'review', async page => {
            const text = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await text.fill('Invoice reviewed before phone correction');
            await text.blur();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].regions[2].text === 'Invoice reviewed before phone correction');
            const before = await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0]);
            await page.setViewportSize({ width: 390, height: 844 });
            const views = page.getByRole('navigation', { name: 'Workspace views', exact: true });
            await views.getByRole('button', { name: 'Page image', exact: true }).click();
            await page.getByRole('combobox', { name: 'Page zoom', exact: true }).selectOption('2');
            await page.getByRole('button', { name: 'Prepare photo', exact: true }).click();
            const tools = page.getByRole('navigation', { name: 'Photo tools', exact: true });
            const closeBox = await page.getByRole('button', { name: 'Close photo preparation', exact: true }).boundingBox();
            assert.ok(closeBox && closeBox.width >= 44 && closeBox.height >= 44, 'The photo preparation close button must have a touch-sized target');
            for (const button of await tools.getByRole('button').all()) {
                const box = await button.boundingBox();
                assert.ok(box && box.width >= 44 && box.height >= 44, 'Photo steps must have touch-sized targets');
            }
            assert.equal(await tools.getByRole('button', { name: 'Check', exact: true }).getAttribute('aria-pressed'), 'true');
            assert.equal(await page.getByRole('button', { name: 'Correct perspective', exact: true }).isVisible(), false);
            await tools.getByRole('button', { name: 'Lighting', exact: true }).click();
            assert.equal(await page.getByRole('button', { name: 'Flatten shadows', exact: true }).isVisible(), true);
            assert.equal(await page.getByRole('button', { name: 'Check photo quality', exact: true }).isVisible(), false);
            await tools.getByRole('button', { name: 'Corners', exact: true }).click();
            const back = page.getByRole('button', { name: 'Back to photo tools', exact: true });
            await back.waitFor();
            await page.waitForFunction(canvasFitState, false);
            assert.equal(await page.getByRole('combobox', { name: 'Page zoom', exact: true }).inputValue(), 'page', 'Entering corner editing resets a previous custom zoom to Fit page');
            for (const selector of ['.workspace-header', '.command-bar', '.mobile-workspace-nav', '.image-toolbar', '.photo-tool-toggle', '.photo-preparation']) {
                assert.equal(await page.locator(selector).isVisible(), false, `${selector} must give its space to the page while choosing corners`);
            }
            for (const name of ['Back to photo tools', 'Apply perspective', 'Zoom out', 'Zoom in', 'Fit page']) {
                const box = await page.getByRole('button', { name, exact: true }).boundingBox();
                assert.ok(box && box.width >= 44 && box.height >= 44 && box.y >= 0 && box.y + box.height <= 844, `${name} must stay reachable at 390×844`);
            }
            // Zoom still works in focus mode, and Fit page restores all four handles.
            await page.getByRole('combobox', { name: 'Page zoom', exact: true }).selectOption('1');
            await assertFitPage(page, 'Phone corner editor');
            const focus = page.getByRole('region', { name: 'Photo corner editing', exact: true });
            const exact = focus.getByText('Exact corner positions', { exact: true });
            await exact.click();
            const cornerX = focus.getByRole('spinbutton', { name: 'Top left corner X', exact: true });
            const cornerY = focus.getByRole('spinbutton', { name: 'Top left corner Y', exact: true });
            const numberStyle = await cornerX.evaluate(input => ({ height: input.getBoundingClientRect().height, fontSize: parseFloat(getComputedStyle(input).fontSize), min: input.min, max: input.max }));
            assert.ok(numberStyle.height >= 44 && numberStyle.fontSize >= 16, 'Numeric corner entry must support touch and avoid phone input zoom');
            assert.deepEqual([numberStyle.min, numberStyle.max], ['0', String(before.width - 1)]);
            await cornerX.fill(String(before.width - 1));
            assert.equal(await focus.getByRole('button', { name: 'Apply perspective', exact: true }).isEnabled(), false, 'A degenerate corner selection must not be applied');
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'transform').length), 0);
            await cornerX.fill('0');
            await cornerY.fill('0');
            assert.equal(await focus.getByRole('button', { name: 'Apply perspective', exact: true }).isEnabled(), true, 'Valid numeric coordinates restore Apply');
            for (const name of ['Back to photo tools', 'Apply perspective']) {
                const box = await focus.getByRole('button', { name, exact: true }).boundingBox();
                assert.ok(box && box.y >= 0 && box.y + box.height <= 844, `${name} stays reachable with numeric entry expanded`);
            }
            await exact.click();
            await page.waitForFunction(canvasFitState, false);
            const geometry = await page.evaluate(() => {
                const rect = element => { const b = element.getBoundingClientRect(); return { x: b.x, y: b.y, width: b.width, height: b.height }; };
                return {
                    screen: { width: innerWidth, height: innerHeight },
                    viewport: rect(document.querySelector('.canvas-shell .viewport')),
                    paper: rect(document.querySelector('.canvas-shell .paper')),
                    handles: [...document.querySelectorAll('.photo-corner')].map(rect),
                };
            });
            assert.ok(geometry.viewport.height >= 540, `Corner editing needs a usable canvas: ${JSON.stringify(geometry)}`);
            assert.ok(geometry.paper.width >= 300 && geometry.paper.height >= 420, `The fitted portrait page must be large enough to place corners: ${JSON.stringify(geometry)}`);
            assert.equal(geometry.handles.length, 4);
            for (const box of geometry.handles) {
                assert.ok(box.width >= 44 && box.height >= 44, 'Each corner handle remains at least 44 CSS pixels');
                assert.ok(box.x >= geometry.viewport.x && box.y >= geometry.viewport.y && box.x + box.width <= geometry.viewport.x + geometry.viewport.width && box.y + box.height <= geometry.viewport.y + geometry.viewport.height, 'Every fitted corner handle must be entirely inside the canvas');
                assert.ok(box.x >= 0 && box.y >= 0 && box.x + box.width <= 390 && box.y + box.height <= 844, 'Every fitted corner handle must be entirely on screen');
            }
            const corner = page.getByRole('button', { name: 'Top left corner', exact: true });
            const initialImage = await page.getByRole('img', { name: /^Working page:/ }).getAttribute('src');
            await page.evaluate(() => {
                window.ocrCornerTouches = [];
                document.querySelector('.photo-corner').addEventListener('pointerdown', event => window.ocrCornerTouches.push({ pointerType: event.pointerType, trusted: event.isTrusted }), { capture: true });
            });
            const session = await page.context().newCDPSession(page);
            try {
                await session.send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 1 });
                const box = geometry.handles[0];
                const start = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
                const touch = (x, y) => ({ x, y, radiusX: 6, radiusY: 6, force: 1, id: 1 });
                await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [touch(start.x, start.y)] });
                for (let step = 1; step <= 4; step++) {
                    await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [touch(start.x + 36 * step / 4, start.y + 42 * step / 4)] });
                }
                await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
            } finally { await session.detach(); }
            await page.waitForFunction(() => document.querySelector('.photo-corner')?.title !== 'Top left: 0, 0 pixels');
            const moved = (await corner.getAttribute('title')).match(/^Top left: (\d+), (\d+) pixels$/);
            assert.ok(moved, 'The moved corner exposes its actual image coordinates');
            assert.ok(Math.abs(Number(moved[1]) - 36 * before.width / geometry.paper.width) <= 2, 'A touch drag must map horizontal screen movement to source-image coordinates');
            assert.ok(Math.abs(Number(moved[2]) - 42 * before.height / geometry.paper.height) <= 2, 'A touch drag must map vertical screen movement to source-image coordinates');
            const touches = await page.evaluate(() => window.ocrCornerTouches);
            assert.deepEqual(touches, [{ pointerType: 'touch', trusted: true }], 'The browser must deliver a trusted touch pointer rather than a synthetic mouse event');
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'transform').length), 0);
            assert.equal(await page.getByRole('img', { name: /^Working page:/ }).getAttribute('src'), initialImage);
            phoneCornerEvidence = { ...geometry, drag: { screenDelta: { x: 36, y: 42 }, imagePoint: { x: Number(moved[1]), y: Number(moved[2]) }, events: touches }, transformsBeforeApply: 0 };
            await page.screenshot({ path: path.join(output, 'ocr-phone-corner-focus.png'), fullPage: true });
            await back.click();
            await page.getByRole('textbox', { name: 'Project name', exact: true }).waitFor();
            await tools.waitFor();
            assert.equal(await page.getByRole('button', { name: 'Correct perspective', exact: true }).isVisible(), true);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'transform').length), 0, 'Back must leave the source image unchanged');
            await tools.getByRole('button', { name: 'Corners', exact: true }).click();
            await back.waitFor();
            await page.keyboard.press('Escape');
            await views.waitFor();
            await tools.waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'transform').length), 0, 'Escape must leave the source image unchanged');
            await tools.getByRole('button', { name: 'Corners', exact: true }).click();
            await page.getByRole('button', { name: 'Apply perspective', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].imagePath.endsWith('-perspective'));
            const applied = await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0]);
            assert.equal(applied.originalImagePath, before.originalImagePath);
            assert.deepEqual([applied.width, applied.height], [900, 1300]);
            assert.equal(applied.status, 'pending');
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'transform').length), 1);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].imagePath === '/fixtures/northstar-page-1.png');
            const restored = await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0]);
            assert.equal(restored.originalImagePath, before.originalImagePath);
            assert.deepEqual(restored.regions, before.regions, 'Undo restores reviewed text and original recognition evidence');
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
        });

        await test('corner controls reject invalid selections and perspective undo restores review evidence', 'review', async page => {
            const text = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await text.fill('Invoice reviewed before photo correction');
            await text.blur();
            await page.getByRole('button', { name: 'Prepare photo', exact: true }).click();
            await page.getByRole('button', { name: 'Correct perspective', exact: true }).click();
            const corner = page.getByRole('button', { name: 'Top left corner', exact: true });
            const box = await corner.boundingBox();
            assert.ok(box && box.width >= 44 && box.height >= 44, 'Photo corner targets must remain at least 44 CSS pixels');
            await corner.focus();
            await page.keyboard.press('ArrowRight');
            await page.keyboard.press('Shift+ArrowDown');
            await page.getByRole('region', { name: 'Photo preparation', exact: true }).getByText('Exact corner positions', { exact: true }).click();
            const x = page.getByRole('spinbutton', { name: 'Top left corner X', exact: true });
            const y = page.getByRole('spinbutton', { name: 'Top left corner Y', exact: true });
            assert.equal(await x.inputValue(), '1');
            assert.equal(await y.inputValue(), '10');
            await x.fill('999');
            assert.equal(await page.getByRole('button', { name: 'Apply perspective', exact: true }).isEnabled(), false);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'transform').length), 0);
            await x.fill('40');
            await y.fill('40');
            await page.getByRole('button', { name: 'Apply perspective', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].imagePath.endsWith('-perspective'));
            const saved = await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0]);
            assert.equal(saved.originalImagePath, '/fixtures/northstar-page-1.png');
            assert.deepEqual([saved.width, saved.height], [900, 1300]);
            assert.equal(saved.status, 'pending');
            assert.equal(saved.regions.length, 0);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            await text.waitFor();
            assert.equal(await text.inputValue(), 'Invoice reviewed before photo correction');
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].imagePath === '/fixtures/northstar-page-1.png');
            assert.equal(await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0].regions[2].originalText), 'Inv0ice NS-2026-1042');
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
        });

        await test('measured tilt and lighting preparation preserve the original and remain undoable', 'review', async page => {
            await page.getByRole('button', { name: 'Prepare photo', exact: true }).click();
            await page.getByRole('button', { name: 'Check photo quality', exact: true }).click();
            const angle = page.getByRole('spinbutton', { name: /^Tilt correction \(degrees\)/ });
            await page.waitForFunction(() => [...document.querySelectorAll('input[type="number"]')].some(input => input.value === '-2.7'));
            await angle.fill('30');
            assert.equal(await page.getByRole('button', { name: 'Apply tilt correction', exact: true }).isEnabled(), false);
            await angle.fill('-2.7');
            await page.getByRole('button', { name: 'Apply tilt correction', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].imagePath.endsWith('-deskewed'));
            const corrected = await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0]);
            assert.equal(corrected.originalImagePath, '/fixtures/northstar-page-1.png');
            assert.equal(corrected.regions.length, 0);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.calls.find(call => call.method === 'transform').data.angle), -2.7);
            await page.getByRole('button', { name: 'Flatten shadows', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].imagePath.endsWith('-prepared'));
            const preparation = await page.evaluate(() => window.ocrSmoke.calls.find(call => call.method === 'prepare').data);
            assert.equal(preparation.path, corrected.imagePath);
            assert.equal(preparation.config.stages[0].cleanup.whiten_background, true);
            assert.equal(preparation.config.stages[0].cleanup.deskew, false);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.lastSaved?.pages[0].imagePath.endsWith('-deskewed'));
            assert.equal(await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0].originalImagePath), corrected.originalImagePath);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
        });

        await test('desktop and compact layouts remain usable', 'review', async page => {
            await page.getByRole('button', { name: 'Next issue', exact: true }).click();
            await assertFitPage(page, 'Desktop');
            await page.screenshot({ path: path.join(output, 'ocr-workspace-desktop.png'), fullPage: true });
            await page.setViewportSize({ width: 390, height: 844 });
            const nav = page.getByRole('navigation', { name: 'Workspace views', exact: true });
            await nav.getByRole('button', { name: 'Page image', exact: true }).click();
            await assertFitPage(page, 'Compact');
            await page.screenshot({ path: path.join(output, 'ocr-workspace-compact.png'), fullPage: true });
            const sizes = await page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth }));
            assert.ok(sizes.documentWidth <= sizes.width + 2, `Compact layout overflows: ${sizes.documentWidth} > ${sizes.width}`);
            await nav.getByRole('button', { name: /^Pages\b/ }).click();
            await page.getByRole('complementary', { name: 'Document pages', exact: true }).waitFor();
            assert.equal(await page.getByRole('button', { name: /^Page \d:/ }).count(), 3);
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            await nav.getByRole('button', { name: 'Text & tools', exact: true }).click();
            assert.equal(await page.getByRole('textbox', { name: 'Text for region 3', exact: true }).inputValue(), 'Invoice NS-2026-1042');
            await page.screenshot({ path: path.join(output, 'ocr-phone-text-review.png'), fullPage: true });
            await nav.getByRole('button', { name: /^Pages\b/ }).click();
            await page.screenshot({ path: path.join(output, 'ocr-phone-page-list.png'), fullPage: true });
            for (const button of await nav.getByRole('button').all()) {
                const bounds = await button.boundingBox();
                assert.ok(bounds && bounds.height >= 44, 'Phone view navigation must have touch-sized targets');
            }
            await page.setViewportSize({ width: 320, height: 667 });
            await nav.getByRole('button', { name: 'Page image', exact: true }).click();
            const narrow = await page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth }));
            assert.ok(narrow.documentWidth <= narrow.width + 2, `Narrow phone layout overflows: ${narrow.documentWidth} > ${narrow.width}`);
            assert.ok((await page.locator('.canvas-shell .viewport').boundingBox())?.height >= 80, 'Narrow phone retains room for the page image');
            await page.setViewportSize({ width: 844, height: 390 });
            await assertFitPage(page, 'Phone landscape');
            const landscape = await page.evaluate(() => {
                const preview = document.querySelector('.canvas-shell .viewport').getBoundingClientRect();
                return { width: innerWidth, height: innerHeight, documentWidth: document.documentElement.scrollWidth, previewTop: preview.top, previewBottom: preview.bottom };
            });
            assert.ok(landscape.documentWidth <= landscape.width + 2, 'Landscape phone must not overflow horizontally');
            assert.ok(landscape.previewTop >= 0 && landscape.previewBottom <= landscape.height, 'Landscape page preview must remain within the screen');
        });
    } catch (error) {
        results.push({ name: 'browser setup', status: 'failed', error: error.stack ?? String(error) });
        console.error(`Browser setup failed: ${error.message}`);
    } finally {
        await browser?.close();
        await server.close();
    }
    const report = { synthetic_ui_only: true, tests: results, browserErrors, phoneCornerEvidence, passed: results.filter(item => item.status === 'passed').length, failed: results.filter(item => item.status === 'failed').length, screenshots: ['ocr-workspace-desktop.png', 'ocr-workspace-compact.png', 'ocr-phone-model-setup.png', 'ocr-phone-model-storage.png', 'ocr-phone-model-removal-confirmation.png', 'ocr-phone-photo-preparation.png', 'ocr-phone-corner-focus.png', 'ocr-phone-import-progress.png', 'ocr-phone-export-progress.png', 'ocr-phone-text-review.png', 'ocr-phone-page-list.png'].filter(name => existsSync(path.join(output, name))) };
    await writeFile(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
    console.log(`Browser verification: ${report.passed} passed, ${report.failed} failed. Report: ${path.join(output, 'report.json')}`);
    if (report.failed || browserErrors.length) process.exitCode = 1;
}
