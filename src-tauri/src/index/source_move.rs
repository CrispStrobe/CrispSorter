// Local file moves retain source identities, owners, and review evidence.
// Only URI forms emitted by local ingest are eligible for path-based updates.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const MAX_LOCATION_ROWS: usize = 65_536;
const MAX_LOCATION_SOURCES: usize = 256;
const MAX_LOCATION_METADATA_BYTES: usize = 8 * 1024 * 1024;

fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn uri_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if normalized.starts_with('/') {
        normalized
    } else {
        format!("/{normalized}")
    }
}

fn relocated_local_uri(stored: &str, old_path: &str, new_path: &str) -> Option<String> {
    if stored == old_path {
        return Some(new_path.to_owned());
    }
    #[cfg(windows)]
    if stored == old_path.replace('\\', "/") {
        return Some(new_path.replace('\\', "/"));
    }
    // Older path adapters inserted an extra slash before an absolute path.
    if stored == format!("crisp+local://local/{old_path}") {
        return Some(format!("crisp+local://local/{new_path}"));
    }
    let rest = stored.strip_prefix("crisp+local://")?;
    let (authority, path) = rest.split_once('/')?;
    if format!("/{path}") != uri_path(old_path) {
        return None;
    }
    if authority != "local" {
        let (user, machine) = authority.split_once('@')?;
        uuid::Uuid::parse_str(user).ok()?;
        // Current local producers use the nil machine ID. A different machine
        // may have the same path, but a local filesystem move is not its move.
        if !uuid::Uuid::parse_str(machine).ok()?.is_nil() {
            return None;
        }
    }
    Some(format!("crisp+local://{authority}{}", uri_path(new_path)))
}

fn move_timestamp(previous: i64) -> Result<i64> {
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    Ok(now.max(
        previous
            .checked_add(1)
            .ok_or_else(|| anyhow!("Location timestamp overflow"))?,
    ))
}

fn check_completed_move(old_path: &str, new_path: &str) -> Result<()> {
    anyhow::ensure!(!Path::new(old_path).try_exists().context("Checking the original source path")?,
        "The original path exists again; its indexed source was not reassigned. Refresh the source locations before retrying");
    anyhow::ensure!(
        std::fs::metadata(new_path)
            .context("Checking the moved source file")?
            .is_file(),
        "The move destination must be an existing file"
    );
    Ok(())
}

impl LocalIndex {
    /// Update metadata after a local file move, including bare paths and the
    /// local URI forms emitted by foreground/background ingest. Returns the
    /// number of indexed source occurrences changed; unindexed files return 0.
    pub async fn update_location_by_local_path(
        &self,
        old_path: &str,
        new_path: &str,
    ) -> Result<usize> {
        self.update_location_by_local_path_with_volume(old_path, new_path, None)
            .await
    }

    /// Supply the destination's observed volume ID when the platform can resolve
    /// it. Unknown volume identity clears the stale source volume in both storage
    /// forms, so a moved file cannot remain hidden by its former drive's state.
    pub async fn update_location_by_local_path_with_volume(
        &self,
        old_path: &str,
        new_path: &str,
        volume_id: Option<&str>,
    ) -> Result<usize> {
        self.move_local_path(old_path, new_path, volume_id, false)
            .await
    }

    /// Filesystem-aware adapter for completed sorter moves and deferred retries.
    /// Recheck while holding the writer lease, including after metadata lookup.
    pub async fn update_location_after_local_move(
        &self,
        old_path: &str,
        new_path: &str,
        volume_id: Option<&str>,
    ) -> Result<usize> {
        self.move_local_path(old_path, new_path, volume_id, true)
            .await
    }

