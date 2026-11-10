import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

interface ReviewFixture {
    id: string;
    extension?: string;
    status?: 'queued' | 'unfinished' | 'extracting' | 'analyzing' | 'review' | 'ready' | 'moving' | 'error';
    title?: string;
    author?: string;
    year?: string;
    accepted?: boolean;
    size?: number;
}

const reviewFiles: ReviewFixture[] = [
    { id: 'alpha', title: 'Original A', author: 'Ada', year: '2020' },
    { id: 'beta', title: 'Original B', author: 'Ben', year: '2021' },
];

async function seedBatch(page: Page, items: ReviewFixture[]) {
    await page.evaluate(async (seed) => {
        const root = window as any;
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        root.__reviewFailClear = false;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'batch_session_clear' && root.__reviewFailClear) {
                root.__fixtureCalls.push({ command, args });
                throw new Error('Fixture clear failure: database is locked');
            }
            return originalInvoke(command, args);
        };
        await originalInvoke('plugin:store|set', { key: 'exportPath', value: '/fixtures/sorted' });
        await originalInvoke('plugin:store|set', { key: 'pathTemplate', value: '{Author}/{Year}/{Title}' });
        const storePath = '/src/lib/batch/store.svelte.ts';
        const draftPath = '/src/lib/batch/reviewDrafts.svelte.ts';
        const { batchManager } = await import(storePath);
        const { reviewDrafts } = await import(draftPath);
        await reviewDrafts.load();
        batchManager.resetFilters();
        batchManager.items = seed.map(item => ({
            id: item.id,
            originalPath: `/fixtures/${item.id}.${item.extension ?? 'pdf'}`,
            originalName: `${item.id}.${item.extension ?? 'pdf'}`,
            extension: item.extension ?? 'pdf', size: item.size ?? 4096, modifiedAt: 1,
            status: item.status ?? 'review', isAccepted: item.accepted ?? false,
            suggestedTitle: item.title ?? `Title ${item.id}`,
            suggestedAuthor: item.author ?? 'Author', suggestedYear: item.year ?? '2024',
            targetPath: `/fixtures/sorted/${item.id}.${item.extension ?? 'pdf'}`,
        }));
        root.__reviewItems = () => batchManager.items.map((item: any) => ({
            id: item.id, title: item.suggestedTitle, author: item.suggestedAuthor,
            year: item.suggestedYear, accepted: item.isAccepted, targetPath: item.targetPath,
        }));
        root.__fixtureCalls = [];
    }, items);
}

function batchFileNames(page: Page) {
    return page.locator('.batch-container table.dense-table .file-name');
}

async function openReview(page: Page) {
    await page.locator('.main-nav').getByRole('button', { name: 'Review', exact: true }).click();
    await expect(batchFileNames(page).first()).toBeVisible();
}

async function itemsSnapshot(page: Page) {
    return page.evaluate(() => (window as any).__reviewItems());
}

async function savedDrafts(page: Page) {
    return page.evaluate(async () => {
        const [drafts] = await (window as any).__TAURI_INTERNALS__.invoke('plugin:store|get', { key: 'batchMetadataDrafts' });
        return drafts ?? {};
    });
}

async function callsFor(page: Page, command: string) {
    return page.evaluate(name => (window as any).__fixtureCalls.filter((call: any) => call.command === name), command);
}

async function respondToConfirmation(page: Page, action: () => Promise<unknown>, message: string, accept: boolean) {
    const pendingDialog = page.waitForEvent('dialog');
    const pendingAction = action();
    const dialog = await pendingDialog;
    expect(dialog.type()).toBe('confirm');
    expect(dialog.message()).toContain(message);
    if (accept) await dialog.accept();
    else await dialog.dismiss();
    await pendingAction;
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    // Session restoration must finish before replacing the shared store.
    await expect(page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first()).toBeVisible();
});

