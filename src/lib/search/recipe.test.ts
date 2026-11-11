import { describe, expect, it } from 'vitest';
import { activeSearchFilters, defaultSearchFilters, localSearchArguments, normalizeSearchRecipe,
    normalizeSearchView, restoreSavedSearches, restoreSearchHistory, searchRecipeKey, searchRequestKey, searchResultLimitForBackend, validateSearchFilters } from './recipe';

describe('saved search recipes', () => {
    it('restores old bookmarks and history with fresh defaults instead of current filters', () => {
        const old = { name: 'Old warranty', query: 'warranty', mode: 'text', limit: 50, savedAt: 1 };
        const [saved] = restoreSavedSearches([old]);
        expect(saved.filters).toEqual(defaultSearchFilters());
        expect(saved.smart).toBe(false);
        expect(saved.docIdScope).toEqual([]);
        expect(saved.ownerId).toBeNull();
        expect(saved.view).toEqual(normalizeSearchView(null));
        const [history] = restoreSearchHistory([{ query: 'old', mode: 'text', ts: 3, resultCount: 2 }]);
        expect(history.limit).toBe(20);
        expect(history.filters).toEqual(defaultSearchFilters());
    });

    it('round-trips every query option and result scope without retaining mutable references', () => {
        const original = normalizeSearchRecipe({
            query: ' warranty ', mode: 'text', limit: 100, smart: true, docIdScope: ['z', 'a'],
            filters: { ext: '.PDF, docx,pdf', language: 'DE', yearMin: 2000, yearMax: 2025,
                folderPrefix: '/papers', urlDomain: 'example.org', tag: 'reviewed', audioMin: 0,
                audioMax: 300, cameraMake: 'Canon', cameraModel: 'R5', colbert: true, omni: true,
                fuzzy: true, synonyms: true, includeUnmounted: true },
            view: { sort: 'year-desc', compact: true, preview: 'indexed', reportFormat: 'json',
                displayTags: ['reviewed', 'approved', 'reviewed'], tagCloud: true },
        });
        const [restored] = restoreSavedSearches(JSON.parse(JSON.stringify([{ ...original, name: 'Research', savedAt: 123 }])));
        expect(normalizeSearchRecipe(restored)).toEqual(original);
        expect(restored.filters.ext).toBe('docx,pdf');
        expect(restored.filters.language).toBe('de');
        expect(restored.filters.audioMin).toBe(0);
        expect(restored.view.displayTags).toEqual(['approved', 'reviewed']);
        restored.filters.tag = 'changed';
        restored.view.displayTags.push('changed');
        expect(original.filters.tag).toBe('reviewed');
        expect(original.view.displayTags).toEqual(['approved', 'reviewed']);
    });

    it('distinguishes equal query text with different filters, modes, limits or scope', () => {
        const recipe = normalizeSearchRecipe({ query: 'report' });
        for (const variant of [
            { ...recipe, filters: { ...recipe.filters, language: 'de' } },
            { ...recipe, mode: 'text' as const }, { ...recipe, limit: 50 },
            { ...recipe, docIdScope: ['one'] }, { ...recipe, smart: true },
        ]) expect(searchRecipeKey(variant)).not.toBe(searchRecipeKey(recipe));
        expect(searchRecipeKey({ ...recipe, filters: { ...recipe.filters, ext: 'pdf,docx' } }))
            .toBe(searchRecipeKey({ ...recipe, filters: { ...recipe.filters, ext: '.DOCX,pdf,pdf' } }));
    });

    it('recovers usable records from malformed settings', () => {
        expect(restoreSavedSearches({ query: 'wrong shape' })).toEqual([]);
        expect(restoreSavedSearches([null, {}, 'x', { query: 3 }, { query: 'ok', filters: null }])).toHaveLength(1);
        expect(normalizeSearchRecipe({ query: 'ok', mode: 'bogus', limit: 9999 }).mode).toBe('hybrid');
        expect(normalizeSearchRecipe({ query: 'ok', limit: 9999 }).limit).toBe(20);
        expect(normalizeSearchView({ sort: 'invalid', compact: 'true', preview: 'bad', reportFormat: 'pdf', displayTags: [null, 3, ' a ', 'a', ''], tagCloud: 1 }))
            .toEqual({ ...normalizeSearchView(null), displayTags: ['a'] });
    });

    it('keeps view variants as distinct bookmarks without invalidating the executed search', () => {
        const recipe = normalizeSearchRecipe({ query: 'report', filters: { language: 'de' } });
        const variant = normalizeSearchRecipe({ ...recipe, view: { sort: 'title', compact: true, preview: 'indexed', displayTags: ['reviewed'] } });
        expect(searchRecipeKey(variant)).not.toBe(searchRecipeKey(recipe));
        expect(searchRequestKey(variant)).toBe(searchRequestKey(recipe));
        expect(localSearchArguments(variant)).toEqual(localSearchArguments(recipe));
        expect(searchRequestKey(normalizeSearchRecipe({ ...variant, filters: { language: 'en' } })))
            .not.toBe(searchRequestKey(recipe));
    });

    it('uses the active backend window and preserves a saved larger limit until it is applied', () => {
        for (const backend of ['local', 'Local', 'hybrid']) expect(searchResultLimitForBackend(backend)).toBe(500);
        for (const backend of ['remote', 'Remote', null, 'unknown']) expect(searchResultLimitForBackend(backend)).toBe(200);
        const [saved] = restoreSavedSearches([{ query: 'research', limit: 500 }]);
        expect(saved.limit).toBe(500);
        const effective = normalizeSearchRecipe(saved, searchResultLimitForBackend('remote'));
        expect(effective.limit).toBe(200);
        expect(saved.limit).toBe(500);
        expect(localSearchArguments(effective).limit).toBe(200);
        expect(normalizeSearchRecipe({ query: 'research', limit: 50 }, 200).limit).toBe(50);
    });

    it('preserves exact owner scopes, including the empty owner, through saved searches and requests', () => {
        for (const ownerId of ['one', 'two', '']) {
            const recipe = normalizeSearchRecipe({ query: 'needle', ownerId, docIdScope: ['shared'] });
            expect(restoreSavedSearches([recipe])[0].ownerId).toBe(ownerId);
            expect(localSearchArguments(recipe)).toMatchObject({ ownerId, docIdScope: ['shared'], searchFilters: { owner_id: ownerId } });
            expect(searchRequestKey(recipe)).not.toBe(searchRequestKey(normalizeSearchRecipe({ ...recipe, ownerId: null })));
        }
    });
});