    async fn move_local_path(
        &self,
        old_path: &str,
        new_path: &str,
        volume_id: Option<&str>,
        require_completed_move: bool,
    ) -> Result<usize> {
        anyhow::ensure!(
            Path::new(old_path).is_absolute() && Path::new(new_path).is_absolute(),
            "A source move requires absolute local paths"
        );
        anyhow::ensure!(
            !old_path.contains('\0') && !new_path.contains('\0'),
            "A source path contains a NUL character"
        );
        anyhow::ensure!(
            Path::new(new_path).file_name().is_some(),
            "The destination must name a file"
        );
        if old_path == new_path {
            return Ok(0);
        }
        let _guard = self.write_guard().await?;
        if require_completed_move {
            check_completed_move(old_path, new_path)?;
        }
        let exact = BTreeSet::from([
            old_path.to_owned(),
            format!("crisp+local://local/{old_path}"),
            format!("crisp+local://local{}", uri_path(old_path)),
        ]);
        #[cfg(windows)]
        let exact = {
            let mut exact = exact;
            exact.insert(old_path.replace('\\', "/"));
            exact
        };
        let canonical_suffix = format!("@{}{}", uuid::Uuid::nil(), uri_path(old_path));
        let filter = format!("location_uri IN ({}) OR (starts_with(location_uri, 'crisp+local://') AND ends_with(location_uri, {}))",
            exact.iter().map(|value| quoted(value)).collect::<Vec<_>>().join(","), quoted(&canonical_suffix));
        let rows = self.location_rows(&filter).await?;
        let mut updates = BTreeMap::new();
        let mut metadata = BTreeMap::new();
        let mut metadata_bytes = 0usize;
        let mut previous = 0;
        for batch in &rows {
            let ids = str_col(batch, "doc_id")?;
            let owners = str_col(batch, "owner_id")?;
            let locations = str_col(batch, "location_uri")?;
            let times = ts_ms_col_opt(batch, "indexed_at");
            let old_metadata = str_col_opt(batch, "metadata_json");
            for row in 0..batch.num_rows() {
                let Some(destination) =
                    relocated_local_uri(locations.value(row), old_path, new_path)
                else {
                    continue;
                };
                updates.insert(
                    SourceOccurrence {
                        doc_id: ids.value(row).to_owned(),
                        owner_id: owners.value(row).to_owned(),
                        location_uri: locations.value(row).to_owned(),
                    },
                    destination,
                );
                if let Some(times) = times.filter(|column| !column.is_null(row)) {
                    previous = previous.max(times.value(row));
                }
                let old_metadata = old_metadata
                    .filter(|column| !column.is_null(row))
                    .map(|column| column.value(row).to_owned());
                if !metadata.contains_key(&old_metadata) {
                    metadata_bytes = metadata_bytes
                        .checked_add(old_metadata.as_ref().map_or(0, String::len))
                        .ok_or_else(|| anyhow!("Location metadata size overflow"))?;
                    anyhow::ensure!(metadata_bytes <= MAX_LOCATION_METADATA_BYTES && metadata.len() < MAX_LOCATION_SOURCES,
                        "Too much distinct source metadata for one move; no location metadata was changed");
                    let mut value: serde_json::Value = old_metadata
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .map(serde_json::from_str)
                        .transpose()?
                        .unwrap_or_else(|| serde_json::json!({}));
                    let object = value
                        .as_object_mut()
                        .ok_or_else(|| anyhow!("Stored source metadata is not an object"))?;
                    if let Some(parent) = Path::new(new_path).parent() {
                        object.insert(
                            "parent_dir".into(),
                            parent.to_string_lossy().as_ref().into(),
                        );
                    }
                    object.remove("volume_id");
                    if let Some(volume_id) = volume_id {
                        object.insert("volume_id".into(), volume_id.into());
                    }
                    metadata.insert(old_metadata, value.to_string());
                }
            }
        }
        self.apply_location_changes(
            &rows,
            &updates,
            previous,
            Some((
                new_path,
                volume_id,
                &metadata,
                require_completed_move.then_some(old_path),
            )),
        )
        .await
    }

    async fn location_rows(&self, filter: &str) -> Result<Vec<RecordBatch>> {
        let rows: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(filter)
            .select(lancedb::query::Select::Columns(vec![
                "id".into(),
                "doc_id".into(),
                "owner_id".into(),
                "location_uri".into(),
                "indexed_at".into(),
                "metadata_json".into(),
            ]))
            .limit(MAX_LOCATION_ROWS + 1)
            .execute()
            .await?
            .try_collect()
            .await?;
        anyhow::ensure!(
            rows.iter().map(RecordBatch::num_rows).sum::<usize>() <= MAX_LOCATION_ROWS,
            "Too many indexed rows share this source location; no location metadata was changed"
        );
        Ok(rows)
    }

