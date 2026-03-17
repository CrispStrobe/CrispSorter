/// Unified search module: Tantivy FTS + LanceDB vector search with RRF reranking.
///
/// `SearchEngine` holds references to both indexes and the embedder.
/// The three public methods cover all search modes exposed in the UI.
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use super::embedder::Embedder;
use super::fts_index::FtsIndex;
use super::local_index::LocalIndex;
use super::schema::{SearchFilters, SearchResult};

// ── SearchEngine ────────────────────────────────────────────────────────────

pub struct SearchEngine {
    pub fts:      Arc<FtsIndex>,
    pub vector:   Arc<LocalIndex>,
    pub embedder: Arc<Mutex<Embedder>>,
}

impl SearchEngine {
    pub fn new(fts: Arc<FtsIndex>, vector: Arc<LocalIndex>, embedder: Arc<Mutex<Embedder>>) -> Self {
        SearchEngine { fts, vector, embedder }
    }

    // ── Text-only search ───────────────────────────────────────────────────

    /// BM25 full-text search via Tantivy, then hydrate metadata from LanceDB.
    /// Returns one result per document; picks the chunk whose text best matches
    /// the query terms (not always the first chunk).
    pub async fn search_text(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let hits = self.fts.search(query, filters, limit)?;
        if hits.is_empty() { return Ok(vec![]); }

        let doc_ids: Vec<String> = hits.iter().map(|h| h.doc_id.clone()).collect();
        let score_map: HashMap<String, f32> =
            hits.iter().map(|h| (h.doc_id.clone(), h.score)).collect();

        // Extract query terms for best-chunk selection (words ≥ 3 chars).
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
            .filter(|w| w.len() >= 3)
            .collect();
        let term_refs: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();

        let mut results = self.vector
            .fetch_best_chunk_per_doc(&doc_ids, &term_refs, &score_map)
            .await?;

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }

    // ── Vector-only search ─────────────────────────────────────────────────

    /// Embed `query_text`, then run ANN search in LanceDB.
    pub async fn search_vector(
        &self,
        query_text: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let embedding = self.embed_query(query_text).await?;
        self.vector.search_vector(&embedding, filters, limit).await
    }

    // ── Hybrid search (RRF) ────────────────────────────────────────────────

    /// Run FTS and vector search in parallel, then merge with Reciprocal Rank
    /// Fusion (k = 60).  Returns up to `limit` results sorted by RRF score desc.
    ///
    /// Snippet strategy:
    ///   • Docs returned by vector search → use the ANN chunk directly.
    ///     The ANN already picked the most semantically relevant chunk;
    ///     throwing it away and re-fetching chunk_index=0 loses that context.
    ///   • Docs that only appeared in FTS  → best-chunk selection from LanceDB.
    pub async fn search_hybrid(
        &self,
        query_text: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Embed first, then run both searches concurrently.
        let embedding = self.embed_query(query_text).await?;

        let fts_clone    = self.fts.clone();
        let vec_clone    = self.vector.clone();
        let q_owned      = query_text.to_owned();
        let filters_fts  = filters.clone();
        let filters_vec  = filters.clone();
        let emb_clone    = embedding.clone();

        let fts_task = tokio::spawn(async move {
            fts_clone.search(&q_owned, &filters_fts, limit * 2)
        });
        let vec_task = tokio::spawn(async move {
            vec_clone.search_vector(&emb_clone, &filters_vec, limit * 2).await
        });

        let (fts_result, vec_result) = tokio::try_join!(fts_task, vec_task)?;
        let fts_hits = fts_result?;
        let vec_hits = vec_result?;

        // RRF merge → (doc_id, rrf_score)
        let merged = rrf_merge(&fts_hits, &vec_hits, 60, limit);
        if merged.is_empty() { return Ok(vec![]); }
        let score_map: HashMap<String, f32> = merged.iter().cloned().collect();

        // Index vector results by doc_id (first/best chunk per doc from ANN).
        // Vec search is already sorted best-chunk-first by cosine similarity.
        let vec_by_doc: HashMap<String, SearchResult> = vec_hits
            .into_iter()
            .fold(HashMap::new(), |mut map, r| {
                map.entry(r.doc_id.clone()).or_insert(r);
                map
            });

        // FTS-only doc_ids need hydration from LanceDB.
        let fts_only_ids: Vec<String> = merged.iter()
            .map(|(id, _)| id.clone())
            .filter(|id| !vec_by_doc.contains_key(id))
            .collect();

        let mut fts_hydrated: HashMap<String, SearchResult> = HashMap::new();
        if !fts_only_ids.is_empty() {
            let terms: Vec<String> = query_text
                .split_whitespace()
                .map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_owned())
                .filter(|w| w.len() >= 3)
                .collect();
            let term_refs: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();
            let fts_score_sub: HashMap<String, f32> = fts_only_ids.iter()
                .filter_map(|id| score_map.get(id).map(|&s| (id.clone(), s)))
                .collect();
            let hydrated = self.vector
                .fetch_best_chunk_per_doc(&fts_only_ids, &term_refs, &fts_score_sub)
                .await?;
            for r in hydrated { fts_hydrated.insert(r.doc_id.clone(), r); }
        }

