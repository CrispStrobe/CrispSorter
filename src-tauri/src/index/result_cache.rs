//! LRU cache for search results.
//!
//! Keyed on (query, mode, filters_hash). Invalidated via a global
//! generation counter that bumps on every ingest operation. Avoids
//! redundant re-embedding for repeated queries.

use std::collections::HashMap;
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

/// Simple LRU-ish cache with generation-based invalidation.
pub struct ResultCache {
    entries: HashMap<u64, CacheEntry>,
    order: Vec<u64>,
    capacity: usize,
}

impl ResultCache {
    pub fn new(capacity: usize) -> Self {
        ResultCache {
            entries: HashMap::with_capacity(capacity),
            order: Vec::with_capacity(capacity),
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

    pub fn get(&mut self, query: &str, mode: &str, filters_hash: u64) -> Option<Vec<SearchResult>> {
        let key = Self::make_key(query, mode, filters_hash);
        let gen = current_gen();
        if let Some(entry) = self.entries.get(&key) {
            if entry.generation == gen {
                self.order.retain(|&k| k != key);
                self.order.push(key);
                return Some(entry.results.clone());
            }
            // Stale
            self.entries.remove(&key);
            self.order.retain(|&k| k != key);
        }
        None
    }

    pub fn put(&mut self, query: &str, mode: &str, filters_hash: u64, results: Vec<SearchResult>) {
        let key = Self::make_key(query, mode, filters_hash);
        let gen = current_gen();
        while self.entries.len() >= self.capacity && !self.order.is_empty() {
            let oldest = self.order.remove(0);
            self.entries.remove(&oldest);
        }
        self.entries.insert(key, CacheEntry { results, generation: gen });
        self.order.retain(|&k| k != key);
        self.order.push(key);
    }
}

/// Compute a hash of SearchFilters for cache keying.
pub fn hash_filters(filters: &super::schema::SearchFilters) -> u64 {
    let json = serde_json::to_string(filters).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut hasher);
    hasher.finish()
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
}
