//! Legacy full-text upgrade contracts against the real migration, Lance rows,
//! Tantivy commits and recovery directories. No desktop or model adapters run.
use crate::index::{
    fts_index::{FtsIndex, TantivyInput},
    ingest::chunk_row_id,
    migrations::RebuildFtsForBodyTranslated,
    schema::{DocumentChunk, SearchFilters},
    LocalIndex,
};
use crate::migrations::{Migration, MigrationContext};
use std::{path::Path, sync::Arc};

fn row(doc: &str, owner: &str, index: i32, text: &str) -> DocumentChunk {
    serde_json::from_value(serde_json::json!({
        "id": chunk_row_id(doc, index), "doc_id": doc, "owner_id": owner,
        "location_uri": format!("file:///{owner}/{doc}.txt"), "filename": format!("{doc}.txt"),
        "full_text": text, "chunk_index": index, "chunk_total": index + 1,
        "source_hash": doc, "indexed_at": 1, "tags": [],
    })).unwrap()
}

fn seed_fts(path: &Path, legacy: bool, text: &str) {
    if legacy {
        use tantivy::schema::{IndexRecordOption, SchemaBuilder, TextFieldIndexing, TextOptions, STORED, STRING};
        std::fs::create_dir_all(path).unwrap();
        let mut schema = SchemaBuilder::new();
        for name in ["doc_id", "owner_id", "language"] { schema.add_text_field(name, STRING | STORED); }
        let positional = TextOptions::default().set_indexing_options(TextFieldIndexing::default()
            .set_tokenizer("ascii_folding").set_index_option(IndexRecordOption::WithFreqsAndPositions));
        for name in ["title", "headings", "body"] { schema.add_text_field(name, positional.clone()); }
        drop(tantivy::Index::create_in_dir(path, schema.build()).unwrap());
    }
    let fts = FtsIndex::open_or_create(path).unwrap();
    assert_eq!(fts.fields.body_translated.is_none(), legacy);
    let mut writer = fts.writer().unwrap();
    fts.add_document(&mut writer, TantivyInput {
        doc_id: "old-display", owner_id: "owner", language: "en", title: "", headings: "",
        body: text, body_translated: None,
    }).unwrap();
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
}

async fn fixture(rows: &[DocumentChunk]) -> (tempfile::TempDir, Arc<LocalIndex>, MigrationContext) {
    let dir = tempfile::tempdir().unwrap();
    seed_fts(&dir.path().join("fts"), true, "previouskeeper");
    let local = Arc::new(LocalIndex::open_or_create(dir.path(), 4).await.unwrap());
    local.append_legacy_rows_for_test(rows).await.unwrap();
    let ctx = MigrationContext { lance: Some(local.clone()), sqlite: None, data_dir: dir.path().to_owned() };
    (dir, local, ctx)
}

fn assert_clean(dir: &Path) {
    assert!(dir.join("fts/.v103_done").exists());
    for name in ["fts.v103-staging", "fts.v103-backup", ".v103_in_progress"] {
        assert!(!dir.join(name).exists(), "unfinished recovery artifact: {name}");
    }
}

#[tokio::test]
async fn v103_rebuild_preserves_late_sparse_and_contiguous_text_without_crossing_gaps_or_owners() {
    let mut first = row("contiguous", "owner", 0, "start overlap");
    first.chunk_start_char = Some(0); first.chunk_end_char = Some(13);
    let mut second = row("contiguous", "owner", 1, "overlap finish");
    second.chunk_start_char = Some(6); second.chunk_end_char = Some(20);
    let mut late = row("late", "owner", 1, "lateneedle finalpage");
    late.indexed_at = 2;
    late.headings_text = Some("lateheading".into());
    late.text_translated = Some("translatedneedle".into());
    let rows = vec![
        row("late", "owner", 0, "introduction"), row("late", "owner", 1, "discardedrevision"), late,
        row("gap", "owner", 0, "alpha"), row("gap", "owner", 1, "beta"), first, second,
        row("collision", "first", 0, "ownerfirst"), row("collision", "second", 0, "ownersecond"),
        row("sparse", "owner", 7, "sparseneedle"),
    ];
    let (dir, _, ctx) = fixture(&rows).await;
    RebuildFtsForBodyTranslated.apply(&ctx).await.unwrap();
    let fts = FtsIndex::open_or_create(&dir.path().join("fts")).unwrap();
    assert!(fts.fields.body_translated.is_some());
    assert_eq!(fts.doc_count(), 6);
    for term in ["lateneedle", "lateheading", "translatedneedle", "sparseneedle", "alpha", "beta", "\"start overlap finish\""] {
        assert_eq!(fts.search(term, &SearchFilters::default(), 20).unwrap().len(), 1, "missing {term}");
    }
    for term in ["previouskeeper", "discardedrevision", "\"alpha beta\""] {
        assert!(fts.search(term, &SearchFilters::default(), 20).unwrap().is_empty(), "unexpected {term}");
    }
    for (owner, term, forbidden) in [("first", "ownerfirst", "ownersecond"), ("second", "ownersecond", "ownerfirst")] {
        let filters = SearchFilters { owner_id: Some(owner.into()), ..Default::default() };
        let hits = fts.search(term, &filters, 20).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].owner_id, owner);
        assert!(fts.search(forbidden, &filters, 20).unwrap().is_empty());
    }
    assert_clean(dir.path());
}

