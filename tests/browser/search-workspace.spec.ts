// UI state and IPC contracts. Retrieval correctness is covered by native suites.
import { test, expect, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function search(page: Page, query = 'warranty') {
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill(query);
    await page.locator('.search-btn').click();
    await expect(page.locator('.result-card')).toHaveCount(3);
}

async function lastSearch(page: Page) {
    return page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search').at(-1)?.args);
}

async function summaryFixture(page: Page, scope = 'local_indexed_documents') {
    await page.evaluate(scope => {
        const root = window as any;
        root.__summaryGeneration = 'snapshot-1'; root.__summaryScope = scope; root.__holdSummary = false; root.__pendingSummaries = [];
        const make = (query: string) => {
            const facet = (value: string) => ({ values: [{ value, documents: 633 }], distinctValues: 1, omittedValues: 0, missingDocuments: 0 });
            return { version: 1, scope: root.__summaryScope, query, effectiveQuery: query, generation: root.__summaryGeneration,
                matchingDocuments: 633, scannedRows: 900, facetValueLimit: 25,
                facets: { extensions: facet('pdf'), languages: facet('en'), years: facet('2026'),
                    tags: { ...facet('reviewed'), distinctValues: 31, omittedValues: 30 } } };
        };
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'index_search_summary') {
                root.__fixtureCalls.push({ command, args });
                const value = make(args.query);
                return root.__holdSummary ? new Promise(resolve => root.__pendingSummaries.push(() => resolve(value))) : Promise.resolve(value);
            }
            if (command === 'index_search_summary_generation') { root.__fixtureCalls.push({ command, args }); return Promise.resolve(root.__summaryGeneration); }
            if (command === 'index_cancel_search_summary') { root.__fixtureCalls.push({ command, args }); return Promise.resolve(true); }
            return original(command, args, options);
        };
    }, scope);
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    await page.evaluate(() => {
        const root = window as any;
        root.__fixtureResults = [
            { doc_id: 'z', chunk_index: 2, score: .9, title: 'Zeta report', author: 'A. Researcher', year: 2020, filename: 'Zeta.txt', location_uri: '/research/zeta.txt', source_hash: 'a'.repeat(64), snippet: 'warranty reviewed source', tags: ['reviewed'] },
            { doc_id: 'a', chunk_index: 0, score: .7, title: 'Alpha report', year: 2026, filename: 'Alpha.txt', location_uri: '/research/alpha.txt', snippet: 'warranty annual source', tags: ['reviewed'] },
            { doc_id: 'u', chunk_index: 1, score: .5, title: 'Unknown report', filename: 'Unknown.txt', location_uri: '/archive/unknown.txt', snippet: 'warranty reference', tags: ['unreviewed'] },
        ];
        Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
            writeText: async (text: string) => { root.__copiedText = text; },
        } });
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'index_read_passage') return Promise.resolve({ text: `Stored warranty passage for ${args.docId}.`, start_char: 0, end_char: 40, offset_unit: 'utf8_bytes', truncated: false });
            return original(command, args, options);
        };
    });
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await expect(page.getByRole('textbox', { name: 'Search indexed documents' })).toBeVisible();
});