        // Assemble final results in RRF rank order.
        let mut results: Vec<SearchResult> = Vec::with_capacity(merged.len());
        for (doc_id, rrf_score) in &merged {
            let base = if let Some(r) = vec_by_doc.get(doc_id) {
                Some(r.clone())
            } else {
                fts_hydrated.get(doc_id).cloned()
            };
            if let Some(mut r) = base {
                r.score = *rrf_score;
                results.push(r);
            }
        }

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut emb = self.embedder.lock().await;
        let dense = emb.embed_dense(vec![text.to_owned()])?;
        dense.vectors.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("embedder returned no vectors"))
    }
}

// ── RRF ────────────────────────────────────────────────────────────────────

/// Reciprocal Rank Fusion.
///
/// `fts_hits`  — ranked list from Tantivy (index 0 = best)
/// `vec_hits`  — ranked list from LanceDB ANN (index 0 = best)
/// `k`         — RRF constant (typically 60)
///
/// Returns a list of (doc_id, rrf_score) sorted by score descending, truncated
/// to `limit` entries.
fn rrf_merge(
    fts_hits:  &[super::fts_index::FtsHit],
    vec_hits:  &[SearchResult],
    k: usize,
    limit: usize,
) -> Vec<(String, f32)> {
    let mut scores: HashMap<String, f32> = HashMap::new();

    for (rank, hit) in fts_hits.iter().enumerate() {
        let entry = scores.entry(hit.doc_id.clone()).or_insert(0.0);
        *entry += 1.0 / (k + rank + 1) as f32;
    }
    for (rank, hit) in vec_hits.iter().enumerate() {
        let entry = scores.entry(hit.doc_id.clone()).or_insert(0.0);
        *entry += 1.0 / (k + rank + 1) as f32;
    }

    let mut ranked: Vec<(String, f32)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);
    ranked
}

// ── Pure-logic tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::fts_index::FtsHit;

    fn make_fts_hits(ids: &[&str]) -> Vec<FtsHit> {
        ids.iter().enumerate()
            .map(|(i, id)| FtsHit { doc_id: id.to_string(), score: (ids.len() - i) as f32 })
            .collect()
    }

    fn make_vec_hits(ids: &[&str]) -> Vec<SearchResult> {
        ids.iter().enumerate().map(|(i, id)| SearchResult {
            doc_id: id.to_string(),
            location_uri: String::new(),
            owner_id: String::new(),
            title: None, author: None, year: None,
            filename: None, ext: None, language: None,
            snippet: String::new(),
            score: 1.0 / (i + 1) as f32,
            chunk_index: 0,
        }).collect()
    }

    #[test]
    fn rrf_summation_bug_demonstration() {
        // Doc A has 10 chunks in top vector results
        // Doc B has 1 chunk at rank 0 in FTS and 1 chunk at rank 0 in Vector
        let fts = make_fts_hits(&["b"]); // b is #1
        let mut vec = Vec::new();
        for _ in 0..10 {
            vec.push(SearchResult {
                doc_id: "a".to_string(),
                location_uri: String::new(),
                owner_id: String::new(),
                title: None, author: None, year: None,
                filename: None, ext: None, language: None,
                snippet: String::new(),
                score: 0.9,
                chunk_index: 0,
            });
        }
        vec.push(SearchResult {
            doc_id: "b".to_string(),
            location_uri: String::new(),
            owner_id: String::new(),
            title: None, author: None, year: None,
            filename: None, ext: None, language: None,
            snippet: String::new(),
            score: 0.95,
            chunk_index: 0,
        });

        let merged = rrf_merge(&fts, &vec, 60, 10);
        let a_score = merged.iter().find(|(id, _)| id == "a").unwrap().1;
        let b_score = merged.iter().find(|(id, _)| id == "b").unwrap().1;

        println!("Doc A score (10 vector chunks): {}", a_score);
        println!("Doc B score (1 FTS rank 0, 1 Vector rank 10): {}", b_score);

        // Doc A should NOT outrank Doc B just because it has many chunks,
        // but currently it does.
        assert!(a_score > b_score, "BUG: Doc A outranks Doc B because of summed chunk scores");
    }

    #[test]
    fn rrf_common_doc_scores_higher() {
        let fts = make_fts_hits(&["a", "b", "c"]);
        let vec = make_vec_hits(&["c", "d", "a"]);
        let merged = rrf_merge(&fts, &vec, 60, 10);
        // "a" is rank 0 in fts and rank 2 in vec → high RRF
        // "c" is rank 2 in fts and rank 0 in vec → high RRF
        // Both should outrank "b" (only in fts) and "d" (only in vec)
        let ids: Vec<&str> = merged.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"a"), "a should appear in merged results");
        assert!(ids.contains(&"c"), "c should appear in merged results");
        // a appears in both lists → should score higher than b (fts-only)
        let a_score = merged.iter().find(|(id, _)| id == "a").unwrap().1;
        let b_score = merged.iter().find(|(id, _)| id == "b").unwrap().1;
        assert!(a_score > b_score, "a (in both lists) should outscore b (fts-only)");
    }

    #[test]
    fn rrf_limit_respected() {
        let fts = make_fts_hits(&["a", "b", "c", "d", "e"]);
        let vec = make_vec_hits(&["e", "d", "c", "b", "a"]);
        let merged = rrf_merge(&fts, &vec, 60, 3);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn rrf_empty_lists() {
        let merged = rrf_merge(&[], &[], 60, 10);
        assert!(merged.is_empty());
    }
}
