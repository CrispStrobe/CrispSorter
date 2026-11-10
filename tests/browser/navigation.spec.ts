import { expect, test } from '@playwright/test';
import { fixtures } from './fixtures';

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first()).toBeVisible();
});

test('quick navigation finds a workflow by task and opens it with the keyboard', async ({ page }) => {
    await page.keyboard.press('Control+k');
    const dialog = page.getByRole('dialog', { name: 'Go to a tool or workflow' });
    const search = dialog.getByRole('combobox', { name: 'Find a tool or workflow' });
    await expect(search).toBeFocused();
    await search.fill('rename');
    await expect(dialog.getByRole('option')).toHaveCount(1);
    await expect(dialog.getByRole('option', { selected: true })).toContainText('Review');
    await search.press('Enter');
    await expect(dialog).not.toBeVisible();
    await expect(page.locator('.main-nav').getByRole('button', { name: 'Review', exact: true })).toHaveAttribute('aria-current', 'page');
    await expect(page.locator('.batch-container').getByText('Warranty.txt', { exact: true }).first()).toBeVisible();
});

test('palette arrows, empty results, modal focus and dismissal stay predictable', async ({ page }) => {
    const trigger = page.locator('.main-nav').getByRole('button', { name: 'Quick navigation', exact: true });
    await trigger.click();
    const dialog = page.getByRole('dialog', { name: 'Go to a tool or workflow' });
    const search = dialog.getByRole('combobox');
    const close = dialog.getByRole('button', { name: 'Close quick navigation' });
    async function expectFocusCycle() {
        await expect(search).toBeFocused();
        await page.keyboard.press('Tab');
        await expect(close).toBeFocused();
        await page.keyboard.press('Tab');
        await expect(search).toBeFocused();
        await page.keyboard.press('Shift+Tab');
        await expect(close).toBeFocused();
        await page.keyboard.press('Shift+Tab');
        await expect(search).toBeFocused();
    }
    await expectFocusCycle();
    await search.press('ArrowDown');
    await expect(dialog.getByRole('option', { selected: true })).toContainText('Review');
    await search.press('ArrowUp');
    await expect(dialog.getByRole('option', { selected: true })).toContainText('Inbox');
    await search.fill('zzzz-no-tool');
    await expect(dialog.getByText('No matching destinations')).toBeVisible();
    await expect(search).not.toHaveAttribute('aria-activedescendant');
    await search.press('Enter');
    await expect(dialog).toBeVisible();
    await expectFocusCycle();
    await page.keyboard.press('Tab');
    await expect(close).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(dialog).not.toBeVisible();
    await expect(trigger).toBeFocused();

    await trigger.click();
    await expect(search).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(dialog).not.toBeVisible();
    await expect(trigger).toBeFocused();
});

test('shortcut respects composition and conflicting modifiers', async ({ page }) => {
    await page.keyboard.press('Control+Shift+k');
    await expect(page.getByRole('dialog', { name: 'Go to a tool or workflow' })).not.toBeVisible();
    await page.evaluate(() => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true, isComposing: true, bubbles: true })));
    await expect(page.getByRole('dialog', { name: 'Go to a tool or workflow' })).not.toBeVisible();
    await page.keyboard.press('Control+k');
    await expect(page.getByRole('dialog', { name: 'Go to a tool or workflow' })).toBeVisible();
});

test('mobile tools expose quick navigation and dismiss without trapping focus', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const nav = page.getByRole('navigation', { name: 'Mobile navigation' });
    const tools = nav.getByRole('button', { name: 'Tools', exact: true });
    await tools.click();
    await page.locator('#mobile-tools-menu').getByRole('button', { name: 'Quick navigation', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Go to a tool or workflow' });
    await expect(dialog.getByRole('combobox')).toBeFocused();
    await expect(page.locator('#mobile-tools-menu')).not.toBeVisible();
    await page.keyboard.press('Escape');
    await expect(tools).toBeFocused();

    await tools.click();
    await page.keyboard.press('Escape');
    await expect(page.locator('#mobile-tools-menu')).not.toBeVisible();
    await expect(tools).toBeFocused();

    await tools.click();
    await nav.getByRole('button', { name: 'Inbox', exact: true }).click();
    await expect(page.locator('#mobile-tools-menu')).not.toBeVisible();
});

test('mobile capabilities also gate searchable destinations', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto('/?fixturePlatform=ios');
    await expect(page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first()).toBeVisible();
    await page.getByRole('navigation', { name: 'Mobile navigation' }).getByRole('button', { name: 'Tools', exact: true }).click();
    await page.locator('#mobile-tools-menu').getByRole('button', { name: 'Quick navigation', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: 'Go to a tool or workflow' });
    await expect(dialog.getByRole('option').filter({ hasText: /^OCR/ })).toHaveCount(0);
    await expect(dialog.getByRole('option').filter({ hasText: /^AIToolkit/ })).toHaveCount(0);
    await dialog.getByRole('combobox').fill('scan');
    await expect(dialog.getByText('No matching destinations')).toBeVisible();
});

test('expanded quick-navigation labels fit beside the icon with the shortcut below in both languages', async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 600 });
    for (const [language, label] of [['en', 'Quick navigation'], ['de', 'Schnellnavigation']] as const) {
        await page.evaluate(async language => {
            const modulePath = '/src/lib/i18n.svelte.ts';
            const { i18n } = await import(modulePath);
            i18n.setLanguage(language);
        }, language);
        const trigger = page.locator('.main-nav').getByRole('button', { name: label, exact: true });
        await expect(trigger.locator('span')).toHaveText(label);
        await expect(trigger.locator('span')).toBeVisible();
        const layout = await trigger.evaluate(button => {
            const caption = button.querySelector('span')!;
            const shortcut = button.querySelector('kbd')!;
            const captionBox = caption.getBoundingClientRect();
            const buttonBox = button.getBoundingClientRect();
            return {
                labelFits: caption.scrollWidth <= caption.clientWidth && captionBox.right <= buttonBox.right,
                shortcutBelowLabel: shortcut.getBoundingClientRect().top >= captionBox.bottom,
            };
        });
        expect(layout).toEqual({ labelFits: true, shortcutBelowLabel: true });
    }
});

test('short and collapsed navigation retain reachable named controls', async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 600 });
    const nav = page.getByRole('navigation', { name: 'Main navigation' });
    const settings = nav.getByRole('button', { name: 'Settings', exact: true });
    const collapse = nav.getByRole('button', { name: 'Collapse navigation', exact: true });
    await expect(settings).toBeInViewport();
    await expect(collapse).toBeInViewport();
    await nav.getByRole('button', { name: 'PDF Tools', exact: true }).click();
    await expect(settings).toBeInViewport();
    await collapse.click();
    await expect(nav.getByRole('button', { name: 'Expand navigation', exact: true })).toBeVisible();
    await nav.getByRole('button', { name: 'Quick navigation', exact: true }).click();
    await expect(page.getByRole('dialog', { name: 'Go to a tool or workflow' })).toBeVisible();
});