describe('search filter contract', () => {
    it('sends all advertised local metadata filters and options to the native boundary', () => {
        const recipe = normalizeSearchRecipe({ query: 'warranty', mode: 'text', limit: 50, docIdScope: ['a'], filters: {
            ext: 'pdf', language: 'de', yearMin: 2000, yearMax: 2025, folderPrefix: '/papers',
            urlDomain: 'example.org', tag: 'invoice', audioMin: 0, audioMax: 30,
            cameraMake: 'Canon', cameraModel: 'R5', colbert: true, omni: true,
            fuzzy: true, synonyms: true, includeUnmounted: true,
        }});
        expect(localSearchArguments(recipe)).toEqual({
            query: 'warranty', mode: 'text', limit: 50, ownerId: null, includeUnmounted: true,
            fuzzy: true, synonyms: true, docIdScope: ['a'], tag: 'invoice',
            searchFilters: { query_intent: 'advanced', owner_id: null, language: 'de', year_min: 2000, year_max: 2025,
                tags: [], ext: ['pdf'], parent_dir_prefix: '/papers', url_domain: 'example.org',
                audio_duration_min_seconds: 0, audio_duration_max_seconds: 30,
                image_camera_make: 'Canon', image_camera_model: 'R5', colbert_rerank: true, omni_search: true },
        });
    });

    it('preserves intent through bookmarks, requests, and stale-result identity', () => {
        const old = normalizeSearchRecipe({ version: 3, query: 'alpha AND beta' });
        expect(old.queryIntent).toBe('advanced');
        const natural = normalizeSearchRecipe({ ...old, queryIntent: 'natural' });
        expect(restoreSavedSearches([{ ...natural, name: 'Question' }])[0].queryIntent).toBe('natural');
        expect(localSearchArguments(natural).searchFilters.query_intent).toBe('natural');
        expect(searchRequestKey(natural)).not.toBe(searchRequestKey(old));
        expect(normalizeSearchRecipe({ ...natural, queryIntent: 'unknown' }).queryIntent).toBe('advanced');
    });

    it('counts zero duration as an active filter and ignores blank fields', () => {
        expect(activeSearchFilters({ ...defaultSearchFilters(), ext: ' ', audioMin: 0, fuzzy: true }))
            .toEqual(['audioMin', 'fuzzy']);
    });

    it('rejects inverted, fractional and nonfinite ranges before searching', () => {
        const defaults = defaultSearchFilters();
        expect(validateSearchFilters({ ...defaults, yearMin: 2025, yearMax: 2020 })).toBe('yearRange');
        expect(validateSearchFilters({ ...defaults, yearMin: 2020.5 })).toBe('year');
        expect(validateSearchFilters({ ...defaults, audioMin: -1 })).toBe('audio');
        expect(validateSearchFilters({ ...defaults, audioMax: Infinity })).toBe('audio');
        expect(validateSearchFilters({ ...defaults, audioMin: 40, audioMax: 20 })).toBe('audioRange');
        expect(validateSearchFilters({ ...defaults, audioMin: 0, audioMax: 0 })).toBeNull();
    });
});