test('bookmarks restore sort, compact layout, report format, display tags and preview preference', async ({ page }) => {
    await search(page);
    await page.getByRole('combobox', { name: 'Sort results', exact: true }).selectOption('year-desc');
    await page.getByLabel('Compact results', { exact: true }).check();
    await page.getByRole('combobox', { name: 'Report format', exact: true }).selectOption('json');
    await page.getByRole('button', { name: 'Tags in returned results', exact: true }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^reviewed/ }).click();
    await page.locator('.result-card[data-document-id="a"]').getByTitle('Inspect indexed passage', { exact: true }).click();
    await page.getByRole('button', { name: 'Close preview', exact: true }).click();
    await page.getByRole('button', { name: 'Save this search', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Save this search', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Clear display filters', exact: true }).click();
    await page.getByRole('combobox', { name: 'Sort results', exact: true }).selectOption('filename');
    await page.getByLabel('Compact results', { exact: true }).uncheck();
    await page.getByRole('combobox', { name: 'Report format', exact: true }).selectOption('csv');
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await page.locator('.saved-dropdown .saved-name').first().click();
    await expect(page.locator('.result-card')).toHaveCount(2);
    await expect(page.locator('.result-card').first()).toHaveAttribute('data-document-id', 'a');
    await expect(page.getByLabel('Compact results', { exact: true })).toBeChecked();
    await expect(page.getByRole('combobox', { name: 'Report format', exact: true })).toHaveValue('json');
    await expect(page.getByRole('combobox', { name: 'Sort results', exact: true })).toHaveValue('year-desc');
    await expect(page.getByRole('button', { name: 'Remove displayed tag reviewed', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Preview Alpha report', exact: true }).click();
    await expect(page.getByRole('region', { name: 'Indexed passage', exact: true })).toContainText('Stored warranty passage for a.');
    expect(await page.evaluate(async () => (await (window as any).__TAURI_INTERNALS__.invoke('plugin:store|get', { key: 'savedSearches' }))[0][0].view))
        .toEqual({ sort: 'year-desc', compact: true, reportFormat: 'json', preview: 'indexed', displayTags: ['reviewed'], tagCloud: true });
});

test('keyboard traverses results and preview and restores focus to the current document', async ({ page }) => {
    await search(page);
    const input = page.getByRole('textbox', { name: 'Search indexed documents' });
    await input.focus();
    await input.press('ArrowDown');
    await expect(page.getByRole('button', { name: 'Preview Zeta report', exact: true })).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(page.getByRole('button', { name: 'Preview Alpha report', exact: true })).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(page.getByRole('button', { name: 'Close preview', exact: true })).toBeFocused();
    await page.keyboard.press('Alt+ArrowDown');
    await expect(page.locator('.preview-title')).toHaveText('Unknown report');
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: 'Preview Unknown report', exact: true })).toBeFocused();
    await expect(page.locator('.preview-pane')).toHaveCount(0);
    await page.keyboard.press('Home');
    await expect(page.getByRole('button', { name: 'Preview Zeta report', exact: true })).toBeFocused();
    await page.keyboard.press('End');
    await expect(page.getByRole('button', { name: 'Preview Unknown report', exact: true })).toBeFocused();
    await page.keyboard.press('Control+Shift+f');
    await expect(input).toBeFocused();
});

test('refinement includes only displayed documents and leaves a removable explicit scope', async ({ page }) => {
    await search(page);
    await page.getByRole('button', { name: 'Tags in returned results', exact: true }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^reviewed/ }).click();
    await expect(page.locator('.result-card')).toHaveCount(2);
    await page.getByRole('button', { name: 'Search within displayed documents', exact: true }).click();
    const input = page.getByRole('textbox', { name: 'Search indexed documents' });
    await expect(input).toBeFocused();
    await expect(input).toHaveValue('');
    await expect(page.getByRole('button', { name: 'Remove document scope', exact: true })).toContainText('2');
    await input.fill('renewal');
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'renewal', docIdScope: ['a', 'z'] });
    await page.getByRole('button', { name: 'Remove document scope', exact: true }).click();
    await expect(page.getByText('Search options changed. Search again to update the results.', { exact: true })).toBeVisible();
    await page.locator('.search-btn').click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'renewal', docIdScope: null });
});

test('display filter chips explain an empty returned window and can recover individual tags', async ({ page }) => {
    await search(page);
    await page.getByRole('button', { name: 'Tags in returned results', exact: true }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^reviewed/ }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^unreviewed/ }).click();
    await expect(page.locator('.result-card')).toHaveCount(0);
    await expect(page.getByText('No document in this returned window has all selected tags.', { exact: true })).toBeVisible();
    await expect(page.locator('.search-window-note')).toContainText('this returned window');
    await page.getByRole('button', { name: 'Remove displayed tag reviewed', exact: true }).click();
    await expect(page.locator('.result-card')).toHaveCount(1);
    await expect(page.locator('.result-card')).toHaveAttribute('data-document-id', 'u');
    await expect(page.getByText('Search options changed. Search again to update the results.', { exact: true })).toHaveCount(0);
});

