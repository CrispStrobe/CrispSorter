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
use super::reranker::RerankerHandle;
use super::schema::{SearchFilters, SearchResult};

// ── SearchEngine ────────────────────────────────────────────────────────────

pub struct SearchEngine {
    pub fts: Arc<FtsIndex>,
    pub vector: Arc<LocalIndex>,
    pub embedder: Arc<Mutex<Embedder>>,
    /// Optional cross-encoder reranker. When set, each search method fetches
    /// `rerank_top_n` candidates (instead of `limit`), scores them with the
    /// reranker, then truncates to the requested `limit`.
    reranker: Option<RerankerHandle>,
    rerank_top_n: usize,
}

impl SearchEngine {
    pub fn new(
        fts: Arc<FtsIndex>,
        vector: Arc<LocalIndex>,
        embedder: Arc<Mutex<Embedder>>,
    ) -> Self {
        SearchEngine {
            fts,
            vector,
            embedder,
            reranker: None,
            rerank_top_n: 50,
        }
    }

    /// Enable cross-encoder reranking. `top_n` controls how many candidates
    /// are scored per query (recall vs latency tradeoff; default 50).
    pub fn with_reranker(mut self, handle: RerankerHandle, top_n: usize) -> Self {
        self.reranker = Some(handle);
        self.rerank_top_n = top_n.max(1);
        self
    }

    fn fetch_limit(&self, requested: usize) -> usize {
        if self.reranker.is_some() {
            self.rerank_top_n.max(requested)
        } else {
            requested
        }
    }

