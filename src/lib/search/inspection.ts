import { normalizeSearchRecipe, type SearchRecipe } from './recipe';

export interface InspectableHit {
    doc_id: string;
    chunk_index: number;
    score: number;
    location_uri: string;
    snippet: string;
    snippet_format?: 'plain' | 'marked';
    title?: string;
    filename?: string;
    author?: string;
    year?: number;
    ext?: string;
    language?: string;
    source_hash?: string;
    indexed_at?: number;
    catalog_source?: string;
    url?: string;
    tags?: string[];
}
export interface ResultGroup<T extends InspectableHit = InspectableHit> { doc_id: string; best: T; chunks: T[] }
export type ResultSort = 'relevance' | 'title' | 'filename' | 'year-desc' | 'year-asc' | 'location';

/** Keep the strongest passage and deterministic ties without mutating search responses. */
export function groupResults<T extends InspectableHit>(hits: readonly T[]): ResultGroup<T>[] {
    const documents = new Map<string, Map<number, T>>();
    for (const hit of hits) {
        let chunks = documents.get(hit.doc_id);
        if (!chunks) { chunks = new Map(); documents.set(hit.doc_id, chunks); }
        const previous = chunks.get(hit.chunk_index);
        if (!previous || finiteScore(hit.score) > finiteScore(previous.score)) chunks.set(hit.chunk_index, hit);
    }
    return [...documents].map(([doc_id, values]) => {
        const chunks = [...values.values()].sort((a, b) => a.chunk_index - b.chunk_index);
        const best = [...chunks].sort((a, b) => finiteScore(b.score) - finiteScore(a.score) || a.chunk_index - b.chunk_index)[0];
        return { doc_id, best, chunks };
    });
}
const finiteScore = (value: number): number => Number.isFinite(value) ? value : 0;
const compare = (a: string, b: string): number => a.localeCompare(b, undefined, { numeric: true, sensitivity: 'base' });

export function sortGroups<T extends InspectableHit>(groups: readonly ResultGroup<T>[], sort: ResultSort): ResultGroup<T>[] {
    return [...groups].sort((a, b) => {
        let order = 0;
        if (sort === 'title') order = compare(a.best.title || a.best.filename || '', b.best.title || b.best.filename || '');
        else if (sort === 'filename') order = compare(a.best.filename || '', b.best.filename || '');
        else if (sort === 'location') order = compare(a.best.location_uri, b.best.location_uri);
        else if (sort.startsWith('year')) {
            const ay = a.best.year, by = b.best.year;
            // Unknown years always follow known years in either direction.
            order = ay == null ? (by == null ? 0 : 1) : by == null ? -1 : (ay - by) * (sort === 'year-desc' ? -1 : 1);
        } else {
            // A stable sort retains the engine's order for equal relevance
            // scores, including any tie-break information not in the score.
            return Number(Boolean(a.best.catalog_source)) - Number(Boolean(b.best.catalog_source))
                || finiteScore(b.best.score) - finiteScore(a.best.score);
        }
        return order || compare(a.best.title || a.best.filename || '', b.best.title || b.best.filename || '')
            || compare(a.best.location_uri, b.best.location_uri) || a.doc_id.localeCompare(b.doc_id);
    });
}

export function escapeHtml(value: string): string {
    return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
}

