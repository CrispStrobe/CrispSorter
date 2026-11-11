import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';
import type { BatchStatus } from '../../src/lib/types';

interface RecoveryItem {
    id: string;
    status: BatchStatus;
    accepted?: boolean;
    extractedText?: string;
}

async function seedRecovery(page: Page, items: RecoveryItem[]) {
    await page.evaluate(async seed => {
        const root = window as any;
        const storePath = '/src/lib/batch/store.svelte.ts';
        const clientPath = '/src/lib/llm/client.ts';
        const { batchManager } = await import(storePath);
        const { llmClient } = await import(clientPath);
        const originalInvoke = root.__TAURI_INTERNALS__.invoke;
        for (const [key, value] of Object.entries({
            providers: [{ id: 'fixture', selectedModel: 'fixture-model', models: ['fixture-model'], apiKey: '' }],
            activeProviderId: 'fixture', roundRobinProviders: [], parsingFormat: 'xml', authorSortEnabled: false,
            pdfBackend: 'rust', pdfMetadataPrefill: false, conversionTimeoutSeconds: 0,
            exportPath: '/fixtures/sorted', pathTemplate: '{Author}/{Year}/{Title}',
        })) await originalInvoke('plugin:store|set', { key, value });

        root.__recoveryManager = batchManager;
        root.__recoveryAnalyses = [];
        root.__recoveryExtractGate = false;
        root.__recoveryPersistGate = false;
        root.__recoverySortGate = false;
        root.__recoveryDeleteError = false;
        root.__recoveryClearError = false;
        root.__recoveryTextLoadErrorId = '';
        root.__recoverySortResults = {};
        const text = 'Saved extraction from the interrupted document. '.repeat(8);
        // Only external recognition/filesystem boundaries are mocked; the real
        // batch workers, cancellation flags, persistence calls and UI run.
        llmClient.query = async (_provider: string, _model: string, prompt: string) => {
            root.__recoveryAnalyses.push(prompt);
            return '<TITLE>Recovered title</TITLE><AUTHOR>Recovered author</AUTHOR><YEAR>2026</YEAR>';
        };
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'batch_session_get_extracted_text' && args.itemId === root.__recoveryTextLoadErrorId) {
                root.__fixtureCalls.push({ command, args });
                throw new Error('Fixture stored text failure');
            }
            if (command === 'extract_pdf_native') {
                root.__fixtureCalls.push({ command, args });
                if (root.__recoveryExtractGate) {
                    await new Promise<void>(resolve => { root.__releaseRecoveryExtraction = () => { root.__recoveryExtractGate = false; resolve(); }; });
                }
                return text;
            }
            if (command === 'batch_session_upsert_items_bulk' && batchManager.isStopping && root.__recoveryPersistGate) {
                root.__fixtureCalls.push({ command, args: JSON.parse(JSON.stringify(args)) });
                await new Promise<void>(resolve => { root.__releaseRecoveryPersistence = () => { root.__recoveryPersistGate = false; resolve(); }; });
                return;
            }
            if (command === 'batch_session_delete_items' && root.__recoveryDeleteError) {
                root.__fixtureCalls.push({ command, args });
                throw new Error('Fixture cleanup delete failure');
            }
            if (command === 'batch_session_clear' && root.__recoveryClearError) {
                root.__fixtureCalls.push({ command, args });
                throw new Error('Fixture cleanup clear failure');
            }
            if (command === 'execute_batch') {
                root.__fixtureCalls.push({ command, args });
                if (root.__recoverySortGate) {
                    await new Promise<void>(resolve => { root.__releaseRecoverySort = () => { root.__recoverySortGate = false; resolve(); }; });
                }
                return Object.fromEntries(args.payload.items.map((item: any) => [
                    item.id, root.__recoverySortResults[item.id] ?? { success: true },
                ]));
            }
            return originalInvoke(command, args);
        };
        batchManager.resetFilters();
        batchManager.items = seed.map((item, index) => ({
            id: item.id, originalPath: `/fixtures/${item.id}.pdf`, originalName: `${item.id}.pdf`,
            extension: 'pdf', size: 1024 + index, modifiedAt: 1, status: item.status,
            isAccepted: item.accepted ?? item.status === 'review', skipHistoryCheck: true,
            extractedText: item.extractedText, suggestedTitle: `Saved ${item.id}`, suggestedAuthor: 'Saved author',
            suggestedYear: '2024', targetPath: `/fixtures/sorted/${item.id}.pdf`,
        }));
        root.__recoverySnapshot = () => JSON.parse(JSON.stringify({
            processing: batchManager.isProcessing, stopping: batchManager.isStopping, executing: batchManager.isExecuting,
            items: batchManager.items,
        }));
        root.__fixtureCalls = [];
    }, items);
}