    /// The caller already holds the index writer lease. This helper is shared
    /// by explicit URI/document moves so every location edit advances its time.
    pub(super) async fn update_location_filter_locked(
        &self,
        filter: &str,
        new_uri: &str,
    ) -> Result<usize> {
        anyhow::ensure!(
            !new_uri.trim().is_empty(),
            "A destination location is required"
        );
        let rows = self.location_rows(filter).await?;
        let mut updates = BTreeMap::new();
        let mut previous = 0;
        for batch in &rows {
            let ids = str_col(batch, "doc_id")?;
            let owners = str_col(batch, "owner_id")?;
            let locations = str_col(batch, "location_uri")?;
            let times = ts_ms_col_opt(batch, "indexed_at");
            for row in 0..batch.num_rows() {
                if locations.value(row) == new_uri {
                    continue;
                }
                updates.insert(
                    SourceOccurrence {
                        doc_id: ids.value(row).to_owned(),
                        owner_id: owners.value(row).to_owned(),
                        location_uri: locations.value(row).to_owned(),
                    },
                    new_uri.to_owned(),
                );
                if let Some(times) = times.filter(|column| !column.is_null(row)) {
                    previous = previous.max(times.value(row));
                }
            }
        }
        self.apply_location_changes(&rows, &updates, previous, None)
            .await
    }

    async fn apply_location_changes(
        &self,
        rows: &[RecordBatch],
        updates: &BTreeMap<SourceOccurrence, String>,
        previous: i64,
        local_path: Option<(
            &str,
            Option<&str>,
            &BTreeMap<Option<String>, String>,
            Option<&str>,
        )>,
    ) -> Result<usize> {
        if updates.is_empty() {
            return Ok(0);
        }
        anyhow::ensure!(
            updates.len() <= MAX_LOCATION_SOURCES,
            "Too many indexed sources share this location; no location metadata was changed"
        );
        let mut destinations = BTreeMap::new();
        for (source, destination) in updates {
            if let Some(previous) =
                destinations.insert((&source.owner_id, destination), &source.doc_id)
            {
                anyhow::ensure!(previous == &source.doc_id,
                    "The move would combine distinct indexed source identities; refresh the selected sources first");
            }
            anyhow::ensure!(self.document_occurrences(&source.doc_id).await?.len() == 1,
                "This legacy identity represents multiple source occurrences; refresh the selected source before moving its index metadata");
            let conflicts = self
                .document_ids_for_source(&source.owner_id, destination)
                .await?;
            anyhow::ensure!(conflicts.iter().all(|id| id == &source.doc_id),
                "The destination already has another indexed identity for this owner; refresh the source selection before changing location metadata");
        }
        // A partial-column merge preserves every text/vector/evidence array that
        // is not supplied here and commits all URI variants in one Lance version.
        let mut row_ids = Vec::new();
        let mut doc_ids = Vec::new();
        let mut owner_ids = Vec::new();
        let mut locations = Vec::new();
        let mut changed_metadata = Vec::new();
        let mut seen = BTreeSet::new();
        for batch in rows {
            let ids = str_col(batch, "id")?;
            let docs = str_col(batch, "doc_id")?;
            let owners = str_col(batch, "owner_id")?;
            let uris = str_col(batch, "location_uri")?;
            let metadata = str_col_opt(batch, "metadata_json");
            for row in 0..batch.num_rows() {
                let source = SourceOccurrence {
                    doc_id: docs.value(row).to_owned(),
                    owner_id: owners.value(row).to_owned(),
                    location_uri: uris.value(row).to_owned(),
                };
                let Some(destination) = updates.get(&source) else {
                    continue;
                };
                anyhow::ensure!(seen.insert((source.doc_id.clone(), source.owner_id.clone(), ids.value(row).to_owned())),
                    "This source contains duplicate legacy chunk identities; refresh it before moving its index metadata");
                row_ids.push(ids.value(row).to_owned());
                doc_ids.push(source.doc_id);
                owner_ids.push(source.owner_id);
                locations.push(destination.clone());
                if let Some((_, _, changes, _)) = local_path {
                    let previous = metadata
                        .filter(|column| !column.is_null(row))
                        .map(|column| column.value(row).to_owned());
                    changed_metadata.push(
                        changes
                            .get(&previous)
                            .ok_or_else(|| anyhow!("Missing prepared source metadata"))?
                            .clone(),
                    );
                }
            }
        }
        let count = row_ids.len();
        let mut names = vec!["id", "doc_id", "owner_id", "location_uri", "indexed_at"];
        let mut columns: Vec<arrow_array::ArrayRef> = vec![
            Arc::new(StringArray::from(row_ids)),
            Arc::new(StringArray::from(doc_ids)),
            Arc::new(StringArray::from(owner_ids)),
            Arc::new(StringArray::from(locations)),
            Arc::new(TimestampMillisecondArray::from(vec![
                move_timestamp(
                    previous
                )?;
                count
            ])),
        ];
        if let Some((path, volume_id, _, _)) = local_path {
            let path = Path::new(path);
            let filename = path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned());
            let parent = path
                .parent()
                .map(|value| value.to_string_lossy().into_owned());
            let extension = path
                .extension()
                .map(|value| value.to_string_lossy().to_ascii_lowercase());
            names.extend([
                "filename",
                "parent_dir",
                "ext",
                "volume_id",
                "metadata_json",
            ]);
            columns.extend([
                Arc::new(StringArray::from(vec![filename; count])) as arrow_array::ArrayRef,
                Arc::new(StringArray::from(vec![parent; count])),
                Arc::new(StringArray::from(vec![extension; count])),
                Arc::new(StringArray::from(vec![volume_id; count])),
                Arc::new(StringArray::from(changed_metadata)),
            ]);
        }
        let indices = names
            .iter()
            .map(|name| self.schema.index_of(name))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let schema = Arc::new(self.schema.project(&indices)?);
        let batch = RecordBatch::try_new(Arc::clone(&schema), columns)?;
        let reader = arrow_array::RecordBatchIterator::new(vec![Ok(batch)], schema);
        let mut merge = self.table.merge_insert(&["id", "doc_id", "owner_id"]);
        merge.when_matched_update_all(None);
        if let Some((new_path, _, _, Some(old_path))) = local_path {
            check_completed_move(old_path, new_path)?;
        }
        merge
            .execute(Box::new(reader))
            .await
            .context("Updating indexed source locations")?;
        super::super::result_cache::invalidate();
        Ok(updates.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, owner: &str, uri: &str, chunk: i32) -> DocumentChunk {
        let mut row: DocumentChunk = serde_json::from_value(serde_json::json!({
            "id": format!("{id}-{owner}-{chunk}"), "doc_id": id, "owner_id": owner,
            "location_uri": uri, "filename": "old.txt", "title": "Reviewed title",
            "ext": "txt", "parent_dir": "/old", "full_text": "searchable body",
            "embedding": [1.0,0.0], "embedding_model": "fixture", "chunk_index": chunk,
            "chunk_total": 2, "indexed_at": 9_000_000_000_000_i64,
            "source_hash": "unchanged-content", "tags": ["keep"], "doc_status": "approved",
            "volume_id": "old-volume",
            "metadata_json": "{\"field_evidence\":[{\"review_status\":\"accepted\"}],\"parent_dir\":\"/old\",\"volume_id\":\"old-volume\"}"
        }))
        .unwrap();
        row.embedding = Some(vec![1.0, 0.0]);
        row
    }