    /// If a reranker is configured, score `results` against `query` and
    /// re-sort by reranker score descending. Items that the reranker scored
    /// as NaN (load failure / scoring error) keep their original RRF order
    /// at the back of the list.
    async fn maybe_rerank(
        &self,
        query: &str,
        mut results: Vec<SearchResult>,
        limit: usize,
    ) -> Vec<SearchResult> {
        let Some(ref handle) = self.reranker else {
            results.truncate(limit);
            return results;
        };
        if results.is_empty() {
            return results;
        }
        // Cap to top_n: the reranker only needs to score the candidate window,
        // not the entire result set. If `fetch_limit` already bounded this,
        // the truncation is a no-op.
        let n = results.len().min(self.rerank_top_n);
        results.truncate(n);

        let docs: Vec<&str> = results.iter().map(|r| r.snippet.as_str()).collect();
        let scores = handle.score_batch(query, &docs).await;

        // Annotate each result with its reranker score; preserve the RRF
        // score for the NaN fallback path so we keep stable ordering.
        let mut paired: Vec<(SearchResult, f32, f32)> = results
            .into_iter()
            .zip(scores.into_iter())
            .map(|(r, rr)| {
                let rrf = r.score;
                (r, rr, rrf)
            })
            .collect();

        paired.sort_by(|a, b| {
            // Valid scores first, sorted desc; NaN entries fall to the back
            // and tie-break by original RRF score.
            match (a.1.is_nan(), b.1.is_nan()) {
                (false, false) => b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal),
                (true, true) => b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal),
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
            }
        });

        let mut out: Vec<SearchResult> = paired
            .into_iter()
            .map(|(mut r, rr, rrf)| {
                // Replace .score with reranker score when available; the
                // RRF score is no longer meaningful once we've reranked.
                r.score = if rr.is_nan() { rrf } else { rr };
                r
            })
            .collect();
        out.truncate(limit);
        out
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
        // When reranking is on, pull a wider candidate window so the cross
        // encoder has enough material to re-sort to `limit`.
        let inner_limit = self.fetch_limit(limit);
        let hits = self.fts.search(query, filters, inner_limit)?;
        if hits.is_empty() {
            return Ok(vec![]);
        }

        let doc_ids: Vec<String> = hits.iter().map(|h| h.doc_id.clone()).collect();
        let score_map: HashMap<String, f32> =
            hits.iter().map(|h| (h.doc_id.clone(), h.score)).collect();

        // Extract query terms for best-chunk selection (words ≥ 3 chars).
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|w| {
                w.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_owned()
            })
            .filter(|w| w.len() >= 3)
            .collect();
        let term_refs: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();

        let mut results = self
            .vector
            .fetch_best_chunk_per_doc(&doc_ids, &term_refs, &score_map)
            .await?;

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(self.maybe_rerank(query, results, limit).await)
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
        let inner_limit = self.fetch_limit(limit);
        let results = self
            .vector
            .search_vector(&embedding, filters, inner_limit)
            .await?;
        Ok(self.maybe_rerank(query_text, results, limit).await)
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

        // When reranking is on, pull a wider candidate window so the cross
        // encoder has enough material to re-sort to `limit`. The internal
        // *2 multiplier still applies on top so RRF has slack on each side.
        let inner_limit = self.fetch_limit(limit);

        let fts_clone = self.fts.clone();
        let vec_clone = self.vector.clone();
        let q_owned = query_text.to_owned();
        let filters_fts = filters.clone();
        let filters_vec = filters.clone();
        let emb_clone = embedding.clone();

        let fts_task = tokio::spawn(async move {
            fts_clone.search(&q_owned, &filters_fts, inner_limit * 2)
        });
        let vec_task = tokio::spawn(async move {
            vec_clone
                .search_vector(&emb_clone, &filters_vec, inner_limit * 2)
                .await
        });

        let (fts_result, vec_result) = tokio::try_join!(fts_task, vec_task)?;
        let fts_hits = fts_result?;
        let vec_hits = vec_result?;

        // Optional 3rd modality: BGE-M3 / SPLADE sparse retrieval, scored on
        // the union of FTS+ANN candidates. Cheap (no extra DB scan beyond
        // what we'd already need to hydrate snippets) and only runs when the
        // active embedder has a sparse head.
        let sparse_hits = self
            .maybe_sparse_search(query_text, &fts_hits, &vec_hits, filters, inner_limit)
            .await;

        // RRF merge — 2-way (no sparse) or 3-way (with sparse).
        let mut lists: Vec<Vec<String>> = vec![
            doc_ids_from_fts(&fts_hits),
            doc_ids_from_results(&vec_hits),
        ];
        if let Some(ref sparse) = sparse_hits {
            lists.push(doc_ids_from_results(sparse));
        }
        let merged = rrf_merge_n(&lists, 60, inner_limit);
        if merged.is_empty() {
            return Ok(vec![]);
        }
        let score_map: HashMap<String, f32> = merged.iter().cloned().collect();

        // Index vector results by doc_id (first/best chunk per doc from ANN).
        // Vec search is already sorted best-chunk-first by cosine similarity.
        let vec_by_doc: HashMap<String, SearchResult> =
            vec_hits.into_iter().fold(HashMap::new(), |mut map, r| {
                map.entry(r.doc_id.clone()).or_insert(r);
                map
            });

        // FTS-only doc_ids need hydration from LanceDB.
        let fts_only_ids: Vec<String> = merged
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| !vec_by_doc.contains_key(id))
            .collect();

        let mut fts_hydrated: HashMap<String, SearchResult> = HashMap::new();
        if !fts_only_ids.is_empty() {
            let terms: Vec<String> = query_text
                .split_whitespace()
                .map(|w| {
                    w.to_lowercase()
                        .trim_matches(|c: char| !c.is_alphanumeric())
                        .to_owned()
                })
                .filter(|w| w.len() >= 3)
                .collect();
            let term_refs: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();
            let fts_score_sub: HashMap<String, f32> = fts_only_ids
                .iter()
                .filter_map(|id| score_map.get(id).map(|&s| (id.clone(), s)))
                .collect();
            let hydrated = self
                .vector
                .fetch_best_chunk_per_doc(&fts_only_ids, &term_refs, &fts_score_sub)
                .await?;
            for r in hydrated {
                fts_hydrated.insert(r.doc_id.clone(), r);
            }
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

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(self.maybe_rerank(query_text, results, limit).await)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    /// Encode `text` as a sparse query vector if the active embedder has a
    /// sparse head, then score it against the union of FTS + ANN candidates
    /// using `LocalIndex::search_sparse_in_pool`. Returns `None` when the
    /// embedder is dense-only or any step fails (sparse is purely additive).
    async fn maybe_sparse_search(
        &self,
        query_text: &str,
        fts_hits: &[super::fts_index::FtsHit],
        vec_hits: &[SearchResult],
        filters: &super::schema::SearchFilters,
        limit: usize,
    ) -> Option<Vec<SearchResult>> {
        let mut emb = self.embedder.lock().await;
        if !emb.has_sparse() {
            return None;
        }
        // BGE-M3 / SPLADE are trained without prefixes — pass query through as-is.
        let mut sparse_vecs = match emb.embed_sparse(vec![query_text.to_owned()]) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[search] sparse query embed failed, skipping: {e:#}");
                return None;
            }
        };
        drop(emb);
        let sparse_q = sparse_vecs.pop().flatten()?;

        // Union of doc_ids from both retrieval sources, dedup'd.
        let mut pool: std::collections::BTreeSet<String> =
            fts_hits.iter().map(|h| h.doc_id.clone()).collect();
        pool.extend(vec_hits.iter().map(|r| r.doc_id.clone()));
        let pool: Vec<String> = pool.into_iter().collect();
        if pool.is_empty() {
            return None;
        }

        match self
            .vector
            .search_sparse_in_pool(&sparse_q, &pool, filters, limit)
            .await
        {
            Ok(hits) if !hits.is_empty() => Some(hits),
            Ok(_) => None,
            Err(e) => {
                eprintln!("[search] sparse pool scoring failed, skipping: {e:#}");
                None
            }
        }
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        use super::embedder::EmbedRole;
        let mut emb = self.embedder.lock().await;
        let dense = emb.embed_dense(vec![text.to_owned()], EmbedRole::Query)?;
        dense
            .vectors
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("embedder returned no vectors"))
    }
}

