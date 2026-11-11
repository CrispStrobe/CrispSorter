//! LRU cache for search results.
//!
//! Keyed on (query, mode, filters_hash). Invalidated via a global
//! generation counter that bumps on every ingest operation. Avoids
//! redundant re-embedding for repeated queries.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use super::schema::SearchResult;

/// Global generation counter. Bumped on every ingest operation.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Bump the generation counter (call after every ingest batch).
pub fn invalidate() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

fn current_gen() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

#[derive(Clone)]
struct CacheEntry {
    results: Vec<SearchResult>,
    generation: u64,
}

/// Simple LRU cache with generation-based invalidation.
///
/// Uses a `VecDeque` for O(1) front-eviction instead of `Vec::remove(0)`.
pub struct ResultCache {
    entries: HashMap<u64, CacheEntry>,
    order: VecDeque<u64>,
    capacity: usize,
    /// Test-only: freeze the generation this cache stamps and compares.
    ///
    /// The generation counter is process-global and `invalidate()` bumps it
    /// after every ingest batch — correct in production, but it means a test
    /// exercising LRU eviction on its *own* cache instance still fails when
    /// any other test in the suite ingests between its `put` and `get`.
    /// `lru_touch_prevents_eviction` failed exactly that way in CI while
    /// passing locally. Pinning removes the shared dependency instead of
    /// papering over it with retries or `--test-threads=1`.
    #[cfg(test)]
    pinned_generation: Option<u64>,
}

impl ResultCache {
    pub fn new(capacity: usize) -> Self {
        ResultCache {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
            #[cfg(test)]
            pinned_generation: None,
        }
    }

    /// The generation this cache stamps entries with and compares against.
    fn generation(&self) -> u64 {
        #[cfg(test)]
        if let Some(g) = self.pinned_generation {
            return g;
        }
        current_gen()
    }

    /// Capture before starting an asynchronous search. Its result must retain
    /// this generation even if ingestion completes before the search does.
    pub fn generation_token(&self) -> u64 {
        self.generation()
    }

    /// Test-only constructor whose generation never moves under it.
    #[cfg(test)]
    fn new_pinned(capacity: usize) -> Self {
        let mut c = Self::new(capacity);
        c.pinned_generation = Some(current_gen());
        c
    }

    fn make_key(query: &str, mode: &str, filters_hash: u64) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        query.hash(&mut hasher);
        mode.hash(&mut hasher);
        filters_hash.hash(&mut hasher);
        hasher.finish()
    }

    /// Remove `key` from the order deque (O(n) scan, but n ≤ 32).
    fn touch(&mut self, key: u64) {
        if let Some(pos) = self.order.iter().position(|&k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }

    pub fn get(&mut self, query: &str, mode: &str, filters_hash: u64) -> Option<Vec<SearchResult>> {
        let key = Self::make_key(query, mode, filters_hash);
        let gen = self.generation();
        // Check + clone before mutating to avoid borrow-checker conflict.
        let hit = self
            .entries
            .get(&key)
            .filter(|e| e.generation == gen)
            .map(|e| e.results.clone());
        if hit.is_some() {
            self.touch(key);
            return hit;
        }
        // Stale or missing — remove if present.
        if self.entries.remove(&key).is_some() {
            if let Some(pos) = self.order.iter().position(|&k| k == key) {
                self.order.remove(pos);
            }
        }
        None
    }

    pub fn put(&mut self, query: &str, mode: &str, filters_hash: u64, results: Vec<SearchResult>) {
        self.put_at_generation(query, mode, filters_hash, results, self.generation());
    }

    /// Do not relabel a result that started before an index mutation. Stamping
    /// the captured generation also closes the race with invalidation between
    /// this comparison and the actual insertion.
    pub fn put_at_generation(
        &mut self,
        query: &str,
        mode: &str,
        filters_hash: u64,
        results: Vec<SearchResult>,
        generation: u64,
    ) {
        if self.capacity == 0 || self.generation() != generation {
            return;
        }
        let key = Self::make_key(query, mode, filters_hash);
        // Evict oldest if at capacity
        while !self.entries.contains_key(&key) && self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.entries.insert(
            key,
            CacheEntry {
                results,
                generation,
            },
        );
        self.touch(key);
    }
}