/** Backend snippets may carry only mark tags; never trust other markup. */
export function plainSnippet(value: string, marked = false): string {
    if (!marked) return value;
    return value.replace(/<\/?mark>/g, '').replace(/&(amp|lt|gt|quot|#39);/g,
        (_, name: string) => ({ amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" })[name] || '');
}

/** Display hints only; the native parser remains authoritative for matching. */
export function queryTerms(query: string): string[] {
    const tokens = query.match(/"(?:\\.|[^"\\])*"|[()]|[^\s()"]+|"/gu) || [];
    const terms: string[] = [];
    let negateNext = false;
    const exclusions = [false];
    for (let token of tokens) {
        if (/^(AND|OR)$/i.test(token)) continue;
        if (/^NOT$/i.test(token)) { negateNext = !negateNext; continue; }
        if (token === '(') { exclusions.push(exclusions.at(-1)! !== negateNext); negateNext = false; continue; }
        if (token === ')') { if (exclusions.length > 1) exclusions.pop(); negateNext = false; continue; }
        if (/^(title|body|headings|translated):$/i.test(token)) continue;
        if (/^(w|pre)\/\d+$/i.test(token)) continue;
        const excluded = exclusions.at(-1)! !== negateNext;
        negateNext = false;
        if (excluded) continue;
        token = token.replace(/^(title|body|headings|translated):/i, '').replace(/~[012]$/, '').replace(/[?*]/g, '');
        if (token.startsWith('"') && token.endsWith('"')) token = token.slice(1, -1).replace(/\\(["\\])/g, '$1');
        // Split punctuation like the native simple tokenizer. This is a visual
        // aid, not a claim that fuzzy or semantic matches are exact occurrences.
        terms.push(...(token.match(/[\p{L}\p{N}]+/gu) || []));
    }
    return [...new Set(terms.map(term => term.toLocaleLowerCase()))].slice(0, 64);
}

export interface TextRange { start: number; end: number }
export function occurrenceRanges(text: string, terms: readonly string[], max = 1000): TextRange[] {
    const needles = [...new Set(terms.filter(Boolean))].sort((a, b) => b.length - a.length);
    if (!needles.length) return [];
    const pattern = needles.map(term => term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|');
    const re = new RegExp(pattern, 'giu');
    const ranges: TextRange[] = [];
    for (const match of text.matchAll(re)) {
        ranges.push({ start: match.index!, end: match.index! + match[0].length });
        if (ranges.length >= max) break;
    }
    return ranges;
}

export function highlightedSnippet(value: string, query: string, marked = false): string {
    const text = plainSnippet(value, marked);
    // Find offsets in the original text, then escape each segment. Matching on
    // HTML entities corrupts source text and can manufacture visible hits.
    const ranges = occurrenceRanges(text, queryTerms(query));
    let offset = 0;
    let html = '';
    for (const range of ranges) {
        html += escapeHtml(text.slice(offset, range.start)) + '<mark>' + escapeHtml(text.slice(range.start, range.end)) + '</mark>';
        offset = range.end;
    }
    return html + escapeHtml(text.slice(offset));
}

export interface SearchReportInput {
    recipe: SearchRecipe;
    executedQuery: string;
    searchedAt: string;
    exportedAt: string;
    elapsedMs: number;
    sort: ResultSort;
    displayTags: string[];
    scope: 'displayed' | 'selected';
    returnedDocuments: number;
    returnedPassages: number;
    groups: readonly ResultGroup[];
}

export function buildSearchReport(input: SearchReportInput) {
    return {
        format: 'CrispSorter search report', version: 1,
        searchedAt: input.searchedAt, exportedAt: input.exportedAt,
        recipe: normalizeSearchRecipe(input.recipe), executedQuery: input.executedQuery,
        elapsedMs: input.elapsedMs, sort: input.sort, displayTags: [...input.displayTags].sort(), scope: input.scope,
        coverage: {
            returnedDocuments: input.returnedDocuments, returnedPassages: input.returnedPassages,
            exportedDocuments: input.groups.length, requestedLimit: input.recipe.limit,
            completeMatchSet: false,
            note: 'This report contains the returned search window. It does not establish a complete corpus match count. Sorting and display tags apply within that window.',
        },
        documents: input.groups.map(group => ({
            docId: group.doc_id, title: group.best.title || '', filename: group.best.filename || '',
            author: group.best.author || '', year: group.best.year ?? null,
            location: group.best.location_uri, sourceUrl: group.best.url || '',
            sourceHash: group.best.source_hash || '', indexedAt: group.best.indexed_at ?? null,
            language: group.best.language || '', fileType: group.best.ext || '',
            catalogSource: group.best.catalog_source || '', tags: [...(group.best.tags || [])],
            passages: group.chunks.map(hit => ({ chunkIndex: hit.chunk_index, score: finiteScore(hit.score), excerpt: plainSnippet(hit.snippet, hit.snippet_format === 'marked') })),
        })),
    };
}
export type SearchReport = ReturnType<typeof buildSearchReport>;

function csvCell(value: unknown): string {
    let text = value == null ? '' : String(value);
    // Quote syntax alone does not stop spreadsheet formula interpretation.
    if (/^\s*[=+@-]|^[\t\r\n]/u.test(text)) text = "'" + text;
    return '"' + text.replace(/"/g, '""') + '"';
}
export function searchReportCsv(report: SearchReport): string {
    const headers = ['query', 'executed_query', 'mode', 'filters_json', 'searched_at', 'doc_id', 'title', 'filename', 'author', 'year', 'location', 'source_url', 'source_hash', 'indexed_at', 'chunk_index', 'score', 'excerpt', 'coverage'];
    const rows: unknown[][] = [headers];
    for (const doc of report.documents) for (const passage of doc.passages) rows.push([
        report.recipe.query, report.executedQuery, report.recipe.mode, JSON.stringify(report.recipe.filters), report.searchedAt,
        doc.docId, doc.title, doc.filename, doc.author, doc.year, doc.location, doc.sourceUrl, doc.sourceHash,
        doc.indexedAt, passage.chunkIndex, passage.score, passage.excerpt, report.coverage.note,
    ]);
    return '\uFEFF' + rows.map(row => row.map(csvCell).join(',')).join('\r\n') + '\r\n';
}

export function searchReportHtml(report: SearchReport): string {
    return '<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
        + '<title>CrispSorter search report</title><style>body{max-width:70rem;margin:2rem auto;padding:0 1.2rem;font:16px/1.6 system-ui;color:#182331}article{border-top:1px solid #bcc8d7;padding:1rem 0}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f2f5f8;padding:1rem}code{overflow-wrap:anywhere}dt{font-weight:bold}dd{margin:0 0 .5rem}h1,h2{line-height:1.2}@media print{article{break-inside:avoid}}</style>'
        + '<h1>CrispSorter search report</h1><p>' + escapeHtml(report.coverage.note) + '</p>'
        + '<dl><dt>Query</dt><dd>' + escapeHtml(report.recipe.query) + '</dd><dt>Searched</dt><dd>' + escapeHtml(report.searchedAt)
        + '</dd><dt>Exported documents</dt><dd>' + report.documents.length + '</dd></dl><details><summary>Complete search recipe</summary><pre>'
        + escapeHtml(JSON.stringify({ recipe: report.recipe, executedQuery: report.executedQuery, sort: report.sort, displayTags: report.displayTags, scope: report.scope }, null, 2)) + '</pre></details>'
        + report.documents.map(doc => '<article><h2>' + escapeHtml(doc.title || doc.filename || doc.docId) + '</h2><p><code>'
            + escapeHtml(doc.location) + '</code></p><p>' + escapeHtml([doc.author, doc.year, doc.language].filter(Boolean).join(' · '))
            + '</p><p>Document: <code>' + escapeHtml(doc.docId) + '</code> · Source hash: <code>' + escapeHtml(doc.sourceHash || 'Unavailable') + '</code></p>'
            + doc.passages.map(p => '<p>Chunk ' + p.chunkIndex + ' · Score ' + p.score + '</p><pre>' + escapeHtml(p.excerpt) + '</pre>').join('') + '</article>').join('') + '</html>';
}
