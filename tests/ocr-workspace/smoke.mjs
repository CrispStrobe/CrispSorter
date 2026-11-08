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
    console.log('Scenarios: ?scenario=review, queue-failure, queue-control');
    for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => { await server.close(); process.exit(0); });
} else {
    await mkdir(output, { recursive: true });
    // Restricted workspaces may not expose /tmp. Keep browser profiles and
    // artifacts under the explicitly writable output directory.
    const browserTemp = path.join(output, 'tmp');
    await mkdir(browserTemp, { recursive: true });
    process.env.TMPDIR = browserTemp;
    let playwright;
    try { playwright = require('playwright'); }
    catch {
        const runtimeModules = process.env.CODEX_PRIMARY_RUNTIME_NODE_MODULES;
        if (!runtimeModules) throw new Error('Install playwright as a development tool before running this browser smoke harness.');
        playwright = require(path.join(runtimeModules, 'playwright'));
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

        await test('next issue navigates from uncertain text to an unrecognized page', 'review', async page => {
            await page.getByRole('button', { name: 'Next issue', exact: true }).click();
            assert.equal(await page.locator('.region-card.selected textarea').inputValue(), 'Inv0ice NS-2026-1042');
            await page.getByRole('button', { name: 'Next issue', exact: true }).click();
            await page.getByRole('heading', { name: 'Ready to recognize', exact: true }).waitFor();
            assert.equal(await page.getByRole('button', { name: /^Page 2:/ }).getAttribute('aria-current'), 'page');
            await page.keyboard.press('F8');
            assert.equal(await page.getByRole('button', { name: /^Page 1:/ }).getAttribute('aria-current'), 'page');
        });

        await test('export preflight blocks incomplete pages and exports explicit approved scope', 'review', async page => {
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByText(/recognition is pending/).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.exports.length), 0);
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            await page.getByLabel('Pages', { exact: true }).selectOption('current');
            await page.getByLabel('Output format', { exact: true }).selectOption('text');
            await page.getByRole('button', { name: 'Save Plain text', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const request = await page.evaluate(() => window.ocrSmoke.exports[0]);
            assert.equal(request.pages.length, 1);
            assert.equal(request.pages[0].image_path, '/fixtures/northstar-page-3.png');
            assert.equal(request.pages[0].regions[2].text, 'Invoice NS-2026-1042');
            assert.equal(request.overwrite, false);
            assert.deepEqual(request.sourceUris, ['/fixtures/northstar-statement.pdf']);
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

        await test('desktop and compact layouts remain usable', 'review', async page => {
            await page.getByRole('button', { name: 'Next issue', exact: true }).click();
            await page.screenshot({ path: path.join(output, 'ocr-workspace-desktop.png'), fullPage: true });
            await page.setViewportSize({ width: 390, height: 844 });
            await page.screenshot({ path: path.join(output, 'ocr-workspace-compact.png'), fullPage: true });
            const sizes = await page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth }));
            assert.ok(sizes.documentWidth <= sizes.width + 2, `Compact layout overflows: ${sizes.documentWidth} > ${sizes.width}`);
            assert.ok(await page.getByRole('textbox', { name: 'Text for region 3', exact: true }).isVisible());
        });
    } catch (error) {
        results.push({ name: 'browser setup', status: 'failed', error: error.stack ?? String(error) });
        console.error(`Browser setup failed: ${error.message}`);
    } finally {
        await browser?.close();
        await server.close();
    }
    const report = { tests: results, browserErrors, passed: results.filter(item => item.status === 'passed').length, failed: results.filter(item => item.status === 'failed').length, screenshots: ['ocr-workspace-desktop.png', 'ocr-workspace-compact.png'].filter(name => existsSync(path.join(output, name))) };
    await writeFile(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
    console.log(`Browser verification: ${report.passed} passed, ${report.failed} failed. Report: ${path.join(output, 'report.json')}`);
    if (report.failed || browserErrors.length) process.exitCode = 1;
}
