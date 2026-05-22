use super::*;

fn make_store() -> (BatchSessionStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let s = BatchSessionStore::open_or_create(dir.path()).unwrap();
    (s, dir)
}

fn sample_item(id: &str) -> BatchItemRow {
    BatchItemRow {
        id: id.to_owned(),
        original_path: format!("/docs/{id}.pdf"),
        original_name: format!("{id}.pdf"),
        extension: "pdf".to_owned(),
        size_bytes: 12345,
        modified_at: 1_700_000_000_000,
        status: "queued".to_owned(),
        error_message: None,
        status_detail: None,
        detected_language: None,
        audio_duration_seconds: None,
        audio_codec: None,
        audio_sample_rate_hz: None,
        audio_channels: None,
        audio_bitrate_kbps: None,
        metadata_read_status: None,
        suggested_title: None,
        suggested_author: None,
        suggested_year: None,
        target_path: None,
        is_accepted: false,
        is_ignored: None,
        duplicate_group_id: None,
        is_duplicate_primary: None,
        chapter_group_id: None,
        chapter_suffix: None,
        is_chapter_representative: None,
        chapter_group_size: None,
        chapter_is_edited_volume: None,
        extracted_text_preview: None,
    }
}

#[test]
fn bulk_upsert_large_set() {
    let (store, _dir) = make_store();
    let items: Vec<BatchItemRow> = (0..2000).map(|i| sample_item(&format!("large-{i:04}"))).collect();
    store.upsert_items_bulk(&items).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 2000);
    assert_eq!(loaded[1999].id, "large-1999");
}

#[test]
fn bulk_delete_large_set() {
    let (store, _dir) = make_store();
    let items: Vec<BatchItemRow> = (0..1000).map(|i| sample_item(&format!("del-large-{i:04}"))).collect();
    store.upsert_items_bulk(&items).unwrap();
    
    let ids_to_delete: Vec<String> = (0..500).map(|i| format!("del-large-{i:04}")).collect();
    store.delete_items(&ids_to_delete).unwrap();
    
    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 500);
    assert!(!loaded.iter().any(|i| i.id == "del-large-0000"));
}

#[test]
fn interleaved_upsert_and_clear() {
    let (store, _dir) = make_store();
    let items1: Vec<BatchItemRow> = (0..100).map(|i| sample_item(&format!("batch1-{i:03}"))).collect();
    store.upsert_items_bulk(&items1).unwrap();
    store.clear().unwrap();
    let items2: Vec<BatchItemRow> = (0..50).map(|i| sample_item(&format!("batch2-{i:03}"))).collect();
    store.upsert_items_bulk(&items2).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 50);
}