async function snapshot(page: Page) {
    return page.evaluate(() => (window as any).__recoverySnapshot());
}

async function callsFor(page: Page, command: string) {
    return page.evaluate(name => (window as any).__fixtureCalls.filter((call: any) => call.command === name), command);
}

async function openSortPlan(page: Page) {
    await page.locator('nav.main-nav').getByRole('button', { name: 'Review', exact: true }).click();
    await page.getByRole('button', { name: 'Sort', exact: true }).click();
    const plan = page.getByRole('dialog', { name: 'Review sort plan', exact: true });
    await expect(plan).toBeVisible();
    await plan.getByRole('button', { name: 'Confirm and execute', exact: true }).click();
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    await expect.poll(() => page.evaluate(async () => {
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        return batchManager.sessionReady && !batchManager.isUpdatingBatch;
    }), { timeout: 30000 }).toBe(true);
});

test('Stop drains its worker and final save before Resume, retaining completed work and extracted text', async ({ page }) => {
    await seedRecovery(page, [
        { id: 'completed', status: 'review', extractedText: 'Already reviewed text.' },
        { id: 'interrupted', status: 'unfinished' },
    ]);
    await page.evaluate(() => { (window as any).__recoveryExtractGate = true; });
    const resume = page.getByRole('button', { name: 'Resume', exact: true });
    await expect(resume).toBeEnabled();
    await resume.click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseRecoveryExtraction)).toBe('function');
    await page.getByRole('button', { name: 'Stop', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Stopping…', exact: true })).toBeDisabled();
    await expect(page.locator('.processing-notice')).toContainText('Completed work will be kept.');
    await expect(page.getByRole('button', { name: 'Clear All', exact: true })).toBeDisabled();
    await page.evaluate(async () => {
        const manager = (window as any).__recoveryManager;
        manager.stopAll();
        await manager.forceReset();
        await manager.resetToQueued();
        await manager.reprocessItems(['interrupted']);
        await manager.processAll();
    });
    expect(await snapshot(page)).toMatchObject({ processing: true, stopping: true, items: [
        expect.objectContaining({ id: 'completed', status: 'review', suggestedTitle: 'Saved completed' }),
        expect.objectContaining({ id: 'interrupted', status: 'extracting' }),
    ] });
    expect(await callsFor(page, 'extract_pdf_native')).toHaveLength(1);

    await page.evaluate(() => {
        (window as any).__recoveryPersistGate = true;
        (window as any).__releaseRecoveryExtraction();
    });
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseRecoveryPersistence)).toBe('function');
    await expect(page.getByRole('button', { name: 'Stopping…', exact: true })).toBeDisabled();
    expect(await snapshot(page)).toMatchObject({ processing: true, stopping: true, items: [
        expect.objectContaining({ id: 'completed', status: 'review' }),
        expect.objectContaining({ id: 'interrupted', status: 'unfinished', extractedText: expect.stringContaining('Saved extraction') }),
    ] });
    expect(await page.evaluate(() => (window as any).__recoveryAnalyses)).toEqual([]);
    await page.evaluate(() => { (window as any).__releaseRecoveryPersistence(); });
    await expect(resume).toBeEnabled();
    await resume.click();
    await expect.poll(() => snapshot(page)).toMatchObject({ processing: false, stopping: false, items: [
        expect.objectContaining({ id: 'completed', status: 'review', suggestedTitle: 'Saved completed', suggestedAuthor: 'Saved author' }),
        expect.objectContaining({ id: 'interrupted', status: 'review', suggestedTitle: 'Recovered title', suggestedAuthor: 'Recovered author' }),
    ] });
    expect(await callsFor(page, 'extract_pdf_native')).toHaveLength(1);
    const prompts = await page.evaluate(() => (window as any).__recoveryAnalyses);
    expect(prompts).toHaveLength(1);
    expect(prompts[0]).toContain('interrupted.pdf');
    expect(prompts[0]).toContain('Saved extraction from the interrupted document.');
});