// ── RRF ────────────────────────────────────────────────────────────────────

/// Two-way RRF kept for the legacy bug-fix test below. Production code
/// (search_hybrid) uses `rrf_merge_n` directly so the same fusion logic
/// covers 2-way (no sparse) and 3-way (with sparse) without duplication.
#[cfg(test)]
fn rrf_merge(
    fts_hits: &[super::fts_index::FtsHit],
    vec_hits: &[SearchResult],
    k: usize,
    limit: usize,
) -> Vec<(String, f32)> {
    rrf_merge_n(
        &[
            doc_ids_from_fts(fts_hits),
            doc_ids_from_results(vec_hits),
        ],
        k,
        limit,
    )
}

/// Generalized N-way Reciprocal Rank Fusion. Each list is a slice of doc_ids
/// already sorted best-first. Per-list deduplication keeps only the best rank
/// for each document, so a doc appearing as multiple chunks in the same list
/// doesn't bloat its score. Used to fuse FTS + dense ANN + sparse signals.
fn rrf_merge_n(lists: &[Vec<String>], k: usize, limit: usize) -> Vec<(String, f32)> {
    let mut scores: HashMap<String, f32> = HashMap::new();

    for list in lists {
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (rank, doc_id) in list.iter().enumerate() {
            seen.entry(doc_id.clone()).or_insert(rank);
        }
        for (doc_id, rank) in seen {
            let entry = scores.entry(doc_id).or_insert(0.0);
            *entry += 1.0 / (k + rank + 1) as f32;
        }
    }

    let mut ranked: Vec<(String, f32)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);
    ranked
}

fn doc_ids_from_fts(hits: &[super::fts_index::FtsHit]) -> Vec<String> {
    hits.iter().map(|h| h.doc_id.clone()).collect()
}

fn doc_ids_from_results(results: &[SearchResult]) -> Vec<String> {
    results.iter().map(|r| r.doc_id.clone()).collect()
}

