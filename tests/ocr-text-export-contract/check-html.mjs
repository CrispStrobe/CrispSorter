import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_MODULE_PATH);
const output = path.resolve(process.argv[2]);
const fixtures = JSON.parse(await readFile(path.join(output, 'fixtures.json'), 'utf8'));
const browser = await chromium.launch();
const reports = [];
try {
  for (const fixture of fixtures.cases) for (const width of [390, 1440]) {
    const page = await browser.newPage({ viewport: { width, height: 844 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(pathToFileURL(path.join(output, `${fixture.id}.html`)).href);
    const actual = await page.evaluate(() => ({
      pages: [...document.querySelectorAll('section')].map(section => [...section.querySelectorAll('p')].map(p => p.textContent)),
      directions: [...document.querySelectorAll('p')].map(p => ({ attribute: p.dir, computed: getComputedStyle(p).direction, bidi: getComputedStyle(p).unicodeBidi })),
      width: innerWidth, scrollWidth: document.documentElement.scrollWidth,
      viewport: document.querySelector('meta[name=viewport]')?.content,
      scripts: document.querySelectorAll('script').length,
    }));
    reports.push({ fixture: fixture.id, browser: browser.version(), errors, ...actual });
    await writeFile(path.join(output, 'html-browser.json'), JSON.stringify(reports, null, 2));
    assert.deepEqual(actual.pages, fixture.pages);
    assert.equal(actual.scripts, 0);
    assert.deepEqual(errors, []);
    assert.match(actual.viewport, /width=device-width/);
    assert.ok(actual.scrollWidth <= width + 2);
    assert.equal(actual.directions[0].computed, 'rtl');
    assert.ok(actual.directions.every(direction => direction.attribute === 'auto' && direction.bidi === 'plaintext'));
    await page.screenshot({ path: path.join(output, `${fixture.id}-${width}.png`), fullPage: true });
    await page.close();
  }
} finally { await browser.close(); }