test('final batch checkpoint failure stays visible and releases controls without a completed receipt', async ({ page }) => {
    await seedRecovery(page, [{ id: 'checkpoint', status: 'review' }]);
    const messages: string[] = [];
    page.on('console', message => { if (message.text().includes('batch.processing ')) messages.push(message.text()); });
    const outcome = await page.evaluate(async () => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any, options: any) => {
            if (command === 'batch_session_upsert_items_bulk') throw new Error('Fixture final checkpoint failed');
            return original(command, args, options);
        };
        const modulePath = '/src/lib/batch/store.svelte.ts';
        const { batchManager } = await import(modulePath);
        try {
            // An empty selection reaches the real final checkpoint without
            // invoking an extractor or language model.
            await batchManager.processAll({ extractionOnly: true }, new Set());
            return { error: null, processing: batchManager.isProcessing };
        } catch (error) { return { error: String(error), processing: batchManager.isProcessing }; }
    });
    expect(outcome).toEqual({ error: 'Error: Fixture final checkpoint failed', processing: false });
    expect(messages.some(message => message.includes('phase=failed') && message.includes('saved=false'))).toBe(true);
    expect(messages.some(message => message.includes('phase=completed'))).toBe(false);
    await expect(page.getByRole('button', { name: 'Clear All', exact: true })).toBeEnabled();
});

test('one failing worker does not unlock the batch while another worker is still draining', async ({ page }) => {
    await seedRecovery(page, [{ id: 'broken', status: 'queued' }, { id: 'draining', status: 'queued' }]);
    await page.evaluate(async () => {
        const root = window as any;
        root.__recoveryTextLoadErrorId = 'broken';
        root.__recoveryExtractGate = true;
        await root.__TAURI_INTERNALS__.invoke('plugin:store|set', { key: 'extractionWorkers', value: 2 });
    });
    await page.getByLabel('Processing preset', { exact: true }).selectOption('advanced');
    await page.getByRole('button', { name: 'Start', exact: true }).click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseRecoveryExtraction)).toBe('function');
    expect(await snapshot(page)).toMatchObject({ processing: true, items: [
        expect.objectContaining({ id: 'broken', status: 'queued' }),
        expect.objectContaining({ id: 'draining', status: 'extracting' }),
    ] });
    await expect(page.getByRole('button', { name: 'Clear All', exact: true })).toBeDisabled();
    await page.evaluate(() => { (window as any).__releaseRecoveryExtraction(); });
    await expect(page.getByRole('alert')).toContainText('Fixture stored text failure');
    await expect.poll(() => snapshot(page)).toMatchObject({ processing: false, items: [
        expect.objectContaining({ id: 'broken', status: 'queued' }),
        expect.objectContaining({ id: 'draining', status: 'review', suggestedTitle: 'Recovered title' }),
    ] });
});

test('sort cleanup retains copy fallbacks and failures, and retries a failed database deletion', async ({ page }) => {
    await seedRecovery(page, [
        { id: 'clean', status: 'review' }, { id: 'copy-warning', status: 'review' }, { id: 'failed', status: 'review' },
    ]);
    await page.evaluate(() => {
        const root = window as any;
        root.__recoverySortGate = true;
        root.__recoveryDeleteError = true;
        root.__recoverySortResults = { 'copy-warning': { success: true, error: 'COPY_FALLBACK: original locked' }, failed: { success: false, error: 'LOCKED' } };
    });
    await openSortPlan(page);
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseRecoverySort)).toBe('function');
    await page.evaluate(async () => {
        const manager = (window as any).__recoveryManager;
        await manager.forceReset();
        await manager.processAll();
    });
    expect(await snapshot(page)).toMatchObject({ executing: true, processing: false });
    expect(await callsFor(page, 'execute_batch')).toHaveLength(1);
    expect(await callsFor(page, 'extract_pdf_native')).toHaveLength(0);
    await page.evaluate(() => { (window as any).__releaseRecoverySort(); });
    const report = page.locator('.report-modal');
    const keepProblems = report.getByRole('button', { name: 'Keep problematic files in batch', exact: true });
    await keepProblems.click();
    await expect(report.getByRole('alert')).toContainText('Fixture cleanup delete failure');
    await expect(keepProblems).toBeEnabled();
    expect((await snapshot(page)).items.map((item: any) => item.id)).toEqual(['clean', 'copy-warning', 'failed']);
    expect((await callsFor(page, 'batch_session_delete_items')).at(-1).args.ids).toEqual(['clean']);

    await page.evaluate(() => { (window as any).__recoveryDeleteError = false; });
    await keepProblems.click();
    await expect(report).not.toBeVisible();
    expect((await snapshot(page)).items).toEqual([
        expect.objectContaining({ id: 'copy-warning', status: 'done', errorMessage: 'COPY_FALLBACK' }),
        expect.objectContaining({ id: 'failed', status: 'error', errorMessage: 'LOCKED' }),
    ]);
    await page.locator('nav.main-nav').getByRole('button', { name: 'Inbox', exact: true }).click();
    const table = page.locator('.batch-container table.dense-table');
    await expect(table.locator('.file-name')).toHaveText(['copy-warning.pdf', 'failed.pdf']);
    const fallback = table.getByRole('row').filter({ hasText: 'copy-warning.pdf' });
    await expect(fallback).toContainText('Copied; original kept');
    await expect(fallback.getByRole('textbox', { name: 'Suggested Title', exact: true })).toBeDisabled();
    await page.getByRole('button', { name: 'Sort', exact: true }).click();
    const plan = page.getByRole('dialog', { name: 'Review sort plan', exact: true });
    await expect(plan.getByText('/fixtures/failed.pdf', { exact: true })).toBeVisible();
    await expect(plan.getByText('/fixtures/copy-warning.pdf', { exact: true })).toHaveCount(0);
    await plan.getByRole('button', { name: 'Cancel', exact: true }).click();
    expect(await callsFor(page, 'execute_batch')).toHaveLength(1);
});