test('citation and source path retain indexed provenance and the executed query', async ({ page }) => {
    await search(page);
    const card = page.locator('.result-card[data-document-id="z"]');
    await expect(card.locator('.result-path')).toHaveText('/research/zeta.txt');
    await expect(card.locator('.result-origin')).toHaveText('Indexed passage 3');
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('unexecuted edit');
    await card.getByTitle('Copy citation', { exact: true }).click();
    const citation = await page.evaluate(() => (window as any).__copiedText);
    expect(citation).toContain('Zeta report\nA. Researcher · 2020');
    expect(citation).toContain('Source: /research/zeta.txt');
    expect(citation).toContain('Document: z\nIndexed passage: 3');
    expect(citation).toContain(`Source SHA-256: ${'a'.repeat(64)}`);
    expect(citation).toContain('Search: warranty');
    expect(citation).not.toContain('unexecuted edit');
    await card.getByTitle('Copy path', { exact: true }).click();
    expect(await page.evaluate(() => (window as any).__copiedText)).toBe('/research/zeta.txt');
});

test('a new or cleared search closes an obsolete desktop preview', async ({ page }) => {
    await search(page);
    await page.locator('.result-card[data-document-id="z"]').getByTitle('Inspect indexed passage', { exact: true }).click();
    await expect(page.locator('.preview-pane')).toBeVisible();
    await search(page, 'renewal');
    await expect(page.locator('.preview-pane')).toHaveCount(0);
    await page.locator('.result-card[data-document-id="a"]').getByTitle('Inspect indexed passage', { exact: true }).click();
    await page.getByRole('button', { name: 'Clear search', exact: true }).click();
    await expect(page.locator('.preview-pane')).toHaveCount(0);
    await expect(page.locator('.result-card')).toHaveCount(0);
});

test('loading a larger returned window keeps current presentation and exact request options', async ({ page }) => {
    await page.evaluate(() => {
        const root = window as any;
        const template = root.__fixtureResults[0];
        root.__fixtureResults = Array.from({ length: 20 }, (_, i) => ({ ...template, doc_id: `doc-${i}`, title: `Report ${i}`, filename: `report-${i}.txt` }));
    });
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.locator('.search-btn').click();
    await expect(page.locator('.result-card')).toHaveCount(20);
    await page.getByRole('combobox', { name: 'Sort results', exact: true }).selectOption('filename');
    await page.getByLabel('Compact results', { exact: true }).check();
    const more = page.getByRole('button', { name: 'Load more results', exact: true });
    await expect(more).toBeEnabled();
    await more.click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'warranty', limit: 50 });
    await expect(page.getByRole('combobox', { name: 'Sort results', exact: true })).toHaveValue('filename');
    await expect(page.getByLabel('Compact results', { exact: true })).toBeChecked();
    await expect(page.locator('.search-window-note')).toContainText('Requested limit 50');
});

