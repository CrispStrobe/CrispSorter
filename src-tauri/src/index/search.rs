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
    /// `None` when the index was init'd without vector capabilities.
    /// Vector / hybrid search return a clear error in that case;
    /// text-only (BM25) still works.
    pub embedder: Option<Arc<Mutex<Embedder>>>,
    /// Optional cross-encoder reranker. When set, each search method fetches
    /// `rerank_top_n` candidates (instead of `limit`), scores them with the
    /// reranker, then truncates to the requested `limit`.
    reranker: Option<RerankerHandle>,
    rerank_top_n: usize,
    /// Bi-encoder fallback reranker (P13.5 follow-up).  When `true`
    /// AND `reranker` is `None`, `maybe_rerank` reranks top-N
    /// candidates by cosine similarity against the query, computed
    /// via the loaded `embedder`.  Reuses the already-paid-for
    /// dense model — no extra download or memory cost.  Less
    /// accurate per-pair than the dedicated cross-encoder but
    /// closes the "no reranker model installed" gap with a real
    /// recall lift over no-rerank.
    use_embedder_as_reranker: bool,
    /// Stage Z — alternate reranker for non-Latin-script queries.
    /// When set AND `has_nonlatin_script(query)` returns `true`,
    /// `maybe_rerank` routes to this handle instead of `reranker`.
    /// Useful for installing a CJK/Arabic-optimised reranker (e.g.
    /// `bge-reranker-v2-m3`, which excels at Chinese/Japanese/Korean)
    /// alongside a Latin-primary one — zero config for monolingual
    /// users, automatic upgrade for multilingual users.
    reranker_multilingual: Option<RerankerHandle>,
}

/// Returns `true` when ≥ 25% of non-whitespace characters in `query`
/// are outside the Latin + Latin Extended Unicode blocks.  Covers CJK
/// (0x3000-0x9FFF, 0xAC00-0xD7FF, 0xF900-0xFAFF, 0x20000-0x3FFFF),
/// Arabic (0x0600-0x06FF), Hebrew (0x0590-0x05FF), Cyrillic (0x0400-0x04FF),
/// Devanagari (0x0900-0x097F), Thai (0x0E00-0x0E7F), and more.
///
/// Used by `SearchEngine::maybe_rerank` to route queries to the
/// multilingual reranker when the primary one is Latin-optimised.
pub(crate) fn has_nonlatin_script(query: &str) -> bool {
    let mut total = 0usize;
    let mut nonlatin = 0usize;
    for c in query.chars() {
        if c.is_whitespace() || c.is_ascii_punctuation() {
            continue;
        }
        total += 1;
        let cp = c as u32;
        // Latin ranges: Basic Latin (0–0x7F), Latin-1 Supplement (0x80–0xFF),
        // Latin Extended-A/B (0x100–0x24F), Latin Extended Additional (0x1E00–0x1EFF).
        let in_latin = cp <= 0x024F || (0x1E00..=0x1EFF).contains(&cp);
        if !in_latin {
            nonlatin += 1;
        }
    }
    // Require at least 4 relevant chars to avoid false positives on very
    // short or purely-numeric queries (e.g. "2024" or "ok").
    total >= 4 && nonlatin * 4 >= total
}

impl SearchEngine {
    pub fn new(
        fts: Arc<FtsIndex>,
        vector: Arc<LocalIndex>,
        embedder: Option<Arc<Mutex<Embedder>>>,
    ) -> Self {
        SearchEngine {
            fts,
            vector,
            embedder,
            reranker: None,
            rerank_top_n: 50,
            use_embedder_as_reranker: false,
            reranker_multilingual: None,
        }
    }

    /// Enable cross-encoder reranking. `top_n` controls how many candidates
    /// are scored per query (recall vs latency tradeoff; default 50).
    pub fn with_reranker(mut self, handle: RerankerHandle, top_n: usize) -> Self {
        self.reranker = Some(handle);
        self.rerank_top_n = top_n.max(1);
        self
    }

