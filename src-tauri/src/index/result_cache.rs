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
}

impl ResultCache {
    pub fn new(capacity: usize) -> Self {
        ResultCache {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
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
        let gen = current_gen();
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
        let key = Self::make_key(query, mode, filters_hash);
        let gen = current_gen();
        // Evict oldest if at capacity
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.entries.insert(key, CacheEntry { results, generation: gen });
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
    filters.language.hash(&mut h);
    filters.year_min.hash(&mut h);
    filters.year_max.hash(&mut h);
    filters.tags.hash(&mut h);
    filters.prefer_translated_lang.hash(&mut h);
    filters.ext.hash(&mut h);
    filters.source_hash_prefix.hash(&mut h);
    filters.parent_dir_prefix.hash(&mut h);
    // f64 fields: hash by bit pattern (NaN == NaN for cache purposes)
    filters.audio_duration_min_seconds.map(|v| v.to_bits()).hash(&mut h);
    filters.audio_duration_max_seconds.map(|v| v.to_bits()).hash(&mut h);
    filters.image_camera_make.hash(&mut h);
    filters.image_camera_model.hash(&mut h);
    filters.omni_search.hash(&mut h);
    filters.colbert_rerank.hash(&mut h);
    filters.url_domain.hash(&mut h);
    filters.tag.hash(&mut h);
    filters.fuzzy.hash(&mut h);
    filters.synonyms.hash(&mut h);
    filters.doc_id_scope.hash(&mut h);
    filters.doc_status.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_and_miss() {
        let mut cache = ResultCache::new(4);
        cache.put("hello", "hybrid", 0, vec![]);
        assert!(cache.get("hello", "hybrid", 0).is_some());
        assert!(cache.get("world", "hybrid", 0).is_none());
    }

    #[test]
    fn invalidation_clears_stale() {
        let mut cache = ResultCache::new(4);
        cache.put("hello", "hybrid", 0, vec![]);
        assert!(cache.get("hello", "hybrid", 0).is_some());
        invalidate();
        assert!(cache.get("hello", "hybrid", 0).is_none());
    }

    #[test]
    fn evicts_oldest_at_capacity() {
        let mut cache = ResultCache::new(2);
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
        let mut cache = ResultCache::new(10);
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
        let mut cache = ResultCache::new(10);
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
        let mut cache = ResultCache::new(2);
        cache.put("a", "h", 0, vec![]);
        cache.put("b", "h", 0, vec![]);
        // Touch "a" to promote it to MRU
        let _ = cache.get("a", "h", 0);
        // Insert "c" → should evict "b" (now the oldest), not "a"
        cache.put("c", "h", 0, vec![]);
        assert!(cache.get("a", "h", 0).is_some(), "a was touched, must survive");
        assert!(cache.get("b", "h", 0).is_none(), "b is LRU, must be evicted");
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
        let mut cache = ResultCache::new(4);
        cache.put("q", "h", 0, vec![]);
        cache.put("q", "h", 0, vec![]);
        // Internal deque should deduplicate — adding 3 more should not evict "q"
        cache.put("a", "h", 0, vec![]);
        cache.put("b", "h", 0, vec![]);
        cache.put("c", "h", 0, vec![]);
        assert!(cache.get("q", "h", 0).is_some(), "q should still be within capacity 4");
    }

    #[test]
    fn capacity_one_works() {
        let mut cache = ResultCache::new(1);
        cache.put("a", "h", 0, vec![]);
        assert!(cache.get("a", "h", 0).is_some());
        cache.put("b", "h", 0, vec![]);
        assert!(cache.get("a", "h", 0).is_none(), "a should be evicted at cap=1");
        assert!(cache.get("b", "h", 0).is_some());
    }
}
