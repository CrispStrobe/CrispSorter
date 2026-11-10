import { chromium, type FullConfig } from '@playwright/test';

/** Vite compiles the app on the first browser request; on a cold CI runner
 * that can take longer than a test's 5 s expectation. Load it once here so
 * every spec, including whichever runs first, starts from a warm server. */
export default async function globalSetup(config: FullConfig) {
    const { baseURL, launchOptions } = config.projects[0].use;
    const browser = await chromium.launch(launchOptions);
    try {
        const page = await browser.newPage();
        await page.goto(baseURL ?? 'http://127.0.0.1:1420', { timeout: 120_000 });
        await page.locator('nav.main-nav').waitFor({ state: 'attached', timeout: 120_000 });
    } finally {
        await browser.close();
    }
}
