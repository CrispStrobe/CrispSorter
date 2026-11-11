// Browser transport/rendering contracts; native suites qualify query matching.
import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

const query = 'warranty~2 AND "carbon dioxide"';

async function inspect(page: Page, mode = 'Full text', text = query) {
    await page.getByRole('button', { name: mode, exact: true }).click();
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill(text);
    await page.locator('.search-btn').click();
    await page.getByTitle('Inspect indexed passage', { exact: true }).click();
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
    await page.evaluate(() => {
        const root = window as any;
        root.__fixtureResults = [{ doc_id: 'book', owner_id: 'reader', chunk_index: 2, score: .9,
            filename: 'Book.txt', title: 'Book', location_uri: '/library/Book.txt', source_hash: 'stored-source',
            snippet: 'warrantee carbon dioxide' }];
        root.__explanationRequests = [];
        root.__explanationResolvers = {};
        const source = (index: number) => {
            const text = index === 2 ? '😀 warrantee carbon dioxide. <img src=x onerror=alert(1)>' : 'Another stored passage.';
            return { passage_version: 1, doc_id: 'book', owner_id: 'reader', text,
                start_char: index * 100, end_char: index * 100 + new TextEncoder().encode(text).byteLength,
                offset_unit: 'utf8_bytes', truncated: false, chunk_index: index,
                passage_number: index === 2 ? 1 : 2, chunk_count: 2,
                previous_chunk_index: index === 2 ? null : 2, next_chunk_index: index === 2 ? 9 : null,
                source_hash: 'stored-source', indexed_at: 1000 };
        };
        const original = root.__TAURI_INTERNALS__.invoke;
        root.__TAURI_INTERNALS__.invoke = async (command: string, args: any, options: any) => {
            if (command === 'index_read_passage') return source(args.chunkIndex);
            if (command === 'index_explain_passage') {
                root.__explanationRequests.push(args);
                const passage = source(args.chunkIndex);
                const span = (needle: string, kind: string, clause: string) => {
                    const index = passage.text.indexOf(needle), bytes = (text: string) => new TextEncoder().encode(text).byteLength;
                    return { start: bytes(passage.text.slice(0, index)), end: bytes(passage.text.slice(0, index + needle.length)), kind, clause };
                };
                const vector = args.mode === 'vector';
                const response = { explanation_version: 1, query: args.query, lexical_query: vector ? '' : args.query,
                    query_intent: args.queryIntent, mode: args.mode, fuzzy: args.fuzzy, synonyms: args.synonyms,
                    passage, lexical_document_match: vector ? null : true, semantic_evidence: args.mode !== 'text',
                    clauses: vector ? null : root.__metadataOnly
                        ? { kind: 'field', label: 'title', field: 'title', matched: true,
                            children: [{ kind: 'term', label: 'review', matched: true, children: [] }] }
                        : { kind: 'and', label: 'AND', matched: true, children: [
                            { kind: 'term', label: 'warranty~2', matched: true, children: [] },
                            { kind: 'phrase', label: 'carbon dioxide', matched: true, children: [] },
                        ] },
                    spans: vector || args.chunkIndex !== 2 || root.__metadataOnly ? [] : [
                        span('warrantee', 'fuzzy', 'warranty~2'), span('carbon dioxide', 'phrase', 'carbon dioxide'),
                    ],
                    spans_truncated: Boolean(root.__limitedEvidence), scope: 'document_query_with_passage_spans', notes: [],
                };
                if (root.__corruptEvidence) response.passage.source_hash = 'different-generation';
                if (root.__holdFirstEvidence && args.chunkIndex === 2) {
                    return new Promise(resolve => { root.__explanationResolvers[2] = () => resolve(response); });
                }
                return response;
            }
            return original(command, args, options);
        };
    });
    await page.getByRole('button', { name: 'Library', exact: true }).first().click();
    await page.locator('.tab-bar').getByRole('button', { name: 'Search', exact: true }).click();
});