// ── Pure-logic tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::fts_index::FtsHit;

    fn make_fts_hits(ids: &[&str]) -> Vec<FtsHit> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| FtsHit {
                doc_id: id.to_string(),
                score: (ids.len() - i) as f32,
            })
            .collect()
    }

    fn make_vec_hits(ids: &[&str]) -> Vec<SearchResult> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| SearchResult {
                doc_id: id.to_string(),
                location_uri: String::new(),
                owner_id: String::new(),
                title: None,
                author: None,
                year: None,
                filename: None,
                ext: None,
                language: None,
                snippet: String::new(),
                score: 1.0 / (i + 1) as f32,
                chunk_index: 0,
                catalog_source: None,
            })
            .collect()
    }

    #[test]
    fn rrf_summation_bug_fixed() {
        // Doc A has 10 chunks in top vector results
        // Doc B has 1 chunk at rank 0 in FTS and 1 chunk at rank 10 in Vector
        let fts = make_fts_hits(&["b"]); // b is #1
        let mut vec = Vec::new();
        for _ in 0..10 {
            vec.push(SearchResult {
                doc_id: "a".to_string(),
                location_uri: String::new(),
                owner_id: String::new(),
                title: None,
                author: None,
                year: None,
                filename: None,
                ext: None,
                language: None,
                snippet: String::new(),
                score: 0.9,
                chunk_index: 0,
                catalog_source: None,
            });
        }
        vec.push(SearchResult {
            doc_id: "b".to_string(),
            location_uri: String::new(),
            owner_id: String::new(),
            title: None,
            author: None,
            year: None,
            filename: None,
            ext: None,
            language: None,
            snippet: String::new(),
            score: 0.95,
            chunk_index: 0,
            catalog_source: None,
        });

        let merged = rrf_merge(&fts, &vec, 60, 10);
        let a_score = merged.iter().find(|(id, _)| id == "a").unwrap().1;
        let b_score = merged.iter().find(|(id, _)| id == "b").unwrap().1;

        println!("Doc A score (10 vector chunks): {}", a_score);
        println!("Doc B score (1 FTS rank 0, 1 Vector rank 10): {}", b_score);

        // Doc B should now outrank Doc A because B appeared at the top of FTS,
        // and A's multiple chunks are no longer being summed.
        assert!(
            b_score > a_score,
            "Doc B should outrank Doc A after RRF fix"
        );
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
        assert!(
            a_score > b_score,
            "a (in both lists) should outscore b (fts-only)"
        );
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

    #[test]
    fn rrf_three_way_boosts_consensus_doc() {
        // doc "x" appears in all three lists → highest RRF
        // doc "y" appears in two lists; doc "z" only in one
        let fts: Vec<String> = vec!["x".into(), "y".into()];
        let vec: Vec<String> = vec!["x".into(), "y".into(), "z".into()];
        let sparse: Vec<String> = vec!["x".into(), "z".into()];
        let merged = rrf_merge_n(&[fts, vec, sparse], 60, 10);
        let x = merged.iter().find(|(id, _)| id == "x").unwrap().1;
        let y = merged.iter().find(|(id, _)| id == "y").unwrap().1;
        let z = merged.iter().find(|(id, _)| id == "z").unwrap().1;
        assert!(x > y, "x (3 lists) should beat y (2 lists)");
        assert!(y > z, "y (2 lists) should beat z (1 list)");
    }

    #[test]
    fn rrf_n_handles_zero_lists() {
        let merged = rrf_merge_n(&[], 60, 10);
        assert!(merged.is_empty());
    }

    #[test]
    fn rrf_n_dedupes_within_list() {
        // A doc appearing twice in the same list should only contribute its
        // best rank — not be summed across chunks.
        let bloated: Vec<String> = vec!["a".into(); 5];
        let single: Vec<String> = vec!["a".into()];
        let merged = rrf_merge_n(&[bloated], 60, 5);
        let merged_single = rrf_merge_n(&[single], 60, 5);
        assert!((merged[0].1 - merged_single[0].1).abs() < 1e-6);
    }
}
