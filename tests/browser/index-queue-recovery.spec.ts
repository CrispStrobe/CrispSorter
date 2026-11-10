// Browser/IPC contracts; persistence and crash repair are exercised against
// real storage engines in tests/index-write-contract.
import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

type SeedFile = { name: string; status: 'pending' | 'done' | 'error'; text?: string };
type QueueStatus = SeedFile['status'] | 'skipped' | 'in_progress';

async function seedQueue(page: Page, files: SeedFile[], status = 'paused') {
    await page.evaluate(({ files, status }) => {
        const root = window as any;
        const original = root.__TAURI_INTERNALS__.invoke;
        const rows: any[] = files.map((file, index) => ({
            rowId: index + 1, jobId: 'queue-fixture', filePath: `/fixtures/${file.name}`,
            docId: null, targetLevel: 3, status: file.status as QueueStatus,
            errorText: file.status === 'error' ? 'Saved extraction failure' : null, retryCount: 0,
        }));
        const texts = new Map(files.map(file => [`/fixtures/${file.name}`, file.text ?? 'Searchable fixture text.']));
        let jobStatus = status;
        let exists = true;
        root.__queueCalls = [];
        root.__queueRows = rows;
        root.__queueGate = false;
        root.__queueAckGate = false;
        root.__queueDeleteFailure = false;
        root.__queueAckFailure = false;
        root.__queueErrorAckFailure = false;
        root.__queueL2Failure = '';
        root.__queueChangedSource = '';
        const summary = () => ({
            id: 'queue-fixture', jobType: 'hinzufuegen', status: jobStatus,
            totalFiles: rows.length, doneFiles: rows.filter(row => row.status === 'done').length,
            errorFiles: rows.filter(row => row.status === 'error').length,
            skippedFiles: rows.filter(row => row.status === 'skipped').length,
            inProgressFiles: rows.filter(row => row.status === 'in_progress').length,
        });
        root.__queueSummary = summary;
        const count = (callback: (row: any) => boolean) => rows.filter(callback).length;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any = {}, options: any) => {
            if (command.startsWith('jobs_') || command === 'index_ingest_l1' || command === 'index_ingest_batch'
                || command === 'index_promote_l2' || command === 'file_sha256') {
                root.__queueCalls.push({ command, args: JSON.parse(JSON.stringify(args)) });
            }
            if (command === 'jobs_list') return exists ? [summary()] : [];
            if (command === 'jobs_get') return exists ? summary() : null;
            if (command === 'jobs_set_status') { jobStatus = args.status; return; }
            if (command === 'jobs_reclaim') {
                const reclaimed = count(row => row.status === 'in_progress');
                for (const row of rows) if (row.status === 'in_progress') row.status = 'pending';
                return reclaimed;
            }
            if (command === 'jobs_retry_errors') {
                if (count(row => row.status === 'in_progress')) throw new Error('Active work must drain');
                const retried = count(row => row.status === 'error');
                for (const row of rows) if (row.status === 'error') { row.status = 'pending'; row.errorText = null; row.retryCount = 0; }
                return retried;
            }
            if (command === 'jobs_list_files') return rows.filter(row => !args.statusFilter || row.status === args.statusFilter)
                .slice(args.offset, args.offset + args.limit).map(row => ({ ...row }));
            if (command === 'jobs_claim_batch') {
                const claimed = rows.filter(row => row.status === 'pending').slice(0, args.batchSize);
                for (const row of claimed) row.status = 'in_progress';
                return claimed.map(row => ({ ...row }));
            }
            if (command === 'jobs_set_doc_id') { rows.find(row => row.rowId === args.rowId)!.docId = args.docId; return; }
            if (command === 'jobs_mark_done') {
                if (root.__queueAckGate) await new Promise<void>(resolve => { root.__releaseQueueAck = () => { root.__queueAckGate = false; resolve(); }; });
                if (root.__queueAckFailure) throw new Error('Fixture acknowledgement storage failure');
                for (const row of rows) if (args.rowIds.includes(row.rowId)) { row.status = 'done'; row.errorText = null; }
                return;
            }
            if (command === 'jobs_mark_error') {
                if (root.__queueErrorAckFailure) throw new Error('Fixture error storage failure');
                const row = rows.find(row => row.rowId === args.rowId)!;
                if (row.status !== 'done') { row.status = 'error'; row.errorText = args.error; }
                return;
            }
            if (command === 'jobs_remove_file' || command === 'jobs_delete') {
                if (root.__queueDeleteFailure) throw new Error('Fixture queue deletion failure');
                if (command === 'jobs_delete') { rows.splice(0); exists = false; return; }
                const position = rows.findIndex(row => row.rowId === args.rowId);
                if (position < 0) return false;
                rows.splice(position, 1); return true;
            }
            if (command === 'index_volume_id_for_path') return null;
            if (command === 'index_ingest_l1' || command === 'index_ingest_batch') {
                if (root.__queueGate) await new Promise<void>(resolve => { root.__releaseQueueWrite = () => { root.__queueGate = false; resolve(); }; });
                return { chunk_count: args.inputs?.length ?? 0, embed_time_ms: 1, write_time_ms: 1,
                    doc_ids: command === 'index_ingest_batch' ? args.inputs.map((input: any) => `committed-${input.locationUri}`) : [] };
            }
            if (command === 'index_promote_l2') return args.docIds.map((_: string, index: number) => ({
                doc_id: `resolved-${args.sourcePaths[index]}`, updated: true,
                error: args.sourcePaths[index].endsWith(root.__queueL2Failure) && root.__queueL2Failure ? 'Fixture metadata read failure' : null,
            }));
            if (command === 'plugin:fs|read_file') return Array.from(new TextEncoder().encode(texts.get(args.path) ?? ''));
            if (command === 'file_sha256') {
                if (args.path.endsWith(root.__queueChangedSource) && root.__queueChangedSource) return 'changed';
                const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(texts.get(args.path) ?? ''));
                return Array.from(new Uint8Array(digest)).map(byte => byte.toString(16).padStart(2, '0')).join('');
            }
            return original(command, args, options);
        };
    }, { files, status });
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Add', exact: true }).click();
    await expect(page.getByRole('button', { name: /L1 Quick-Scan/ })).toBeVisible();
    await expect(page.getByRole('button', { name: /L1 Quick-Scan/ })).toBeEnabled();
}