test('select-all uses visible IDs and bulk approval/removal excludes hidden selections', async ({ page }) => {
    await seedBatch(page, reviewFiles);
    await openReview(page);
    const search = page.getByRole('textbox', { name: 'Search items', exact: true });
    const selectAll = page.getByRole('checkbox', { name: 'Select all visible', exact: true });
    const selection = page.locator('.selection-toolbar');

    await search.fill('alpha');
    await selectAll.check();
    await search.fill('beta');
    // One selected row and one visible row have equal counts but different IDs.
    await expect(selectAll).not.toBeChecked();
    await expect(selection).toContainText('0 selected in this view');
    await expect(selection).toContainText('1 hidden; excluded from actions');
    await expect(selection.getByRole('button', { name: 'Confirm', exact: true })).toBeDisabled();
    expect(await callsFor(page, 'batch_session_upsert_items_bulk')).toHaveLength(0);
    expect((await itemsSnapshot(page)).map((item: any) => item.accepted)).toEqual([false, false]);

    await selectAll.check();
    await expect(selection).toContainText('1 selected in this view');
    await selection.getByRole('button', { name: 'Confirm', exact: true }).click();
    await expect.poll(() => itemsSnapshot(page)).toEqual([
        expect.objectContaining({ id: 'alpha', accepted: false }),
        expect.objectContaining({ id: 'beta', accepted: true }),
    ]);
    const approvalCalls = await callsFor(page, 'batch_session_upsert_items_bulk');
    expect(approvalCalls.at(-1).args.items.map((item: any) => item.id)).toEqual(['beta']);

    await respondToConfirmation(page,
        () => selection.getByRole('button', { name: 'Remove', exact: true }).click(),
        'Remove the selected visible entries from this batch? Your files will stay on disk.', true);
    await expect.poll(() => itemsSnapshot(page)).toEqual([expect.objectContaining({ id: 'alpha', accepted: false })]);
    expect((await callsFor(page, 'batch_session_delete_items')).at(-1).args.ids).toEqual(['beta']);
    await page.locator('.active-batch-filters').getByRole('button', { name: 'Clear filters', exact: true }).click();
    await expect(batchFileNames(page)).toHaveText(['alpha.pdf']);
    await expect(page.getByRole('checkbox', { name: 'Select item', exact: true })).toBeChecked();
});

test('filters include present image/audio types and stage-specific statuses, and clear together', async ({ page }) => {
    await seedBatch(page, [
        { id: 'scan', extension: 'png', status: 'queued' },
        { id: 'recording', extension: 'mp3', status: 'error' },
        { id: 'book', extension: 'epub', status: 'unfinished' },
        { id: 'report', extension: 'docx', status: 'review' },
    ]);
    await page.getByRole('button', { name: 'Show Filters', exact: true }).click();
    const extensions = page.locator('#ext-filter');
    const statuses = page.locator('#status-filter');
    await expect(extensions.locator('option')).toHaveText(['All', 'EPUB', 'MP3', 'PNG']);
    expect(await statuses.locator('option').evaluateAll(options => options.map(option => (option as HTMLOptionElement).value)))
        .toEqual(['all', 'queued', 'unfinished', 'extracting', 'analyzing', 'error']);
    await extensions.selectOption('mp3');
    await statuses.selectOption('error');
    await page.locator('#size-filter').fill('1');
    await page.getByRole('textbox', { name: 'Search items', exact: true }).fill('absent');
    await expect(page.getByText('No files match these filters', { exact: true })).toBeVisible();
    await page.locator('.active-batch-filters').getByRole('button', { name: 'Clear filters', exact: true }).click();
    await expect(extensions).toHaveValue('all');
    await expect(statuses).toHaveValue('all');
    await expect(page.locator('#size-filter')).toHaveValue('0');
    await expect(page.getByRole('textbox', { name: 'Search items', exact: true })).toHaveValue('');
    await expect(batchFileNames(page)).toHaveText(['book.epub', 'recording.mp3', 'scan.png']);
    await expect(page.locator('.view-count')).toHaveText('3 / 3 files shown');

    await openReview(page);
    await page.getByRole('button', { name: 'Show Filters', exact: true }).click();
    await expect(page.locator('#ext-filter option')).toHaveText(['All', 'DOCX']);
    expect(await page.locator('#status-filter option').evaluateAll(options => options.map(option => (option as HTMLOptionElement).value)))
        .toEqual(['all', 'review', 'ready', 'moving']);
});