test('an active remote index visibly caps a saved request and its report at 200 results', async ({ page }) => {
    await page.locator('.tab-bar').getByRole('button', { name: 'Add', exact: true }).click();
    await page.evaluate(async () => {
        const root = window as any;
        await root.__TAURI_INTERNALS__.invoke('plugin:store|set', { key: 'savedSearches', value: [
            { query: 'warranty', name: 'Large saved window', mode: 'text', limit: 500, savedAt: 1 },
        ] });
        const template = root.__fixtureResults[0];
        root.__fixtureResults = Array.from({ length: 200 }, (_, i) => ({ ...template, doc_id: `remote-${i}`, snippet: '' }));
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'index_get_config') return Promise.resolve({ enabled: true, backend_type: 'remote', use_vector: true });
            if (command === 'index_write_search_report') {
                root.__remoteWindowReport = JSON.parse(args.content);
                return Promise.resolve(args.path);
            }
            return original(command, args, options);
        };
    });
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await page.locator('.saved-dropdown .saved-name').filter({ hasText: 'Large saved window' }).click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'warranty', limit: 200 });
    await expect(page.getByText('Result limit adjusted from 500 to 200 for the active index.', { exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Load more results', exact: true })).toHaveCount(0);
    await expect(page.locator('.search-window-note')).toContainText('Requested limit 200');
    await page.getByRole('button', { name: 'Search filters', exact: true }).click();
    const limits = page.getByRole('combobox', { name: 'Maximum results', exact: true });
    await expect(limits).toHaveValue('200');
    await expect(limits.locator('option[value="500"]')).toHaveCount(0);
    await page.getByRole('combobox', { name: 'Report format', exact: true }).selectOption('json');
    await page.getByRole('button', { name: 'Export displayed', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__remoteWindowReport?.recipe.limit)).toBe(200);
    expect(await page.evaluate(() => (window as any).__remoteWindowReport.coverage.requestedLimit)).toBe(200);
    await page.getByRole('button', { name: 'Save this search', exact: true }).click();
    await expect.poll(() => page.evaluate(async () => {
        const [saved] = await (window as any).__TAURI_INTERNALS__.invoke('plugin:store|get', { key: 'savedSearches' });
        return saved.at(-1).limit;
    })).toBe(200);
});

test('explicit full-corpus counts remain separate from returned-window counts and facets rerun real filters', async ({ page }) => {
    await summaryFixture(page);
    await search(page);
    await expect(page.getByRole('button', { name: 'Count all full-text matches', exact: true })).toHaveCount(0);
    await expect(page.locator('.corpus-summary')).toContainText('Semantic and hybrid searches show the returned window.');
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await search(page);
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search_summary'))).toHaveLength(0);
    await page.getByRole('button', { name: 'Count all full-text matches', exact: true }).click();
    await expect(page.locator('.corpus-total')).toContainText('633 matching indexed documents in total');
    await expect(page.locator('.corpus-total')).toContainText('3 in the returned window');
    await expect(page.locator('.corpus-summary')).toContainText('Separate catalog filename references are excluded.');
    await page.locator('.corpus-facets summary').click();
    await expect(page.locator('.corpus-facets')).toContainText('Top 1 of 31 values; 30 additional values are not displayed.');
    await page.getByRole('button', { name: 'Filter by Tags: reviewed (633)', exact: true }).click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'warranty', tag: 'reviewed' });
    await expect(page.getByRole('button', { name: 'Remove Tag', exact: true })).toBeVisible();
    await expect(page.locator('.corpus-total')).toHaveCount(0);
});

test('obsolete and cancelled scans cannot replace a newer query and focus invalidates changed generations', async ({ page }) => {
    await summaryFixture(page);
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await search(page);
    await page.evaluate(() => { (window as any).__holdSummary = true; });
    await page.getByRole('button', { name: 'Count all full-text matches', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__pendingSummaries.length)).toBe(1);
    await search(page, 'renewal');
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_cancel_search_summary').length)).toBe(1);
    await page.evaluate(() => { const root = window as any; root.__pendingSummaries.shift()(); root.__holdSummary = false; });
    await expect(page.locator('.corpus-total')).toHaveCount(0);
    await page.getByRole('button', { name: 'Count all full-text matches', exact: true }).click();
    await expect(page.locator('.corpus-total')).toContainText('633');
    await page.evaluate(() => { (window as any).__summaryGeneration = 'snapshot-2'; window.dispatchEvent(new Event('focus')); });
    await expect(page.locator('.corpus-notice')).toHaveText('The index or source scope changed. Count again to refresh the totals.');
    await expect(page.locator('.corpus-total')).toHaveCount(0);
    await page.evaluate(() => { (window as any).__holdSummary = true; });
    await page.getByRole('button', { name: 'Count all full-text matches', exact: true }).click();
    await page.getByRole('button', { name: 'Cancel match count', exact: true }).click();
    await page.evaluate(() => (window as any).__pendingSummaries.shift()());
    await expect(page.locator('.corpus-notice')).toHaveText('Match count cancelled.');
    await expect(page.locator('.corpus-total')).toHaveCount(0);
});