    fn paths() -> (&'static str, &'static str) {
        if cfg!(windows) {
            (r"C:\old\source's %20.txt", r"C:\new\renamed.PDF")
        } else {
            ("/old/source's %20.txt", "/new/renamed.PDF")
        }
    }

    async fn snapshot(index: &LocalIndex) -> BTreeMap<String, RecordBatch> {
        let rows: Vec<RecordBatch> = index
            .table
            .query()
            .execute()
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let mut values = BTreeMap::new();
        for batch in rows {
            for row in 0..batch.num_rows() {
                values.insert(
                    str_col(&batch, "id").unwrap().value(row).to_owned(),
                    batch.slice(row, 1),
                );
            }
        }
        values
    }

    #[tokio::test]
    async fn local_path_moves_known_variants_atomically_without_changing_identity_or_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalIndex::open_or_create(dir.path(), 2).await.unwrap();
        let (old, new) = paths();
        let canonical = format!(
            "crisp+local://{}@{}{}",
            uuid::Uuid::from_u128(1),
            uuid::Uuid::nil(),
            uri_path(old)
        );
        let legacy = format!("crisp+local://local/{old}");
        let other_machine = format!(
            "crisp+local://{}@{}{}",
            uuid::Uuid::from_u128(1),
            uuid::Uuid::from_u128(2),
            uri_path(old)
        );
        let mut rows = Vec::new();
        for (id, owner, uri) in [
            ("bare", "alice", old),
            ("canonical", "bob", canonical.as_str()),
            ("legacy", "carol", legacy.as_str()),
        ] {
            for chunk in 0..2 {
                rows.push(row(id, owner, uri, chunk));
            }
        }
        rows.extend([
            row("remote", "bob", &other_machine, 0),
            row(
                "cloud",
                "bob",
                &format!("crisp+drive://drive{}", uri_path(old)),
                0,
            ),
            row("neighbor", "alice", &format!("{old}.bak"), 0),
        ]);
        local.append_legacy_rows_for_test(&rows).await.unwrap();
        let before = snapshot(&local).await;
        let version = local.table.version().await.unwrap();
        assert_eq!(
            local
                .update_location_by_local_path_with_volume(old, new, Some("new-volume"))
                .await
                .unwrap(),
            3
        );
        assert_eq!(local.table.version().await.unwrap(), version + 1);
        let after = snapshot(&local).await;
        assert_eq!(before.len(), after.len());
        let mut times = BTreeSet::new();
        for (id, previous) in &before {
            let current = &after[id];
            let selected = ["bare-", "canonical-", "legacy-"]
                .iter()
                .any(|prefix| id.starts_with(prefix));
            for (column, name) in previous.schema().fields().iter().enumerate() {
                if selected
                    && [
                        "location_uri",
                        "filename",
                        "parent_dir",
                        "ext",
                        "indexed_at",
                        "volume_id",
                        "metadata_json",
                    ]
                    .contains(&name.name().as_str())
                {
                    continue;
                }
                assert_eq!(
                    previous.column(column).to_data(),
                    current.column(column).to_data(),
                    "{id}: {}",
                    name.name()
                );
            }
            if selected {
                let old_uri = str_col(previous, "location_uri").unwrap().value(0);
                assert_eq!(
                    str_col(current, "location_uri").unwrap().value(0),
                    relocated_local_uri(old_uri, old, new).unwrap()
                );
                assert_eq!(
                    str_col(current, "filename").unwrap().value(0),
                    "renamed.PDF"
                );
                assert_eq!(str_col(current, "ext").unwrap().value(0), "pdf");
                assert_eq!(
                    str_col(current, "volume_id").unwrap().value(0),
                    "new-volume"
                );
                let mut expected: serde_json::Value =
                    serde_json::from_str(str_col(previous, "metadata_json").unwrap().value(0))
                        .unwrap();
                expected["parent_dir"] = Path::new(new)
                    .parent()
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
                    .into();
                expected["volume_id"] = "new-volume".into();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(
                        str_col(current, "metadata_json").unwrap().value(0)
                    )
                    .unwrap(),
                    expected
                );
                times.insert(ts_ms_col_opt(current, "indexed_at").unwrap().value(0));
            }
        }
        assert_eq!(times, BTreeSet::from([9_000_000_000_001]));
        assert_eq!(
            local.update_location_by_local_path(old, new).await.unwrap(),
            0
        );
        assert_eq!(
            local.update_location_by_local_path(new, old).await.unwrap(),
            3
        );
        for current in snapshot(&local).await.values() {
            let id = str_col(current, "doc_id").unwrap().value(0);
            if !["bare", "canonical", "legacy"].contains(&id) {
                continue;
            }
            assert!(str_col(current, "volume_id").unwrap().is_null(0));
            let metadata: serde_json::Value =
                serde_json::from_str(str_col(current, "metadata_json").unwrap().value(0)).unwrap();
            assert!(metadata.get("volume_id").is_none());
            assert_eq!(metadata["field_evidence"][0]["review_status"], "accepted");
        }
    }

    #[tokio::test]
    async fn ambiguous_legacy_identity_or_destination_collision_refuses_the_entire_move() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalIndex::open_or_create(dir.path(), 2).await.unwrap();
        let (old, new) = paths();
        local
            .append_legacy_rows_for_test(&[
                row("shared", "alice", old, 0),
                row("shared", "bob", "other-source", 1),
                row("good", "carol", old, 0),
            ])
            .await
            .unwrap();
        let version = local.table.version().await.unwrap();
        assert!(local
            .update_location_by_local_path(old, new)
            .await
            .unwrap_err()
            .to_string()
            .contains("multiple source occurrences"));
        assert_eq!(local.table.version().await.unwrap(), version);
        let second = tempfile::tempdir().unwrap();
        let local = LocalIndex::open_or_create(second.path(), 2).await.unwrap();
        local
            .append_legacy_rows_for_test(&[
                row("old", "alice", old, 0),
                row("destination", "alice", new, 0),
            ])
            .await
            .unwrap();
        let version = local.table.version().await.unwrap();
        assert!(local
            .update_location_by_local_path(old, new)
            .await
            .unwrap_err()
            .to_string()
            .contains("destination"));
        assert_eq!(local.table.version().await.unwrap(), version);
    }

    #[tokio::test]
    async fn explicit_location_updates_advance_the_existing_document_clock() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalIndex::open_or_create(dir.path(), 2).await.unwrap();
        local
            .append_legacy_rows_for_test(&[
                row("clock", "alice", "old", 0),
                row("clock", "alice", "old", 1),
            ])
            .await
            .unwrap();
        local.update_location("clock", "middle").await.unwrap();
        local.update_location_by_uri("middle", "new").await.unwrap();
        for current in snapshot(&local).await.values() {
            assert_eq!(str_col(current, "location_uri").unwrap().value(0), "new");
            assert_eq!(
                ts_ms_col_opt(current, "indexed_at").unwrap().value(0),
                9_000_000_000_002
            );
        }
        assert!(local
            .update_location_by_local_path("relative.txt", "/new.txt")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn completed_move_rechecks_recreated_source_after_waiting_for_writer_lease() {
        let dir = tempfile::tempdir().unwrap();
        let local = Arc::new(
            LocalIndex::open_or_create(&dir.path().join("index"), 2)
                .await
                .unwrap(),
        );
        let old = dir.path().join("source.txt");
        let new = dir.path().join("moved.txt");
        std::fs::write(&old, b"original contents").unwrap();
        local
            .append_legacy_rows_for_test(&[row("selected", "alice", old.to_str().unwrap(), 0)])
            .await
            .unwrap();
        std::fs::rename(&old, &new).unwrap();
        check_completed_move(old.to_str().unwrap(), new.to_str().unwrap()).unwrap();
        let version = local.table.version().await.unwrap();
        let lease = local.write_guard().await.unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn({
            let local = local.clone();
            let old = old.clone();
            let new = new.clone();
            async move {
                started.send(()).unwrap();
                local
                    .update_location_after_local_move(
                        old.to_str().unwrap(),
                        new.to_str().unwrap(),
                        None,
                    )
                    .await
            }
        });
        ready.await.unwrap();
        std::fs::write(&old, b"new unrelated file at the old address").unwrap();
        drop(lease);
        assert!(waiting
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("original path exists again"));
        assert_eq!(local.table.version().await.unwrap(), version);
        assert_eq!(
            local
                .fetch_document_summary("selected")
                .await
                .unwrap()
                .unwrap()
                .location_uri,
            old.to_string_lossy()
        );
        std::fs::remove_file(&old).unwrap();
        assert_eq!(
            local
                .update_location_after_local_move(
                    old.to_str().unwrap(),
                    new.to_str().unwrap(),
                    None
                )
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            local
                .fetch_document_summary("selected")
                .await
                .unwrap()
                .unwrap()
                .location_uri,
            new.to_string_lossy()
        );
        assert_eq!(std::fs::read(&new).unwrap(), b"original contents");
    }

    #[test]
    fn path_mapping_keeps_literal_characters_and_rejects_other_machines() {
        let old = r"C:\Users\owner\a #100%20.txt";
        let new = r"C:\Sorted\b.txt";
        let authority = format!("{}@{}", uuid::Uuid::from_u128(1), uuid::Uuid::nil());
        let uri = format!("crisp+local://{authority}{}", uri_path(old));
        assert_eq!(
            relocated_local_uri(&uri, old, new).unwrap(),
            format!("crisp+local://{authority}/C:/Sorted/b.txt")
        );
        assert!(relocated_local_uri(
            &uri.replace(
                &uuid::Uuid::nil().to_string(),
                &uuid::Uuid::from_u128(2).to_string()
            ),
            old,
            new
        )
        .is_none());
        assert!(relocated_local_uri("crisp+local://not-a-user@00000000-0000-0000-0000-000000000000/C:/Users/owner/a #100%20.txt", old, new).is_none());
    }
}