async function queueCalls(page: Page, command: string) {
    return page.evaluate(command => (window as any).__queueCalls.filter((call: any) => call.command === command), command);
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
});

test('restores errors from a legacy completed job and retries only unfinished files', async ({ page }) => {
    await seedQueue(page, [{ name: 'kept.txt', status: 'done' }, { name: 'retry.txt', status: 'error' }], 'done');
    await expect(page.locator('.stats-bar')).toContainText('1 Fehler');
    await page.getByRole('button', { name: /L1 Quick-Scan/ }).click();
    await expect(page.locator('.stats-bar')).toContainText('2 fertig');
    await expect(page.getByRole('button', { name: /L1 Quick-Scan/ })).toBeDisabled();
    expect((await queueCalls(page, 'index_ingest_l1'))[0].args.files.map((file: any) => file.locationUri)).toEqual(['/fixtures/retry.txt']);
    expect(await queueCalls(page, 'jobs_retry_errors')).toHaveLength(1);
    expect(await page.evaluate(() => (window as any).__queueSummary().status)).toBe('done');
});

test('Stop holds controls until both the index write and durable acknowledgement finish', async ({ page }) => {
    await seedQueue(page, [{ name: 'drain.txt', status: 'pending' }]);
    await page.evaluate(() => { (window as any).__queueGate = true; });
    await page.getByRole('button', { name: /L1 Quick-Scan/ }).click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseQueueWrite)).toBe('function');
    await page.getByRole('button', { name: 'Stoppen', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Wird gestoppt …', exact: true })).toBeDisabled();
    await expect(page.getByRole('button', { name: 'Alle löschen', exact: true })).toBeDisabled();
    await expect(page.getByRole('button', { name: 'Dateien', exact: true })).toBeDisabled();
    await page.evaluate(() => { (window as any).__queueAckGate = true; (window as any).__releaseQueueWrite(); });
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseQueueAck)).toBe('function');
    expect(await queueCalls(page, 'jobs_reclaim')).toHaveLength(1);
    await expect(page.getByRole('button', { name: 'Wird gestoppt …', exact: true })).toBeDisabled();
    await page.evaluate(() => (window as any).__releaseQueueAck());
    await expect(page.getByRole('button', { name: /L1 Quick-Scan/ })).toBeVisible();
    await expect(page.locator('.stats-bar')).toContainText('1 fertig');
    expect(await queueCalls(page, 'index_ingest_l1')).toHaveLength(1);
});