test('same document IDs from distinct owners retain separate selection, preview and passage requests', async ({ page }) => {
    await page.locator('.tab-bar').getByRole('button', { name: 'Add', exact: true }).click();
    await page.evaluate(() => {
        const root = window as any, template = root.__fixtureResults[0];
        root.__fixtureResults = [
            { ...template, doc_id: 'shared', owner_id: 'alpha', title: 'Alpha owner source', chunk_index: 0, location_uri: 'crisp+vps://server/alpha/shared.txt' },
            { ...template, doc_id: 'shared', owner_id: 'beta', title: 'Beta owner source', chunk_index: 0, location_uri: 'crisp+vps://server/beta/shared.txt' },
        ];
        root.__passageOwners = [];
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'index_get_config') return Promise.resolve({ enabled: true, backend_type: 'remote', use_vector: true });
            if (command === 'index_read_passage') {
                root.__passageOwners.push(args.ownerId);
                const value = { text: `Stored content for ${args.ownerId}`, start_char: 0, end_char: 24, offset_unit: 'utf8_bytes', truncated: false };
                return args.ownerId === 'alpha' ? new Promise(resolve => { root.__resolveAlphaPassage = () => resolve(value); }) : Promise.resolve(value);
            }
            return original(command, args, options);
        };
    });
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('warranty');
    await page.locator('.search-btn').click();
    await expect(page.locator('.result-card')).toHaveCount(2);
    const alpha = page.locator('.result-card[data-owner-id="alpha"]'), beta = page.locator('.result-card[data-owner-id="beta"]');
    await alpha.getByTitle('Select for batch sorter', { exact: true }).click();
    await expect(alpha.getByTitle('Remove from batch selection', { exact: true })).toHaveAttribute('aria-pressed', 'true');
    await expect(beta.getByTitle('Select for batch sorter', { exact: true })).toHaveAttribute('aria-pressed', 'false');
    await expect(page.getByRole('button', { name: 'Search within displayed documents', exact: true })).toBeDisabled();
    await alpha.getByTitle('Inspect indexed passage', { exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__passageOwners)).toEqual(['alpha']);
    await beta.getByTitle('Inspect indexed passage', { exact: true }).click();
    await expect(page.getByRole('region', { name: 'Indexed passage', exact: true })).toContainText('Stored content for beta');
    await page.evaluate(() => (window as any).__resolveAlphaPassage());
    await expect(page.getByRole('region', { name: 'Indexed passage', exact: true })).not.toContainText('Stored content for alpha');
    expect(await page.evaluate(() => (window as any).__passageOwners)).toEqual(['alpha', 'beta']);
});

test('complete export captures exact query scope and reports completion when publication has already started', async ({ page }) => {
    await summaryFixture(page);
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'plugin:dialog|save') return Promise.resolve('/reports/all.json');
            if (command === 'index_export_lexical_report') {
                root.__completeExport = args.request;
                return new Promise(resolve => { root.__finishCompleteExport = () => resolve({ path: args.request.path,
                    documents: 633, scope: 'local_indexed_documents', generation: args.request.expectedGeneration,
                    format: args.request.format, sort: args.request.sort }); });
            }
            if (command === 'index_cancel_search_summary') return Promise.resolve(false);
            return original(command, args, options);
        };
    });
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await search(page);
    await page.getByRole('button', { name: 'Tags in returned results', exact: true }).click();
    await page.locator('.search-tagcloud-wrap .tc-chip').filter({ hasText: /^reviewed/ }).click();
    await expect(page.locator('.result-card')).toHaveCount(2);
    await page.getByRole('combobox', { name: 'Complete report format', exact: true }).selectOption('json');
    await page.getByRole('combobox', { name: 'Complete report sort', exact: true }).selectOption('title');
    await page.getByRole('button', { name: 'Export all full-text matches', exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as any).__completeExport)).toMatchObject({ query: 'warranty', mode: 'text',
        tag: null, docIdScope: null, format: 'json', sort: 'title', expectedGeneration: 'snapshot-1', path: '/reports/all.json' });
    const request = await page.evaluate(() => (window as any).__completeExport);
    expect(request).not.toHaveProperty('limit'); expect(request).not.toHaveProperty('displayTags');
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('edited while exporting');
    await page.getByRole('button', { name: 'Cancel complete export', exact: true }).click();
    await expect(page.locator('.corpus-export-feedback')).toContainText('The report is being published. Waiting for its final result.');
    await page.evaluate(() => (window as any).__finishCompleteExport());
    await expect(page.locator('.corpus-export-feedback')).toContainText('Exported all 633 matching documents: /reports/all.json');
    await expect(page.locator('.corpus-export-feedback')).not.toContainText('cancelled');
    expect((await page.evaluate(() => (window as any).__completeExport)).query).toBe('warranty');
});

