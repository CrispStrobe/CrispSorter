/** A complete, versioned local search; older saved queries restore clean defaults. */
export type SearchMode = 'hybrid' | 'text' | 'vector';

export interface SearchFilters {
    ext: string;
    language: string;
    yearMin: number | null;
    yearMax: number | null;
    folderPrefix: string;
    urlDomain: string;
    tag: string;
    audioMin: number | null;
    audioMax: number | null;
    cameraMake: string;
    cameraModel: string;
    colbert: boolean;
    omni: boolean;
    fuzzy: boolean;
    synonyms: boolean;
    includeUnmounted: boolean;
}

export interface SearchRecipe {
    version: 2;
    query: string;
    mode: SearchMode;
    limit: number;
    smart: boolean;
    filters: SearchFilters;
    docIdScope: string[];
}

export interface SavedSearch extends SearchRecipe { name: string; savedAt: number }
export interface SearchHistoryEntry extends SearchRecipe { ts: number; resultCount: number }

export function defaultSearchFilters(): SearchFilters {
    return {
        ext: '', language: '', yearMin: null, yearMax: null, folderPrefix: '',
        urlDomain: '', tag: '', audioMin: null, audioMax: null, cameraMake: '',
        cameraModel: '', colbert: false, omni: false, fuzzy: false,
        synonyms: false, includeUnmounted: false,
    };
}

function record(value: unknown): Record<string, unknown> {
    return value && typeof value === 'object' && !Array.isArray(value)
        ? value as Record<string, unknown> : {};
}
const text = (value: unknown): string => typeof value === 'string' ? value.trim() : '';
const number = (value: unknown): number | null => typeof value === 'number' && Number.isFinite(value) ? value : null;

export function normalizeSearchRecipe(value: unknown): SearchRecipe {
    const raw = record(value);
    const input = record(raw.filters);
    const filters = defaultSearchFilters();
    for (const key of ['ext', 'language', 'folderPrefix', 'urlDomain', 'tag', 'cameraMake', 'cameraModel'] as const) filters[key] = text(input[key]);
    filters.ext = [...new Set(filters.ext.split(',').map(ext => ext.trim().replace(/^\./, '').toLowerCase()).filter(Boolean))].sort().join(',');
    filters.language = filters.language.toLowerCase();
    for (const key of ['yearMin', 'yearMax', 'audioMin', 'audioMax'] as const) filters[key] = number(input[key]);
    for (const key of ['colbert', 'omni', 'fuzzy', 'synonyms', 'includeUnmounted'] as const) filters[key] = input[key] === true;
    return {
        version: 2,
        query: text(raw.query),
        mode: raw.mode === 'text' || raw.mode === 'vector' ? raw.mode : 'hybrid',
        limit: [10, 20, 50, 100].includes(Number(raw.limit)) ? Number(raw.limit) : 20,
        smart: raw.smart === true,
        filters,
        docIdScope: Array.isArray(raw.docIdScope) ? [...new Set(raw.docIdScope.filter((id): id is string => typeof id === 'string' && id.length > 0))].sort() : [],
    };
}

export function restoreSavedSearches(value: unknown): SavedSearch[] {
    if (!Array.isArray(value)) return [];
    return value.flatMap(item => {
        const recipe = normalizeSearchRecipe(item);
        if (!recipe.query) return [];
        const raw = record(item);
        return [{ ...recipe, name: text(raw.name) || recipe.query, savedAt: number(raw.savedAt) ?? 0 }];
    });
}

export function restoreSearchHistory(value: unknown): SearchHistoryEntry[] {
    if (!Array.isArray(value)) return [];
    return value.flatMap(item => {
        const recipe = normalizeSearchRecipe(item);
        if (!recipe.query) return [];
        const raw = record(item);
        return [{ ...recipe, ts: number(raw.ts) ?? 0, resultCount: number(raw.resultCount) ?? 0 }];
    }).slice(0, 50);
}

/** Excludes name/time so queries with different filters are distinct bookmarks. */
export function searchRecipeKey(recipe: SearchRecipe): string {
    return JSON.stringify(normalizeSearchRecipe(recipe));
}

export function validateSearchFilters(filters: SearchFilters): 'year' | 'yearRange' | 'audio' | 'audioRange' | null {
    for (const value of [filters.yearMin, filters.yearMax]) {
        if (value != null && (!Number.isInteger(value) || value < 1 || value > 9999)) return 'year';
    }
    if (filters.yearMin != null && filters.yearMax != null && filters.yearMin > filters.yearMax) return 'yearRange';
    for (const value of [filters.audioMin, filters.audioMax]) {
        if (value != null && (!Number.isFinite(value) || value < 0)) return 'audio';
    }
    if (filters.audioMin != null && filters.audioMax != null && filters.audioMin > filters.audioMax) return 'audioRange';
    return null;
}

/** Tauri argument names match the native command, including the shared scalar filter fields. */
export function localSearchArguments(recipe: SearchRecipe, candidateLimit = recipe.limit) {
    const f = recipe.filters;
    return {
        query: recipe.query, mode: recipe.mode, limit: candidateLimit, ownerId: null,
        includeUnmounted: f.includeUnmounted, fuzzy: f.fuzzy, synonyms: f.synonyms,
        docIdScope: recipe.docIdScope.length ? recipe.docIdScope : null,
        tag: f.tag || null,
        searchFilters: {
            owner_id: null, language: f.language || null, year_min: f.yearMin, year_max: f.yearMax,
            tags: [], ext: f.ext ? f.ext.split(',') : [], parent_dir_prefix: f.folderPrefix || null,
            url_domain: f.urlDomain || null,
            audio_duration_min_seconds: f.audioMin, audio_duration_max_seconds: f.audioMax,
            image_camera_make: f.cameraMake || null, image_camera_model: f.cameraModel || null,
            colbert_rerank: f.colbert, omni_search: f.omni,
        },
    };
}

export function activeSearchFilters(filters: SearchFilters): (keyof SearchFilters)[] {
    return (Object.keys(defaultSearchFilters()) as (keyof SearchFilters)[]).filter(key => {
        const value = filters[key];
        return typeof value === 'string' ? value.trim().length > 0 : value != null && value !== false;
    });
}
