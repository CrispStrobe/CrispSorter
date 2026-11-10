use super::*;

fn make_store() -> (BatchSessionStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (BatchSessionStore::open_or_create(dir.path()).unwrap(), dir)
}

fn item(id: &str) -> BatchItemRow {
    serde_json::from_value(serde_json::json!({
        "id": id, "originalPath": format!("/docs/{id}.pdf"), "originalName": format!("{id}.pdf"),
        "extension": "pdf", "size": 42, "modifiedAt": 100, "status": "review", "isAccepted": false
    })).unwrap()
}

fn replacement(item: BatchItemRow, text: Option<&str>) -> BatchReplacementItem {
    BatchReplacementItem { item, extracted_text: text.map(str::to_owned) }
}

#[test]
fn replacement_retains_text_for_same_source_even_with_new_id() {
    let (store, _dir) = make_store();
    let old = item("retained");
    store.upsert_items_bulk(&[old.clone(), item("removed")]).unwrap();
    store.set_extracted_text("retained", "Complete retained text").unwrap();
    store.set_extracted_text("removed", "Removed text").unwrap();
    let mut renamed = old;
    renamed.id = "new-id".to_owned();
    store.replace(&[
        replacement(renamed, None),
        replacement(item("imported"), Some("Imported full text")),
    ]).unwrap();
    let rows = store.load().unwrap();
    assert_eq!(rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["new-id", "imported"]);
    assert_eq!(rows[0].extracted_text_preview.as_deref(), Some("Complete retained text"));
    assert_eq!(store.get_extracted_text("new-id").unwrap().as_deref(), Some("Complete retained text"));
    assert_eq!(store.get_extracted_text("imported").unwrap().as_deref(), Some("Imported full text"));
    assert!(store.get_extracted_text("retained").unwrap().is_none());
    assert!(store.get_extracted_text("removed").unwrap().is_none());
}

#[test]
fn supplied_empty_text_wins_and_unicode_previews_are_bounded() {
    let (store, _dir) = make_store();
    store.upsert_item(&item("kept")).unwrap();
    store.set_extracted_text("kept", "Old text").unwrap();
    let full_text = "🦀".repeat(700);
    store.replace(&[
        replacement(item("kept"), Some("")),
        replacement(item("unicode"), Some(&full_text)),
    ]).unwrap();
    assert_eq!(store.get_extracted_text("kept").unwrap().as_deref(), Some(""));
    let rows = store.load().unwrap();
    assert_eq!(rows[1].extracted_text_preview.as_ref().unwrap().chars().count(), 500);
    assert_eq!(store.get_extracted_text("unicode").unwrap(), Some(full_text));
}

#[test]
fn reused_id_or_changed_source_revision_does_not_inherit_text() {
    let (store, _dir) = make_store();
    store.upsert_items_bulk(&[item("changed-path"), item("changed-time")]).unwrap();
    store.set_extracted_text("changed-path", "Different document").unwrap();
    store.set_extracted_text("changed-time", "Earlier document revision").unwrap();
    let mut changed_path = item("changed-path");
    changed_path.original_path = "/other/document.pdf".to_owned();
    let mut changed_time = item("changed-time");
    changed_time.modified_at += 1;
    store.replace(&[replacement(changed_path, None), replacement(changed_time, None)]).unwrap();
    assert!(store.get_extracted_text("changed-path").unwrap().is_none());
    assert!(store.get_extracted_text("changed-time").unwrap().is_none());
}

#[test]
fn case_distinct_source_path_does_not_inherit_text() {
    let (store, _dir) = make_store();
    store.upsert_item(&item("same-id")).unwrap();
    store.set_extracted_text("same-id", "Text belonging to lower-case path").unwrap();
    let mut other_source = item("same-id");
    other_source.original_path = "/docs/Same-id.pdf".to_owned();
    store.replace(&[replacement(other_source, None)]).unwrap();
    assert!(store.get_extracted_text("same-id").unwrap().is_none());
}

#[test]
fn failed_insert_rolls_back_rows_text_and_staging_then_retry_succeeds() {
    let (store, _dir) = make_store();
    store.upsert_item(&item("current")).unwrap();
    store.set_extracted_text("current", "Do not lose this text").unwrap();
    store.conn.lock().unwrap().execute_batch(
        "CREATE TRIGGER reject_replacement BEFORE INSERT ON batch_items
         WHEN NEW.id = 'blocked' BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END",
    ).unwrap();
    let replacements = [replacement(item("first"), Some("First new text")), replacement(item("blocked"), None)];
    assert!(store.replace(&replacements).is_err());
    assert_eq!(store.load().unwrap().iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["current"]);
    assert_eq!(store.get_extracted_text("current").unwrap().as_deref(), Some("Do not lose this text"));
    assert!(store.get_extracted_text("first").unwrap().is_none());
    store.conn.lock().unwrap().execute_batch("DROP TRIGGER reject_replacement").unwrap();
    store.replace(&replacements).unwrap();
    assert_eq!(store.load().unwrap().len(), 2);
}

#[test]
fn invalid_replacements_leave_current_batch_untouched() {
    let (store, _dir) = make_store();
    store.upsert_item(&item("current")).unwrap();
    store.set_extracted_text("current", "Preserved").unwrap();
    let duplicate = replacement(item("duplicate"), None);
    assert!(store.replace(&[duplicate.clone(), duplicate]).is_err());
    let mut duplicate_source = item("other-id");
    duplicate_source.original_path = "/DOCS/first.pdf".to_owned();
    assert!(store.replace(&[replacement(item("first"), None), replacement(duplicate_source, None)]).is_err());
    let mut invalid_status = item("invalid");
    invalid_status.status = "unknown".to_owned();
    assert!(store.replace(&[replacement(invalid_status, None)]).is_err());
    assert_eq!(store.load().unwrap()[0].id, "current");
    assert_eq!(store.get_extracted_text("current").unwrap().as_deref(), Some("Preserved"));
}

#[test]
fn provider_ids_are_case_sensitive_and_empty_replacement_is_explicit() {
    let (store, _dir) = make_store();
    let mut first = item("first");
    first.original_path = "content://documents/A".to_owned();
    let mut second = item("second");
    second.original_path = "content://documents/a".to_owned();
    store.replace(&[replacement(first, Some("First")), replacement(second, Some("Second"))]).unwrap();
    assert_eq!(store.load().unwrap().len(), 2);
    store.replace(&[]).unwrap();
    assert!(store.load().unwrap().is_empty());
    assert!(store.get_extracted_text("first").unwrap().is_none());
}