test('failed deletion leaves the durable queue visible and exposes the storage error', async ({ page }) => {
    await seedQueue(page, [{ name: 'keep-visible.txt', status: 'pending' }]);
    await page.evaluate(() => { (window as any).__queueDeleteFailure = true; });
    await page.getByRole('button', { name: 'keep-visible.txt aus Warteschlange entfernen', exact: true }).click();
    await expect(page.locator('.file-list')).toContainText('keep-visible.txt');
    await expect(page.getByRole('alert')).toContainText('Fixture queue deletion failure');
    await page.getByRole('button', { name: 'Alle löschen', exact: true }).click();
    await expect(page.locator('.file-list')).toContainText('keep-visible.txt');
    await expect(page.getByRole('alert')).toContainText('Warteschlange konnte nicht gelöscht werden');
    await page.evaluate(() => { (window as any).__queueDeleteFailure = false; });
    await page.getByRole('button', { name: 'Alle löschen', exact: true }).click();
    await expect(page.locator('.file-list')).toHaveCount(0);
});

test('L2 records individual failures and uses resolved document identities on retry', async ({ page }) => {
    await seedQueue(page, [{ name: 'first.txt', status: 'pending' }, { name: 'second.txt', status: 'pending' }]);
    await page.evaluate(() => { (window as any).__queueL2Failure = 'second.txt'; });
    await page.getByRole('button', { name: 'L2', exact: true }).click();
    await page.getByRole('button', { name: /L2 Metadaten-Lesen/ }).click();
    await expect(page.locator('.stats-bar')).toContainText('1 fertig');
    await expect(page.locator('.stats-bar')).toContainText('1 Fehler');
    expect(await page.evaluate(() => (window as any).__queueSummary().status)).toBe('paused');
    expect((await queueCalls(page, 'index_promote_l2'))[0].args.sourcePaths).toEqual(['/fixtures/first.txt', '/fixtures/second.txt']);
    await page.evaluate(() => { (window as any).__queueL2Failure = ''; });
    await page.getByRole('button', { name: /L2 Metadaten-Lesen/ }).click();
    await expect(page.locator('.stats-bar')).toContainText('2 fertig');
    expect((await queueCalls(page, 'index_promote_l2'))[1].args.sourcePaths).toEqual(['/fixtures/second.txt']);
    expect(await page.evaluate(() => (window as any).__queueRows[1].docId)).toBe('resolved-/fixtures/second.txt');
});