test('metadata drafts survive rows and tabs, persist separately, and apply only on explicit Save', async ({ page }) => {
    await seedBatch(page, reviewFiles);
    await openReview(page);
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    const title = page.getByRole('textbox', { name: 'Edit Title', exact: true });
    await title.fill('Corrected A');
    await expect.poll(() => savedDrafts(page)).toMatchObject({ alpha: { title: 'Corrected A', originalPath: '/fixtures/alpha.pdf', base: { title: 'Original A' } } });
    expect(await itemsSnapshot(page)).toEqual([
        expect.objectContaining({ id: 'alpha', title: 'Original A', accepted: false, targetPath: '/fixtures/sorted/alpha.pdf' }),
        expect.objectContaining({ id: 'beta', title: 'Original B', accepted: false }),
    ]);
    expect(await callsFor(page, 'batch_session_upsert_item')).toHaveLength(0);

    await batchFileNames(page).filter({ hasText: /^beta\.pdf$/ }).click();
    await expect(title).toHaveValue('Original B');
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    await expect(title).toHaveValue('Corrected A');
    await page.locator('.main-nav').getByRole('button', { name: 'Inbox', exact: true }).click();
    await openReview(page);
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    await expect(title).toHaveValue('Corrected A');
    await expect(page.locator('.detail-pane')).toContainText('Draft saved; changes are not applied yet.');

    await page.locator('.detail-pane').getByRole('button', { name: 'Save', exact: true }).click();
    await expect.poll(() => itemsSnapshot(page)).toEqual([
        expect.objectContaining({ id: 'alpha', title: 'Corrected A', accepted: true, targetPath: '/fixtures/sorted/Ada/2020/Corrected A.pdf' }),
        expect.objectContaining({ id: 'beta', title: 'Original B', accepted: false }),
    ]);
    await expect.poll(() => savedDrafts(page)).toEqual({});
    const writes = await callsFor(page, 'batch_session_upsert_item');
    expect(writes.at(-1).args.item).toMatchObject({ id: 'alpha', suggestedTitle: 'Corrected A', isAccepted: true });
});

test('Save & next applies one draft and advances to the next visible document', async ({ page }) => {
    await seedBatch(page, reviewFiles);
    await openReview(page);
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    await page.getByRole('textbox', { name: 'Edit Title', exact: true }).fill('Reviewed A');
    await page.locator('.detail-pane').getByRole('button', { name: 'Save & next', exact: false }).click();
    await expect(page.getByRole('textbox', { name: 'Edit Title', exact: true })).toHaveValue('Original B');
    await expect(page.getByRole('textbox', { name: 'Edit Author', exact: true })).toHaveValue('Ben');
    await expect(page.locator('.detail-navigation')).toContainText('2 / 2');
    await expect.poll(() => itemsSnapshot(page)).toEqual([
        expect.objectContaining({ id: 'alpha', title: 'Reviewed A', accepted: true }),
        expect.objectContaining({ id: 'beta', title: 'Original B', accepted: false }),
    ]);
    await expect.poll(() => savedDrafts(page)).toEqual({});
});

