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
    console.log('Scenarios: ?scenario=review, queue-failure, queue-control, batch-recovery, extraction-delay, project-recovery, prepare-unchanged');
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
    let browser;
    try {
        browser = await chromium.launch({ headless: true, ...(browserPath ? { executablePath: browserPath } : {}) });
        async function fixture(scenario, size = { width: 1440, height: 1000 }) {
            const page = await browser.newPage({ viewport: size, deviceScaleFactor: 1 });
            page.setDefaultTimeout(10_000);
            page.on('pageerror', error => browserErrors.push({ scenario, message: error.message }));
            page.on('dialog', dialog => void dialog.dismiss());
            await page.goto(`${url}/?scenario=${scenario}`);
            await page.waitForFunction(() => {
                const input = document.querySelector('[aria-label="Project name"]');
                return input?.value === 'Northstar · quarterly statement' && !input.disabled;
            });
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
            assert.equal(await page.getByRole('spinbutton', { name: /^Tilt correction \(degrees\)/ }).inputValue(), '-2.7');
            await page.screenshot({ path: path.join(output, 'ocr-phone-photo-preparation.png'), fullPage: true });
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
            await page.getByText('Exact corner positions', { exact: true }).click();
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
    const report = { synthetic_ui_only: true, tests: results, browserErrors, passed: results.filter(item => item.status === 'passed').length, failed: results.filter(item => item.status === 'failed').length, screenshots: ['ocr-workspace-desktop.png', 'ocr-workspace-compact.png', 'ocr-phone-photo-preparation.png', 'ocr-phone-text-review.png', 'ocr-phone-page-list.png'].filter(name => existsSync(path.join(output, name))) };
    await writeFile(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
    console.log(`Browser verification: ${report.passed} passed, ${report.failed} failed. Report: ${path.join(output, 'report.json')}`);
    if (report.failed || browserErrors.length) process.exitCode = 1;
}