test('short text is indexed, empty extraction is retryable, and long shared prefixes keep distinct identities', async ({ page }) => {
    const prefix = 'Common extracted passage. '.repeat(3500);
    await seedQueue(page, [
        { name: 'large-one.txt', status: 'pending', text: prefix + 'first ending' },
        { name: 'large-two.txt', status: 'pending', text: prefix + 'second ending' },
        { name: 'short.txt', status: 'pending', text: 'A' },
        { name: 'empty.txt', status: 'pending', text: '  ' },
    ]);
    await page.getByRole('button', { name: 'L3', exact: true }).click();
    await page.getByRole('button', { name: /L3 Volltext-Indexierung/ }).click();
    await expect(page.locator('.stats-bar')).toContainText('3 fertig');
    await expect(page.locator('.stats-bar')).toContainText('1 Fehler');
    const writes = await queueCalls(page, 'index_ingest_batch');
    expect(writes).toHaveLength(1);
    expect(new Set(writes[0].args.inputs.map((input: any) => input.sourceHash)).size).toBe(3);
    expect(writes[0].args.inputs.every((input: any) => input.sourceHash === input.sourceFingerprint)).toBe(true);
    expect(await page.evaluate(() => (window as any).__queueRows[2].docId)).toBe('committed-/fixtures/short.txt');
    expect(writes[0].args.inputs.some((input: any) => input.fullText === 'A')).toBe(true);
    await expect(page.locator('.file-row').filter({ hasText: 'empty.txt' })).toContainText('Kein durchsuchbarer Text');
    await expect(page.getByRole('button', { name: /L3 Volltext-Indexierung \(1\)/ })).toBeEnabled();
});

test('identical file copies keep the shared byte fingerprint and acknowledge separate committed occurrences', async ({ page }) => {
    await seedQueue(page, [
        { name: 'first-copy.txt', status: 'pending', text: 'Identical source bytes.' },
        { name: 'second-copy.txt', status: 'pending', text: 'Identical source bytes.' },
    ]);
    await page.getByRole('button', { name: 'L3', exact: true }).click();
    await page.getByRole('button', { name: /L3 Volltext-Indexierung/ }).click();
    await expect(page.locator('.stats-bar')).toContainText('2 fertig');
    const inputs = (await queueCalls(page, 'index_ingest_batch'))[0].args.inputs;
    expect(inputs[0].sourceHash).toBe(inputs[1].sourceHash);
    expect(inputs[0].sourceHash).toBe(inputs[0].sourceFingerprint);
    expect(await page.evaluate(() => (window as any).__queueRows.map((row: any) => row.docId))).toEqual([
        'committed-/fixtures/first-copy.txt', 'committed-/fixtures/second-copy.txt',
    ]);
    const commands = await page.evaluate(() => (window as any).__queueCalls.map((call: any) => call.command));
    expect(commands.lastIndexOf('jobs_set_doc_id')).toBeLessThan(commands.indexOf('jobs_mark_done'));
});

test('a source changed during extraction never reaches the index writer', async ({ page }) => {
    await seedQueue(page, [{ name: 'changed.txt', status: 'pending' }]);
    await page.evaluate(() => { (window as any).__queueChangedSource = 'changed.txt'; });
    await page.getByRole('button', { name: 'L3', exact: true }).click();
    await page.getByRole('button', { name: /L3 Volltext-Indexierung/ }).click();
    await expect(page.locator('.stats-bar')).toContainText('1 Fehler');
    await expect(page.locator('.file-row')).toContainText('während der Extraktion verändert');
    expect(await queueCalls(page, 'index_ingest_batch')).toHaveLength(0);
});

test('storage acknowledgement failure remains resumable instead of reporting completion', async ({ page }) => {
    await seedQueue(page, [{ name: 'recover-write.txt', status: 'pending' }]);
    await page.evaluate(() => {
        (window as any).__queueAckFailure = true;
        (window as any).__queueErrorAckFailure = true;
    });
    await page.getByRole('button', { name: /L1 Quick-Scan/ }).click();
    await expect(page.getByRole('alert')).toContainText('Fixture acknowledgement storage failure');
    await expect(page.getByRole('button', { name: /L1 Quick-Scan \(1\)/ })).toBeEnabled();
    expect(await page.evaluate(() => (window as any).__queueSummary())).toMatchObject({ status: 'paused', doneFiles: 0, inProgressFiles: 0 });
    await page.evaluate(() => { (window as any).__queueAckFailure = false; (window as any).__queueErrorAckFailure = false; });
    await page.getByRole('button', { name: /L1 Quick-Scan/ }).click();
    await expect(page.locator('.stats-bar')).toContainText('1 fertig');
    expect(await queueCalls(page, 'index_ingest_l1')).toHaveLength(2);
});