test('sorting an accepted document requires resolving its unapplied metadata draft', async ({ page }) => {
    await seedBatch(page, [{ ...reviewFiles[0], accepted: true }]);
    await openReview(page);
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    await page.getByRole('textbox', { name: 'Edit Title', exact: true }).fill('Draft destination');
    await page.getByRole('button', { name: 'Sort', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('Save or discard metadata drafts for accepted files before sorting.');
    await expect(page.getByRole('dialog', { name: 'Review sort plan' })).not.toBeVisible();
    expect(await callsFor(page, 'execute_batch')).toHaveLength(0);
    expect(await itemsSnapshot(page)).toEqual([expect.objectContaining({ id: 'alpha', title: 'Original A', accepted: true })]);

    await page.getByRole('button', { name: 'Discard draft', exact: true }).click();
    await page.getByRole('button', { name: 'Sort', exact: true }).click();
    await expect(page.getByRole('dialog', { name: 'Review sort plan' })).toBeVisible();
    expect(await callsFor(page, 'execute_batch')).toHaveLength(0);
});

test('clearing a batch asks for confirmation and preserves rows and drafts on storage failure', async ({ page }) => {
    await seedBatch(page, reviewFiles);
    await openReview(page);
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    await page.getByRole('textbox', { name: 'Edit Title', exact: true }).fill('Keep this draft');
    await expect.poll(() => savedDrafts(page)).toMatchObject({ alpha: { title: 'Keep this draft' } });
    const clear = page.getByRole('button', { name: 'Clear All', exact: true });
    const message = 'Remove all entries and metadata drafts from this batch? Your files will stay on disk.';
    await respondToConfirmation(page, () => clear.click(), message, false);
    expect(await callsFor(page, 'batch_session_clear')).toHaveLength(0);
    expect((await itemsSnapshot(page)).map((item: any) => item.id)).toEqual(['alpha', 'beta']);

    await page.evaluate(() => { (window as any).__reviewFailClear = true; });
    await respondToConfirmation(page, () => clear.click(), message, true);
    await expect(page.getByRole('alert')).toContainText('Fixture clear failure: database is locked');
    expect((await itemsSnapshot(page)).map((item: any) => item.id)).toEqual(['alpha', 'beta']);
    expect(await savedDrafts(page)).toMatchObject({ alpha: { title: 'Keep this draft' } });

    await page.evaluate(() => { (window as any).__reviewFailClear = false; });
    await respondToConfirmation(page, () => clear.click(), message, true);
    await expect.poll(() => itemsSnapshot(page)).toEqual([]);
    await expect.poll(() => savedDrafts(page)).toEqual({});
    await expect(page.getByText('Nothing to review yet', { exact: true })).toBeVisible();
    expect(await callsFor(page, 'batch_session_clear')).toHaveLength(2);
    expect(await callsFor(page, 'execute_batch')).toHaveLength(0);
    expect(await callsFor(page, 'delete_duplicate_copy')).toHaveLength(0);
});

test('a pending metadata Save locks batch edits across tab remounts and preserves other drafts', async ({ page }) => {
    await seedBatch(page, reviewFiles);
    await openReview(page);
    const title = page.getByRole('textbox', { name: 'Edit Title', exact: true });
    await batchFileNames(page).filter({ hasText: /^beta\.pdf$/ }).click();
    await title.fill('Keep B draft');
    await expect.poll(() => savedDrafts(page)).toMatchObject({ beta: { title: 'Keep B draft' } });
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    await title.fill('Applied A');
    await expect.poll(() => savedDrafts(page)).toMatchObject({ alpha: { title: 'Applied A' } });
    await page.getByRole('checkbox', { name: 'Select item', exact: true }).first().check();
    await page.evaluate(() => {
        const root = window as any;
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'batch_session_upsert_item' && args.item.id === 'alpha') {
                root.__fixtureCalls.push({ command, args: JSON.parse(JSON.stringify(args)) });
                return new Promise<void>(resolve => { root.__releaseReviewSave = resolve; });
            }
            return originalInvoke(command, args);
        };
    });
    await page.locator('.detail-pane').getByRole('button', { name: 'Save', exact: true }).click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseReviewSave)).toBe('function');

    const clear = page.getByRole('button', { name: 'Clear All', exact: true });
    const remove = page.locator('.selection-toolbar').getByRole('button', { name: 'Remove', exact: true });
    await expect(clear).toBeDisabled();
    await expect(remove).toBeDisabled();
    await expect(title).toBeDisabled();
    for (const field of ['Suggested Title', 'Suggested Author', 'Suggested Year']) {
        const inputs = page.getByRole('textbox', { name: field, exact: true });
        await expect(inputs).toHaveCount(2);
        await expect(inputs.first()).toBeDisabled();
        await expect(inputs.last()).toBeDisabled();
    }

    await page.locator('.main-nav').getByRole('button', { name: 'Inbox', exact: true }).click();
    await openReview(page);
    await batchFileNames(page).filter({ hasText: /^beta\.pdf$/ }).click();
    await page.getByRole('checkbox', { name: 'Select item', exact: true }).last().check();
    await expect(title).toHaveValue('Keep B draft');
    await expect(title).toBeDisabled();
    await expect(clear).toBeDisabled();
    await expect(remove).toBeDisabled();
    await expect(page.getByRole('textbox', { name: 'Suggested Title', exact: true }).last()).toBeDisabled();

    await page.evaluate(() => { (window as any).__releaseReviewSave(); });
    await expect(title).toBeEnabled();
    await expect(clear).toBeEnabled();
    await expect(remove).toBeEnabled();
    await expect.poll(() => itemsSnapshot(page)).toEqual([
        expect.objectContaining({ id: 'alpha', title: 'Applied A', accepted: true }),
        expect.objectContaining({ id: 'beta', title: 'Original B', accepted: false }),
    ]);
    await expect.poll(() => savedDrafts(page)).toEqual({
        beta: expect.objectContaining({ title: 'Keep B draft' }),
    });
    await title.fill('New B after save');
    await expect.poll(() => savedDrafts(page)).toEqual({
        beta: expect.objectContaining({ title: 'New B after save' }),
    });
    const writes = await callsFor(page, 'batch_session_upsert_item');
    expect(writes).toHaveLength(1);
    expect(writes[0].args.item).toMatchObject({ id: 'alpha', suggestedTitle: 'Applied A', isAccepted: true });
    expect(await callsFor(page, 'batch_session_clear')).toHaveLength(0);
    expect(await callsFor(page, 'batch_session_delete_items')).toHaveLength(0);
});