    /// Enable the alternate reranker for non-Latin-script queries.
    /// When a query is detected as predominantly CJK / Arabic / Cyrillic /
    /// etc. (≥ 25% of non-whitespace characters outside Latin + Latin
    /// Extended Unicode blocks), `maybe_rerank` routes to `handle`
    /// instead of the primary `reranker`.  When the primary reranker
    /// is absent, the multilingual one becomes the sole reranker for
    /// all queries.
    pub fn with_multilingual_reranker(mut self, handle: RerankerHandle, top_n: usize) -> Self {
        self.reranker_multilingual = Some(handle);
        if self.reranker.is_none() {
            // When no primary reranker is set, the multilingual one applies
            // to all queries; rerank_top_n controls the candidate window.
            self.rerank_top_n = self.rerank_top_n.max(top_n.max(1));
        }
        self
    }

    /// Enable the bi-encoder fallback reranker.  Wins only when no
    /// dedicated cross-encoder is configured (`with_reranker` not
    /// called).  When both are configured the dedicated one runs —
    /// it's more accurate per-pair, this flag is for users who
    /// don't have a separate reranker model installed.
    pub fn with_embedder_as_reranker(mut self, enabled: bool, top_n: usize) -> Self {
        self.use_embedder_as_reranker = enabled;
        // Only bump rerank_top_n when no dedicated reranker already
        // claimed its value — that path's caller set it explicitly.
        if enabled && self.reranker.is_none() {
            self.rerank_top_n = top_n.max(1);
        }
        self
    }

    fn fetch_limit(&self, requested: usize) -> usize {
        if self.reranker.is_some()
            || self.reranker_multilingual.is_some()
            || self.use_embedder_as_reranker
        {
            self.rerank_top_n.max(requested)
        } else {
            requested
        }
    }