test('Empty batch keeps the report and entries after a storage failure, then clears on retry', async ({ page }) => {
    await seedRecovery(page, [{ id: 'sorted', status: 'review' }]);
    await page.evaluate(() => { (window as any).__recoveryClearError = true; });
    await openSortPlan(page);
    const report = page.locator('.report-modal');
    const empty = report.getByRole('button', { name: 'Empty batch', exact: true });
    await empty.click();
    await expect(report.getByRole('alert')).toContainText('Fixture cleanup clear failure');
    await expect(empty).toBeEnabled();
    expect((await snapshot(page)).items).toEqual([expect.objectContaining({ id: 'sorted', status: 'done' })]);
    await page.evaluate(() => { (window as any).__recoveryClearError = false; });
    await empty.click();
    await expect(report).not.toBeVisible();
    expect((await snapshot(page)).items).toEqual([]);
    expect(await callsFor(page, 'batch_session_clear')).toHaveLength(2);
});

test('a moved file retains an index warning until a retried update is durably acknowledged', async ({ page }) => {
    await seedRecovery(page, [{ id: 'index-warning', status: 'review' }]);
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__failIndexUpdate = true;
        root.__holdIndexUpdate = true;
        root.__failIndexAcknowledgement = false;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}) => {
            if (command === 'index_update_location_by_path') {
                root.__fixtureCalls.push({ command, args });
                if (root.__holdIndexUpdate) {
                    await new Promise<void>(resolve => { root.__releaseIndexUpdate = () => { root.__holdIndexUpdate = false; resolve(); }; });
                }
                if (root.__failIndexUpdate) throw new Error('Fixture index storage failure');
                return;
            }
            if (command === 'batch_session_upsert_item' && root.__failIndexAcknowledgement) {
                throw new Error('Fixture index acknowledgement failure');
            }
            return original(command, args);
        };
    });
    await openSortPlan(page);
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseIndexUpdate)).toBe('function');
    const durableMoves = await callsFor(page, 'batch_session_upsert_items_bulk');
    expect(durableMoves.at(-1).args.items).toEqual([expect.objectContaining({
        status: 'done', statusDetail: 'moved (index update pending)',
        errorMessage: expect.stringContaining('INDEX_UPDATE_FAILED:'),
    })]);
    expect(await callsFor(page, 'execute_batch')).toHaveLength(1);
    await page.evaluate(() => (window as any).__releaseIndexUpdate());
    const report = page.locator('.report-modal');
    await expect(report.getByRole('alert')).toContainText('search index updates are still pending');
    await report.getByRole('button', { name: 'Keep problematic files in batch', exact: true }).click();
    await page.locator('nav.main-nav').getByRole('button', { name: 'Inbox', exact: true }).click();
    const row = page.locator('.batch-container table.dense-table').getByRole('row').filter({ hasText: 'index-warning.pdf' });
    await expect(row).toContainText('Index update pending');
    await row.locator('.file-name').click();
    const retry = page.getByRole('button', { name: 'Retry search index update', exact: true });
    await page.evaluate(() => { (window as any).__failIndexUpdate = false; (window as any).__failIndexAcknowledgement = true; });
    await retry.click();
    await expect(page.getByRole('alert')).toContainText('Fixture index acknowledgement failure');
    expect((await snapshot(page)).items[0]).toMatchObject({ status: 'done', errorMessage: expect.stringContaining('INDEX_UPDATE_FAILED:') });
    await page.evaluate(() => { (window as any).__failIndexAcknowledgement = false; });
    await retry.click();
    await expect.poll(() => snapshot(page)).toMatchObject({ items: [expect.objectContaining({ status: 'done', statusDetail: 'moved' })] });
    expect((await snapshot(page)).items[0].errorMessage).toBeUndefined();
    expect(await callsFor(page, 'execute_batch')).toHaveLength(1);
    expect(await callsFor(page, 'index_update_location_by_path')).toHaveLength(3);
    expect((await callsFor(page, 'index_update_location_by_path')).at(-1).args.requireUpdate).toBe(true);
});