test('failed draft restoration stays read-only and Retry recovers preserved drafts without overwriting them', async ({ page }) => {
    await page.addInitScript(() => {
        const root = window as any;
        root.__draftLoadFailure = true;
        root.__draftReadCount = 0;
        root.__draftWrites = [];
        const preservedDrafts = {
            alpha: {
                originalPath: '/fixtures/alpha.pdf', title: 'Recovered draft', author: 'Ada', year: '2020',
                base: { title: 'Original A', author: 'Ada', year: '2020' },
            },
        };
        // Init-script order is unspecified. Intercept both an existing fixture
        // bridge and one installed after this script, before the app can load.
        const wrapped = new WeakSet<object>();
        const wrap = (bridge: any) => {
            if (!bridge || wrapped.has(bridge)) return bridge;
            wrapped.add(bridge);
            const originalInvoke = bridge.invoke;
            bridge.invoke = async (command: string, args: any = {}) => {
                if (command === 'plugin:store|get' && args.key === 'batchMetadataDrafts') {
                    root.__draftReadCount += 1;
                    if (root.__draftLoadFailure) throw new Error('Fixture draft storage read failure');
                    return [JSON.parse(JSON.stringify(preservedDrafts)), true];
                }
                if (command === 'plugin:store|set' && args.key === 'batchMetadataDrafts') {
                    root.__draftWrites.push(JSON.parse(JSON.stringify(args.value)));
                }
                return originalInvoke(command, args);
            };
            return bridge;
        };
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', {
            configurable: true,
            get: () => bridge,
            set: value => { bridge = wrap(value); },
        });
    });
    await page.reload();
    await expect(page.locator('.batch-container').getByText('Pending scan.png', { exact: true }).first()).toBeVisible();
    await seedBatch(page, reviewFiles);
    await openReview(page);
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    const storageError = page.locator('.draft-storage-error');
    await expect(storageError).toContainText('Saved metadata drafts could not be loaded. Retry before editing them.');
    for (const field of ['Edit Title', 'Edit Author', 'Edit Year']) {
        await expect(page.getByRole('textbox', { name: field, exact: true })).toBeDisabled();
    }
    const failedReads = await page.evaluate(() => (window as any).__draftReadCount);
    expect(failedReads).toBeGreaterThan(0);
    expect(await page.evaluate(() => (window as any).__draftWrites)).toEqual([]);

    await page.evaluate(() => { (window as any).__draftLoadFailure = false; });
    await storageError.getByRole('button', { name: 'Retry', exact: true }).click();
    await expect(storageError).not.toBeVisible();
    const title = page.getByRole('textbox', { name: 'Edit Title', exact: true });
    await expect(title).toBeEnabled();
    await expect(title).toHaveValue('Recovered draft');
    expect(await page.evaluate(() => (window as any).__draftReadCount)).toBeGreaterThan(failedReads);
    expect(await page.evaluate(() => (window as any).__draftWrites)).toEqual([]);
    expect(await itemsSnapshot(page)).toEqual([
        expect.objectContaining({ id: 'alpha', title: 'Original A', accepted: false, targetPath: '/fixtures/sorted/alpha.pdf' }),
        expect.objectContaining({ id: 'beta', title: 'Original B', accepted: false }),
    ]);
    expect(await callsFor(page, 'batch_session_upsert_item')).toHaveLength(0);
});