    /// If a reranker is configured, score `results` against `query` and
    /// re-sort by reranker score descending. Items that the reranker scored
    /// as NaN (load failure / scoring error) keep their original RRF order
    /// at the back of the list.
    ///
    /// Two reranker sources are wired:
    ///   * Dedicated cross-encoder (`self.reranker`) — set via
    ///     [`Self::with_reranker`].  Highest quality per pair, one
    ///     model invocation per candidate.
    ///   * Embedder bi-encoder fallback
    ///     (`self.use_embedder_as_reranker` + `self.embedder`) —
    ///     reuses the loaded dense model; one batch embed +
    ///     cosine.  Less accurate per pair but free in terms of
    ///     extra disk / RAM, so it's the right default for users
    ///     who haven't downloaded a dedicated reranker.
    /// When both are configured the dedicated cross-encoder wins.
    async fn maybe_rerank(
        &self,
        query: &str,
        mut results: Vec<SearchResult>,
        limit: usize,
    ) -> Vec<SearchResult> {
        // Pick the scoring path.  Returns NaN on failure → caller's
        // NaN-fallback preserves the original RRF order for that doc.
        enum RerankPath<'a> {
            Dedicated(&'a RerankerHandle),
            EmbedderBiEncoder, // uses self.embedder, gated on the flag
            None,
        }
        // Stage Z: when the query is predominantly non-Latin-script (CJK,
        // Arabic, Cyrillic, …) AND a multilingual reranker is configured,
        // prefer it over the primary cross-encoder.  When no primary exists
        // the multilingual handle fires for all queries.
        let use_multilingual = self.reranker_multilingual.is_some()
            && (self.reranker.is_none() || has_nonlatin_script(query));

        let path = if use_multilingual {
            RerankPath::Dedicated(self.reranker_multilingual.as_ref().unwrap())
        } else if let Some(ref h) = self.reranker {
            RerankPath::Dedicated(h)
        } else if self.use_embedder_as_reranker && self.embedder.is_some() {
            RerankPath::EmbedderBiEncoder
        } else {
            RerankPath::None
        };
        if matches!(path, RerankPath::None) {
            results.truncate(limit);
            return results;
        }
        if results.is_empty() {
            return results;
        }
        // Cap to top_n: the reranker only needs to score the candidate window,
        // not the entire result set. If `fetch_limit` already bounded this,
        // the truncation is a no-op.
        let n = results.len().min(self.rerank_top_n);
        results.truncate(n);

        let docs: Vec<&str> = results.iter().map(|r| r.snippet.as_str()).collect();
        let scores: Vec<f32> = match path {
            RerankPath::Dedicated(handle) => handle.score_batch(query, &docs).await,
            RerankPath::EmbedderBiEncoder => {
                // Embedder is in an Arc<Mutex<…>> shared with the
                // ingest pipeline.  Hold the lock for the duration
                // of the embed batch — bi-encoder reranking with
                // ~50 candidates is a single batched forward pass,
                // fast enough that other callers waiting briefly
                // is fine.  On any error, NaN the whole row so
                // the NaN-fallback path keeps RRF order intact
                // instead of bouncing the entire query.
                //
                // The outer dispatch guarantees `self.embedder.is_some()`
                // because RerankPath::EmbedderBiEncoder is only
                // reached under that guard — but match defensively
                // anyway to avoid panicking inside a search query.
                match self.embedder.as_ref() {
                    None => vec![f32::NAN; docs.len()],
                    Some(embedder) => {
                        let docs_owned: Vec<String> =
                            docs.iter().map(|s| s.to_string()).collect();
                        let mut emb = embedder.lock().await;
                        match emb.rerank_biencoder(query, &docs_owned) {
                            Ok(s) => s,
                            Err(e) => {
                                eprintln!(
                                    "[search] embedder bi-encoder rerank failed, \
                                     falling back to RRF order: {e:#}"
                                );
                                vec![f32::NAN; docs.len()]
                            }
                        }
                    }
                }
            }
            RerankPath::None => unreachable!(),
        };

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

    /// P13.5 follow-up — when the caller asked for
    /// `filters.prefer_translated_lang = X`, swap each result's
    /// `snippet` from the original-text-derived preview to a
    /// `text_translated`-derived one for rows whose
    /// `text_translated_lang` matches.  No-op otherwise (every
    /// `SearchResult.snippet` is already the original preview).
    ///
    /// Called BEFORE [`Self::maybe_rerank`] so the reranker scores
    /// against the user-facing snippet, not the original-language
    /// one that won't match an English query.  Coverage caveat:
    /// the FTS / vector channels themselves still score against
    /// the original `full_text` columns — true cross-lingual
    /// retrieval-side query rewrite needs a Tantivy schema field
    /// for translated text, which is a separate slice.
    /// Stage AE wiring — when `filters.colbert_rerank` is set AND the
    /// active embedder has a ColBERT head, embed the query as per-token
    /// vectors and call `LocalIndex::rerank_with_colbert` on `results`
    /// before the final reranker pass.  Falls back to the input
    /// unchanged whenever any prerequisite is missing (no flag, no
    /// embedder, no ColBERT head, embedding error, empty results) —
    /// the re-rank is purely additive on supported corpora.
    ///
    /// Holds the embedder mutex for one query-side encode; the
    /// document-side multivecs are already on disk in
    /// `multivec_packed`, so the DB round-trip is one filter scan.
    async fn maybe_colbert_rerank(
        &self,
        query: &str,
        results: Vec<SearchResult>,
        filters: &SearchFilters,
        limit: usize,
    ) -> Vec<SearchResult> {
        if !filters.colbert_rerank || results.is_empty() {
            return results;
        }
        let Some(embedder) = self.embedder.as_ref() else {
            return results;
        };
        let query_multivec = {
            let mut emb = embedder.lock().await;
            if !emb.has_colbert() {
                return results;
            }
            match emb.embed_multivec(vec![query.to_owned()]) {
                Ok(mut v) if !v.is_empty() => v.remove(0),
                _ => return results,
            }
        };
        if query_multivec.is_empty() {
            return results;
        }
        let snapshot = results.clone();
        match self
            .vector
            .rerank_with_colbert(results, &query_multivec, limit)
            .await
        {
            Ok(reranked) => reranked,
            Err(e) => {
                eprintln!("[search] colbert_rerank failed, keeping RRF order: {e:#}");
                snapshot
            }
        }
    }

    fn apply_translation_snippet(
        results: &mut [SearchResult],
        prefer_translated_lang: Option<&str>,
    ) {
        let Some(tgt) = prefer_translated_lang else {
            return;
        };
        for r in results.iter_mut() {
            // Match the target lang on a per-row basis — a result
            // set with mixed translations is fine; rows whose
            // target lang doesn't match keep their original snippet.
            if r.text_translated_lang.as_deref() == Some(tgt) {
                if let Some(ref translated) = r.text_translated {
                    r.snippet = translated.chars().take(400).collect();
                }
            }
        }
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
        Self::apply_translation_snippet(&mut results, filters.prefer_translated_lang.as_deref());
        let results = self.maybe_colbert_rerank(query, results, filters, limit).await;
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
        let mut results = self
            .vector
            .search_vector(&embedding, filters, inner_limit)
            .await?;
        Self::apply_translation_snippet(&mut results, filters.prefer_translated_lang.as_deref());
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
        Self::apply_translation_snippet(&mut results, filters.prefer_translated_lang.as_deref());
        let results = self.maybe_colbert_rerank(query_text, results, filters, limit).await;
        Ok(self.maybe_rerank(query_text, results, limit).await)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    /// Encode `text` as a sparse query vector if the active embedder has a
    /// sparse head, then score it against the union of FTS + ANN candidates
    /// using `LocalIndex::search_sparse_in_pool`. Returns `None` when the
    /// embedder is missing, dense-only, or any step fails (sparse is purely
    /// additive).
    async fn maybe_sparse_search(
        &self,
        query_text: &str,
        fts_hits: &[super::fts_index::FtsHit],
        vec_hits: &[SearchResult],
        filters: &super::schema::SearchFilters,
        limit: usize,
    ) -> Option<Vec<SearchResult>> {
        let embedder = self.embedder.as_ref()?;
        let mut emb = embedder.lock().await;
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
        let embedder = self.embedder.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Vector search needs an embedder. Enable \
                 `Vektor-Embeddings verwenden` in Settings → Search Index \
                 and re-init the catalog."
            )
        })?;
        let mut emb = embedder.lock().await;
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

// ── ColBERT MaxSim ───────────────────────────────────────────────────────────

/// Late-interaction ColBERT MaxSim score.
///
/// For each query token vector, finds the maximum dot product against
/// all document token vectors, then sums these per-query-token maxima.
/// Both `query_vecs` and `doc_vecs` are expected to be L2-normalised
/// (as produced by BGE-M3's ColBERT head).
///
/// Returns 0.0 when either side is empty.
pub(crate) fn maxsim(query_vecs: &[Vec<f32>], doc_vecs: &[Vec<f32>]) -> f32 {
    if query_vecs.is_empty() || doc_vecs.is_empty() {
        return 0.0;
    }
    query_vecs
        .iter()
        .map(|qv| {
            doc_vecs
                .iter()
                .map(|dv| qv.iter().zip(dv.iter()).map(|(a, b)| a * b).sum::<f32>())
                .fold(f32::NEG_INFINITY, f32::max)
        })
        .sum()
}

/// Unpack `multivec_packed` bytes (little-endian f32) back into token vectors.
///
/// `dim` is the ColBERT projection dimension (128 for BGE-M3).
/// Returns an empty Vec on size mismatch or when `packed` is empty.
pub(crate) fn unpack_multivec(packed: &[u8], n_tokens: i16, dim: usize) -> Vec<Vec<f32>> {
    let n = n_tokens as usize;
    if packed.is_empty() || n == 0 || dim == 0 { return vec![]; }
    let expected = n * dim * 4;
    if packed.len() < expected { return vec![]; }
    (0..n)
        .map(|i| {
            (0..dim)
                .map(|j| {
                    let off = (i * dim + j) * 4;
                    f32::from_le_bytes(packed[off..off + 4].try_into().unwrap())
                })
                .collect()
        })
        .collect()
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
                metadata_json: None,
                catalog_source: None,
                volume_id: None,
                indexed_at: 0,
                source_hash: String::new(),
                text_translated: None,
                text_translated_lang: None,
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
                metadata_json: None,
                catalog_source: None,
                volume_id: None,
                indexed_at: 0,
                source_hash: String::new(),
                text_translated: None,
                text_translated_lang: None,
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
            metadata_json: None,
            catalog_source: None,
            volume_id: None,
            indexed_at: 0,
            source_hash: String::new(),
            text_translated: None,
            text_translated_lang: None,
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

    // ── P13.5 follow-up: apply_translation_snippet ────────────────────────

    fn mk_result(snippet: &str, translated: Option<&str>, lang: Option<&str>) -> SearchResult {
        SearchResult {
            doc_id: "d".into(),
            location_uri: String::new(),
            owner_id: String::new(),
            title: None,
            author: None,
            year: None,
            filename: None,
            ext: None,
            language: None,
            snippet: snippet.to_string(),
            score: 0.5,
            chunk_index: 0,
            metadata_json: None,
            catalog_source: None,
            volume_id: None,
            indexed_at: 0,
            source_hash: String::new(),
            text_translated: translated.map(|s| s.to_string()),
            text_translated_lang: lang.map(|s| s.to_string()),
        }
    }

    #[test]
    fn translation_snippet_swap_no_op_when_filter_unset() {
        // prefer_translated_lang = None must leave every snippet
        // untouched.  Critical: 99 % of search calls don't use this
        // surface, and an unconditional swap would break them.
        let mut rows = vec![
            mk_result("bosnian original", Some("english translation"), Some("en")),
            mk_result("german original", None, None),
        ];
        SearchEngine::apply_translation_snippet(&mut rows, None);
        assert_eq!(rows[0].snippet, "bosnian original");
        assert_eq!(rows[1].snippet, "german original");
    }

    #[test]
    fn translation_snippet_swap_replaces_when_lang_matches() {
        let mut rows = vec![mk_result(
            "Bok, kako si?",
            Some("Hello, how are you?"),
            Some("en"),
        )];
        SearchEngine::apply_translation_snippet(&mut rows, Some("en"));
        assert_eq!(rows[0].snippet, "Hello, how are you?");
    }

    #[test]
    fn translation_snippet_swap_skips_rows_with_wrong_target_lang() {
        // Mixed result set: one row was translated to French, another
        // to English.  Filter says "en" — only the EN row swaps; the
        // FR row keeps its original snippet (downstream caller can
        // decide whether to surface a "no English translation
        // available" badge for it).
        let mut rows = vec![
            mk_result("le texte", Some("Le texte traduit"), Some("fr")),
            mk_result("der text", Some("The translated text"), Some("en")),
        ];
        SearchEngine::apply_translation_snippet(&mut rows, Some("en"));
        assert_eq!(rows[0].snippet, "le texte", "fr row must NOT swap when filter is en");
        assert_eq!(rows[1].snippet, "The translated text");
    }

    #[test]
    fn translation_snippet_swap_skips_rows_without_translation() {
        // Row matches the target lang on the column but
        // text_translated is None (corrupt / partial backfill).
        // Filter SQL guards against this with `IS NOT NULL` but the
        // helper defends in depth too.
        let mut rows = vec![mk_result("original", None, Some("en"))];
        SearchEngine::apply_translation_snippet(&mut rows, Some("en"));
        assert_eq!(rows[0].snippet, "original");
    }

    #[test]
    fn translation_snippet_swap_truncates_to_400_chars() {
        // Big translations get the same 400-char preview window
        // the original snippet uses.  Pinned so a future "store
        // the full translation in snippet" mistake gets caught.
        let long: String = "x".repeat(800);
        let mut rows = vec![mk_result("short", Some(&long), Some("en"))];
        SearchEngine::apply_translation_snippet(&mut rows, Some("en"));
        assert_eq!(rows[0].snippet.chars().count(), 400);
    }

    // ── has_nonlatin_script ────────────────────────────────────────────────

    #[test]
    fn nonlatin_japanese_is_detected() {
        // "Tokyo conference" in Japanese (kanji + kana)
        assert!(has_nonlatin_script("東京会議"));
    }

    #[test]
    fn nonlatin_arabic_is_detected() {
        assert!(has_nonlatin_script("مرحبا بالعالم"));
    }

    #[test]
    fn nonlatin_cyrillic_is_detected() {
        assert!(has_nonlatin_script("привет мир"));
    }

    #[test]
    fn nonlatin_mixed_latin_majority_is_not_detected() {
        // 5 Japanese out of 22 total non-whitespace chars ≈ 23% < 25% threshold
        assert!(!has_nonlatin_script("hello world foobar こんにちは"));
    }

    #[test]
    fn nonlatin_pure_ascii_query_is_not_detected() {
        assert!(!has_nonlatin_script("search the documents"));
    }

    #[test]
    fn nonlatin_short_query_is_not_detected() {
        // 3 non-whitespace chars (< 4 threshold)
        assert!(!has_nonlatin_script("你好"));
    }

    #[test]
    fn nonlatin_german_with_umlauts_is_not_detected() {
        // Umlauts (ä/ö/ü) are in Latin Extended block → not non-Latin.
        assert!(!has_nonlatin_script("Universität für Wissenschaft"));
    }

    #[test]
    fn nonlatin_numeric_only_is_not_detected() {
        assert!(!has_nonlatin_script("2024 1234"));
    }

    #[test]
    fn nonlatin_chinese_query_is_detected() {
        assert!(has_nonlatin_script("深度学习模型评估"));
    }

    // ── ColBERT MaxSim ──────────────────────────────────────────────────────

    #[test]
    fn maxsim_identical_single_vector_scores_one() {
        let v = vec![vec![1.0_f32, 0.0, 0.0]];
        let score = maxsim(&v, &v);
        assert!((score - 1.0).abs() < 1e-5, "score={score}");
    }

    #[test]
    fn maxsim_orthogonal_vectors_score_zero() {
        let q = vec![vec![1.0_f32, 0.0]];
        let d = vec![vec![0.0_f32, 1.0]];
        let score = maxsim(&q, &d);
        assert!(score.abs() < 1e-5, "score={score}");
    }

    #[test]
    fn maxsim_empty_inputs_return_zero() {
        assert_eq!(maxsim(&[], &[vec![1.0]]), 0.0);
        assert_eq!(maxsim(&[vec![1.0]], &[]), 0.0);
    }

    #[test]
    fn unpack_multivec_round_trips_pack() {
        // Build a small 2-token × 3-dim matrix of known values.
        let vecs: Vec<Vec<f32>> = vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]];
        let mut packed = Vec::new();
        for v in &vecs {
            for &f in v {
                packed.extend_from_slice(&f.to_le_bytes());
            }
        }
        let unpacked = unpack_multivec(&packed, 2, 3);
        assert_eq!(unpacked.len(), 2);
        assert_eq!(unpacked[0], vecs[0]);
        assert_eq!(unpacked[1], vecs[1]);
    }

    #[test]
    fn unpack_multivec_truncated_returns_empty() {
        // 5 bytes can't hold even one full float → empty
        assert!(unpack_multivec(&[0u8; 5], 1, 3).is_empty());
    }
}