test('an index switch during the Save dialog cannot silently export a different corpus', async ({ page }) => {
    await summaryFixture(page);
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'plugin:dialog|save') return new Promise(resolve => { root.__finishReportDialog = () => resolve('/reports/captured.csv'); });
            if (command === 'index_export_lexical_report') {
                root.__capturedExport = args.request;
                return args.request.expectedGeneration !== root.__summaryGeneration
                    ? Promise.reject(new Error('The active index changed. Previous report preserved.'))
                    : Promise.resolve({ path: args.request.path, documents: 0, scope: 'local_indexed_documents',
                        generation: root.__summaryGeneration, format: 'csv', sort: 'filename' });
            }
            return original(command, args, options);
        };
    });
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await search(page);
    await page.getByRole('button', { name: 'Export all full-text matches', exact: true }).click();
    await expect.poll(() => page.evaluate(() => Boolean((window as any).__finishReportDialog))).toBe(true);
    await page.evaluate(() => { const root = window as any; root.__summaryGeneration = 'different-index'; root.__finishReportDialog(); });
    await expect(page.locator('.corpus-export-feedback')).toContainText('The active index changed. Previous report preserved.');
    expect((await page.evaluate(() => (window as any).__capturedExport)).expectedGeneration).toBe('snapshot-1');
    await expect(page.locator('.corpus-export-feedback')).not.toContainText('Exported all');
    await expect(page.locator('.result-card')).toHaveCount(3);
});

test('accepted cancellation reports no completed full-corpus file', async ({ page }) => {
    await summaryFixture(page);
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'index_export_lexical_report') return new Promise((_resolve, reject) => { root.__cancelCompleteExport = reject; });
            if (command === 'index_cancel_search_summary') { root.__cancelCompleteExport(new Error('The matching-document scan was cancelled')); return Promise.resolve(true); }
            return original(command, args, options);
        };
    });
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
    await search(page);
    await page.getByRole('button', { name: 'Export all full-text matches', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Cancel complete export', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Cancel complete export', exact: true }).click();
    await expect(page.locator('.corpus-export-feedback')).toContainText('The matching-document scan was cancelled');
    await expect(page.locator('.corpus-export-feedback')).not.toContainText('Exported all');
    await expect(page.getByRole('button', { name: 'Export all full-text matches', exact: true })).toBeEnabled();
});


async function remoteIndex(page: Page) {
    await page.locator('.tab-bar').getByRole('button', { name: 'Add', exact: true }).click();
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) =>
            command === 'index_get_config' ? Promise.resolve({ enabled: true, backend_type: 'remote', use_vector: true })
                : original(command, args, options);
    });
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('button', { name: 'Full text', exact: true }).click();
}

