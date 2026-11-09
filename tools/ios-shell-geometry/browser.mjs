import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const out = resolve(process.argv[2]);
const { chromium } = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE_PATH).href);
const browser = await chromium.launch({ headless: true });
const source = JSON.parse(await readFile(join(out, 'Resources/source.json'), 'utf8'));
const html = await readFile(join(out, 'Resources/current.html'), 'utf8');
const report = { schema_version: 1, kind: 'source-matched-browser-shell-geometry', status: 'failed',
  scope: 'Exact source shell CSS/viewport in simplified fixture markup. CDP supplies CSS safe-area env values. No actual iOS app navigation.',
  browser: browser.version(), source, cases: [] };
const cases = [
  { name: 'desktop', width: 1440, height: 960, insets: { top: 0, right: 0, bottom: 0, left: 0 } },
  { name: 'portrait', width: 402, height: 874, insets: { top: 62, right: 0, bottom: 34, left: 0 } },
  { name: 'short-landscape', width: 740, height: 360, insets: { top: 0, right: 34, bottom: 21, left: 62 } },
  { name: 'short-landscape-large-text', width: 740, height: 360, insets: { top: 0, right: 34, bottom: 21, left: 62 }, largeText: true },
];
const inside = (r, b, label) => {
  assert.ok(r && r.width > 0 && r.height > 0, `${label} is empty`);
  assert.ok(r.x >= b.x - 0.75 && r.y >= b.y - 0.75 && r.x + r.width <= b.x + b.width + 0.75 && r.y + r.height <= b.y + b.height + 0.75,
    `${label} is outside ${JSON.stringify(b)}: ${JSON.stringify(r)}`);
};
try {
  for (const scenario of cases) {
    const context = await browser.newContext({ viewport: { width: scenario.width, height: scenario.height }, isMobile: scenario.width < 768, hasTouch: scenario.width < 768 });
    const page = await context.newPage();
    const result = { ...scenario, status: 'failed' };
    try {
      const cdp = await context.newCDPSession(page);
      await cdp.send('Emulation.setSafeAreaInsetsOverride', { insets: scenario.insets });
      await page.setContent(html);
      if (scenario.largeText) {
        // An explicit user-style stress case, separate from the native host's
        // WKPreferences minimumFontSize control. Position/size rules stay exact.
        await page.addStyleTag({ content: '.mobile-tab span, .mobile-tools-menu button { font-size: 32px !important; }' });
        result.text_override = '32px navigation labels and popup actions';
      }
      await page.waitForFunction(() => typeof window.measureGeometry === 'function');
      let metrics = await page.evaluate(() => { window.measureGeometry(); return window.__geometry; });
      assert.deepEqual(metrics.padding, scenario.insets, 'The real CSS env insets must match the requested CDP geometry');
      const safe = { x: scenario.insets.left, y: scenario.insets.top,
        width: scenario.width - scenario.insets.left - scenario.insets.right,
        height: scenario.height - scenario.insets.top - scenario.insets.bottom };
      inside(metrics.shell, { x: 0, y: 0, width: scenario.width, height: scenario.height }, 'shell');
      if (scenario.width >= 768) {
        inside(metrics.desktopNavigation, safe, 'desktop navigation');
        assert.equal(metrics.desktopNavigation.width, 200);
        assert.equal(metrics.navigation.width, 0);
      } else {
        inside(metrics.navigation, safe, 'phone navigation');
        inside(metrics.tools, metrics.navigation, 'Tools');
        assert.ok(metrics.tools.width >= 44 && metrics.tools.height >= 44);
        await page.getByRole('button', { name: 'Tools', exact: true }).click();
        metrics = await page.evaluate(() => { window.measureGeometry(); return window.__geometry; });
        inside(metrics.menu, safe, 'Tools menu');
        assert.ok(metrics.menu.y + metrics.menu.height <= metrics.navigation.y - 1, 'Tools menu overlaps the actual navigation height');
        assert.ok(metrics.menu.scrollWidth <= metrics.menu.clientWidth + 1, 'Tools menu requires horizontal scrolling');
        for (const label of ['Translate', 'OCR', 'PDF tools', 'Drives', 'History']) {
          const button = page.getByRole('menu', { name: 'Tools menu' }).getByRole('button', { name: label, exact: true });
          await button.scrollIntoViewIfNeeded();
          inside(await button.boundingBox(), metrics.menu, `${label} action after scrolling`);
          await button.click();
          assert.equal(await button.evaluate(element => document.activeElement === element), true);
        }
      }
      result.metrics = metrics;
      result.status = 'passed';
      await page.screenshot({ path: join(out, `${scenario.name}.png`) });
    } catch (error) {
      result.error = String(error);
      await page.screenshot({ path: join(out, `failure-${scenario.name}.png`) }).catch(() => {});
    } finally {
      report.cases.push(result);
      await writeFile(join(out, 'report.json'), JSON.stringify(report, null, 2) + '\n');
      await context.close();
    }
  }
  report.status = report.cases.every(item => item.status === 'passed') ? 'passed' : 'failed';
} finally {
  await browser.close();
  await writeFile(join(out, 'report.json'), JSON.stringify(report, null, 2) + '\n');
}
console.log(JSON.stringify({ status: report.status, cases: report.cases.map(({ name, status, error }) => ({ name, status, error })) }));
if (report.status !== 'passed') process.exitCode = 1;
