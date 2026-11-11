#![allow(dead_code)]
#[path = "../../crisp-index-server/src/fts.rs"]
mod fts;
#[path = "../../crisp-index-server/src/index.rs"]
mod index;
#[path = "../../crisp-index-server/src/auth.rs"]
mod auth;
#[path = "../../shared/source_paths.rs"]
mod source_paths;
#[cfg(test)]
#[path = "../../crisp-index-protocol/src/lib.rs"]
mod protocol_contract;

mod desktop;
#[path = "adapters.rs"]
mod sync;
pub mod embedder {
    pub static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    #[derive(Clone)]
    pub struct ServerEmbedder;
    impl ServerEmbedder {
        pub async fn embed_query(&self, _: &str) -> anyhow::Result<Vec<f32>> {
            CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("Model loading is outside this contract")
        }
        pub async fn hydrate_missing_embeddings(
            &self,
            _: &mut crisp_index_protocol::IngestBatch,
        ) -> anyhow::Result<()> {
            anyhow::bail!("Model loading is outside this contract")
        }
    }
}
#[cfg(test)]
mod http_contract;
#[cfg(test)]
mod remote_client_complete_contract;
#[cfg(test)]
mod remote_client_member_contract;
#[cfg(test)]
mod retrieval_benchmark;
#[cfg(test)]
mod complete_contract;
#[cfg(test)]
mod container_contract;
#[cfg(test)]
mod passage_contract;
#[cfg(test)]
mod explanation_contract;
#[path = "../../crisp-index-server/src/queue.rs"]
mod queue;
#[path = "../../crisp-index-server/src/routes/mod.rs"]
mod routes;
#[path = "../../crisp-index-server/src/state.rs"]
mod state;
#[cfg(test)]
mod sync_contract;

#[cfg(test)]
mod tests {
    use crate::fts::{DocumentKey, FtsDocument, FtsStore, StoredChunk};
    use crisp_index_protocol::SearchFilters;

