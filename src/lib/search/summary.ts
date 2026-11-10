import { localSearchArguments, normalizeSearchRecipe, type SearchRecipe } from './recipe';

export type CorpusFacet = 'extensions' | 'languages' | 'years' | 'tags';
export interface CorpusFacetValue { value: string; documents: number }
export interface CorpusFacetSummary {
    values: CorpusFacetValue[];
    distinctValues: number;
    omittedValues: number;
    missingDocuments: number;
}
export interface LexicalSummary {
    version: 1;
    scope: 'local_indexed_documents';
    query: string;
    effectiveQuery: string;
    generation: string;
    matchingDocuments: number;
    scannedRows: number;
    facetValueLimit: number;
    facets: Record<CorpusFacet, CorpusFacetSummary>;
}
export type CorpusReportFormat = 'csv' | 'json';
export type CorpusReportSort = 'filename' | 'title' | 'year-asc' | 'year-desc' | 'location';
export interface CorpusReportReceipt {
    path: string;
    documents: number;
    scope: 'local_indexed_documents';
    generation: string;
    format: CorpusReportFormat;
    sort: CorpusReportSort;
}

export function lexicalExportArguments(recipe: SearchRecipe, executedQuery: string, options: {
    requestId: string; path: string; format: CorpusReportFormat; sort: CorpusReportSort; expectedGeneration: string | null;
}) {
    return { request: { ...lexicalSummaryArguments(recipe, executedQuery, options.requestId), ...options } };
}

export function readCorpusReportReceipt(value: unknown, format: CorpusReportFormat, sort: CorpusReportSort): CorpusReportReceipt {
    const raw = value as Partial<CorpusReportReceipt> | null;
    if (!raw || typeof raw.path !== 'string' || !raw.path || raw.scope !== 'local_indexed_documents'
        || typeof raw.generation !== 'string' || !raw.generation || raw.format !== format || raw.sort !== sort
        || typeof raw.documents !== 'number' || !Number.isSafeInteger(raw.documents) || raw.documents < 0) {
        throw new Error('The index returned an invalid complete-report receipt. Check the selected destination before trying again.');
    }
    return raw as CorpusReportReceipt;
}

/** Keep malformed/legacy responses from turning an absent total into zero. */
export function readLexicalSummary(value: unknown, expectedQuery: string): LexicalSummary {
    const fail = (): never => { throw new Error('The index returned an invalid matching-document summary. Search again to refresh it.'); };
    if (!value || typeof value !== 'object' || Array.isArray(value)) return fail();
    const raw = value as Record<string, any>;
    const count = (item: unknown): number => typeof item === 'number' && Number.isSafeInteger(item) && item >= 0 ? item : fail();
    if (raw.version !== 1 || raw.scope !== 'local_indexed_documents' || raw.query !== expectedQuery
        || typeof raw.effectiveQuery !== 'string' || typeof raw.generation !== 'string' || !raw.generation
        || !raw.facets || typeof raw.facets !== 'object') return fail();
    const matchingDocuments = count(raw.matchingDocuments);
    const facetValueLimit = count(raw.facetValueLimit);
    if (facetValueLimit < 1 || facetValueLimit > 1000) return fail();
    const facets = {} as Record<CorpusFacet, CorpusFacetSummary>;
    for (const key of ['extensions', 'languages', 'years', 'tags'] as const) {
        const facet = raw.facets[key];
        if (!facet || !Array.isArray(facet.values) || facet.values.length > facetValueLimit) return fail();
        const values = facet.values.map((entry: any) => {
            if (!entry || typeof entry.value !== 'string' || !entry.value) return fail();
            const documents = count(entry.documents);
            if (documents < 1 || documents > matchingDocuments) return fail();
            return { value: entry.value, documents };
        });
        if (new Set(values.map((entry: CorpusFacetValue) => entry.value)).size !== values.length) return fail();
        const distinctValues = count(facet.distinctValues), omittedValues = count(facet.omittedValues);
        const missingDocuments = count(facet.missingDocuments);
        if (distinctValues !== values.length + omittedValues || missingDocuments > matchingDocuments) return fail();
        facets[key] = { values, distinctValues, omittedValues, missingDocuments };
    }
    const scannedRows = count(raw.scannedRows);
    if (scannedRows < matchingDocuments) return fail();
    return { version: 1, scope: 'local_indexed_documents', query: raw.query, effectiveQuery: raw.effectiveQuery,
        generation: raw.generation, matchingDocuments, scannedRows, facetValueLimit, facets };
}

export function canCountLexicalMatches(recipe: SearchRecipe | null, backend: unknown): boolean {
    return recipe?.mode === 'text' && typeof backend === 'string' && ['local', 'hybrid'].includes(backend.toLowerCase());
}

export function lexicalSummaryArguments(recipe: SearchRecipe, executedQuery: string, requestId: string) {
    const { limit: _limit, ...args } = localSearchArguments({ ...recipe, query: executedQuery });
    return { ...args, requestId };
}

/** Corpus chips change actual query filters and reset the returned-window tag view. */
export function recipeWithCorpusFacet(recipe: SearchRecipe, key: CorpusFacet, value: string): SearchRecipe {
    const next = normalizeSearchRecipe(recipe);
    if (key === 'extensions') next.filters.ext = value;
    else if (key === 'languages') next.filters.language = value;
    else if (key === 'tags') next.filters.tag = value;
    else {
        const year = Number(value);
        if (!Number.isInteger(year) || year < 1 || year > 9999) throw new Error('This stored year cannot be used as a publication-year filter.');
        next.filters.yearMin = year; next.filters.yearMax = year;
    }
    next.view.displayTags = [];
    return next;
}