test('remote corpus totals are owner scoped, exceed the result window and apply real facet filters', async ({ page }) => {
    await summaryFixture(page, 'remote_indexed_documents');
    await remoteIndex(page);
    await page.evaluate(async () => {
        await (window as any).__TAURI_INTERNALS__.invoke('plugin:store|set', { key: 'savedSearches', value: [
            { name: 'Remote owner scope', query: 'warranty', mode: 'text', ownerId: 'reader', limit: 200,
                docIdScope: ['z', 'a', 'u'], filters: { folderPrefix: '/recorded/library', fuzzy: true } },
        ] });
    });
    // Remount to restore the saved recipe through the real store adapter.
    await page.locator('.tab-bar').getByRole('button', { name: 'Add', exact: true }).click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('button', { name: 'Saved searches', exact: true }).click();
    await page.locator('.saved-dropdown .saved-name').filter({ hasText: 'Remote owner scope' }).click();
    await expect(page.locator('.result-card')).toHaveCount(3);
    await page.getByRole('button', { name: 'Search filters', exact: true }).click();
    await expect(page.locator('label[title="Include hits from drives that aren\'t currently mounted"]')).toHaveCount(0);
    await expect(page.locator('.corpus-summary')).toContainText('Entire remote text index');
    await expect(page.locator('.corpus-summary')).toContainText('Desktop drive mounts do not change this scope.');
    await page.getByRole('button', { name: 'Count all full-text matches', exact: true }).click();
    await expect(page.locator('.corpus-total')).toContainText('633 matching indexed documents in total');
    await expect.poll(() => page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search_summary_generation').at(-1)?.args))
        .toMatchObject({ query: 'warranty', mode: 'text', ownerId: 'reader', docIdScope: ['a', 'u', 'z'], fuzzy: true,
            searchFilters: { parent_dir_prefix: '/recorded/library' } });
    await page.locator('.corpus-facets summary').click();
    await page.getByRole('button', { name: 'Filter by Tags: reviewed (633)', exact: true }).click();
    await expect.poll(() => lastSearch(page)).toMatchObject({ query: 'warranty', ownerId: 'reader', tag: 'reviewed',
        searchFilters: { parent_dir_prefix: '/recorded/library' } });
    await expect(page.locator('.corpus-total')).toHaveCount(0);
});

test('unsupported remote totals fail visibly and a wrong backend scope cannot become an exact count', async ({ page }) => {
    await summaryFixture(page, 'local_indexed_documents');
    await remoteIndex(page);
    await search(page);
    await page.getByRole('button', { name: 'Count all full-text matches', exact: true }).click();
    await expect(page.locator('.corpus-notice')).toContainText('invalid matching-document summary');
    await expect(page.locator('.corpus-total')).toHaveCount(0);
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'index_search_summary' || command === 'index_search_summary_generation')
                return Promise.reject(new Error('This server does not advertise complete lexical search. Upgrade the server.'));
            if (command === 'plugin:dialog|save') { root.__unsupportedSave = true; return Promise.resolve('/reports/all.csv'); }
            return original(command, args, options);
        };
    });
    await page.getByRole('button', { name: 'Count all full-text matches', exact: true }).click();
    await expect(page.locator('.corpus-notice')).toContainText('Upgrade the server.');
    await page.getByRole('button', { name: 'Export all full-text matches', exact: true }).click();
    await expect(page.locator('.corpus-export-feedback')).toContainText('Upgrade the server.');
    expect(await page.evaluate(() => Boolean((window as any).__unsupportedSave))).toBe(false);
    await expect(page.locator('.result-card')).toHaveCount(3);
});

test('remote complete export captures generation before Save and accepts only the remote receipt', async ({ page }) => {
    await summaryFixture(page, 'remote_indexed_documents');
    await remoteIndex(page);
    await page.evaluate(() => {
        const root = window as any, original = root.__TAURI_INTERNALS__.invoke;
        root.__summaryGeneration = 'remote-v1:connection-a:generation-a';
        root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
            if (command === 'plugin:dialog|save') return new Promise(resolve => { root.__finishRemoteSave = () => resolve('/reports/remote.json'); });
            if (command === 'index_export_lexical_report') {
                root.__remoteExport = args.request;
                return Promise.resolve({ path: args.request.path, documents: 633, scope: 'remote_indexed_documents',
                    generation: args.request.expectedGeneration, format: args.request.format, sort: args.request.sort });
            }
            return original(command, args, options);
        };
    });
    await search(page);
    await page.getByRole('combobox', { name: 'Complete report format', exact: true }).selectOption('json');
    await page.getByRole('button', { name: 'Export all full-text matches', exact: true }).click();
    await expect.poll(() => page.evaluate(() => Boolean((window as any).__finishRemoteSave))).toBe(true);
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('edited draft');
    await page.evaluate(() => (window as any).__finishRemoteSave());
    await expect(page.locator('.corpus-export-feedback')).toContainText('Exported all 633 matching documents: /reports/remote.json');
    expect(await page.evaluate(() => (window as any).__remoteExport)).toMatchObject({ query: 'warranty', mode: 'text',
        expectedGeneration: 'remote-v1:connection-a:generation-a', format: 'json' });
    expect(await page.evaluate(() => (window as any).__fixtureCalls.filter((call: any) => call.command === 'index_search_summary_generation').at(-1)?.args))
        .toMatchObject({ query: 'warranty', mode: 'text' });
});
