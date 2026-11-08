import { defineConfig } from '@playwright/test';
export default defineConfig({
    testDir: './tests/browser',
    workers: 1,
    timeout: 45000,
    use: {
        baseURL: 'http://127.0.0.1:1420',
        viewport: { width: 1400, height: 950 },
        launchOptions: { executablePath: process.env.CRISPSORTER_CHROMIUM_PATH, args: ['--no-sandbox'] },
        screenshot: 'only-on-failure',
        trace: 'retain-on-failure',
    },
    webServer: { command: 'npm run dev -- --host 127.0.0.1', url: 'http://127.0.0.1:1420', reuseExistingServer: true, timeout: 60000 },
});