test('a failed metadata save restores applied values and keeps the draft available for retry', async ({ page }) => {
    await seedBatch(page, reviewFiles);
    await openReview(page);
    await batchFileNames(page).filter({ hasText: /^alpha\.pdf$/ }).click();
    const title = page.getByRole('textbox', { name: 'Edit Title', exact: true });
    await title.fill('Retry draft');
    await expect.poll(() => savedDrafts(page)).toMatchObject({ alpha: { title: 'Retry draft' } });
    await page.evaluate(() => {
        const root = window as any;
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        root.__reviewFailSave = true;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'batch_session_upsert_item' && args.item.id === 'alpha') {
                root.__fixtureCalls.push({ command, args: JSON.parse(JSON.stringify(args)) });
                if (root.__reviewFailSave) throw new Error('Fixture metadata save failure');
                return;
            }
            return originalInvoke(command, args);
        };
    });
    const save = page.locator('.detail-pane').getByRole('button', { name: 'Save', exact: true });
    await save.click();
    await expect(page.locator('.detail-pane').getByRole('alert')).toContainText('Fixture metadata save failure');
    await expect(title).toBeEnabled();
    await expect(title).toHaveValue('Retry draft');
    await expect(page.getByRole('button', { name: 'Clear All', exact: true })).toBeEnabled();
    expect(await itemsSnapshot(page)).toEqual([
        expect.objectContaining({ id: 'alpha', title: 'Original A', author: 'Ada', year: '2020', accepted: false, targetPath: '/fixtures/sorted/alpha.pdf' }),
        expect.objectContaining({ id: 'beta', title: 'Original B', accepted: false }),
    ]);
    expect(await savedDrafts(page)).toMatchObject({ alpha: { title: 'Retry draft', base: { title: 'Original A' } } });

    await page.evaluate(() => { (window as any).__reviewFailSave = false; });
    await save.click();
    await expect(page.locator('.detail-pane').getByRole('alert')).not.toBeVisible();
    await expect.poll(() => itemsSnapshot(page)).toEqual([
        expect.objectContaining({ id: 'alpha', title: 'Retry draft', accepted: true, targetPath: '/fixtures/sorted/Ada/2020/Retry draft.pdf' }),
        expect.objectContaining({ id: 'beta', title: 'Original B', accepted: false }),
    ]);
    await expect.poll(() => savedDrafts(page)).toEqual({});
    expect(await callsFor(page, 'batch_session_upsert_item')).toHaveLength(2);
});

test('Cmd/Ctrl+A selects every visible file instead of page text, but not while typing', async ({ page }) => {
    await seedBatch(page, reviewFiles);
    await openReview(page);
    const selectAll = page.getByRole('checkbox', { name: 'Select all visible', exact: true });
    const modifier = process.platform === 'darwin' ? 'Meta' : 'Control';
    await page.locator('.batch-container .file-name').first().click();
    await page.keyboard.press(`${modifier}+A`);
    await expect(selectAll).toBeChecked();
    await expect(page.getByRole('checkbox', { name: 'Select item', exact: true })).toHaveCount(2);
    for (const box of await page.getByRole('checkbox', { name: 'Select item', exact: true }).all()) await expect(box).toBeChecked();
    expect(await page.evaluate(() => window.getSelection()?.toString() ?? '')).toBe('');

    // In a text field the shortcut keeps its normal text meaning.
    await page.keyboard.press('Escape');
    await expect(selectAll).not.toBeChecked();
    const search = page.getByRole('textbox', { name: 'Search items', exact: true });
    await search.fill('pdf');
    await search.press(`${modifier}+A`);
    await expect(selectAll).not.toBeChecked();
    expect(await search.evaluate((input: HTMLInputElement) => input.value.slice(input.selectionStart ?? 0, input.selectionEnd ?? 0))).toBe('pdf');
});