    fn key(owner: &str, id: &str) -> DocumentKey {
        DocumentKey {
            owner_id: owner.into(),
            doc_id: id.into(),
        }
    }
    fn chunk(owner: &str, id: &str, index: i32, text: &str, start: Option<i32>) -> StoredChunk {
        StoredChunk {
            key: key(owner, id),
            chunk_index: index,
            indexed_at: 0,
            row_order: 0,
            title: format!("Title {id}"),
            language: "en".into(),
            year: Some(2024),
            headings: "Report heading".into(),
            text: text.into(), translated: None,
            start,
            end: start.map(|s| s + text.len() as i32),
        }
    }
    fn document(chunks: Vec<StoredChunk>) -> FtsDocument {
        FtsDocument::from_chunks(chunks).unwrap().unwrap()
    }
    fn publish(store: &FtsStore, docs: Vec<FtsDocument>) {
        let mut builder = store.begin_rebuild().unwrap();
        for doc in &docs {
            builder.add(doc).unwrap();
        }
        store.publish_rebuild(builder).unwrap();
    }
    fn ids(store: &FtsStore, query: &str, filters: &SearchFilters) -> Vec<DocumentKey> {
        let mut ids: Vec<_> = store
            .prepare(query, filters)
            .unwrap()
            .hits(100)
            .unwrap()
            .into_iter()
            .map(|hit| hit.key)
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn migration_publishes_only_complete_generation_and_preserves_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("fts");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("sentinel"), "prior generation").unwrap();
        let store = FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap();
        assert!(store.needs_rebuild().unwrap());
        let abandoned = store.begin_rebuild().unwrap();
        drop(abandoned);
        assert!(store.needs_rebuild().unwrap());
        assert!(!store.capabilities().unwrap().fts_ready);
        publish(
            &store,
            vec![document(vec![chunk("a", "one", 0, "alpha", None)])],
        );
        let abandoned = store.begin_rebuild().unwrap();
        drop(abandoned);
        assert_eq!(
            ids(&store, "alpha", &Default::default()),
            vec![key("a", "one")]
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("sentinel")).unwrap(),
            "prior generation"
        );
        drop(store);
        let reopened = FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap();
        assert_eq!(
            ids(&reopened, "alpha", &Default::default()),
            vec![key("a", "one")]
        );
    }

    #[test]
    fn all_chunks_are_searchable_but_unknown_boundaries_do_not_invent_phrases() {
        let dir = tempfile::tempdir().unwrap();
        let store = FtsStore::open_or_create(dir.path()).unwrap();
        publish(
            &store,
            vec![document(vec![
                chunk("a", "legacy", 0, "start alpha", None),
                chunk("a", "legacy", 1, "beta conclusion", None),
            ])],
        );
        assert_eq!(
            ids(&store, "conclusion", &Default::default()),
            vec![key("a", "legacy")]
        );
        assert_eq!(ids(&store, "alpha AND beta", &Default::default()).len(), 1);
        assert!(ids(&store, "\"alpha beta\"", &Default::default()).is_empty());
        assert!(ids(&store, "alpha w/1024 beta", &Default::default()).is_empty());
        assert_eq!(
            ids(&store, "beta pre/0 conclusion", &Default::default()).len(),
            1
        );
    }

    #[test]
    fn verified_overlaps_preserve_exact_original_phrase_positions() {
        let dir = tempfile::tempdir().unwrap();
        let store = FtsStore::open_or_create(dir.path()).unwrap();
        let doc = document(vec![
            chunk("a", "overlap", 0, "alpha beta", Some(0)),
            chunk("a", "overlap", 1, "beta gamma", Some(6)),
        ]);
        assert_eq!(doc.body_segments, vec!["alpha beta gamma"]);
        publish(&store, vec![doc]);
        assert_eq!(
            ids(&store, "\"alpha beta gamma\"", &Default::default()).len(),
            1
        );
        assert!(ids(&store, "gamma pre/2 alpha", &Default::default()).is_empty());
        assert_eq!(
            ids(&store, "alpha pre/1 gamma", &Default::default()).len(),
            1
        );
    }

    #[test]
    fn corrupt_overlap_and_utf8_offsets_fail_before_publication() {
        assert!(FtsDocument::from_chunks(vec![
            chunk("a", "bad", 0, "alpha beta", Some(0)),
            chunk("a", "bad", 1, "delta gamma", Some(6))
        ])
        .is_err());
        let mut bad = chunk("a", "utf8", 0, "café", Some(0));
        bad.end = Some(4);
        assert!(FtsDocument::from_chunks(vec![bad]).is_err());
        assert!(FtsDocument::from_chunks(vec![
            chunk("a", "one", 0, "text", None),
            chunk("b", "two", 1, "text", None)
        ])
        .is_err());
    }

    #[test]
    fn owner_language_and_year_apply_before_candidate_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = FtsStore::open_or_create(dir.path()).unwrap();
        let a = document(vec![chunk("a", "shared", 0, "alpha", None)]);
        let mut b = document(vec![chunk("b", "shared", 0, "alpha alpha alpha", None)]);
        b.language = "de".into();
        b.year = Some(2020);
        publish(&store, vec![a, b]);
        let filters = SearchFilters {
            owner_id: Some("a".into()),
            language: Some("en".into()),
            year_min: Some(2024),
            year_max: Some(2024),
            ..Default::default()
        };
        assert_eq!(
            store.prepare("alpha", &filters).unwrap().hits(1).unwrap()[0].key,
            key("a", "shared")
        );
        let invalid = SearchFilters {
            year_min: Some(2025),
            year_max: Some(2020),
            ..Default::default()
        };
        assert!(store.prepare("alpha", &invalid).is_err());
    }

    #[test]
    fn native_field_boolean_and_unicode_grammar_runs_on_server_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = FtsStore::open_or_create(dir.path()).unwrap();
        let mut yes = document(vec![chunk(
            "a",
            "one",
            0,
            "İstanbul Straße café 北京 東京",
            None,
        )]);
        yes.title = "Annual Report".into();
        let no = document(vec![chunk("a", "two", 0, "İstanbul excluded", None)]);
        publish(&store, vec![yes, no]);
        for query in [
            "title:\"annual report\"",
            "İstanbul AND NOT excluded",
            "STRASSE",
            "cafe",
            "北京",
            "東京",
        ] {
            assert_eq!(
                ids(&store, query, &Default::default()),
                vec![key("a", "one")],
                "{query}"
            );
        }
        for query in [
            "alpha OR",
            "\"unfinished",
            "title:",
            "alpha) beta",
            "unknown:foo",
        ] {
            assert!(
                store.prepare(query, &Default::default()).is_err(),
                "{query}"
            );
        }
    }

    #[test]
    fn durable_pending_repairs_gate_reads_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = FtsStore::open_or_create(dir.path()).unwrap();
        publish(
            &store,
            vec![document(vec![chunk("a", "one", 0, "before", None)])],
        );
        store.mark_pending(&[key("a", "one")]).unwrap();
        assert!(!store.capabilities().unwrap().fts_ready);
        assert!(store.prepare("before", &Default::default()).is_err());
        drop(store);
        let store = FtsStore::open_or_create(dir.path()).unwrap();
        assert_eq!(store.pending().unwrap(), vec![key("a", "one")]);
        store
            .finish_repairs(&[(
                key("a", "one"),
                Some(document(vec![chunk("a", "one", 0, "after", None)])),
            )])
            .unwrap();
        assert!(store.pending().unwrap().is_empty());
        assert!(ids(&store, "before", &Default::default()).is_empty());
        assert_eq!(ids(&store, "after", &Default::default()).len(), 1);
        store.mark_pending(&[key("a", "one")]).unwrap();
        store.finish_repairs(&[(key("a", "one"), None)]).unwrap();
        assert!(ids(&store, "after", &Default::default()).is_empty());
    }

    #[test]
    fn prepared_hybrid_gate_keeps_owner_and_query_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = FtsStore::open_or_create(dir.path()).unwrap();
        publish(
            &store,
            vec![
                document(vec![chunk("a", "yes", 0, "alpha", None)]),
                document(vec![chunk("a", "no", 0, "alpha excluded", None)]),
                document(vec![chunk("b", "yes", 0, "alpha", None)]),
            ],
        );
        let prepared = store
            .prepare(
                "alpha AND NOT excluded",
                &SearchFilters {
                    owner_id: Some("a".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let candidates = vec![key("a", "yes"), key("a", "no"), key("b", "yes")];
        assert_eq!(
            prepared.matching_keys(&candidates).unwrap(),
            [key("a", "yes")].into_iter().collect()
        );
        store.mark_pending(&[key("a", "yes")]).unwrap();
        store.finish_repairs(&[(key("a", "yes"), None)]).unwrap();
        assert_eq!(prepared.matching_keys(&candidates).unwrap().len(), 1);
        assert!(store
            .prepare(
                "alpha AND NOT excluded",
                &SearchFilters {
                    owner_id: Some("a".into()),
                    ..Default::default()
                }
            )
            .unwrap()
            .matching_keys(&candidates)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn concurrent_server_process_lease_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = FtsStore::open_or_create(dir.path()).unwrap();
        assert!(FtsStore::open_or_create(dir.path()).is_err());
        drop(store);
        assert!(FtsStore::open_or_create(dir.path()).is_ok());
    }

    #[test]
    fn legacy_duplicate_chunk_uses_latest_content_once() {
        let old = chunk("a", "one", 0, "outdated", None);
        let mut new = old.clone();
        new.text = "current".into();
        new.indexed_at = 1;
        assert_eq!(document(vec![new, old]).body_segments, vec!["current"]);
    }
}

#[path = "../../shared/source_identity.rs"]
mod source_identity;
