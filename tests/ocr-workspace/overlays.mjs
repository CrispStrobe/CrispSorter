import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';

const surfaces = [
    { name: 'short-landscape', width: 740, height: 360, insets: { top: 17, right: 9, bottom: 39, left: 61 } },
    { name: 'portrait', width: 390, height: 844, insets: { top: 59, right: 27, bottom: 34, left: 11 } },
];
const desktop = { name: 'setup', width: 1440, height: 1000, insets: { top: 0, right: 0, bottom: 0, left: 0 } };

/** Actual production components, actual CSS env values, synthetic safe areas.
 * The separate native WKWebView control covers real platform inset geometry. */
export async function verifySafeOverlays(page, output, repo) {
    const session = await page.context().newCDPSession(page);
    const report = {
        source_commit: process.env.GITHUB_SHA ?? null,
        browser: page.context().browser().version(),
        insets_method: 'Emulation.setSafeAreaInsetsOverride',
        protocol: 'https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setSafeAreaInsetsOverride',
        synthetic_insets: true,
        components: {}, observations: [], result: 'running',
        scope: 'Production component geometry and interactions; fixture store IPC and saved projects. No model request, network transmission or physical-device claim.',
    };
    for (const file of ['src/lib/components/OcrWorkbench.svelte', 'src/lib/components/ocr/ModelStorage.svelte', 'src/lib/components/ThirdPartyAiConsent.svelte']) {
        report.components[file] = createHash('sha256').update(await readFile(path.join(repo, file))).digest('hex');
    }
    let surface = desktop;
    async function size(next) {
        surface = next;
        await page.setViewportSize({ width: next.width, height: next.height });
        // This changes browser-provided env() values; no production CSS is
        // replaced, and an unsupported protocol method is a failing check.
        await session.send('Emulation.setSafeAreaInsetsOverride', { insets: next.insets });
        await page.waitForFunction(insets => {
            const style = getComputedStyle(document.querySelector('.safe-area-frame'));
            return ['top', 'right', 'bottom', 'left'].every(edge =>
                parseFloat(style.getPropertyValue(`padding-${edge}`)) === insets[edge]);
        }, next.insets);
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    }
    async function geometry(locator, label, centered = false) {
        const box = await locator.evaluate(element => {
            const r = element.getBoundingClientRect();
            return { left: r.left, top: r.top, right: r.right, bottom: r.bottom, width: r.width, height: r.height,
                scrollHeight: element.scrollHeight, clientHeight: element.clientHeight,
                scrollWidth: element.scrollWidth, clientWidth: element.clientWidth };
        });
        report.observations.push({ label, surface, box });
        const { left, top, right, bottom } = surface.insets;
        assert.ok(box.left >= left - 1 && box.right <= surface.width - right + 1, `${label}: outside horizontal safe edges`);
        assert.ok(box.top >= top - 1 && box.bottom <= surface.height - bottom + 1, `${label}: outside vertical safe edges`);
        assert.ok(box.width > 0 && box.height > 0, `${label}: empty rectangle`);
        assert.ok(box.scrollWidth <= box.clientWidth + 1, `${label}: horizontal content overflow`);
        if (centered) {
            assert.ok(Math.abs((box.left + box.right) / 2 - (left + surface.width - right) / 2) <= 1.5, `${label}: centered against raw horizontal viewport`);
            assert.ok(Math.abs((box.top + box.bottom) / 2 - (top + surface.height - bottom) / 2) <= 1.5, `${label}: centered against raw vertical viewport`);
        }
        return box;
    }
    async function reachable(locator, label, owner) {
        await locator.scrollIntoViewIfNeeded();
        const box = await geometry(locator, label);
        const parent = await owner.boundingBox();
        assert.ok(parent && box.left >= parent.x - 1 && box.right <= parent.x + parent.width + 1 &&
            box.top >= parent.y - 1 && box.bottom <= parent.y + parent.height + 1, `${label}: clipped by overlay`);
        assert.ok(await locator.evaluate(element => {
            const r = element.getBoundingClientRect();
            const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
            return hit === element || element.contains(hit);
        }), `${label}: visible center is not hit-testable`);
        return box;
    }
    async function modal(locator, label) {
        const box = await geometry(locator, label, true);
        assert.equal(await locator.evaluate(element => element.matches(':modal')), true, `${label}: lost native modal behavior`);
        assert.equal(await locator.evaluate(element => element.contains(document.activeElement)), true, `${label}: focus escaped native dialog`);
        return box;
    }
    try {
        // A short workspace must expose its last saved item through the menu's
        // own scroll box. The enclosing OCR workspace stays non-scrolling.
        for (const next of surfaces) {
            await size(desktop);
            await page.getByRole('button', { name: /^Projects/ }).click();
            const menu = page.getByRole('region', { name: 'Saved projects', exact: true });
            await size(next);
            const bounds = await geometry(menu, 'saved-project-menu');
            assert.ok(bounds.scrollHeight > bounds.clientHeight, 'Long saved list did not exercise scrolling');
            const workspace = await page.getByRole('region', { name: 'OCR document workspace', exact: true }).boundingBox();
            assert.ok(bounds.bottom <= workspace.y + workspace.height - 11, 'Saved menu exceeds its containing workspace');
            const last = menu.getByRole('button', { name: /^Saved overlay project 20/ });
            await reachable(last, 'last-saved-project', menu);
            await last.click();
            await page.waitForFunction(() => document.querySelector('[aria-label="Project name"]').value === 'Saved overlay project 20');
            await page.getByRole('button', { name: /^Projects/ }).click();
            await reachable(menu.getByRole('button', { name: 'Close projects', exact: true }), 'close-saved-projects', menu);
            await menu.getByRole('button', { name: 'Close projects', exact: true }).click();
            await menu.waitFor({ state: 'hidden' });
        }
        assert.ok(await page.evaluate(() => window.ocrSmoke.calls.some(call => call.method === 'load' && call.data.projectId === 'ocr-overlay-saved-20')));

        await size(desktop);
        await page.getByRole('button', { name: 'Remove page', exact: true }).click();
        const confirmation = page.getByRole('dialog', { name: 'Remove this page?', exact: true });
        await confirmation.waitFor();
        for (const next of surfaces) {
            await size(next);
            const bounds = await modal(confirmation, 'workspace-confirmation');
            if (next.name === 'short-landscape') assert.ok(bounds.scrollHeight > bounds.clientHeight, 'Long page name did not exercise dialog scrolling');
            for (const name of ['Cancel', 'Remove page']) {
                assert.ok((await reachable(confirmation.getByRole('button', { name, exact: true }), `confirmation-${name}`, confirmation)).height >= 44);
            }
            await page.screenshot({ path: path.join(output, `ocr-overlay-${next.name}-confirmation.png`) });
        }
        await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click();
        await confirmation.waitFor({ state: 'hidden' });
        // The last surface is a phone, where the page rail is intentionally
        // behind the mobile "Pages" view; count the cancelled removal on the
        // desktop layout, where every page card is rendered.
        await size(desktop);
        assert.equal(await page.getByRole('button', { name: /^Page \d:/ }).count(), 3);

        // The shortcut dialog uses the same top-layer rule, with a close button
        // at the top rather than actions at the bottom.
        await size(desktop);
        await page.getByRole('button', { name: 'Keyboard shortcuts', exact: true }).click();
        const help = page.getByRole('dialog', { name: 'Keyboard shortcuts', exact: true });
        await help.waitFor();
        await size(surfaces[0]);
        await modal(help, 'workspace-shortcuts');
        await reachable(help.getByRole('button', { name: 'Close shortcuts', exact: true }), 'close-shortcuts', help);
        await help.getByRole('button', { name: 'Close shortcuts', exact: true }).click();
        await help.waitFor({ state: 'hidden' });

        await size(desktop);
        await page.getByRole('button', { name: 'OCR models', exact: true }).click();
        await page.getByRole('button', { name: 'Manage model storage', exact: true }).click();
        const storage = page.getByRole('region', { name: 'OCR model storage', exact: true });
        const remove = storage.getByRole('button', { name: /^Remove archive language model/ });
        await remove.click();
        const modelDialog = page.getByRole('dialog', { name: 'Remove downloaded model?', exact: true });
        await modelDialog.waitFor();
        for (const next of surfaces) {
            await size(next);
            const bounds = await modal(modelDialog, 'model-removal');
            if (next.name === 'short-landscape') assert.ok(bounds.scrollHeight > bounds.clientHeight, 'Long model name did not exercise dialog scrolling');
            for (const name of ['Cancel model removal', 'Keep model', 'Remove model']) {
                assert.ok((await reachable(modelDialog.getByRole('button', { name, exact: true }), `model-${name}`, modelDialog)).height >= 44);
            }
        }
        await modelDialog.getByRole('button', { name: 'Keep model', exact: true }).click();
        await modelDialog.waitFor({ state: 'hidden' });
        assert.equal(await page.evaluate(() => window.ocrSmoke.calls.filter(call => call.method === 'removeMobileModel').length), 0);
        // Keep the existing native Escape/close behavior after rotation.
        await size(desktop);
        await remove.click();
        await size(surfaces[0]);
        await page.keyboard.press('Escape');
        await modelDialog.waitFor({ state: 'hidden' });

        // The real consent gate must still reject a decline and persist only
        // explicit approval. Long disclosed endpoints force the scroll path.
        for (const [index, next] of surfaces.entries()) {
            await size(next);
            const provider = `overlay-provider-${index}`;
            await page.evaluate(({ provider }) => window.overlaySmoke.request(provider,
                `https://example.invalid/${'document-disclosure-path/'.repeat(80)}`), { provider });
            const card = page.locator('.consent-card');
            await card.waitFor();
            const bounds = await geometry(card, 'consent-card', true);
            if (next.name === 'short-landscape') assert.ok(bounds.scrollHeight > bounds.clientHeight, 'Long endpoint did not exercise consent scrolling');
            for (const selector of ['.consent-deny', '.consent-allow']) {
                assert.ok((await reachable(page.locator(selector), selector, card)).height >= 44);
            }
            await page.screenshot({ path: path.join(output, `ocr-overlay-${next.name}-consent.png`) });
            await page.locator(index === 0 ? '.consent-deny' : '.consent-allow').click();
            await card.waitFor({ state: 'hidden' });
            await page.waitForFunction(() => window.overlaySmoke.outcome !== 'pending');
            const state = await page.evaluate(() => ({ outcome: window.overlaySmoke.outcome,
                consent: window.overlaySmoke.storedConsent(), saves: window.overlaySmoke.saveCount }));
            report.observations.push({ label: 'consent-decision', surface, state });
            assert.equal(state.outcome, index === 0 ? 'declined' : 'allowed');
            assert.deepEqual(state.consent, index === 0 ? [] : [provider]);
            assert.equal(state.saves, index);
        }
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1 && scrollY === 0));
        report.result = 'passed';
    } catch (error) {
        report.result = 'failed'; report.failure = error.stack ?? String(error);
        throw error;
    } finally {
        await writeFile(path.join(output, 'safe-overlay-state.json'), JSON.stringify(report, null, 2));
        await session.detach();
    }
}
