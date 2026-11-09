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
    console.log('Scenarios: ?scenario=review, word-review, native-word, proofreading, proofreading-save-failure, proofreading-load-failure, text-repair, structure, page-draft, zone-empty, zone-delay, queue-failure, queue-control, extraction-delay, extraction-objects, table-continuation, office-export, project-recovery, prepare-unchanged');
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
        async function wordColors(word) {
            return word.evaluate(element => ({
                text: getComputedStyle(element.querySelector('.word-text') ?? element).color,
                background: getComputedStyle(element).backgroundColor,
            }));
        }
        async function wordReadingText(words) {
            return words.locator('[aria-label="OCR words in reading order"]').evaluate(element => {
                const copy = element.cloneNode(true);
                copy.querySelectorAll('.word-score').forEach(score => score.remove());
                return copy.textContent;
            });
        }
        async function saveSnapshot(page) {
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(() => document.querySelector('.save-state')?.getAttribute('data-save-state') === 'saved');
            await page.getByText('Project saved', { exact: true }).waitFor();
            return page.evaluate(() => window.ocrSmoke.lastSaved ?? window.ocrSmoke.initialProject);
        }
        async function beginAreaDrag(page) {
            await page.getByRole('button', { name: 'Re-OCR area', exact: true }).click();
            const paper = await page.locator('.canvas-shell .paper').boundingBox();
            assert.ok(paper);
            // Both points are strictly inside source region 3; the request must
            // expand this small rectangle to cover the full existing passage.
            await page.mouse.move(paper.x + paper.width * 100 / 1000, paper.y + paper.height * 280 / 1400);
            await page.mouse.down();
            await page.mouse.move(paper.x + paper.width * 150 / 1000, paper.y + paper.height * 295 / 1400);
        }
        async function enableStructure(page) {
            await page.getByRole('button', { name: 'Structure', exact: true }).click();
            const panel = page.locator('[data-structure-panel]');
            await panel.getByRole('button', { name: 'Enable document structure', exact: true }).click();
            const saved = await saveSnapshot(page);
            return { panel, saved, blocks: saved.pages[0].structure.blocks };
        }
        async function expandStructureDetail(panel, name) {
            const summary = panel.getByText(name, { exact: true });
            if (!await summary.evaluate(element => element.closest('details').open)) await summary.click();
        }

        async function reopenFixtureProject(page) {
            const loads = await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'load').length);
            await page.getByRole('button', { name: 'New', exact: true }).click();
            await page.getByText('New project ready', { exact: true }).waitFor();
            await page.getByRole('button', { name: 'Projects', exact: true }).click();
            await page.getByRole('region', { name: 'Saved projects', exact: true }).getByRole('button', { name: /Northstar · quarterly statement/ }).click();
            await page.getByText('Project restored', { exact: true }).waitFor();
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'load').length), loads + 1);
        }
        async function pasteSelection(page, editor, start, end, text) {
            await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
            await page.evaluate(value => navigator.clipboard.writeText(value), text);
            await editor.evaluate((input, range) => { input.focus(); input.setSelectionRange(...range); }, [start, end]);
            await page.keyboard.press((process.platform === 'darwin' ? 'Meta' : 'Control') + '+v');
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

        await test('word review selects Unicode ranges and preserves corrections through history, save and export', 'word-review', async page => {
            const original = '😀 Inv0ice NS-2026-1042';
            const corrected = '😀 Invoice NS-2026-1042';
            const editor = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            const words = page.locator('[aria-label="Word confidence for selected region"]');
            const invoice = words.getByRole('button', { name: /^Select word \d+: Inv0ice$/ });
            await invoice.waitFor();
            assert.equal(await words.getByRole('checkbox', { name: 'Show scores', exact: true }).isChecked(), false);
            assert.equal(await wordReadingText(words), original);
            await words.getByRole('checkbox', { name: 'Show scores', exact: true }).check();
            assert.equal(await wordReadingText(words), original);
            assert.equal(await invoice.getAttribute('data-word-basis'), 'characters');
            assert.equal(Math.round(Number(await invoice.getAttribute('data-word-confidence')) * 100), 87);
            assert.equal(await invoice.getAttribute('data-word-flagged'), 'true');
            assert.equal(await invoice.getAttribute('data-word-confidence-band'), 'low');
            assert.match(await invoice.innerText(), /87%/);
            assert.match(await invoice.getAttribute('title'), /20%/);
            const medium = words.getByRole('button', { name: /^Select word \d+: NS$/ });
            const high = words.getByRole('button', { name: /^Select word \d+: 2026$/ });
            assert.equal(await medium.getAttribute('data-word-confidence-band'), 'medium');
            assert.equal(await medium.getAttribute('data-word-flagged'), 'true');
            assert.equal(await high.getAttribute('data-word-confidence-band'), 'high');
            assert.equal(await high.getAttribute('data-word-flagged'), 'false');
            const bandColors = await Promise.all([invoice, medium, high].map(wordColors));
            assert.equal(new Set(bandColors.map(color => color.text)).size, 3, 'Low, medium and high words must have different text colors');
            assert.equal(new Set(bandColors.map(color => color.background)).size, 3, 'Low, medium and high words must have different background colors');
            // A high mean must not hide an uncertain character on the source image.
            assert.match(await page.getByRole('button', { name: /^Region 3, text, needs review:/ }).getAttribute('class'), /\blow\b/);
            await words.getByRole('button', { name: 'Next word to check', exact: true }).click();
            assert.deepEqual(await editor.evaluate(input => ({
                start: input.selectionStart, end: input.selectionEnd,
                text: input.value.slice(input.selectionStart, input.selectionEnd), focused: document.activeElement === input,
            })), { start: 3, end: 10, text: 'Inv0ice', focused: true });

            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await invoice.click();
            await page.keyboard.type('Invoice');
            await editor.blur();
            assert.equal(await editor.inputValue(), corrected);
            await page.getByRole('button', { name: 'Approve page', exact: true }).waitFor();
            assert.match(await page.getByRole('button', { name: /^Region 3, text, needs review:/ }).getAttribute('class'), /\blow\b/);
            assert.ok(await words.locator('button[data-word-basis="edited"]').count() > 0);
            assert.equal(await words.locator('button[data-word-basis="characters"]').count(), 0);
            const editedWord = words.getByRole('button', { name: /^Select word \d+: Invoice$/ });
            assert.equal(await editedWord.getAttribute('data-word-confidence-band'), 'edited');
            const editedColors = await wordColors(editedWord);
            assert.ok(bandColors.every(color => color.text !== editedColors.text && color.background !== editedColors.background), 'Edited words need a distinct color from recognition scores');
            assert.equal(await wordReadingText(words), corrected);
            await page.getByRole('button', { name: 'Select region 4', exact: true }).click();
            const unavailableColors = await wordColors(words.locator('button[data-word-confidence-band="unscored"]').first());
            assert.notEqual(unavailableColors.text, editedColors.text, 'Unscored text must differ from manually edited text');
            assert.notEqual(unavailableColors.background, editedColors.background, 'Unscored background must differ from manually edited text');
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();

            await words.getByRole('button', { name: 'Original OCR', exact: true }).click();
            assert.equal(await words.getByRole('button', { name: /^Select word / }).count(), 0);
            const originalWord = words.getByRole('button', { name: /^Inspect original word \d+: Inv0ice$/ });
            assert.equal(Math.round(Number(await originalWord.getAttribute('data-word-confidence')) * 100), 87);
            assert.equal(await originalWord.getAttribute('data-word-confidence-band'), 'low');
            assert.deepEqual(await wordColors(originalWord), bandColors[0]);
            assert.equal(await wordReadingText(words), original);
            const priorSelection = await editor.evaluate(input => [input.selectionStart, input.selectionEnd]);
            assert.ok(await originalWord.evaluate(element => element.tabIndex >= 0));
            await originalWord.press('Enter');
            const originalDetail = words.getByRole('status');
            await originalDetail.waitFor();
            assert.match(await originalDetail.innerText(), /Low confidence.*Mean original character score: 87%.*Lowest character score: 20%/);
            assert.equal(await originalWord.evaluate(element => document.activeElement === element), true);
            assert.equal(await editor.inputValue(), corrected);
            assert.deepEqual(await editor.evaluate(input => [input.selectionStart, input.selectionEnd]), priorSelection);
            await words.getByRole('button', { name: 'Current text', exact: true }).click();
            await page.getByRole('button', { name: 'Save', exact: true }).click();
            await page.waitForFunction(value => window.ocrSmoke.lastSaved?.pages[0].regions[2].text === value, corrected);
            const saved = await page.evaluate(() => window.ocrSmoke.lastSaved.pages[0]);
            assert.equal(saved.approved, false);
            assert.equal(saved.regions[2].review, 'corrected');
            assert.equal(saved.regions[2].originalText, original);
            assert.equal(saved.regions[2].confidence, 0.99);
            assert.equal(saved.regions[2].char_conf[5], 0.20);
            assert.equal(saved.regions[2].char_conf.length, Array.from(original).length);

            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.equal(await editor.inputValue(), original);
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            assert.equal(await invoice.getAttribute('data-word-basis'), 'characters');
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            assert.equal(await editor.inputValue(), corrected);
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            assert.ok(await words.locator('button[data-word-basis="edited"]').count() > 0);

            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('text');
            await page.getByRole('button', { name: 'Save Plain text', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const exported = await page.evaluate(() => window.ocrSmoke.exports[0].pages[0].regions[2]);
            assert.equal(exported.text, corrected);
            assert.equal(exported.original_text, original);
            assert.equal(exported.char_conf_aligned, false);
            assert.deepEqual(exported.char_conf, []);
            assert.equal(exported.original_char_conf_aligned, true);
            assert.deepEqual(exported.original_char_conf, saved.regions[2].char_conf);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
        });

        await test('native word evidence selects genuine source boxes and survives correction without stale scores', 'native-word', async page => {
            const original = '😀 Inv0ice NS-2026-1042';
            const corrected = '😀 Invoice NS-2026-1042';
            const editor = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            const words = page.locator('[aria-label="Word confidence for selected region"]');
            const invoice = words.getByRole('button', { name: 'Select word 1: Inv0ice', exact: true });
            const compound = words.getByRole('button', { name: 'Select word 2: NS-2026-1042', exact: true });
            await invoice.waitFor();
            assert.equal(await words.locator('button[data-word-basis="word"]').count(), 2);
            assert.equal(await wordReadingText(words), original);
            await words.getByRole('checkbox', { name: 'Show scores', exact: true }).check();
            assert.equal(await invoice.getAttribute('data-word-confidence'), '0.42');
            assert.equal(await invoice.getAttribute('data-word-confidence-band'), 'low');
            assert.equal(await compound.getAttribute('data-word-confidence'), null);
            assert.equal(await compound.getAttribute('data-word-confidence-band'), 'unscored');
            assert.equal(await compound.getAttribute('data-word-flagged'), 'true');
            assert.equal(await invoice.locator('.word-score').innerText(), '42%');
            assert.equal(await compound.locator('.word-score').innerText(), 'Unscored');
            assert.match(await invoice.getAttribute('title'), /Original word recognition score: 42%/);
            assert.doesNotMatch(await invoice.getAttribute('title'), /mean|lowest character/i);
            assert.match(await page.getByRole('button', { name: /^Region 3, text, needs review:/ }).getAttribute('class'), /\blow\b/);
            const source = page.locator('rect[data-word-source]');
            async function assertSourceWord(expected) {
                await page.waitForFunction(start => document.querySelector('rect[data-word-source]')?.getAttribute('data-word-source-start') === String(start), expected.start);
                const actual = await source.evaluate(element => Object.fromEntries(
                    ['x', 'y', 'width', 'height', 'data-word-source-start', 'data-word-source-end'].map(name => [name, Number(element.getAttribute(name))]),
                ));
                assert.deepEqual(actual, { x: expected.x, y: expected.y, width: expected.w, height: expected.h,
                    'data-word-source-start': expected.start, 'data-word-source-end': expected.end });
                const loupe = page.locator('[aria-label="Magnified selected source text"][data-word-source-loupe="true"]');
                await loupe.waitFor();
                const [x, y, w, h] = (await loupe.getAttribute('viewBox')).trim().split(/[\s,]+/).map(Number);
                assert.ok(x <= expected.x && y <= expected.y && x + w >= expected.x + expected.w && y + h >= expected.y + expected.h);
                assert.ok(w < 640 && h < 1400, 'The word loupe must crop the genuine word rather than the entire source passage');
            }
            const invoiceBox = { start: 3, end: 10, x: 120, y: 270, w: 115, h: 40 };
            await invoice.click();
            assert.deepEqual(await editor.evaluate(input => ({ start: input.selectionStart, end: input.selectionEnd,
                text: input.value.slice(input.selectionStart, input.selectionEnd), focused: document.activeElement === input })),
                { start: 3, end: 10, text: 'Inv0ice', focused: true });
            await assertSourceWord(invoiceBox);
            await page.screenshot({ path: path.join(output, 'ocr-native-word-review.png'), fullPage: true });
            await compound.click();
            assert.deepEqual(await editor.evaluate(input => [input.selectionStart, input.selectionEnd,
                input.value.slice(input.selectionStart, input.selectionEnd)]), [11, 23, 'NS-2026-1042']);
            await assertSourceWord({ start: 11, end: 23, x: 250, y: 270, w: 240, h: 40 });
            const originalEvidence = await page.evaluate(() => window.ocrSmoke.initialProject.pages[0].regions[2].wordEvidence);
            assert.equal(originalEvidence.source, 'tesseract_tsv');
            assert.equal(originalEvidence.words.length, 2);
            assert.ok(originalEvidence.words.every(word => !Object.hasOwn(word, 'alternatives')), 'No alternatives may be invented when the provider supplies none');

            await invoice.click();
            await page.keyboard.type('Invoice');
            await editor.blur();
            assert.equal(await editor.inputValue(), corrected);
            assert.equal(await words.locator('button[data-word-basis="word"]').count(), 0);
            assert.ok(await words.locator('button[data-word-basis="edited"]').count() > 0);
            await source.waitFor({ state: 'detached' });
            assert.equal(await page.locator('[data-word-source-loupe="true"]').count(), 0);
            await words.getByRole('button', { name: 'Original OCR', exact: true }).click();
            const originalWord = words.getByRole('button', { name: 'Inspect original word 1: Inv0ice', exact: true });
            const priorSelection = await editor.evaluate(input => [input.selectionStart, input.selectionEnd]);
            await originalWord.press('Enter');
            await assertSourceWord(invoiceBox);
            assert.match(await words.getByRole('status').innerText(), /Original word recognition score: 42%/);
            assert.equal(await originalWord.evaluate(element => document.activeElement === element), true);
            assert.equal(await editor.inputValue(), corrected);
            assert.deepEqual(await editor.evaluate(input => [input.selectionStart, input.selectionEnd]), priorSelection);
            await words.getByRole('button', { name: 'Current text', exact: true }).click();
            await source.waitFor({ state: 'detached' });
            const saved = await saveSnapshot(page);
            assert.deepEqual(saved.pages[0].regions[2].wordEvidence, originalEvidence);
            assert.equal(saved.pages[0].regions[2].originalText, original);

            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            await page.waitForFunction(value => document.querySelector('[aria-label="Text for region 3"]')?.value === value, original);
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            await invoice.click();
            await assertSourceWord(invoiceBox);
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            await page.waitForFunction(value => document.querySelector('[aria-label="Text for region 3"]')?.value === value, corrected);
            await source.waitFor({ state: 'detached' });
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('json');
            await page.getByRole('button', { name: 'Save Structured JSON', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const exported = await page.evaluate(() => window.ocrSmoke.exports[0].pages[0].regions[2]);
            assert.equal(exported.text, corrected);
            assert.equal(exported.original_text, original);
            assert.equal(Object.hasOwn(exported, 'word_evidence'), false);
            assert.deepEqual(exported.original_word_evidence, originalEvidence);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
        });

        await test('proofreading decisions retain native scores and accepted suggestions undo without rolling back user vocabulary', 'proofreading', async page => {
            const original = '😀 Inv0ice NS-2026-1042';
            const corrected = '😀 Invoice NS-2026-1042';
            const editor = page.locator('textarea[aria-label="Text for region 3"]');
            const words = page.locator('[aria-label="Word confidence for selected region"]');
            const invoice = words.getByRole('button', { name: 'Select word 1: Inv0ice', exact: true });
            for (const action of ['Add to vocabulary', 'Ignore suggestion', 'Protect identifier']) {
                await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
                await invoice.click();
                await page.getByRole('combobox', { name: 'Save selected term in', exact: true }).selectOption('project');
                await page.getByRole('button', { name: action, exact: true }).click();
                const saved = await saveSnapshot(page);
                assert.equal(await editor.inputValue(), original);
                assert.equal(await invoice.getAttribute('data-word-confidence'), '0.42');
                assert.equal(await invoice.getAttribute('data-word-confidence-band'), 'low');
                assert.equal(saved.pages[0].regions[2].review, 'unreviewed');
                assert.equal(saved.pages[0].regions[2].wordEvidence.words[0].confidence, 0.42);
                if (action === 'Ignore suggestion') assert.equal(saved.proofreading.ignored[0].text, 'Inv0ice');
                else assert.ok(saved.proofreading.entries.some(entry => entry.text === 'Inv0ice' && entry.kind === (action === 'Protect identifier' ? 'protected' : 'word')));
                await page.getByRole('button', { name: 'Undo', exact: true }).click();
            }
            await page.getByRole('button', { name: 'Vocabulary and rules', exact: true }).click();
            const manager = page.locator('[data-proofreading-manager]');
            await manager.getByRole('combobox', { name: 'Vocabulary scope', exact: true }).selectOption('user');
            await manager.getByRole('textbox', { name: 'Vocabulary term', exact: true }).fill('Invoice');
            await manager.getByRole('textbox', { name: 'Entry language', exact: true }).fill('und');
            await manager.getByRole('button', { name: 'Add entry', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.userProofreading.entries.some(entry => entry.text === 'Invoice'));
            await manager.locator('[data-proofreading-user-status="saved"]').waitFor();
            const userSaved = await page.evaluate(() => window.ocrSmoke.userProofreading);
            await manager.getByRole('button', { name: 'Close vocabulary and rules', exact: true }).click();
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            await invoice.click();
            const suggestion = page.getByRole('button', { name: 'Use suggestion: Invoice', exact: true });
            await suggestion.click();
            const preview = page.getByRole('dialog', { name: 'Review proposed corrections', exact: true });
            await preview.waitFor();
            assert.equal(await editor.inputValue(), original);
            assert.equal(await preview.getByRole('checkbox', { name: 'Apply correction 1: Inv0ice → Invoice', exact: true }).isChecked(), true);
            await page.screenshot({ path: path.join(output, 'ocr-proofreading-preview.png'), fullPage: true });
            await preview.getByRole('button', { name: 'Cancel', exact: true }).click();
            assert.equal(await editor.inputValue(), original);
            assert.equal(await invoice.getAttribute('data-word-confidence'), '0.42');
            await suggestion.click();
            await preview.getByRole('button', { name: 'Apply 1 correction', exact: true }).click();
            await preview.waitFor({ state: 'hidden' });
            assert.equal(await editor.inputValue(), corrected);
            const correctedProject = await saveSnapshot(page);
            assert.equal(correctedProject.pages[0].regions[2].originalText, original);
            assert.equal(correctedProject.pages[0].regions[2].wordEvidence.words[0].confidence, 0.42);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            await page.waitForFunction(value => document.querySelector('[aria-label="Text for region 3"]')?.value === value, original);
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            assert.equal(await invoice.getAttribute('data-word-confidence'), '0.42');
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.userProofreading), userSaved);
            const restored = await saveSnapshot(page);
            assert.equal(restored.pages[0].regions[2].text, original);
            assert.equal(restored.proofreading?.entries.some(entry => entry.text === 'Invoice') ?? false, false);
        });

        await test('literal rule previews honor scope and protected identifiers and apply only selected occurrences', 'proofreading', async page => {
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            const words = page.locator('[aria-label="Word confidence for selected region"]');
            const identifier = words.getByRole('button', { name: 'Select word 2: NS-2026-1042', exact: true });
            await identifier.click();
            await page.getByRole('combobox', { name: 'Save selected term in', exact: true }).selectOption('project');
            await page.getByRole('button', { name: 'Protect identifier', exact: true }).click();
            const before = await saveSnapshot(page);
            assert.equal(await identifier.getAttribute('data-word-confidence-band'), 'unscored');
            assert.ok(before.proofreading.entries.some(entry => entry.text === 'NS-2026-1042' && entry.kind === 'protected'));
            await page.getByRole('button', { name: 'Vocabulary and rules', exact: true }).click();
            const manager = page.locator('[data-proofreading-manager]');
            await manager.getByRole('combobox', { name: 'Vocabulary scope', exact: true }).selectOption('user');
            await manager.getByRole('button', { name: 'Correction rules', exact: true }).click();
            await manager.getByRole('textbox', { name: 'Rule name', exact: true }).fill('OCR zero correction');
            await manager.getByRole('textbox', { name: 'Find literal text', exact: true }).fill('0');
            await manager.getByRole('textbox', { name: 'Replace with', exact: true }).fill('o');
            await manager.getByRole('textbox', { name: 'Rule language', exact: true }).fill('und');
            await manager.getByRole('checkbox', { name: 'Whole words only', exact: true }).uncheck();
            await manager.getByRole('button', { name: 'Add rule', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.userProofreading.rules.some(rule => rule.name === 'OCR zero correction'));
            await manager.locator('[data-proofreading-user-status="saved"]').waitFor();
            await manager.getByRole('combobox', { name: 'Correction scope', exact: true }).selectOption('page');
            await manager.getByRole('button', { name: 'Preview rule: OCR zero correction', exact: true }).click();
            const preview = page.getByRole('dialog', { name: 'Review proposed corrections', exact: true });
            await preview.waitFor();
            assert.equal(Number(await preview.locator('[data-skipped-approved]').getAttribute('data-skipped-approved')), 0);
            assert.ok(Number(await preview.locator('[data-skipped-protected]').getAttribute('data-skipped-protected')) > 0);
            await preview.getByRole('button', { name: 'Cancel', exact: true }).click();
            await manager.getByRole('combobox', { name: 'Correction scope', exact: true }).selectOption('project');
            await manager.getByRole('button', { name: 'Preview rule: OCR zero correction', exact: true }).click();
            await preview.waitFor();
            assert.ok(Number(await preview.locator('[data-skipped-approved]').getAttribute('data-skipped-approved')) > 0);
            assert.ok(Number(await preview.locator('[data-skipped-protected]').getAttribute('data-skipped-protected')) > 0);
            assert.ok(await preview.getByRole('checkbox', { name: /^Apply correction / }).count() > 1);
            await preview.getByRole('checkbox', { name: 'Select all corrections', exact: true }).uncheck();
            await preview.getByRole('checkbox', { name: 'Apply correction 1: 0 → o', exact: true }).check();
            await preview.getByRole('button', { name: 'Apply 1 correction', exact: true }).click();
            await preview.waitFor({ state: 'hidden' });
            await manager.getByRole('button', { name: 'Close vocabulary and rules', exact: true }).click();
            const saved = await saveSnapshot(page);
            assert.equal(saved.pages[0].regions[2].text, '😀 Invoice NS-2026-1042');
            assert.deepEqual(saved.pages[0].regions.filter((_, index) => index !== 2), before.pages[0].regions.filter((_, index) => index !== 2));
            assert.deepEqual(saved.pages[2], before.pages[2]);
            assert.deepEqual(saved.pages[0].regions[2].wordEvidence, before.pages[0].regions[2].wordEvidence);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            const restored = await saveSnapshot(page);
            assert.deepEqual(restored.pages, before.pages);
            assert.deepEqual(restored.proofreading, before.proofreading);
            assert.ok(await page.evaluate(() => window.ocrSmoke.userProofreading.rules.some(rule => rule.name === 'OCR zero correction')));
        });

        await test('user vocabulary save failures retain the draft and retry without replacing existing terms', 'proofreading-save-failure', async page => {
            await page.getByRole('button', { name: 'Vocabulary and rules', exact: true }).click();
            const manager = page.locator('[data-proofreading-manager]');
            await manager.getByRole('combobox', { name: 'Vocabulary scope', exact: true }).selectOption('user');
            await manager.getByText('RetainedTerm', { exact: true }).waitFor();
            await manager.getByRole('textbox', { name: 'Vocabulary term', exact: true }).fill('Invoice');
            await manager.getByRole('button', { name: 'Add entry', exact: true }).click();
            await manager.locator('[data-proofreading-user-status="error"]').waitFor();
            assert.equal(await manager.getByText('Invoice', { exact: true }).count(), 1);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.userProofreading.entries.map(entry => entry.text)), ['RetainedTerm']);
            assert.equal(await page.evaluate(() => window.ocrSmoke.userProofreadingSaves.length), 0);
            await manager.getByRole('button', { name: 'Retry saving vocabulary', exact: true }).click();
            await manager.locator('[data-proofreading-user-status="saved"]').waitFor();
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.userProofreading.entries.map(entry => entry.text).sort()), ['Invoice', 'RetainedTerm']);
            assert.equal(await page.evaluate(() => window.ocrSmoke.userProofreadingSaves.length), 1);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'saveUserProofreading').length), 2);
        });

        await test('a user vocabulary load failure never overwrites unread preferences and retry restores them', 'proofreading-load-failure', async page => {
            await page.getByRole('button', { name: 'Vocabulary and rules', exact: true }).click();
            const manager = page.locator('[data-proofreading-manager]');
            await manager.locator('[data-proofreading-load-error]').waitFor();
            await manager.getByRole('combobox', { name: 'Vocabulary scope', exact: true }).selectOption('user');
            assert.equal(await manager.getByRole('textbox', { name: 'Vocabulary term', exact: true }).isDisabled(), true);
            assert.equal(await manager.getByRole('button', { name: 'Import JSON', exact: true }).isDisabled(), true);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'saveUserProofreading').length), 0);
            await manager.getByRole('combobox', { name: 'Vocabulary scope', exact: true }).selectOption('project');
            await manager.getByRole('textbox', { name: 'Vocabulary term', exact: true }).fill('ProjectTerm');
            await manager.getByRole('button', { name: 'Add entry', exact: true }).click();
            const saved = await saveSnapshot(page);
            assert.ok(saved.proofreading.entries.some(entry => entry.text === 'ProjectTerm'));
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.userProofreading.entries.map(entry => entry.text)), ['RetainedTerm']);
            assert.equal(await page.evaluate(() => window.ocrSmoke.userProofreadingSaves.length), 0);
            await manager.getByRole('button', { name: 'Retry loading vocabulary', exact: true }).click();
            await manager.locator('[data-proofreading-user-status="saved"]').waitFor();
            await manager.getByRole('combobox', { name: 'Vocabulary scope', exact: true }).selectOption('user');
            await manager.getByText('RetainedTerm', { exact: true }).waitFor();
            assert.equal(await manager.getByRole('textbox', { name: 'Vocabulary term', exact: true }).isDisabled(), false);
            assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'saveUserProofreading').length), 0);
        });

        await test('word review leaves token and detector evidence unscored and fits compact screens', 'word-review', async page => {
            const words = page.locator('[aria-label="Word confidence for selected region"]');
            let unscoredColors;
            for (const region of [4, 5, 6]) {
                await page.getByRole('button', { name: `Select region ${region}`, exact: true }).click();
                await words.waitFor();
                assert.ok(await words.locator('button[data-word-basis="unavailable"]').count() > 0);
                assert.equal(await words.locator('[data-word-basis="characters"]').count(), 0);
                assert.ok(await words.locator('button[data-word-confidence-band="unscored"]').count() > 0);
                const currentColors = await wordColors(words.locator('button[data-word-basis="unavailable"]').first());
                if (unscoredColors) assert.deepEqual(currentColors, unscoredColors);
                unscoredColors = currentColors;
                assert.equal(await wordReadingText(words), await page.getByRole('textbox', { name: `Text for region ${region}`, exact: true }).inputValue());
                await words.getByRole('checkbox', { name: 'Show scores', exact: true }).check();
                assert.ok(await words.getByText('Unscored', { exact: true }).count() > 0);
                assert.equal(await page.locator('.region-card.selected [aria-label="Original text with character confidence"]').count(), 0);
            }
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            const medium = words.getByRole('button', { name: /^Select word \d+: NS$/ });
            const high = words.getByRole('button', { name: /^Select word \d+: 2026$/ });
            const highColors = await wordColors(high);
            for (const band of ['low', 'medium', 'high']) {
                const colors = await wordColors(words.locator(`[data-word-confidence-band="${band}"]`).first());
                assert.notEqual(colors.text, unscoredColors.text, `${band} text must differ from unavailable confidence`);
                assert.notEqual(colors.background, unscoredColors.background, `${band} background must differ from unavailable confidence`);
            }
            // Changing the project threshold updates both colors and review flags.
            await page.getByRole('tab', { name: 'Recognize', exact: true }).click();
            await page.getByRole('slider', { name: /^Flag recognition scores below/ }).evaluate(input => {
                input.value = '0.65';
                input.dispatchEvent(new Event('change', { bubbles: true }));
            });
            await page.getByRole('tab', { name: 'Review', exact: true }).click();
            assert.equal(await medium.getAttribute('data-word-confidence-band'), 'high');
            assert.equal(await medium.getAttribute('data-word-flagged'), 'false');
            assert.deepEqual(await wordColors(medium), highColors);
            assert.equal(await words.getByRole('button', { name: /^Select word \d+: Inv0ice$/ }).getAttribute('data-word-confidence-band'), 'low');
            // Restore the usual threshold before saving representative screenshots.
            await page.getByRole('tab', { name: 'Recognize', exact: true }).click();
            await page.getByRole('slider', { name: /^Flag recognition scores below/ }).evaluate(input => {
                input.value = '0.85';
                input.dispatchEvent(new Event('change', { bubbles: true }));
            });
            await page.getByRole('tab', { name: 'Review', exact: true }).click();
            assert.equal(await medium.getAttribute('data-word-confidence-band'), 'medium');
            await page.screenshot({ path: path.join(output, 'ocr-word-review-desktop.png'), fullPage: true });
            await page.setViewportSize({ width: 390, height: 844 });
            await words.scrollIntoViewIfNeeded();
            await page.screenshot({ path: path.join(output, 'ocr-word-review-compact.png'), fullPage: true });
            const sizes = await words.evaluate(element => ({
                viewportWidth: innerWidth, documentWidth: document.documentElement.scrollWidth,
                contentWidth: element.scrollWidth, width: element.clientWidth,
            }));
            assert.ok(sizes.documentWidth <= sizes.viewportWidth + 2, `Compact word review overflows the page: ${JSON.stringify(sizes)}`);
            assert.ok(sizes.contentWidth <= sizes.width + 2, `Word controls overflow their panel: ${JSON.stringify(sizes)}`);
            const invoice = words.getByRole('button', { name: /^Select word \d+: Inv0ice$/ });
            assert.ok(await invoice.isVisible());
            await invoice.click();
            assert.equal(await page.getByRole('textbox', { name: 'Text for region 3', exact: true }).evaluate(input => input.value.slice(input.selectionStart, input.selectionEnd)), 'Inv0ice');
        });

        await test('passage editing retains multiline text and source evidence through save, history and export', 'word-review', async page => {
            await page.getByRole('button', { name: 'Select region 3', exact: true }).click();
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            const before = await saveSnapshot(page);
            const original = before.pages[0].regions[2];
            await page.locator('.region-card.selected').getByRole('button', { name: 'Edit passage', exact: true }).click();
            const editor = page.getByRole('textbox', { name: 'Passage 3 text', exact: true });
            await editor.waitFor();
            assert.equal(await editor.evaluate(input => document.activeElement === input), true);
            const corrected = 'Invoice NS-2026-1042 was checked against the source.\nThe complete sentence can be corrected here.\n\nA new paragraph retains its line breaks.';
            await editor.fill(corrected);
            await editor.blur();
            assert.ok(await page.getByRole('button', { name: 'Copy page text', exact: true }).isVisible());
            await page.screenshot({ path: path.join(output, 'ocr-passage-editor.png'), fullPage: true });
            const saved = await saveSnapshot(page);
            const region = saved.pages[0].regions[2];
            assert.equal(region.text, corrected);
            assert.equal(region.review, 'corrected');
            assert.equal(saved.pages[0].approved, false);
            for (const key of ['id', 'x', 'y', 'w', 'h', 'confidence', 'confidenceBasis', 'originalText', 'char_conf', 'charConfidenceUnit', 'charConfidenceAligned']) {
                assert.deepEqual(region[key], original[key], `Passage correction changed original ${key}`);
            }
            assert.equal(saved.pages[0].imagePath, before.pages[0].imagePath);
            assert.equal(saved.pages[0].originalImagePath, before.pages[0].originalImagePath);
            assert.equal(saved.pages[0].documentId, before.pages[0].documentId);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.equal(await editor.inputValue(), original.text);
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            assert.equal(await editor.inputValue(), corrected);
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('text');
            await page.getByRole('button', { name: 'Save Plain text', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const exported = await page.evaluate(() => window.ocrSmoke.exports[0].pages[0]);
            assert.equal(exported.regions[2].text, corrected);
            assert.equal(exported.regions[2].original_text, original.originalText);
            assert.deepEqual(exported.regions[2].original_char_conf, original.char_conf);
            assert.deepEqual(exported.regions[2].char_conf, []);
            assert.equal(exported.image_path, before.pages[0].imagePath);
        });

        await test('passage splits and adjacent merges preserve exact text and flat original source components', 'native-word', async page => {
            const initial = await page.evaluate(() => window.ocrSmoke.initialProject.pages[0]);
            const original = initial.regions[2];
            const originalText = original.text;
            let component;
            let roundTrip;
            async function passage() {
                await page.getByRole('button', { name: 'Text editor', exact: true }).click();
                return page.locator('[data-passage-id]').nth(2);
            }
            for (let iteration = 0; iteration < 2; iteration++) {
                const card = await passage();
                await card.getByRole('textbox', { name: 'Passage 3 text', exact: true }).evaluate(input => {
                    input.focus(); input.setSelectionRange(10, 10);
                });
                await card.getByRole('button', { name: 'Split passage', exact: true }).click();
                const split = page.getByRole('dialog', { name: 'Split passage at caret', exact: true });
                assert.equal(await split.getByRole('button', { name: 'Apply split', exact: true }).isDisabled(), true);
                await split.getByRole('radio', { name: 'Shared source bounds', exact: true }).check();
                await split.getByRole('button', { name: 'Apply split', exact: true }).click();
                await split.waitFor({ state: 'hidden' });
                const divided = (await saveSnapshot(page)).pages[0];
                assert.equal(divided.regions.length, initial.regions.length + 1);
                assert.deepEqual(divided.regions.slice(2, 4).map(region => region.text), [originalText.slice(0, 10), originalText.slice(10)]);
                assert.equal(divided.regions[3].separatorBefore, '');
                for (const region of divided.regions.slice(2, 4)) {
                    assert.deepEqual([region.x, region.y, region.w, region.h], [original.x, original.y, original.w, original.h]);
                    assert.equal(region.confidenceBasis, 'manual');
                    assert.equal(region.confidence, null);
                    assert.equal(region.wordEvidence, undefined);
                    assert.equal(region.sourceComponents.length, 1);
                    assert.ok(region.sharedSourceGroups.length > 0);
                }
                assert.deepEqual(divided.regions[2].sharedSourceGroups, divided.regions[3].sharedSourceGroups);
                const captured = divided.regions[2].sourceComponents[0];
                assert.equal(captured.id, original.id);
                assert.equal(captured.sourceRegionId, original.id);
                assert.equal(captured.originalText, original.originalText);
                assert.equal(captured.textAtCapture, original.text);
                assert.deepEqual(captured.wordEvidence, original.wordEvidence);
                assert.deepEqual(captured.char_conf, original.char_conf);
                if (component) assert.deepEqual(captured, component);
                else component = captured;
                const left = await passage();
                await left.locator('[data-shared-source-groups]').waitFor();
                if (iteration === 0) {
                    const sources = left.locator('[data-passage-sources]');
                    await sources.getByText('Source components (1)', { exact: true }).click();
                    const source = sources.locator(`[data-source-component="${component.id}"]`);
                    await source.getByText('Source component 1', { exact: true }).click();
                    assert.equal(await source.locator('[data-source-original-text]').inputValue(), original.originalText);
                    await source.getByText('Original word evidence (2)', { exact: true }).click();
                    await source.getByRole('button', { name: 'Inspect source component 1 word 1: Inv0ice', exact: true }).waitFor();
                    assert.match(await source.locator('[data-source-word-evidence]').innerText(), /Inv0ice/);
                    await source.locator('[data-source-component-image]').waitFor();
                    await page.screenshot({ path: path.join(output, 'ocr-passage-structure.png'), fullPage: true });
                }
                await left.getByRole('button', { name: 'Merge next passage', exact: true }).click();
                const merge = page.getByRole('dialog', { name: 'Merge adjacent passages', exact: true });
                await merge.getByRole('combobox', { name: 'Passage separator', exact: true }).selectOption('');
                assert.equal(await merge.getByRole('textbox', { name: 'Merged passage preview', exact: true }).inputValue(), originalText);
                await merge.getByRole('button', { name: 'Apply merge', exact: true }).click();
                await merge.waitFor({ state: 'hidden' });
                roundTrip = (await saveSnapshot(page)).pages[0];
                assert.equal(roundTrip.regions.length, initial.regions.length);
                assert.equal(roundTrip.regions[2].text, originalText);
                assert.deepEqual(roundTrip.regions[2].sourceComponents, [component]);
            }
            const card = await passage();
            await card.getByRole('button', { name: 'Merge next passage', exact: true }).click();
            const merge = page.getByRole('dialog', { name: 'Merge adjacent passages', exact: true });
            await merge.getByRole('combobox', { name: 'Passage separator', exact: true }).selectOption({ label: 'Blank line' });
            const joined = `${originalText}\n\n${initial.regions[3].text}`;
            assert.equal(await merge.getByRole('textbox', { name: 'Merged passage preview', exact: true }).inputValue(), joined);
            await merge.getByRole('button', { name: 'Apply merge', exact: true }).click();
            await merge.waitFor({ state: 'hidden' });
            const combined = (await saveSnapshot(page)).pages[0];
            assert.equal(combined.regions.length, initial.regions.length - 1);
            assert.equal(combined.regions[2].text, joined);
            const components = combined.regions[2].sourceComponents;
            assert.equal(components.length, 2);
            assert.equal(new Set(components.map(source => source.id)).size, 2);
            assert.deepEqual(components.find(source => source.id === original.id), component);
            assert.equal(components.find(source => source.sourceRegionId === initial.regions[3].id).originalText, initial.regions[3].originalText);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages[0].regions, roundTrip.regions);
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages[0].regions, combined.regions);
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('json');
            await page.getByRole('button', { name: 'Save Structured JSON', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const exported = await page.evaluate(() => window.ocrSmoke.exports[0].pages[0]);
            assert.deepEqual(exported.regions[2].source_components, components);
            assert.equal(exported.regions[2].word_evidence, undefined);
            const exportedText = exported.regions.map((region, index) => `${index ? region.separator_before ?? '\n' : ''}${region.text}`).join('');
            assert.equal(exportedText, [initial.regions[0].text, initial.regions[1].text, joined, initial.regions[4].text, initial.regions[5].text].join('\n'));
        });

        await test('text repairs require selected proposals and preserve intentional punctuation, source evidence and undo', 'text-repair', async page => {
            const initial = await page.evaluate(() => window.ocrSmoke.initialProject);
            const original = initial.pages[0].regions[0];
            const collapsed = original.text.replace('Quarterly  equipment', 'Quarterly equipment');
            const dehyphenated = original.text.replace('preven-\ntive', 'preventive');
            async function openRepair(kind) {
                await page.getByRole('button', { name: 'Text editor', exact: true }).click();
                const card = page.locator('[data-passage-id]').first();
                await card.locator('textarea[data-passage-text]').focus();
                await card.getByRole('button', { name: 'Repair passage text', exact: true }).click();
                const dialog = page.getByRole('dialog', { name: 'Repair passage text', exact: true });
                await dialog.getByRole('combobox', { name: 'Repair operation', exact: true }).selectOption(kind);
                return dialog;
            }
            const preview = await openRepair('collapse-inline-spaces');
            assert.equal(await preview.getByRole('textbox', { name: 'Current passage', exact: true }).inputValue(), original.text);
            assert.equal(await preview.getByRole('button', { name: 'Apply text repair', exact: true }).isDisabled(), true);
            assert.equal(await preview.getByRole('checkbox', { name: 'Apply text repair 1', exact: true }).isChecked(), false);
            await preview.getByRole('checkbox', { name: 'Select all text repairs', exact: true }).check();
            assert.equal(await preview.getByRole('textbox', { name: 'Proposed passage', exact: true }).inputValue(), collapsed);
            await preview.getByRole('button', { name: 'Cancel', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages[0].regions, initial.pages[0].regions);
            await openRepair('collapse-inline-spaces');
            await preview.getByRole('checkbox', { name: 'Select all text repairs', exact: true }).check();
            await preview.getByRole('button', { name: 'Apply text repair', exact: true }).click();
            await preview.waitFor({ state: 'hidden' });
            const cleaned = await saveSnapshot(page);
            assert.equal(cleaned.pages[0].regions[0].text, collapsed);
            assert.equal(cleaned.pages[0].regions[0].originalText, original.originalText);
            assert.deepEqual(cleaned.pages[0].regions[0].char_conf, original.char_conf);
            assert.equal(cleaned.pages[0].regions[0].review, 'corrected');
            assert.deepEqual(cleaned.pages[2], initial.pages[2]);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages[0].regions, initial.pages[0].regions);
            await openRepair('join-lines');
            assert.equal(await preview.getByRole('button', { name: 'Apply text repair', exact: true }).isDisabled(), true);
            assert.equal(await preview.getByRole('checkbox', { name: 'Apply text repair 1', exact: true }).isChecked(), false);
            await preview.getByRole('button', { name: 'Cancel', exact: true }).click();
            await openRepair('dehyphenate');
            assert.equal(await preview.getByRole('button', { name: 'Apply text repair', exact: true }).isDisabled(), true);
            assert.equal(await preview.getByRole('checkbox', { name: /^Apply text repair / }).count(), 1);
            await preview.getByRole('checkbox', { name: 'Apply text repair 1', exact: true }).check();
            assert.equal(await preview.getByRole('textbox', { name: 'Proposed passage', exact: true }).inputValue(), dehyphenated);
            await page.screenshot({ path: path.join(output, 'ocr-text-repair-preview.png'), fullPage: true });
            await preview.getByRole('button', { name: 'Apply text repair', exact: true }).click();
            await preview.waitFor({ state: 'hidden' });
            const repaired = await saveSnapshot(page);
            assert.equal(repaired.pages[0].regions[0].text, dehyphenated);
            assert.deepEqual(repaired.pages[0].regions[0].char_conf, original.char_conf);
            assert.equal(repaired.pages[0].regions[0].originalText, original.originalText);
            assert.deepEqual(repaired.pages[2], initial.pages[2]);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages[0].regions, initial.pages[0].regions);
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            assert.equal((await saveSnapshot(page)).pages[0].regions[0].text, dehyphenated);
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('json');
            await page.getByRole('button', { name: 'Save Structured JSON', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const exported = await page.evaluate(() => window.ocrSmoke.exports[0].pages[0].regions[0]);
            assert.equal(exported.text, dehyphenated);
            assert.equal(exported.original_text, original.originalText);
            assert.deepEqual(exported.original_char_conf, original.char_conf);
            assert.deepEqual(exported.char_conf, []);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
        });

        await test('structure roles and Unicode selection formatting survive plain paste, history and JSON export', 'structure', async page => {
            const initial = await page.evaluate(() => window.ocrSmoke.initialProject);
            const modifier = process.platform === 'darwin' ? 'Meta' : 'Control';
            assert.equal(initial.pages[0].structure, undefined);
            const { panel, saved: enabled, blocks } = await enableStructure(page);
            assert.equal(enabled.pages[0].structure.version, 1);
            assert.deepEqual(enabled.pages[0].regions, initial.pages[0].regions);
            assert.deepEqual(blocks.map(block => block.sourceRegionId), initial.pages[0].regions.map(region => region.id));
            const choose = panel.getByRole('combobox', { name: 'Structure passage', exact: true });
            await choose.selectOption(blocks[0].id);
            await panel.getByRole('combobox', { name: 'Block role', exact: true }).selectOption('heading');
            await panel.getByRole('combobox', { name: 'Heading level', exact: true }).selectOption('2');
            await panel.getByRole('combobox', { name: 'Text alignment', exact: true }).selectOption('center');
            await panel.getByRole('combobox', { name: 'Break before', exact: true }).selectOption('page');
            await choose.selectOption(blocks[2].id);
            await panel.getByRole('combobox', { name: 'Block role', exact: true }).selectOption('list-item');
            await panel.getByRole('combobox', { name: 'List type', exact: true }).selectOption('number');
            await panel.getByRole('spinbutton', { name: 'List level', exact: true }).fill('1');
            await panel.getByRole('spinbutton', { name: 'List level', exact: true }).press('Tab');
            await panel.getByRole('spinbutton', { name: 'List start', exact: true }).fill('3');
            await panel.getByRole('spinbutton', { name: 'List start', exact: true }).press('Tab');
            await choose.selectOption(blocks[1].id);
            const editor = panel.getByRole('textbox', { name: 'Structure passage text', exact: true });
            const original = initial.pages[0].regions[1];
            const start = original.text.indexOf('Equipment'), end = start + 'Equipment'.length;
            const italicStart = original.text.indexOf('Maintenance'), italicEnd = italicStart + 'Maintenance'.length;
            assert.equal(start, 3, 'Selection offsets must account for the leading UTF-16 surrogate pair');
            await editor.evaluate((input, range) => { input.focus(); input.setSelectionRange(...range); }, [start, end]);
            await panel.getByRole('button', { name: 'Bold selection', exact: true }).click();
            await editor.evaluate((input, range) => { input.focus(); input.setSelectionRange(...range); }, [italicStart, italicEnd]);
            await page.keyboard.press(`${modifier}+i`);
            const formatted = await saveSnapshot(page);
            const block = project => project.pages[0].structure.blocks.find(item => item.id === blocks[1].id);
            assert.deepEqual(block(formatted).runs, [{ start, end, bold: true }, { start: italicStart, end: italicEnd, italic: true }]);
            const heading = formatted.pages[0].structure.blocks[0];
            assert.deepEqual([heading.role, heading.headingLevel, heading.alignment, heading.breakBefore], ['heading', 2, 'center', 'page']);
            assert.deepEqual(formatted.pages[0].structure.blocks[2].list, { kind: 'number', level: 1, start: 3 });
            assert.equal(formatted.pages[0].regions[1].text, original.text);
            assert.deepEqual(formatted.pages[0].regions[1].char_conf, original.char_conf);
            const boldPreview = panel.locator('[data-selected-structure-preview] [data-structure-segment].bold');
            assert.equal((await boldPreview.innerText()).trim(), 'Equipment');
            assert.ok(await boldPreview.evaluate(element => Number(getComputedStyle(element).fontWeight) >= 600));

            const inserted = 'Annual inspection\nSafety checks';
            const expected = original.text.slice(0, start) + inserted + original.text.slice(end);
            await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
            await page.evaluate(text => navigator.clipboard.writeText(text), inserted);
            await editor.evaluate((input, range) => { input.focus(); input.setSelectionRange(...range); }, [start, end]);
            await page.keyboard.press(`${modifier}+v`);
            await page.waitForFunction(value => document.querySelector('[aria-label="Structure passage text"]')?.value === value, expected);
            await editor.blur();
            const pasted = await saveSnapshot(page);
            const shift = inserted.length - (end - start);
            assert.deepEqual(block(pasted).runs, [{ start: italicStart + shift, end: italicEnd + shift, italic: true }]);
            assert.equal(pasted.pages[0].regions[1].text, expected);
            assert.equal(pasted.pages[0].regions[1].originalText, original.originalText);
            assert.deepEqual(pasted.pages[0].regions[1].char_conf, original.char_conf);
            assert.deepEqual(pasted.pages[2], initial.pages[2]);
            await editor.focus();
            await page.keyboard.press(`${modifier}+z`);
            const undone = await saveSnapshot(page);
            assert.deepEqual(undone.pages[0].structure, formatted.pages[0].structure);
            assert.deepEqual(undone.pages[0].regions, formatted.pages[0].regions);
            await editor.focus();
            await page.keyboard.press(`${modifier}+Shift+z`);
            assert.deepEqual((await saveSnapshot(page)).pages[0].structure, pasted.pages[0].structure);
            await choose.selectOption(blocks[1].id);
            await expandStructureDetail(panel, 'Show structure preview');
            await panel.locator('[data-structure-preview]').waitFor();
            await page.screenshot({ path: path.join(output, 'ocr-structure-formatting.png'), fullPage: true });
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('json');
            await page.getByRole('button', { name: 'Save Structured JSON', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const exported = await page.evaluate(() => window.ocrSmoke.exports[0].pages[0]);
            assert.deepEqual(exported.structure, pasted.pages[0].structure);
            const exportedText = exported.regions.find(region => region.text === expected);
            assert.ok(exportedText);
            assert.equal(exportedText.original_text, original.originalText);
            assert.deepEqual(exportedText.original_char_conf, original.char_conf);
            assert.deepEqual(exportedText.char_conf, []);
        });

        await test('structure reading order, columns, figure captions and note links remain explicit through history and export', 'structure', async page => {
            const initial = await page.evaluate(() => window.ocrSmoke.initialProject);
            const { panel, blocks } = await enableStructure(page);
            const choose = panel.getByRole('combobox', { name: 'Structure passage', exact: true });
            const blockIn = (project, id) => project.pages[0].structure.blocks.find(block => block.id === id);
            await choose.selectOption(blocks[2].id);
            await panel.getByRole('button', { name: 'Move block earlier', exact: true }).click();
            const moved = await saveSnapshot(page);
            const order = [0, 2, 1, 3, 4, 5, 6, 7];
            assert.deepEqual(moved.pages[0].regions.map(region => region.id), order.map(index => initial.pages[0].regions[index].id));
            assert.deepEqual(moved.pages[0].structure.blocks.map(block => block.sourceRegionId), moved.pages[0].regions.map(region => region.id));
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages[0].regions.map(region => region.id), initial.pages[0].regions.map(region => region.id));
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            await choose.selectOption(blocks[2].id);
            await expandStructureDetail(panel, 'Sections and columns');
            await panel.getByRole('button', { name: 'Start section here', exact: true }).click();
            await panel.getByRole('combobox', { name: 'Section columns', exact: true }).selectOption('2');
            await panel.getByRole('combobox', { name: 'Second column starts at', exact: true }).selectOption(blocks[3].id);
            await panel.getByRole('button', { name: 'Apply section columns', exact: true }).click();
            await choose.selectOption(blocks[7].id);
            await panel.getByRole('button', { name: 'Start section here', exact: true }).click();
            await panel.getByRole('combobox', { name: 'Block role', exact: true }).selectOption('footnote');
            await choose.selectOption(blocks[5].id);
            await expandStructureDetail(panel, 'Figure and note links');
            await panel.getByRole('textbox', { name: 'Figure alternative text', exact: true }).fill('Valve diagram showing inspection points');
            await panel.getByRole('button', { name: 'Apply figure description', exact: true }).click();
            await choose.selectOption(blocks[6].id);
            await panel.getByRole('combobox', { name: 'Block role', exact: true }).selectOption('caption');
            await panel.getByRole('combobox', { name: 'Caption source', exact: true }).selectOption(blocks[5].id);
            await choose.selectOption(blocks[1].id);
            await panel.getByRole('combobox', { name: 'Footnote target', exact: true }).selectOption(blocks[7].id);
            const sourceText = initial.pages[0].regions[1].text;
            const noteStart = sourceText.indexOf('[1]');
            await panel.getByRole('textbox', { name: 'Structure passage text', exact: true }).evaluate((input, start) => {
                input.focus(); input.setSelectionRange(start, start + 3);
            }, noteStart);
            await panel.getByRole('button', { name: 'Link selected text to note', exact: true }).click();
            const linked = await saveSnapshot(page);
            assert.deepEqual(linked.pages[0].structure.sections.map(section => section.columns), [1, 2, 1]);
            assert.deepEqual(linked.pages[0].structure.blocks.map(block => block.column), [1, 1, 1, 2, 2, 2, 2, 1]);
            assert.equal(blockIn(linked, blocks[5].id).altText, 'Valve diagram showing inspection points');
            assert.equal(blockIn(linked, blocks[6].id).anchorBlockId, blocks[5].id);
            assert.deepEqual(blockIn(linked, blocks[1].id).noteReferences, [{ start: noteStart, end: noteStart + 3, targetBlockId: blocks[7].id }]);
            for (const region of linked.pages[0].regions) {
                const original = initial.pages[0].regions.find(source => source.id === region.id);
                assert.deepEqual([region.text, region.originalText, region.x, region.y, region.w, region.h, region.char_conf],
                    [original.text, original.originalText, original.x, original.y, original.w, original.h, original.char_conf]);
            }
            await expandStructureDetail(panel, 'Show structure preview');
            const preview = panel.locator('[data-structure-preview]');
            await preview.getByRole('button', { name: 'Follow note [1]', exact: true }).click();
            assert.equal(await choose.inputValue(), blocks[7].id);
            assert.equal(await panel.getByRole('textbox', { name: 'Structure passage text', exact: true }).inputValue(), initial.pages[0].regions[7].text);
            await page.screenshot({ path: path.join(output, 'ocr-structure-links.png'), fullPage: true });
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            const unlinked = await saveSnapshot(page);
            assert.equal(blockIn(unlinked, blocks[1].id).noteReferences, undefined);
            assert.equal(blockIn(unlinked, blocks[6].id).anchorBlockId, blocks[5].id);
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages[0].structure, linked.pages[0].structure);
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('json');
            await page.getByRole('button', { name: 'Save Structured JSON', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const exported = await page.evaluate(() => window.ocrSmoke.exports[0].pages[0]);
            assert.deepEqual(exported.structure, linked.pages[0].structure);
            assert.deepEqual(exported.regions.map(region => region.text), linked.pages[0].regions.filter(region => region.kind !== 'picture').map(region => region.text));
            assert.equal(exported.figure_regions.length, 1);
            const sourceFigure = initial.pages[0].regions[5], figure = exported.figure_regions[0];
            assert.deepEqual([figure.x, figure.y, figure.w, figure.h], [sourceFigure.x, sourceFigure.y, sourceFigure.w, sourceFigure.h]);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 0);
        });

        await test('page text drafts keep native undo separate, survive project reload, and retain stale work', 'page-draft', async page => {
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            const before = await saveSnapshot(page), original = before.pages[0].regions[1];
            assert.ok(original.text.includes('\r\n'));
            await page.getByRole('button', { name: 'Edit page text', exact: true }).click();
            const dialog = page.getByRole('dialog', { name: 'Edit page text draft', exact: true });
            const editor = dialog.getByRole('textbox', { name: 'Page text draft', exact: true });
            const originalDraft = await editor.inputValue();
            const start = originalDraft.indexOf('Equipment'), end = start + 'Equipment'.length;
            assert.ok(start > 0);
            const draftText = originalDraft.slice(0, start) + 'Checked' + originalDraft.slice(end);
            await pasteSelection(page, editor, start, end, 'Checked');
            await page.waitForFunction(value => document.querySelector('[data-page-text-draft]')?.value === value, draftText);
            const modifier = process.platform === 'darwin' ? 'Meta' : 'Control';
            await page.keyboard.press(modifier + '+z');
            await page.waitForFunction(value => document.querySelector('[data-page-text-draft]')?.value === value, originalDraft);
            await page.keyboard.press(modifier + '+Shift+z');
            await page.waitForFunction(value => document.querySelector('[data-page-text-draft]')?.value === value, draftText);
            await dialog.getByRole('button', { name: 'Close and keep draft', exact: true }).click();
            await dialog.waitFor({ state: 'hidden' });
            const drafted = await saveSnapshot(page);
            assert.deepEqual(drafted.pages, before.pages);
            assert.equal(drafted.pageTextDraft.text, draftText);
            assert.equal(drafted.pageTextDraft.page.id, before.pages[0].id);
            assert.ok(drafted.pageTextDraft.revision > 1);
            await page.locator('[data-page-text-draft-banner]').waitFor();

            await reopenFixtureProject(page);
            await page.getByRole('button', { name: 'Resume page text draft', exact: true }).click();
            assert.equal(await editor.inputValue(), draftText);
            await dialog.getByRole('button', { name: 'Review changes', exact: true }).click();
            assert.equal(await dialog.getByRole('textbox', { name: 'Current page text', exact: true }).inputValue(), originalDraft);
            assert.equal(await dialog.getByRole('textbox', { name: 'Proposed page text', exact: true }).inputValue(), draftText);
            assert.equal(await dialog.getByRole('checkbox', { name: 'Use combined source coverage', exact: true }).count(), 0);
            await dialog.getByRole('button', { name: 'Back to editing', exact: true }).click();
            assert.equal(await editor.inputValue(), draftText);
            await page.screenshot({ path: path.join(output, 'ocr-page-draft-editor.png'), fullPage: true });
            await dialog.getByRole('button', { name: 'Close and keep draft', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages, before.pages);
            await page.getByRole('button', { name: 'Resume page text draft', exact: true }).click();
            await dialog.getByRole('button', { name: 'Review changes', exact: true }).click();
            await dialog.getByRole('button', { name: 'Apply page changes', exact: true }).click();
            await dialog.waitFor({ state: 'hidden' });
            const applied = await saveSnapshot(page);
            assert.equal(applied.pageTextDraft, undefined);
            assert.equal(applied.pages[0].approved, false);
            const edited = applied.pages[0].regions[1];
            assert.equal(edited.id, original.id);
            assert.equal(edited.text, original.text.replace('Equipment', 'Checked'));
            assert.equal(edited.originalText, original.originalText);
            assert.deepEqual(edited.char_conf, original.char_conf);
            assert.deepEqual(applied.pages[0].structure.blocks[1].runs, []);
            assert.deepEqual(applied.pages.slice(1), before.pages.slice(1));
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            const restored = await saveSnapshot(page);
            assert.deepEqual(restored.pages, before.pages);
            assert.equal(restored.pageTextDraft.text, draftText);

            // Change the underlying source through the normal editor, save it,
            // and actually reload the project: the retained draft is now stale.
            await page.getByRole('button', { name: 'Text editor', exact: true }).click();
            await page.getByRole('textbox', { name: 'Passage 1 text', exact: true }).fill('Revised heading outside the saved draft');
            await page.getByRole('textbox', { name: 'Passage 1 text', exact: true }).blur();
            const changedSource = await saveSnapshot(page);
            await reopenFixtureProject(page);
            await page.getByRole('button', { name: 'Resume page text draft', exact: true }).click();
            assert.equal(await editor.inputValue(), draftText);
            await dialog.getByRole('button', { name: 'Review changes', exact: true }).click();
            await dialog.getByRole('alert').filter({ hasText: /changed|stale|no longer/i }).waitFor();
            assert.equal(await editor.inputValue(), draftText);
            assert.equal(await dialog.getByRole('button', { name: 'Apply page changes', exact: true }).count(), 0);
            await dialog.getByRole('button', { name: 'Close and keep draft', exact: true }).click();
            const retained = await saveSnapshot(page);
            assert.equal(retained.pageTextDraft.text, draftText);
            assert.deepEqual(retained.pages, changedSource.pages);
        });

        await test('page text drafts reject hidden source barriers and combine reviewed passages only with explicit coverage', 'page-draft', async page => {
            const before = await page.evaluate(() => window.ocrSmoke.initialProject);
            const original = before.pages[0], first = original.regions[1], second = original.regions[2];
            await page.getByRole('button', { name: 'Edit page text', exact: true }).click();
            const dialog = page.getByRole('dialog', { name: 'Edit page text draft', exact: true });
            const editor = dialog.getByRole('textbox', { name: 'Page text draft', exact: true });
            const originalDraft = await editor.inputValue();
            const barrierStart = originalDraft.indexOf(original.regions[4].text);
            const barrierEnd = originalDraft.indexOf(original.regions[6].text) + original.regions[6].text.length;
            assert.ok(barrierStart > 0 && barrierEnd > barrierStart);
            const rejectedText = originalDraft.slice(0, barrierStart) + 'Replacement across a picture' + originalDraft.slice(barrierEnd);
            await editor.fill(rejectedText);
            await dialog.getByRole('button', { name: 'Review changes', exact: true }).click();
            await dialog.getByRole('alert').filter({ hasText: /picture|barrier|intervening/i }).waitFor();
            assert.equal(await editor.inputValue(), rejectedText);
            assert.equal(await dialog.getByRole('button', { name: 'Apply page changes', exact: true }).count(), 0);
            await dialog.getByRole('button', { name: 'Close and keep draft', exact: true }).click();
            const rejected = await saveSnapshot(page);
            assert.deepEqual(rejected.pages, before.pages);
            assert.equal(rejected.pageTextDraft.text, rejectedText);

            await page.getByRole('button', { name: 'Resume page text draft', exact: true }).click();
            await editor.fill(originalDraft);
            const start = originalDraft.indexOf(first.text.replace(/\r\n?/g, '\n'));
            const end = originalDraft.indexOf(second.text) + second.text.length;
            const combinedText = '😀 Checked equipment.\nInspected the safety valve.';
            const draftText = originalDraft.slice(0, start) + combinedText + originalDraft.slice(end);
            await pasteSelection(page, editor, start, end, combinedText);
            await page.waitForFunction(value => document.querySelector('[data-page-text-draft]')?.value === value, draftText);
            await dialog.getByRole('button', { name: 'Review changes', exact: true }).click();
            await dialog.getByText('Affected passages and source coverage', { exact: true }).click();
            assert.equal(await dialog.getByRole('textbox', { name: 'Combined passage preview', exact: true }).inputValue(), combinedText);
            const apply = dialog.getByRole('button', { name: 'Apply page changes', exact: true });
            assert.equal(await apply.isDisabled(), true);
            const coverage = dialog.getByRole('checkbox', { name: 'Use combined source coverage', exact: true });
            assert.equal(await coverage.isChecked(), false);
            await coverage.check();
            await page.screenshot({ path: path.join(output, 'ocr-page-draft-preview.png'), fullPage: true });
            await apply.click();
            await dialog.waitFor({ state: 'hidden' });
            const applied = await saveSnapshot(page), result = applied.pages[0], merged = result.regions[1];
            assert.equal(applied.pageTextDraft, undefined);
            assert.equal(result.regions.length, original.regions.length - 1);
            assert.equal(merged.text, combinedText);
            assert.equal(merged.kind, 'text');
            assert.equal(merged.confidenceBasis, 'manual');
            assert.equal(merged.confidence, null);
            assert.deepEqual(merged.char_conf, []);
            assert.deepEqual([merged.x, merged.y, merged.w, merged.h], [80, 220, 390, 230]);
            assert.deepEqual(merged.sourceComponents.map(source => source.sourceRegionId), [first.id, second.id]);
            for (const source of merged.sourceComponents) {
                const captured = original.regions.find(region => region.id === source.sourceRegionId);
                assert.equal(source.originalText, captured.originalText);
                assert.deepEqual(source.char_conf, captured.char_conf);
            }
            for (const untouched of original.regions.filter(region => ![first.id, second.id].includes(region.id))) {
                assert.deepEqual(result.regions.find(region => region.id === untouched.id), untouched);
            }
            const manualBlock = result.structure.blocks.find(block => block.sourceRegionId === merged.id);
            assert.equal(manualBlock.role, 'paragraph');
            assert.deepEqual(manualBlock.runs, []);
            assert.equal(manualBlock.noteReferences, undefined);
            const caption = original.structure.blocks[6];
            assert.deepEqual(result.structure.blocks.find(block => block.id === caption.id), caption);
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            const undone = await saveSnapshot(page);
            assert.deepEqual(undone.pages, before.pages);
            assert.equal(undone.pageTextDraft.text, draftText);
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages, applied.pages);
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('json');
            await page.getByRole('button', { name: 'Save Structured JSON', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const exported = await page.evaluate(() => window.ocrSmoke.exports[0].pages[0]);
            const exportedMerged = exported.regions.find(region => region.id === merged.id);
            assert.equal(exportedMerged.text, combinedText);
            assert.deepEqual(exportedMerged.source_components, merged.sourceComponents);
            assert.deepEqual(exported.structure, result.structure);
            assert.equal(exported.figure_regions.length, 1);
        });

        await test('partial OCR previews replacements, preserves cancellation and expands drawn areas to full passages', 'review', async page => {
            const editor = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            const current = 'Manually checked invoice number';
            await editor.fill(current);
            await editor.blur();
            const before = await saveSnapshot(page);
            const reocr = page.locator('.region-card.selected').getByRole('button', { name: 'Re-OCR region', exact: true });
            await reocr.click();
            const preview = page.getByRole('dialog', { name: 'Review new OCR text', exact: true });
            await preview.waitFor();
            assert.equal(await preview.getByRole('textbox', { name: 'Current text', exact: true }).inputValue(), current);
            assert.equal(await preview.getByRole('textbox', { name: 'New OCR text', exact: true }).inputValue(), 'Invoice NS-2026-1042');
            await page.screenshot({ path: path.join(output, 'ocr-partial-review.png'), fullPage: true });
            // The modal makes the underlying editor inert; inspect its value
            // without asking the accessibility tree to expose an active control.
            assert.equal(await page.locator('textarea[aria-label="Text for region 3"]').inputValue(), current);
            let calls = await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'recognize'));
            assert.equal(calls.length, 1);
            assert.deepEqual(calls[0].data.zone, { x: 80, y: 270, w: 640, h: 40 });
            assert.equal(calls[0].data.pagePath, before.pages[0].imagePath);
            await preview.getByRole('button', { name: 'Keep current text', exact: true }).click();
            await preview.waitFor({ state: 'hidden' });
            assert.deepEqual((await saveSnapshot(page)).pages, before.pages);

            await reocr.click();
            await preview.waitFor();
            await preview.getByRole('button', { name: 'Use new OCR text', exact: true }).click();
            await preview.waitFor({ state: 'hidden' });
            const accepted = await saveSnapshot(page);
            assert.equal(accepted.pages[0].regions[2].text, 'Invoice NS-2026-1042');
            assert.equal(accepted.pages[0].regions[2].originalText, 'Invoice NS-2026-1042');
            assert.equal(accepted.pages[0].regions[2].review, 'unreviewed');
            assert.equal(accepted.pages[0].approved, false);
            assert.deepEqual(accepted.pages[0].regions.filter((_, i) => i !== 2), before.pages[0].regions.filter((_, i) => i !== 2));
            assert.deepEqual(accepted.pages.slice(1), before.pages.slice(1));
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages, before.pages);

            await assertFitPage(page, 'Area OCR');
            await beginAreaDrag(page);
            await page.keyboard.press('Escape');
            await page.mouse.up();
            assert.equal(await preview.count(), 0);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 2);
            await beginAreaDrag(page);
            await page.mouse.up();
            await preview.waitFor();
            calls = await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'recognize'));
            assert.equal(calls.length, 3);
            assert.deepEqual(calls[2].data.zone, { x: 80, y: 270, w: 640, h: 40 });
            assert.equal(await preview.getByRole('textbox', { name: 'Current text', exact: true }).inputValue(), current);
            await preview.getByRole('button', { name: 'Keep current text', exact: true }).click();
            assert.deepEqual((await saveSnapshot(page)).pages, before.pages);
        });

        await test('empty partial OCR retains corrected text and approval', 'zone-empty', async page => {
            const editor = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await editor.fill('Retain this reviewed correction');
            await editor.blur();
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            const before = await saveSnapshot(page);
            await page.locator('.region-card.selected').getByRole('button', { name: 'Re-OCR region', exact: true }).click();
            await page.getByRole('alert').filter({ hasText: 'No text was found in this area. Existing text was preserved.' }).waitFor();
            assert.equal(await page.getByRole('dialog', { name: 'Review new OCR text', exact: true }).count(), 0);
            assert.equal(await editor.inputValue(), 'Retain this reviewed correction');
            assert.deepEqual((await saveSnapshot(page)).pages, before.pages);
            assert.equal(await page.evaluate(() => window.ocrSmoke.recognitions.length), 1);
        });

        await test('delayed partial OCR follows its captured page after navigation', 'zone-delay', async page => {
            const editor = page.getByRole('textbox', { name: 'Text for region 3', exact: true });
            await editor.fill('Correction on the original page');
            await editor.blur();
            const before = await saveSnapshot(page);
            await page.locator('.region-card.selected').getByRole('button', { name: 'Re-OCR region', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.activeNative === 1);
            await page.getByRole('button', { name: /^Page 3:/ }).click();
            assert.equal(await page.evaluate(() => window.ocrSmoke.releaseNext()), true);
            const preview = page.getByRole('dialog', { name: 'Review new OCR text', exact: true });
            await preview.waitFor();
            assert.equal(await preview.getByRole('textbox', { name: 'Current text', exact: true }).inputValue(), 'Correction on the original page');
            assert.match(await page.locator('button[aria-current="page"]').getAttribute('aria-label'), /^Page 3:/);
            await page.keyboard.press('Alt+ArrowLeft');
            assert.match(await page.locator('button[aria-current="page"]').getAttribute('aria-label'), /^Page 3:/);
            await preview.getByRole('button', { name: 'Use new OCR text', exact: true }).click();
            await preview.waitFor({ state: 'hidden' });
            assert.equal(await page.getByRole('button', { name: /^Page 1:/ }).getAttribute('aria-current'), 'page');
            const saved = await saveSnapshot(page);
            assert.equal(saved.pages[0].regions[2].text, 'Invoice NS-2026-1042');
            assert.deepEqual(saved.pages[2], before.pages[2]);
            assert.deepEqual(saved.pages[0].regions.filter((_, i) => i !== 2), before.pages[0].regions.filter((_, i) => i !== 2));
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

        await test('Word export uses explicit object placement and scope without altering saved source evidence', 'office-export', async page => {
            const initial = await page.evaluate(() => window.ocrSmoke.initialProject);
            const first = initial.pages[0], second = initial.pages[1];
            const objects = first.provenance.extractionObjects.objects;
            const table = objects.find(object => object.name === 'Reviewed parts');
            const excluded = objects.find(object => object.name === 'Excluded ledger');
            const energy = objects.find(object => object.name === 'Energy formula');
            const geometry = objects.find(object => object.name === 'Geometry formula');
            const continuation = second.provenance.extractionObjects.objects[0];
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            const scope = page.getByRole('combobox', { name: /^Pages/ });
            const format = page.getByRole('combobox', { name: /^Output format/ });
            await scope.selectOption('current');
            await format.selectOption('docx');
            const panel = page.locator('[data-office-export-options]');
            await panel.waitFor();
            const candidate = object => panel.locator(`[data-office-candidate][data-office-object-id="${object.id}"]`);
            const selection = object => panel.locator(`[data-office-selection][data-office-object-id="${object.id}"]`);
            const saveWord = page.getByRole('button', { name: 'Save Word document', exact: true });
            assert.equal(await panel.locator('[data-office-candidate]').count(), 4);
            assert.equal(await panel.locator('[data-office-selection]').count(), 0);
            assert.equal(await panel.getByRole('checkbox', { checked: true }).count(), 0);
            assert.equal(await candidate(excluded).getByRole('checkbox').isDisabled(), true);
            assert.equal(await panel.getByText('Vendor fields', { exact: true }).count(), 0);

            await candidate(table).getByRole('checkbox').check();
            const placement = selection(table).getByRole('combobox', { name: /^Placement for / });
            assert.equal(await placement.inputValue(), 'append');
            await placement.selectOption('replace-region');
            const replacement = selection(table).getByRole('combobox', { name: /^Replace passage for / });
            assert.equal(await replacement.inputValue(), '');
            await selection(table).getByText(/choose one included table passage to replace/).waitFor();
            assert.equal(await saveWord.isDisabled(), true);
            assert.deepEqual(await replacement.locator('option').evaluateAll(options => options.map(option => option.value)), ['', first.regions[1].id, first.regions[2].id]);
            await replacement.selectOption(first.regions[1].id);
            const omitted = selection(table).getByRole('textbox', { name: /^OCR text replaced by / });
            await omitted.waitFor();
            assert.equal(await omitted.inputValue(), first.regions[1].text);
            assert.equal(await omitted.getAttribute('readonly'), '');

            await candidate(energy).getByRole('checkbox').check();
            await candidate(geometry).getByRole('checkbox').check();
            await selection(geometry).getByRole('button', { name: /earlier on page$/ }).click();
            assert.deepEqual(await panel.locator('[data-office-selection]').evaluateAll(cards => cards.map(card => card.dataset.officeObjectId)), [table.id, geometry.id, energy.id]);

            // A retained selection must not silently follow a narrowed page scope.
            await scope.selectOption('all');
            await candidate(continuation).getByRole('checkbox').check();
            await scope.selectOption('current');
            await selection(continuation).getByText(/outside the selected export scope/).waitFor();
            assert.equal(await candidate(continuation).count(), 0);
            assert.equal(await saveWord.isDisabled(), true);
            await selection(continuation).getByRole('button', { name: /from Word$/ }).click();
            await saveWord.click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            const request = await page.evaluate(() => window.ocrSmoke.exports[0]);
            assert.equal(request.format, 'docx');
            assert.deepEqual(request.officeOptions, { version: 1, objects: [
                { pageId: first.id, objectId: table.id, placement: 'replace-region', regionId: first.regions[1].id },
                { pageId: first.id, objectId: geometry.id, placement: 'append' },
                { pageId: first.id, objectId: energy.id, placement: 'append' },
            ] });
            assert.equal(request.pages.length, 1);
            assert.equal(request.pages[0].id, first.id);
            assert.deepEqual(request.pages[0].provenance.extractionObjects, first.provenance.extractionObjects);
            assert.deepEqual(request.pages[0].regions.map(region => region.text), first.regions.map(region => region.text));
            assert.deepEqual(request.sourceUris, ['/fixtures/northstar-statement.pdf']);
            const notes = page.locator('[data-office-export-warnings]');
            await notes.getByText(/literal editable LaTeX/).waitFor();
            assert.match(await notes.innerText(), /repeated table header rows/);
            const saved = await saveSnapshot(page);
            assert.deepEqual(saved.pages, initial.pages);

            await page.setViewportSize({ width: 390, height: 844 });
            await selection(table).scrollIntoViewIfNeeded();
            const dimensions = await page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth }));
            assert.ok(dimensions.documentWidth <= dimensions.width + 2, 'Word placement controls overflow compact layout');
            await page.screenshot({ path: path.join(output, 'ocr-office-export-compact.png'), fullPage: true });

            await format.selectOption('json');
            assert.equal(await panel.count(), 0);
            await page.getByRole('button', { name: 'Save Structured JSON', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 2);
            const json = await page.evaluate(() => window.ocrSmoke.exports[1]);
            assert.equal(json.officeOptions, undefined);
            assert.deepEqual(json.pages[0].provenance.extractionObjects, first.provenance.extractionObjects);
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

        await test('saved extraction objects retain independent sources, edits, history and exports', 'extraction-objects', async page => {
            const initial = await page.evaluate(() => window.ocrSmoke.initialProject);
            const sourcePage = initial.pages[0];
            const objectsOf = snapshot => snapshot.pages[0].provenance.extractionObjects.objects;
            const cellAt = (object, row, column) => object.currentData.cells.find(cell => cell.row === row && cell.column === column);
            let extractedCount = 0;
            async function extractSource(regionNumber, kind, actionName) {
                await page.getByRole('tab', { name: 'Review', exact: true }).click();
                await page.getByRole('button', { name: 'Regions', exact: true }).click();
                await page.getByRole('checkbox', { name: 'Needs attention', exact: true }).uncheck();
                await page.getByRole('button', { name: 'Select region ' + regionNumber, exact: true }).click();
                await page.getByRole('tab', { name: 'Extract', exact: true }).click();
                await page.getByRole('button', { name: kind, exact: true }).click();
                const action = page.getByRole('button', { name: actionName, exact: true });
                await action.click();
                // Extraction locks this action until its captured-page update
                // completes. A trial click waits without starting a second run.
                await action.click({ trial: true });
                const snapshot = await saveSnapshot(page);
                const objects = objectsOf(snapshot);
                assert.equal(objects.length, ++extractedCount);
                assert.deepEqual(snapshot.pages[0].regions, sourcePage.regions);
                const object = objects.at(-1);
                assert.equal(await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).inputValue(), object.id);
                assert.equal(object.captures.length, 1);
                assert.equal(object.captures[0].pageId, sourcePage.id);
                assert.equal(object.captures[0].documentId, sourcePage.documentId);
                assert.equal(object.captures[0].regionId, sourcePage.regions[regionNumber - 1].id);
                assert.equal(object.captures[0].imagePath, sourcePage.imagePath);
                const sourceRegion = sourcePage.regions[regionNumber - 1];
                assert.deepEqual(object.captures[0].bounds, { x: sourceRegion.x, y: sourceRegion.y, w: sourceRegion.w, h: sourceRegion.h });
                return object;
            }
            async function selectObject(kind, id) {
                await page.getByRole('tab', { name: 'Extract', exact: true }).click();
                await page.getByRole('button', { name: kind, exact: true }).click();
                await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).selectOption(id);
                await page.locator('[data-extraction-object-id="' + id + '"]').waitFor();
            }
            async function writeSelected(actionName) {
                const count = await page.evaluate(() => window.ocrSmoke.writes.length);
                await page.getByRole('button', { name: actionName, exact: true }).click();
                await page.waitForFunction(previous => window.ocrSmoke.writes.length > previous, count);
                return page.evaluate(index => window.ocrSmoke.writes[index], count);
            }

            const firstTable = await extractSource(2, 'Table', 'Extract table');
            const secondTable = await extractSource(3, 'Table', 'Extract table');
            const firstFormula = await extractSource(4, 'Formula', 'Recognize formula');
            const secondFormula = await extractSource(5, 'Formula', 'Recognize formula');
            const fields = await extractSource(6, 'Fields', 'Extract fields');
            const originalObjects = [firstTable, secondTable, firstFormula, secondFormula, fields];
            assert.equal(new Set(originalObjects.map(object => object.id)).size, 5);
            assert.deepEqual(originalObjects.map(object => object.kind), ['table', 'table', 'formula', 'formula', 'fields']);
            assert.equal(firstTable.originalOutputKind, 'model');
            assert.deepEqual(firstTable.originalOutput, {
                html: '<table><tr><th colspan="2">Parts</th></tr><tr><td>Valve</td><td>2</td></tr></table>',
                rows: 2, cols: 2,
            });
            assert.equal(cellAt(firstTable, 0, 0).columnSpan, 2);
            assert.equal(firstTable.currentData.cells.length, 3);
            assert.equal(firstTable.currentData.cells.some(cell => cell.box !== undefined), false);
            assert.equal(cellAt(secondTable, 1, 0).text, '0012');
            assert.equal(firstFormula.originalOutput, 'E=mc^2');
            assert.equal(secondFormula.originalOutput, 'a^2+b^2=c^2');
            assert.equal(fields.captures[0].method, 'text');
            assert.equal(fields.captures[0].text, sourcePage.regions[5].text);
            assert.equal(firstTable.captures[0].method, 'pixels');
            const cropRequests = await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'transform' && call.data.operation === 'crop').map(call => call.data));
            assert.deepEqual(cropRequests.map(request => request.crop), sourcePage.regions.slice(1, 5).map(({ x, y, w, h }) => ({ x, y, w, h })));
            assert.ok(cropRequests.every(request => request.pagePath === sourcePage.imagePath));
            assert.deepEqual(fields.originalOutput, [
                { label: 'vendor', value: 'Northstar Supply Co.', score: 0.97 },
                { label: 'total', value: 'EUR 1,250.00', score: 0.95 },
            ]);

            await selectObject('Table', firstTable.id);
            const panel = page.locator('[data-extraction-objects]');
            await panel.locator('[data-extraction-source-state="current"]').waitFor();
            await expandStructureDetail(panel, 'Original model output');
            assert.deepEqual(JSON.parse(await panel.getByRole('textbox', { name: 'Original extraction output', exact: true }).inputValue()), firstTable.originalOutput);
            const cell = page.getByRole('textbox', { name: 'Row 2, column 1', exact: true });
            const editedCell = 'Reviewed valve\nChecked';
            await cell.fill(editedCell);
            await cell.blur();
            const edited = await saveSnapshot(page);
            assert.equal(cellAt(objectsOf(edited)[0], 1, 0).text, editedCell);
            assert.deepEqual(objectsOf(edited)[0].originalData, firstTable.originalData);
            assert.deepEqual(objectsOf(edited)[0].originalOutput, firstTable.originalOutput);
            assert.deepEqual(objectsOf(edited).slice(1), originalObjects.slice(1));

            const modifier = process.platform === 'darwin' ? 'Meta' : 'Control';
            await cell.focus();
            await page.keyboard.press(modifier + '+z');
            await page.waitForFunction(() => document.querySelector('[aria-label="Row 2, column 1"]')?.value === 'Valve');
            const undone = await saveSnapshot(page);
            assert.deepEqual(objectsOf(undone), originalObjects);
            await cell.focus();
            await page.keyboard.press(modifier + '+Shift+z');
            await page.waitForFunction(value => document.querySelector('[aria-label="Row 2, column 1"]')?.value === value, editedCell);
            await saveSnapshot(page);
            await reopenFixtureProject(page);
            await selectObject('Table', firstTable.id);
            assert.equal(await cell.inputValue(), editedCell);
            assert.equal(await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).locator('option').count(), 2);
            const tableCsv = await writeSelected('Save table as CSV');
            assert.equal(tableCsv.text, '"Parts",""\r\n"Reviewed valve\nChecked","2"');

            await selectObject('Formula', firstFormula.id);
            const latex = page.getByRole('textbox', { name: 'LaTeX', exact: true });
            await latex.fill('E=mc^2+C');
            await latex.blur();
            await page.locator('[data-formula-preview]').waitFor();
            const formulaTex = await writeSelected('Save formula as LaTeX');
            assert.equal(formulaTex.text, 'E=mc^2+C');
            await selectObject('Fields', fields.id);
            const vendor = page.getByRole('textbox', { name: 'vendor', exact: true });
            await vendor.fill('Northstar Supply Company');
            await vendor.blur();
            const fieldsJson = JSON.parse((await writeSelected('Save fields as JSON')).text);
            assert.equal(fieldsJson.id, fields.id);
            assert.equal(fieldsJson.kind, 'fields');
            assert.deepEqual(fieldsJson.originalOutput, fields.originalOutput);
            assert.deepEqual(fieldsJson.originalData, fields.originalData);
            assert.deepEqual(fieldsJson.captures, fields.captures);
            assert.equal(fieldsJson.currentData.fields.find(field => field.label === 'vendor').value, 'Northstar Supply Company');
            assert.equal(fieldsJson.originalData.fields.find(field => field.label === 'vendor').score, 0.97);

            const reviewed = await saveSnapshot(page);
            const reviewedObjects = objectsOf(reviewed);
            assert.deepEqual(reviewedObjects[1], secondTable);
            assert.deepEqual(reviewedObjects[3], secondFormula);
            for (let index = 0; index < originalObjects.length; index++) {
                assert.deepEqual(reviewedObjects[index].originalOutput, originalObjects[index].originalOutput);
                assert.deepEqual(reviewedObjects[index].originalData, originalObjects[index].originalData);
                assert.deepEqual(reviewedObjects[index].captures, originalObjects[index].captures);
            }
            assert.deepEqual(reviewed.pages[0].regions, sourcePage.regions);
            await page.getByRole('tab', { name: 'Review', exact: true }).click();
            await page.getByRole('button', { name: 'Approve page', exact: true }).click();
            await page.getByRole('tab', { name: 'Export', exact: true }).click();
            await page.getByRole('combobox', { name: /^Pages/ }).selectOption('current');
            await page.getByRole('combobox', { name: /^Output format/ }).selectOption('json');
            await page.getByRole('button', { name: 'Save Structured JSON', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.exports.length === 1);
            assert.deepEqual(await page.evaluate(() => window.ocrSmoke.exports[0].pages[0].provenance.extractionObjects.objects), reviewedObjects);

            await page.getByRole('button', { name: 'Rotate page right', exact: true }).click();
            await selectObject('Table', firstTable.id);
            await panel.locator('[data-extraction-source-state="historical"]').waitFor();
            const historical = await saveSnapshot(page);
            assert.notEqual(historical.pages[0].imagePath, sourcePage.imagePath);
            assert.deepEqual(objectsOf(historical), reviewedObjects);
            assert.equal(await cell.inputValue(), editedCell);
            await panel.getByLabel('Captured extraction source', { exact: true }).waitFor();
            await page.screenshot({ path: path.join(output, 'ocr-extraction-objects.png'), fullPage: true });
        });

        await test('table repairs preview exact strings and survive history, reload and compact keyboard use', 'extraction-objects', async page => {
            const initial = await page.evaluate(() => window.ocrSmoke.initialProject);
            const objectsOf = snapshot => snapshot.pages[0].provenance.extractionObjects.objects;
            const cellAt = (table, row, column) => table.cells.find(cell => cell.row === row && cell.column === column);
            for (const number of [2, 3]) {
                await page.getByRole('tab', { name: 'Review', exact: true }).click();
                await page.getByRole('button', { name: 'Regions', exact: true }).click();
                await page.getByRole('checkbox', { name: 'Needs attention', exact: true }).uncheck();
                await page.getByRole('button', { name: 'Select region ' + number, exact: true }).click();
                await page.getByRole('tab', { name: 'Extract', exact: true }).click();
                await page.getByRole('button', { name: 'Table', exact: true }).click();
                const extract = page.getByRole('button', { name: 'Extract table', exact: true });
                await extract.click();
                await extract.click({ trial: true });
            }
            const original = await saveSnapshot(page);
            const [otherTable, originalTable] = objectsOf(original);
            assert.equal(objectsOf(original).length, 2);
            const dialog = page.getByRole('dialog', { name: 'Repair table structure', exact: true });
            async function reviewOperation(operation, coordinates, separator) {
                await page.getByRole('button', { name: 'Repair table structure', exact: true }).click();
                await dialog.getByRole('combobox', { name: 'Table operation', exact: true }).selectOption(operation);
                for (const [label, value] of Object.entries(coordinates)) {
                    await dialog.getByRole('spinbutton', { name: label, exact: true }).fill(String(value));
                }
                if (separator) await dialog.getByRole('combobox', { name: 'Merge separator', exact: true }).selectOption(separator);
                await dialog.getByRole('button', { name: 'Review table changes', exact: true }).click();
                await dialog.locator('[data-table-before]').waitFor();
                await dialog.locator('[data-table-after]').waitFor();
            }
            async function applyOperation() {
                await dialog.getByRole('button', { name: 'Apply table changes', exact: true }).click();
                await dialog.waitFor({ state: 'hidden' });
                const snapshot = await saveSnapshot(page);
                const objects = objectsOf(snapshot);
                assert.deepEqual(objects[0], otherTable);
                assert.deepEqual(objects[1].originalOutput, originalTable.originalOutput);
                assert.deepEqual(objects[1].originalData, originalTable.originalData);
                assert.deepEqual(objects[1].captures, originalTable.captures);
                assert.deepEqual(snapshot.pages[0].regions, initial.pages[0].regions);
                return snapshot;
            }

            // A destructive preview exposes the complete removed strings before
            // the user decides, and Cancel must preserve the current object.
            await reviewOperation('delete-row', { 'Row to delete': 2 });
            assert.deepEqual(await dialog.locator('[data-table-deleted-cells]').getByRole('textbox').evaluateAll(inputs => inputs.map(input => input.value)), ['0012', 'EUR 1,250.00']);
            await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
            await dialog.waitFor({ state: 'hidden' });
            assert.deepEqual(objectsOf(await saveSnapshot(page)), objectsOf(original));

            await reviewOperation('insert-row', { 'Row position': 2 });
            let current = objectsOf(await applyOperation())[1].currentData;
            assert.equal(current.rows, 3);
            assert.equal(current.columns, 2);
            assert.equal(cellAt(current, 1, 0).text, '');
            assert.equal(cellAt(current, 2, 0).text, '0012');
            assert.equal(cellAt(current, 2, 1).text, 'EUR 1,250.00');
            await reviewOperation('insert-column', { 'Column position': 2 });
            current = objectsOf(await applyOperation())[1].currentData;
            assert.equal(current.columns, 3);
            assert.equal(cellAt(current, 0, 2).text, 'Amount');
            assert.equal(cellAt(current, 2, 1).text, '');
            assert.equal(cellAt(current, 2, 2).text, 'EUR 1,250.00');
            await reviewOperation('delete-row', { 'Row to delete': 2 });
            current = objectsOf(await applyOperation())[1].currentData;
            assert.equal(current.rows, 2);
            assert.equal(cellAt(current, 1, 0).text, '0012');
            await reviewOperation('delete-column', { 'Column to delete': 2 });
            const beforeMerge = await applyOperation();
            assert.deepEqual(objectsOf(beforeMerge)[1].currentData, originalTable.currentData);

            const mergeRange = { 'First row': 2, 'Last row': 2, 'First column': 1, 'Last column': 2 };
            const mergedText = '0012\nEUR 1,250.00';
            await reviewOperation('merge', mergeRange, 'newline');
            assert.equal(await dialog.getByRole('textbox', { name: 'Merged cell text', exact: true }).inputValue(), mergedText);
            await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
            await dialog.waitFor({ state: 'hidden' });
            assert.deepEqual(objectsOf(await saveSnapshot(page)), objectsOf(beforeMerge));
            await reviewOperation('merge', mergeRange, 'newline');
            const merged = await applyOperation();
            const mergedTable = objectsOf(merged)[1].currentData;
            const mergedCell = cellAt(mergedTable, 1, 0);
            assert.equal(mergedTable.cells.length, 3);
            assert.equal(mergedCell.columnSpan, 2);
            assert.equal(mergedCell.text, mergedText);
            assert.equal(cellAt(mergedTable, 0, 0).text, 'Code');
            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            await page.getByRole('textbox', { name: 'Row 2, column 2', exact: true }).waitFor();
            assert.deepEqual(objectsOf(await saveSnapshot(page)), objectsOf(beforeMerge));
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            await page.getByRole('textbox', { name: 'Row 2, column 2', exact: true }).waitFor({ state: 'detached' });
            assert.deepEqual(objectsOf(await saveSnapshot(page)), objectsOf(merged));

            // The dialog remains operable without a pointer at a phone width.
            // Split keeps the entire value in its top-left cell; it never
            // interprets the embedded newline as a second cell value.
            await page.setViewportSize({ width: 390, height: 844 });
            const repair = page.getByRole('button', { name: 'Repair table structure', exact: true });
            await repair.focus();
            await repair.press('Enter');
            await dialog.getByRole('combobox', { name: 'Table operation', exact: true }).selectOption('split');
            await dialog.getByRole('spinbutton', { name: 'Cell row', exact: true }).fill('2');
            await dialog.getByRole('spinbutton', { name: 'Cell column', exact: true }).fill('1');
            const review = dialog.getByRole('button', { name: 'Review table changes', exact: true });
            await review.focus();
            await review.press('Enter');
            await dialog.locator('[data-table-after]').waitFor();
            const sizes = await page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth }));
            assert.ok(sizes.documentWidth <= sizes.width + 2, 'Compact table repair overflows the page horizontally');
            const rectangle = await dialog.boundingBox();
            assert.ok(rectangle && rectangle.x >= -1 && rectangle.width <= 392);
            await page.screenshot({ path: path.join(output, 'ocr-table-repair-compact.png'), fullPage: true });
            const apply = dialog.getByRole('button', { name: 'Apply table changes', exact: true });
            await apply.focus();
            await apply.press('Enter');
            await dialog.waitFor({ state: 'hidden' });
            const split = await saveSnapshot(page);
            const splitTable = objectsOf(split)[1].currentData;
            assert.equal(splitTable.cells.length, 4);
            assert.equal(cellAt(splitTable, 1, 0).id, mergedCell.id);
            assert.equal(cellAt(splitTable, 1, 0).text, mergedText);
            assert.equal(cellAt(splitTable, 1, 0).columnSpan, 1);
            assert.equal(cellAt(splitTable, 1, 1).text, '');
            assert.deepEqual(objectsOf(split)[0], otherTable);
            assert.deepEqual(objectsOf(split)[1].originalData, originalTable.originalData);
            assert.deepEqual(objectsOf(split)[1].originalOutput, originalTable.originalOutput);

            await reopenFixtureProject(page);
            await page.getByRole('tab', { name: 'Extract', exact: true }).click();
            await page.getByRole('button', { name: 'Table', exact: true }).click();
            await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).selectOption(originalTable.id);
            assert.equal(await page.getByRole('textbox', { name: 'Row 2, column 1', exact: true }).inputValue(), mergedText);
            assert.equal(await page.getByRole('textbox', { name: 'Row 2, column 2', exact: true }).inputValue(), '');
            const panel = page.locator('[data-extraction-objects]');
            await expandStructureDetail(panel, 'Original model output');
            assert.deepEqual(JSON.parse(await panel.getByRole('textbox', { name: 'Original structured data', exact: true }).inputValue()), originalTable.originalData);
            await page.getByRole('button', { name: 'Save table as CSV', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.writes.length === 1);
            assert.equal(await page.evaluate(() => window.ocrSmoke.writes[0].text), '"Code","Amount"\r\n"0012\nEUR 1,250.00",""');
            const restored = await saveSnapshot(page);
            assert.deepEqual(objectsOf(restored), objectsOf(split));
        });

        await test('continuation joins require explicit mapping and preserve cross-page input evidence', 'table-continuation', async page => {
            const initial = await page.evaluate(() => window.ocrSmoke.initialProject);
            const sources = initial.pages.map(sourcePage => ({
                pageId: sourcePage.id,
                object: sourcePage.provenance.extractionObjects.objects[0],
            }));
            const [first, second, third] = sources;
            const objectsOf = snapshot => snapshot.pages[0].provenance.extractionObjects.objects;
            const rowsOf = table => Array.from({ length: table.rows }, (_, row) =>
                Array.from({ length: table.columns }, (_, column) =>
                    table.cells.find(cell => cell.row === row && cell.column === column)?.text ?? ''));
            await page.getByRole('tab', { name: 'Extract', exact: true }).click();
            await page.getByRole('button', { name: 'Table', exact: true }).click();
            await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).selectOption(first.object.id);
            const dialog = page.getByRole('dialog', { name: 'Join continuation tables', exact: true });
            async function configureJoin(checkMissingMap = false) {
                await page.getByRole('button', { name: 'Join continuation tables', exact: true }).click();
                await dialog.waitFor();
                await dialog.locator('[data-join-input-index="0"]').waitFor();
                assert.equal(await dialog.locator('[data-join-input-index]').count(), 1);
                for (const source of [second, third]) {
                    await dialog.getByRole('combobox', { name: 'Continuation table', exact: true }).selectOption(JSON.stringify([source.pageId, source.object.id]));
                    await dialog.getByRole('button', { name: 'Add table to join', exact: true }).click();
                }
                await dialog.getByRole('button', { name: 'Move table 3 up', exact: true }).click();
                assert.match(await dialog.locator('[data-join-input-index="1"]').innerText(), /Parts — page 3/);
                assert.match(await dialog.locator('[data-join-input-index="2"]').innerText(), /Parts — page 2/);
                await dialog.getByRole('textbox', { name: 'Joined table name', exact: true }).fill('Parts — reviewed continuation');
                assert.equal(await dialog.getByRole('checkbox', { name: 'Set column mapping', exact: true }).isChecked(), true);
                await dialog.getByRole('spinbutton', { name: 'Joined column count', exact: true }).fill('3');
                if (checkMissingMap) {
                    await dialog.getByRole('button', { name: 'Review joined table', exact: true }).click();
                    const error = dialog.getByRole('alert');
                    await error.waitFor();
                    assert.match(await error.innerText(), /column|mapping/i);
                    assert.equal(await dialog.locator('[data-table-join-preview]').count(), 0);
                }
                // Destinations are one-based in the UI. All input mappings are
                // explicit because one table has a different number of columns.
                for (const [index, mapping] of [[1, [1, 2]], [2, [2, 1, 3]], [3, [1, 2]]]) {
                    for (let source = 0; source < mapping.length; source++) {
                        await dialog.getByRole('spinbutton', { name: 'Joined column for table ' + index + ' source column ' + (source + 1), exact: true }).fill(String(mapping[source]));
                    }
                }
                assert.equal(await dialog.getByRole('spinbutton', { name: 'Repeated header rows for table 1', exact: true }).inputValue(), '0');
                await dialog.getByRole('spinbutton', { name: 'Repeated header rows for table 2', exact: true }).fill('1');
                await dialog.getByRole('spinbutton', { name: 'Repeated header rows for table 3', exact: true }).fill('1');
                await dialog.getByRole('button', { name: 'Review joined table', exact: true }).click();
                await dialog.locator('[data-table-join-preview]').waitFor();
                await dialog.locator('[data-join-proposed-table]').waitFor();
                assert.deepEqual(await dialog.locator('[data-join-removed-headers]').getByRole('textbox').evaluateAll(inputs => inputs.map(input => input.value)), ['Qty', 'Item', 'Reference', 'Item', 'Qty']);
                await dialog.getByText(/Removed header rows differ from the first table's retained header rows\./).waitFor();
            }
            await configureJoin(true);
            await page.screenshot({ path: path.join(output, 'ocr-table-continuation-preview.png'), fullPage: true });
            await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
            await dialog.waitFor({ state: 'hidden' });
            assert.deepEqual((await saveSnapshot(page)).pages, initial.pages);

            await configureJoin();
            await dialog.getByRole('button', { name: 'Create joined table', exact: true }).click();
            await dialog.waitFor({ state: 'hidden' });
            const joinedProject = await saveSnapshot(page);
            assert.equal(objectsOf(joinedProject).length, 2);
            const joined = objectsOf(joinedProject).find(object => object.id !== first.object.id);
            assert.ok(joined);
            assert.equal(joined.name, 'Parts — reviewed continuation');
            assert.equal(joined.kind, 'table');
            assert.equal(joined.originalOutputKind, 'table-join');
            assert.equal(joined.excluded, false);
            assert.equal(await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).inputValue(), joined.id);
            assert.deepEqual(objectsOf(joinedProject)[0], first.object);
            assert.deepEqual(joinedProject.pages[1], initial.pages[1]);
            assert.deepEqual(joinedProject.pages[2], initial.pages[2]);
            assert.deepEqual(joinedProject.pages[0].regions, initial.pages[0].regions);
            assert.deepEqual(rowsOf(joined.currentData), [
                ['Item', 'Qty', ''],
                ['Valve', '0012', ''],
                ['Seal', '03', 'LOT-003'],
                ['Pump', '0007', ''],
            ]);
            assert.deepEqual(joined.originalData, joined.currentData);
            assert.equal(joined.tableSources.version, 1);
            assert.equal(joined.tableSources.snapshots.length, 3);
            assert.deepEqual(new Set(joined.captures.map(capture => capture.pageId)), new Set(sources.map(source => source.pageId)));
            for (const source of sources) {
                const snapshot = joined.tableSources.snapshots.find(item => item.objectId === source.object.id);
                assert.ok(snapshot);
                assert.equal(snapshot.ownerPageId, source.pageId);
                assert.deepEqual(snapshot.originalOutput, source.object.originalOutput);
                assert.deepEqual(snapshot.originalData, source.object.originalData);
                assert.deepEqual(snapshot.currentData, source.object.currentData);
                assert.deepEqual(snapshot.captures, source.object.captures);
                assert.equal(Object.hasOwn(snapshot, 'tableSources'), false);
            }

            await page.getByRole('button', { name: 'Undo', exact: true }).click();
            await page.waitForFunction(() => document.querySelector('[aria-label="Saved extraction"]')?.options.length === 1);
            assert.deepEqual((await saveSnapshot(page)).pages, initial.pages);
            await page.getByRole('button', { name: 'Redo', exact: true }).click();
            await page.waitForFunction(() => document.querySelector('[aria-label="Saved extraction"]')?.options.length === 2);
            assert.deepEqual((await saveSnapshot(page)).pages, joinedProject.pages);
            await reopenFixtureProject(page);
            await page.getByRole('tab', { name: 'Extract', exact: true }).click();
            await page.getByRole('button', { name: 'Table', exact: true }).click();
            await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).selectOption(joined.id);
            assert.equal(await page.getByRole('textbox', { name: 'Row 3, column 1', exact: true }).inputValue(), 'Seal');
            assert.equal(await page.getByRole('textbox', { name: 'Row 4, column 2', exact: true }).inputValue(), '0007');
            const panel = page.locator('[data-extraction-objects]');
            await expandStructureDetail(panel, 'Source tables at joining');
            assert.deepEqual(JSON.parse(await panel.locator('[data-joined-source-snapshots]').inputValue()), joined.tableSources);
            assert.deepEqual(JSON.parse(await panel.getByRole('textbox', { name: 'Initial joined structure', exact: true }).inputValue()), joined.originalData);
            await page.getByRole('button', { name: 'Save table as CSV', exact: true }).click();
            await page.waitForFunction(() => window.ocrSmoke.writes.length === 1);
            assert.equal(await page.evaluate(() => window.ocrSmoke.writes[0].text), '"Item","Qty",""\r\n"Valve","0012",""\r\n"Seal","03","LOT-003"\r\n"Pump","0007",""');

            // A join is a captured result. Later source edits change its
            // freshness, not its strings or the evidence recorded at joining.
            await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).selectOption(first.object.id);
            const inputCell = page.getByRole('textbox', { name: 'Row 2, column 1', exact: true });
            await inputCell.fill('Valve inspected');
            await inputCell.blur();
            await saveSnapshot(page);
            await page.getByRole('combobox', { name: 'Saved extraction', exact: true }).selectOption(joined.id);
            await panel.locator('[data-extraction-source-state="historical"]').waitFor();
            assert.equal(await page.getByRole('textbox', { name: 'Row 2, column 1', exact: true }).inputValue(), 'Valve');
            const historical = await saveSnapshot(page);
            assert.deepEqual(objectsOf(historical).find(object => object.id === joined.id), joined);
            assert.deepEqual(historical.pages[1].provenance.extractionObjects.objects, [second.object]);
            assert.deepEqual(historical.pages[2].provenance.extractionObjects.objects, [third.object]);
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
            const saved = await saveSnapshot(page);
            const extracted = saved.pages[0].provenance.extractionObjects.objects;
            assert.equal(extracted.length, 1);
            assert.equal(extracted[0].kind, 'table');
            assert.equal(extracted[0].currentData.rows, 2);
            assert.equal(extracted[0].captures[0].pageId, saved.pages[0].id);
            assert.equal(extracted[0].captures[0].imagePath, saved.pages[0].imagePath);
            assert.equal(saved.pages[2].provenance.extractionObjects, undefined);

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

        await test('entering the passage editor clears an earlier search replacement target', 'review', async page => {
            await page.getByRole('button', { name: 'Find and replace', exact: true }).click();
            await page.getByRole('textbox', { name: 'Find text', exact: true }).fill('Inv0ice');
            await page.getByRole('textbox', { name: 'Replacement text', exact: true }).fill('Receipt');
            const replace = page.getByRole('button', { name: 'Replace', exact: true });
            await page.getByRole('button', { name: 'Next match', exact: true }).click();
            assert.equal(await replace.isEnabled(), true);
            await page.getByRole('button', { name: 'Text editor', exact: true }).click();
            assert.equal(await replace.isDisabled(), true);
            await page.getByRole('textbox', { name: 'Passage 5 text', exact: true }).fill('A separate passage correction.');
            assert.equal(await replace.isDisabled(), true);
            const saved = await saveSnapshot(page);
            assert.equal(saved.pages[0].regions[2].text, 'Inv0ice NS-2026-1042');
            assert.equal(saved.pages[0].regions[4].text, 'A separate passage correction.');

            await page.getByRole('button', { name: 'Next match', exact: true }).click();
            assert.equal(await replace.isEnabled(), true);
            await page.locator('.region-card.selected').getByRole('button', { name: 'Edit passage', exact: true }).click();
            assert.equal(await replace.isDisabled(), true);
            assert.equal(await page.getByRole('textbox', { name: 'Passage 3 text', exact: true }).inputValue(), 'Inv0ice NS-2026-1042');
        });

        await test('desktop and compact layouts remain usable', 'review', async page => {
            await page.getByRole('button', { name: 'Next issue', exact: true }).click();
            await assertFitPage(page, 'Desktop');
            await page.screenshot({ path: path.join(output, 'ocr-workspace-desktop.png'), fullPage: true });
            await page.setViewportSize({ width: 390, height: 844 });
            await assertFitPage(page, 'Compact');
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
    const report = { tests: results, browserErrors, passed: results.filter(item => item.status === 'passed').length, failed: results.filter(item => item.status === 'failed').length, screenshots: ['ocr-workspace-desktop.png', 'ocr-workspace-compact.png', 'ocr-word-review-desktop.png', 'ocr-word-review-compact.png', 'ocr-native-word-review.png', 'ocr-proofreading-preview.png', 'ocr-passage-editor.png', 'ocr-passage-structure.png', 'ocr-text-repair-preview.png', 'ocr-structure-formatting.png', 'ocr-structure-links.png', 'ocr-page-draft-editor.png', 'ocr-page-draft-preview.png', 'ocr-partial-review.png', 'ocr-extraction-objects.png', 'ocr-table-repair-compact.png', 'ocr-table-continuation-preview.png', 'ocr-office-export-compact.png'].filter(name => existsSync(path.join(output, name))) };
    await writeFile(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
    console.log(`Browser verification: ${report.passed} passed, ${report.failed} failed. Report: ${path.join(output, 'report.json')}`);
    if (report.failed || browserErrors.length) process.exitCode = 1;
}