test('whole-job counts and error filtering remain correct beyond the visible page', async ({ page }) => {
    await seedQueue(page, [
        ...Array.from({ length: 550 }, (_, index) => ({ name: `completed-${index}.txt`, status: 'done' as const })),
        { name: 'last-error.txt', status: 'error' },
    ]);
    await expect(page.locator('.stats-bar')).toContainText('551 Dateien');
    await expect(page.locator('.file-row')).toHaveCount(200);
    await expect(page.locator('.queue-pagination')).toContainText('von 551');
    await page.getByRole('combobox', { name: 'Warteschlange filtern', exact: true }).selectOption('error');
    await expect(page.locator('.file-row')).toHaveCount(1);
    await expect(page.locator('.file-row')).toContainText('last-error.txt');
    await page.getByRole('button', { name: /L1 Quick-Scan/ }).click();
    await expect(page.locator('.stats-bar')).toContainText('551 fertig');
    expect((await queueCalls(page, 'index_ingest_l1'))[0].args.files.map((file: any) => file.locationUri)).toEqual(['/fixtures/last-error.txt']);
});

test('restored background work exposes resume controls and keeps failed control actions visible', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any;
        const original = root.__TAURI_INTERNALS__.invoke;
        const state = { status: 'paused', pending: 2, active: 0, current: null, done: 3, errored: 1, last_error: 'Saved source failure' };
        root.__backgroundControlFailure = '';
        root.__backgroundCalls = [];
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any, options: any) => {
            if (command === 'bg_ingest_status') return { ...state };
            if (['bg_ingest_resume', 'bg_ingest_pause', 'bg_ingest_cancel', 'bg_ingest_clear'].includes(command)) {
                root.__backgroundCalls.push(command);
                if (command === root.__backgroundControlFailure) throw new Error('Fixture background storage failure');
                if (command === 'bg_ingest_resume') { state.status = 'running'; state.active = 1; }
                if (command === 'bg_ingest_pause' || command === 'bg_ingest_cancel') { state.status = 'paused'; state.active = 0; }
                if (command === 'bg_ingest_clear') { state.pending = 0; state.done = 0; state.errored = 0; state.last_error = ''; }
                return;
            }
            return original(command, args, options);
        };
    });
    await seedQueue(page, [{ name: 'manual.txt', status: 'pending' }]);
    const panel = page.getByRole('region', { name: 'Hintergrund-Indexierung', exact: true });
    await expect(panel).toContainText('2 ausstehend');
    await expect(panel).toContainText('3 gespeichert');
    await page.evaluate(() => { (window as any).__backgroundControlFailure = 'bg_ingest_resume'; });
    await panel.getByRole('button', { name: /Hintergrund fortsetzen/ }).click();
    await expect(panel.getByRole('alert')).toContainText('Fixture background storage failure');
    await expect(panel.getByRole('button', { name: /Hintergrund fortsetzen/ })).toBeEnabled();
    await page.evaluate(() => { (window as any).__backgroundControlFailure = ''; });
    await panel.getByRole('button', { name: /Hintergrund fortsetzen/ }).click();
    await expect(panel.getByRole('button', { name: 'Hintergrund pausieren', exact: true })).toBeVisible();
    await expect(panel.getByRole('button', { name: 'Hintergrundliste leeren', exact: true })).toBeDisabled();
    await panel.getByRole('button', { name: 'Hintergrund stoppen', exact: true }).click();
    await expect(panel.getByRole('button', { name: /Hintergrund fortsetzen/ })).toBeVisible();
    await page.evaluate(() => { (window as any).__backgroundControlFailure = 'bg_ingest_clear'; });
    await panel.getByRole('button', { name: 'Hintergrundliste leeren', exact: true }).click();
    await expect(panel.getByRole('alert')).toContainText('Fixture background storage failure');
    await expect(panel).toContainText('2 ausstehend');
});
