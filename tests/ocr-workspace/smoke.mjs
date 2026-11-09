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
    console.log('Scenarios: ?scenario=review, word-review, native-word, zone-empty, zone-delay, queue-failure, queue-control, extraction-delay, project-recovery, prepare-unchanged');
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
            await page.locator('.save-state').getByText('All changes saved', { exact: true }).waitFor();
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
    const report = { tests: results, browserErrors, passed: results.filter(item => item.status === 'passed').length, failed: results.filter(item => item.status === 'failed').length, screenshots: ['ocr-workspace-desktop.png', 'ocr-workspace-compact.png', 'ocr-word-review-desktop.png', 'ocr-word-review-compact.png', 'ocr-native-word-review.png', 'ocr-passage-editor.png', 'ocr-partial-review.png'].filter(name => existsSync(path.join(output, name))) };
    await writeFile(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
    console.log(`Browser verification: ${report.passed} passed, ${report.failed} failed. Report: ${path.join(output, 'report.json')}`);
    if (report.failed || browserErrors.length) process.exitCode = 1;
}