test('renders verified Unicode fuzzy and phrase spans while literal find remains independent', async ({ page }) => {
    await inspect(page);
    const passage = page.getByRole('region', { name: 'Indexed passage', exact: true });
    const evidence = passage.getByRole('region', { name: 'Query evidence', exact: true });
    await expect(evidence).toContainText('The lexical expression matches the current indexed document.');
    await expect(passage.locator('mark.query-proof')).toHaveCount(2);
    await expect(passage.locator('mark[data-evidence-kind="fuzzy"]')).toHaveText('warrantee');
    await expect(passage.locator('mark[data-evidence-kind="phrase"]')).toHaveText('carbon dioxide');
    await expect(passage.locator('img')).toHaveCount(0);
    await expect(passage.locator('.passage-text')).toHaveText('😀 warrantee carbon dioxide. <img src=x onerror=alert(1)>');
    await evidence.locator('summary').click();
    await expect(evidence.getByRole('list', { name: 'Query condition evaluation' })).toContainText('warranty~2');
    await passage.getByRole('textbox', { name: 'Find in passage' }).fill('dioxide');
    await expect(passage.locator('mark')).toHaveText('dioxide');
    await expect(passage.locator('mark.query-proof')).toHaveCount(0);
    await passage.getByRole('textbox', { name: 'Find in passage' }).clear();
    await expect(passage.locator('mark.query-proof')).toHaveCount(2);
    expect(await page.evaluate(() => (window as any).__explanationRequests.at(-1))).toMatchObject({
        docId: 'book', ownerId: 'reader', chunkIndex: 2, query, mode: 'text', fuzzy: false, synonyms: false,
    });
});

test('keeps semantic passage evidence free of invented matching-word highlights', async ({ page }) => {
    await inspect(page, 'Semantic', 'carbon dioxide');
    const passage = page.getByRole('region', { name: 'Indexed passage', exact: true });
    const evidence = passage.getByRole('region', { name: 'Query evidence', exact: true });
    await expect(evidence).toContainText('individual words are not attributed to semantic relevance');
    await expect(passage.locator('mark')).toHaveCount(0);
    await expect(evidence.locator('.query-conditions')).toHaveCount(0);
    await passage.getByRole('textbox', { name: 'Find in passage' }).fill('carbon');
    await expect(passage.locator('mark')).toHaveText('carbon');
    await passage.getByRole('button', { name: 'Next stored passage', exact: true }).click();
    await expect(evidence).toContainText('Passage navigation does not rerun or rescore the search.');
    await expect(passage.locator('mark')).toHaveCount(0);
});

test('rejects changed source receipts and provides a working evidence retry', async ({ page }) => {
    await page.evaluate(() => { (window as any).__corruptEvidence = true; });
    await inspect(page);
    const passage = page.getByRole('region', { name: 'Indexed passage', exact: true });
    const evidence = passage.getByRole('region', { name: 'Query evidence', exact: true });
    await expect(evidence).toContainText('The stored passage changed while its query evidence was loading.');
    await expect(passage.locator('mark.query-proof')).toHaveCount(0);
    await expect(passage.locator('.passage-text')).toContainText('😀 warrantee');
    await page.evaluate(() => { (window as any).__corruptEvidence = false; });
    await evidence.getByRole('button', { name: 'Retry query evidence', exact: true }).click();
    await expect(evidence).toContainText('The lexical expression matches the current indexed document.');
    await expect(passage.locator('mark.query-proof')).toHaveCount(2);
});

test('discards old proof requests after passage navigation and retains executed search inputs', async ({ page }) => {
    await page.evaluate(() => { (window as any).__holdFirstEvidence = true; });
    await inspect(page);
    const passage = page.getByRole('region', { name: 'Indexed passage', exact: true });
    const evidence = passage.getByRole('region', { name: 'Query evidence', exact: true });
    await expect(evidence).toContainText('Checking indexed query conditions');
    await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('edited query was not searched');
    await page.getByRole('button', { name: 'Semantic', exact: true }).click();
    await passage.getByRole('button', { name: 'Next stored passage', exact: true }).click();
    await expect(passage).toContainText('Passage 2 / 2');
    await expect(evidence).toContainText('This passage has no contributing query spans.');
    await page.evaluate(() => (window as any).__explanationResolvers[2]());
    await expect(passage.locator('.passage-text')).toHaveText('Another stored passage.');
    await expect(passage.locator('mark.query-proof')).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).__explanationRequests.map((value: any) => ({ query: value.query, mode: value.mode, chunk: value.chunkIndex })))).toEqual([
        { query, mode: 'text', chunk: 2 }, { query, mode: 'text', chunk: 9 },
    ]);
});