#[tokio::test]
async fn v103_failed_build_retains_previous_index_and_retry_rebuilds_new_schema_staging() {
    let mut invalid = row("z-invalid", "owner", 0, "invalidrange");
    invalid.chunk_start_char = Some(0); invalid.chunk_end_char = Some(500);
    let (dir, local, ctx) = fixture(&[row("a-valid", "owner", 0, "validneedle"), invalid]).await;
    let error = RebuildFtsForBodyTranslated.apply(&ctx).await.unwrap_err();
    assert!(format!("{error:#}").contains("offsets"));
    assert!(!dir.path().join("fts/.v103_done").exists());
    assert!(dir.path().join(".v103_in_progress").exists());
    let old = FtsIndex::open_or_create(&dir.path().join("fts")).unwrap();
    assert!(old.fields.body_translated.is_none());
    assert_eq!(old.search("previouskeeper", &SearchFilters::default(), 20).unwrap().len(), 1);
    drop(old);
    // This is the old failure trap: staging already has the new field, but no
    // completion receipt. Its schema must not suppress the retry.
    let staged = FtsIndex::open_or_create(&dir.path().join("fts.v103-staging")).unwrap();
    assert!(staged.fields.body_translated.is_some());
    drop(staged);
    let mut repaired = row("z-invalid", "owner", 0, "repairedneedle");
    repaired.indexed_at = 3;
    local.append_legacy_rows_for_test(&[repaired]).await.unwrap();
    RebuildFtsForBodyTranslated.apply(&ctx).await.unwrap();
    let fts = FtsIndex::open_or_create(&dir.path().join("fts")).unwrap();
    assert_eq!(fts.doc_count(), 2);
    assert_eq!(fts.search("repairedneedle", &SearchFilters::default(), 20).unwrap().len(), 1);
    assert_clean(dir.path());
}

#[tokio::test]
async fn v103_preexisting_staging_prevents_a_partial_current_schema_from_being_accepted() {
    let (dir, _, ctx) = fixture(&[row("late", "owner", 8, "recoverneedle")]).await;
    std::fs::remove_dir_all(dir.path().join("fts")).unwrap();
    seed_fts(&dir.path().join("fts"), false, "partialactive");
    seed_fts(&dir.path().join("fts.v103-staging"), false, "partialstaging");
    // Staging itself is recovery evidence even if the intent file was lost.
    RebuildFtsForBodyTranslated.apply(&ctx).await.unwrap();
    let fts = FtsIndex::open_or_create(&dir.path().join("fts")).unwrap();
    assert_eq!(fts.search("recoverneedle", &SearchFilters::default(), 20).unwrap().len(), 1);
    assert!(fts.search("partialactive OR partialstaging", &SearchFilters::default(), 20).unwrap().is_empty());
    assert_clean(dir.path());
}

#[tokio::test]
async fn v103_displaced_previous_index_is_restored_even_when_the_retry_fails() {
    let mut invalid = row("bad", "owner", 0, "invalidrange");
    invalid.chunk_start_char = Some(0); invalid.chunk_end_char = Some(500);
    let (dir, _, ctx) = fixture(&[invalid]).await;
    std::fs::rename(dir.path().join("fts"), dir.path().join("fts.v103-backup")).unwrap();
    seed_fts(&dir.path().join("fts.v103-staging"), false, "partialstaging");
    std::fs::write(dir.path().join(".v103_in_progress"), b"103\n").unwrap();
    assert!(RebuildFtsForBodyTranslated.apply(&ctx).await.is_err());
    let restored = FtsIndex::open_or_create(&dir.path().join("fts")).unwrap();
    assert!(restored.fields.body_translated.is_none());
    assert_eq!(restored.search("previouskeeper", &SearchFilters::default(), 20).unwrap().len(), 1);
    assert!(!dir.path().join("fts/.v103_done").exists());
    assert!(!dir.path().join("fts.v103-backup").exists());
}

#[tokio::test]
async fn v103_published_receipt_finishes_cleanup_without_rebuilding() {
    let dir = tempfile::tempdir().unwrap();
    seed_fts(&dir.path().join("fts.v103-backup"), true, "previouskeeper");
    seed_fts(&dir.path().join("fts.v103-staging"), false, "discardstaging");
    seed_fts(&dir.path().join("fts"), false, "committedneedle");
    std::fs::write(dir.path().join("fts/.v103_done"), b"103\n").unwrap();
    std::fs::write(dir.path().join(".v103_in_progress"), b"103\n").unwrap();
    let ctx = MigrationContext { lance: None, sqlite: None, data_dir: dir.path().to_owned() };
    RebuildFtsForBodyTranslated.apply(&ctx).await.unwrap();
    let fts = FtsIndex::open_or_create(&dir.path().join("fts")).unwrap();
    assert_eq!(fts.search("committedneedle", &SearchFilters::default(), 20).unwrap().len(), 1);
    assert_clean(dir.path());
}

#[tokio::test]
async fn v103_missing_or_empty_active_directory_recovers_existing_stored_text() {
    for leave_empty_directory in [false, true] {
        let (dir, _, ctx) = fixture(&[row("late", "owner", 9, "recovermissingindex")]).await;
        std::fs::remove_dir_all(dir.path().join("fts")).unwrap();
        if leave_empty_directory { std::fs::create_dir(dir.path().join("fts")).unwrap(); }
        RebuildFtsForBodyTranslated.apply(&ctx).await.unwrap();
        let fts = FtsIndex::open_or_create(&dir.path().join("fts")).unwrap();
        assert_eq!(fts.search("recovermissingindex", &SearchFilters::default(), 20).unwrap().len(), 1);
        assert_clean(dir.path());
    }
}