/// Compute a hash of SearchFilters for cache keying.
///
/// Hashes each field directly instead of round-tripping through JSON
/// serialization. `f64` fields are hashed by their bit pattern.
pub fn hash_filters(filters: &super::schema::SearchFilters) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    filters.owner_id.hash(&mut h);
    filters.mounted_volume_ids.hash(&mut h);
    filters.language.hash(&mut h);
    filters.year_min.hash(&mut h);
    filters.year_max.hash(&mut h);
    filters.tags.hash(&mut h);
    filters.prefer_translated_lang.hash(&mut h);
    filters.ext.hash(&mut h);
    filters.source_hash_prefix.hash(&mut h);
    filters.parent_dir_prefix.hash(&mut h);
    // f64 fields: hash by bit pattern (NaN == NaN for cache purposes)
    filters
        .audio_duration_min_seconds
        .map(|v| v.to_bits())
        .hash(&mut h);
    filters
        .audio_duration_max_seconds
        .map(|v| v.to_bits())
        .hash(&mut h);
    filters.image_camera_make.hash(&mut h);
    filters.image_camera_model.hash(&mut h);
    filters.omni_search.hash(&mut h);
    filters.colbert_rerank.hash(&mut h);
    filters.url_domain.hash(&mut h);
    filters.tag.hash(&mut h);
    filters.query_intent.as_deref().unwrap_or("advanced").hash(&mut h);
    filters.fuzzy.hash(&mut h);
    filters.synonyms.hash(&mut h);
    filters.doc_id_scope.hash(&mut h);
    filters.doc_status.hash(&mut h);
    h.finish()
}

/// Result count is part of a request: a cached top-5 cannot answer top-50.
pub fn hash_request(filters: &super::schema::SearchFilters, limit: usize) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_filters(filters).hash(&mut h);
    limit.hash(&mut h);
    h.finish()
}