test('distinguishes document field matches from absent body spans and discloses span limits', async ({ page }) => {
    await page.evaluate(() => { (window as any).__metadataOnly = true; });
    await inspect(page, 'Full text', 'title:review');
    const passage = page.getByRole('region', { name: 'Indexed passage', exact: true });
    const evidence = passage.getByRole('region', { name: 'Query evidence', exact: true });
    await expect(evidence).toContainText('Matching conditions can refer to another text field or stored passage.');
    await expect(passage.locator('mark.query-proof')).toHaveCount(0);
    await evidence.locator('summary').click();
    await expect(evidence.getByRole('list', { name: 'Query condition evaluation' })).toContainText('title');
    await page.evaluate(() => { (window as any).__metadataOnly = false; (window as any).__limitedEvidence = true; });
    await passage.getByRole('button', { name: 'Next stored passage', exact: true }).click();
    await expect(evidence).toContainText('Additional matching spans were omitted');
});

for (const retrieval of ['image', 'similarity'] as const) {
    test('does not attribute the query box to stored passages after ' + retrieval + ' search', async ({ page }) => {
        await inspect(page);
        const passage = page.getByRole('region', { name: 'Indexed passage', exact: true });
        await expect(passage.locator('mark.query-proof')).toHaveCount(2);
        expect(await page.evaluate(() => (window as any).__explanationRequests.length)).toBe(1);
        await page.getByRole('textbox', { name: 'Search indexed documents' }).fill('carbon dioxide');
        await page.evaluate(() => {
            const root = window as any;
            const original = root.__TAURI_INTERNALS__.invoke;
            root.__TAURI_INTERNALS__.invoke = (command: string, args: any, options: any) => {
                if (command === 'index_find_similar' || command === 'index_search_by_image') {
                    root.__fixtureCalls.push({ command, args });
                    return Promise.resolve(root.__fixtureResults);
                }
                if (command === 'plugin:dialog|open') return Promise.resolve('/fixtures/search.png');
                return original(command, args, options);
            };
        });
        await page.getByTitle(retrieval === 'image'
            ? 'Search by image similarity (ViT + omni embeddings)' : 'Find similar documents', { exact: true }).click();
        await expect.poll(() => page.evaluate(command => (window as any).__fixtureCalls.filter((call: any) => call.command === command).length,
            retrieval === 'image' ? 'index_search_by_image' : 'index_find_similar')).toBe(1);
        await expect(page.locator('.search-btn')).toBeEnabled();
        await expect(page.locator('.result-card')).toHaveCount(1);
        await expect(page.locator('.result-card .chunk-preview mark')).toHaveCount(0);
        await page.getByTitle('Inspect indexed passage', { exact: true }).click();
        await expect(passage.locator('.passage-text')).toContainText('😀 warrantee carbon dioxide.');
        await expect(passage.getByRole('region', { name: 'Query evidence', exact: true })).toHaveCount(0);
        await expect(passage.locator('mark')).toHaveCount(0);
        await passage.getByRole('textbox', { name: 'Find in passage' }).fill('dioxide');
        await expect(passage.locator('mark')).toHaveText('dioxide');
        await passage.getByRole('button', { name: 'Next stored passage', exact: true }).click();
        await expect(passage.locator('.passage-text')).toHaveText('Another stored passage.');
        await expect(passage.getByRole('region', { name: 'Query evidence', exact: true })).toHaveCount(0);
        expect(await page.evaluate(() => (window as any).__explanationRequests.length)).toBe(1);
    });
}