/// An external CLI or another app process can commit without touching this
/// process's invalidation counter. Bind cached answers to the durable table
/// version observed under the shared read lease as well as the request.
pub fn hash_snapshot(filters: &super::schema::SearchFilters, limit: usize, version: u64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_request(filters, limit).hash(&mut h);
    version.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_commits_cannot_reuse_process_local_cached_answers() {
        let mut cache = ResultCache::new_pinned(2);
        let filters = super::super::schema::SearchFilters::default();
        let original = hash_snapshot(&filters, 20, 10);
        let external_commit = hash_snapshot(&filters, 20, 11);
        cache.put("query", "hybrid", original, vec![]);
        assert!(cache.get("query", "hybrid", original).is_some());
        assert!(cache.get("query", "hybrid", external_commit).is_none());
    }

    #[test]
    fn in_flight_result_is_not_relabelled_after_index_changes() {
        let mut cache = ResultCache::new_pinned(2);
        let before = cache.generation_token();
        cache.pinned_generation = Some(before.wrapping_add(1));
        cache.put_at_generation("old", "hybrid", 0, vec![], before);
        assert!(cache.get("old", "hybrid", 0).is_none());
        let after = cache.generation_token();
        cache.put_at_generation("new", "hybrid", 0, vec![], after);
        assert!(cache.get("new", "hybrid", 0).is_some());
    }

    #[test]
    fn replacing_an_entry_at_capacity_does_not_evict_another_query() {
        let mut cache = ResultCache::new_pinned(2);
        cache.put("a", "hybrid", 0, vec![]);
        cache.put("b", "hybrid", 0, vec![]);
        cache.put("b", "hybrid", 0, vec![]);
        assert!(cache.get("a", "hybrid", 0).is_some());
        assert!(cache.get("b", "hybrid", 0).is_some());
    }

    #[test]
    fn zero_capacity_is_a_disabled_cache() {
        let mut cache = ResultCache::new_pinned(0);
        cache.put("query", "hybrid", 0, vec![]);
        assert!(cache.get("query", "hybrid", 0).is_none());
    }

    #[test]
    fn mounted_volume_changes_do_not_reuse_hidden_source_results() {
        let unrestricted = super::super::schema::SearchFilters::default();
        let offline = super::super::schema::SearchFilters {
            mounted_volume_ids: Some(vec![]),
            ..Default::default()
        };
        let online = super::super::schema::SearchFilters {
            mounted_volume_ids: Some(vec!["drive-a".into()]),
            ..Default::default()
        };
        assert_ne!(hash_filters(&unrestricted), hash_filters(&offline));
        assert_ne!(hash_filters(&offline), hash_filters(&online));
        let mut cache = ResultCache::new_pinned(2);
        cache.put("same query", "hybrid", hash_request(&online, 20), vec![]);
        assert!(cache
            .get("same query", "hybrid", hash_request(&offline, 20))
            .is_none());
    }

    #[test]
    fn distinct_result_limits_do_not_share_cached_answers() {
        let filters = super::super::schema::SearchFilters::default();
        let mut cache = ResultCache::new_pinned(4);
        cache.put("same query", "hybrid", hash_request(&filters, 5), vec![]);
        assert!(cache
            .get("same query", "hybrid", hash_request(&filters, 50))
            .is_none());
        assert!(cache
            .get("same query", "hybrid", hash_request(&filters, 5))
            .is_some());
    }

    #[test]
    fn cache_hit_and_miss() {
        let mut cache = ResultCache::new_pinned(4);
        cache.put("hello", "hybrid", 0, vec![]);
        assert!(cache.get("hello", "hybrid", 0).is_some());
        assert!(cache.get("world", "hybrid", 0).is_none());
    }

    #[test]
    fn invalidation_clears_stale() {
        // Deliberately NOT pinned: this test's subject *is* the global
        // generation, so freezing it would make the assertion below
        // unfalsifiable. It is also inherently safe under parallelism —
        // a concurrent `invalidate()` can only reinforce the expected miss,
        // never turn it into a hit. That asymmetry is the rule for the rest
        // of this module: tests asserting a *hit* need pinning, tests
        // asserting a miss-after-invalidate do not.
        let mut cache = ResultCache::new(4);
        cache.put("hello", "hybrid", 0, vec![]);
        // Another test may invalidate concurrently, so only the miss after
        // invalidation is asserted here. Cache hits use pinned generations.
        invalidate();
        assert!(cache.get("hello", "hybrid", 0).is_none());
    }

    #[test]
    fn evicts_oldest_at_capacity() {
        let mut cache = ResultCache::new_pinned(2);
        cache.put("a", "hybrid", 0, vec![]);
        cache.put("b", "hybrid", 0, vec![]);
        cache.put("c", "hybrid", 0, vec![]);
        assert!(cache.get("a", "hybrid", 0).is_none());
        assert!(cache.get("b", "hybrid", 0).is_some());
        assert!(cache.get("c", "hybrid", 0).is_some());
    }

    #[test]
    fn different_modes_are_different_keys() {
        // Same query + filters_hash, different mode → two independent cache slots.
        let mut cache = ResultCache::new_pinned(10);
        cache.put("query", "text", 0, vec![]);
        // "vector" mode was never inserted → cache miss.
        assert!(
            cache.get("query", "vector", 0).is_none(),
            "different mode must be a cache miss"
        );
        // The "text" entry should still be present.
        assert!(
            cache.get("query", "text", 0).is_some(),
            "original 'text' entry should still be a hit"
        );
    }

    #[test]
    fn different_filters_are_different_keys() {
        // Same query + mode, different filters_hash → independent slots.
        let mut cache = ResultCache::new_pinned(10);
        cache.put("hello", "hybrid", 111, vec![]);
        // filters_hash=222 was never inserted.
        assert!(
            cache.get("hello", "hybrid", 222).is_none(),
            "different filters_hash must be a cache miss"
        );
        // filters_hash=111 must still be a hit.
        assert!(
            cache.get("hello", "hybrid", 111).is_some(),
            "original filters_hash=111 should still hit"
        );
    }

    #[test]
    fn lru_touch_prevents_eviction() {
        // Access "a" after inserting "b" → "a" is promoted, "b" is evicted.
        let mut cache = ResultCache::new_pinned(2);
        cache.put("a", "h", 0, vec![]);
        cache.put("b", "h", 0, vec![]);
        // Touch "a" to promote it to MRU
        let _ = cache.get("a", "h", 0);
        // Insert "c" → should evict "b" (now the oldest), not "a"
        cache.put("c", "h", 0, vec![]);
        assert!(
            cache.get("a", "h", 0).is_some(),
            "a was touched, must survive"
        );
        assert!(
            cache.get("b", "h", 0).is_none(),
            "b is LRU, must be evicted"
        );
        assert!(cache.get("c", "h", 0).is_some(), "c was just inserted");
    }

    #[test]
    fn hash_filters_deterministic() {
        use super::super::schema::SearchFilters;
        let f1 = SearchFilters {
            language: Some("de".into()),
            year_min: Some(2020),
            fuzzy: true,
            ..Default::default()
        };
        let f2 = SearchFilters {
            language: Some("de".into()),
            year_min: Some(2020),
            fuzzy: true,
            ..Default::default()
        };
        assert_eq!(hash_filters(&f1), hash_filters(&f2));
    }

    #[test]
    fn hash_filters_differs_on_field_change() {
        use super::super::schema::SearchFilters;
        let f1 = SearchFilters {
            language: Some("de".into()),
            ..Default::default()
        };
        let f2 = SearchFilters {
            language: Some("en".into()),
            ..Default::default()
        };
        assert_ne!(hash_filters(&f1), hash_filters(&f2));
    }

    #[test]
    fn hash_filters_f64_bits() {
        use super::super::schema::SearchFilters;
        let f1 = SearchFilters {
            audio_duration_min_seconds: Some(10.5),
            ..Default::default()
        };
        let f2 = SearchFilters {
            audio_duration_min_seconds: Some(10.5),
            ..Default::default()
        };
        let f3 = SearchFilters {
            audio_duration_min_seconds: Some(10.6),
            ..Default::default()
        };
        assert_eq!(hash_filters(&f1), hash_filters(&f2));
        assert_ne!(hash_filters(&f1), hash_filters(&f3));
    }

    #[test]
    fn put_same_key_twice_no_duplicate() {
        let mut cache = ResultCache::new_pinned(4);
        cache.put("q", "h", 0, vec![]);
        cache.put("q", "h", 0, vec![]);
        // Internal deque should deduplicate — adding 3 more should not evict "q"
        cache.put("a", "h", 0, vec![]);
        cache.put("b", "h", 0, vec![]);
        cache.put("c", "h", 0, vec![]);
        assert!(
            cache.get("q", "h", 0).is_some(),
            "q should still be within capacity 4"
        );
    }

    #[test]
    fn capacity_one_works() {
        let mut cache = ResultCache::new_pinned(1);
        cache.put("a", "h", 0, vec![]);
        assert!(cache.get("a", "h", 0).is_some());
        cache.put("b", "h", 0, vec![]);
        assert!(
            cache.get("a", "h", 0).is_none(),
            "a should be evicted at cap=1"
        );
        assert!(cache.get("b", "h", 0).is_some());
    }
}
