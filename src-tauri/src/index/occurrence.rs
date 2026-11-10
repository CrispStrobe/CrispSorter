// Source identity resolution under the paired index writer lease.
//
// Content fingerprints group duplicate bytes. Source occurrences remain
// independently addressable across owners, locations, and file moves.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use super::super::ingest::{chunk_row_id, TantivyInputOwned};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) struct SourceOccurrence {
    pub(crate) doc_id: String,
    pub(crate) owner_id: String,
    pub(crate) location_uri: String,
}

impl SourceOccurrence {
    pub(crate) fn from_chunk(chunk: &DocumentChunk) -> Self {
        Self { doc_id: chunk.doc_id.clone(), owner_id: chunk.owner_id.clone(), location_uri: chunk.location_uri.clone() }
    }

    pub(crate) fn filter(&self) -> String {
        format!("(doc_id = '{}' AND owner_id = '{}' AND location_uri = '{}')",
            self.doc_id.replace('\'', "''"), self.owner_id.replace('\'', "''"), self.location_uri.replace('\'', "''"))
    }
}

pub(crate) struct OccurrencePlan {
    pub(crate) replace_ids: Vec<String>,
    pub(crate) receipt_rows: Vec<String>,
    pub(crate) absent_occurrences: Vec<SourceOccurrence>,
    pub(crate) preserved_inputs: Vec<TantivyInputOwned>,
    pub(crate) doc_ids: Vec<String>,
    filter: String,
    preserved: Vec<RecordBatch>,
}

type Scope = (String, String);

fn string_at(batch: &RecordBatch, name: &str, row: usize) -> String {
    str_col_opt(batch, name).filter(|c| !c.is_null(row)).map(|c| c.value(row).to_owned()).unwrap_or_default()
}

fn number_at(batch: &RecordBatch, name: &str, row: usize) -> i32 {
    batch.column_by_name(name).and_then(|c| c.as_any().downcast_ref::<Int32Array>())
        .filter(|c| !c.is_null(row)).map(|c| c.value(row)).unwrap_or(-1)
}

fn metadata_at(batch: &RecordBatch, row: usize) -> Result<serde_json::Value> {
    let text = string_at(batch, "metadata_json", row);
    let metadata = if text.is_empty() { serde_json::json!({}) } else { serde_json::from_str(&text)? };
    anyhow::ensure!(metadata.is_object(), "Stored document metadata must be an object");
    Ok(metadata)
}

fn rekey_fields(metadata: &mut serde_json::Value, doc_id: &str) {
    use sha2::{Digest, Sha256};
    if let Some(fields) = metadata.get_mut("field_evidence").and_then(|v| v.as_array_mut()) {
        for field in fields {
            if let (Some(label), Some(original)) = (field.get("label").and_then(|v| v.as_str()), field.get("original_value").and_then(|v| v.as_str())) {
                field["id"] = format!("{:x}", Sha256::digest(format!("{doc_id}\0{label}\0{original}").as_bytes())).into();
            }
        }
    }
}

fn retain_reviews(chunk: &mut DocumentChunk, previous: &RecordBatch, row: usize, full_text: &str) -> Result<()> {
    let mut prior = metadata_at(previous, row)?;
    let mut current: serde_json::Value = chunk.metadata_json.as_deref().map(serde_json::from_str)
        .transpose()?.unwrap_or_else(|| serde_json::json!({}));
    anyhow::ensure!(current.is_object(), "Document metadata must be an object");
    rekey_fields(&mut prior, &chunk.doc_id);
    rekey_fields(&mut current, &chunk.doc_id);
    let mut fields = super::super::fields::read(&current);
    let reviewed = super::super::fields::read(&prior);
    if current.get("field_evidence").is_some() {
        super::super::fields::preserve_reviews(&mut fields, &reviewed);
    } else if string_at(previous, "source_hash", row) == chunk.source_hash {
        fields = reviewed;
        // The same source bytes can yield different OCR or manually corrected
        // text. Keep the human decision, but anchor it in this extraction's
        // full body. Missing evidence must not retain an old page or offset.
        for field in &mut fields {
            let at = full_text.find(&field.original_value);
            field.start_char = at.map(|offset| full_text[..offset].chars().count());
            field.end_char = field.start_char.map(|start| start + field.original_value.chars().count());
            field.page = at.filter(|_| full_text.contains('\u{c}'))
                .map(|offset| full_text[..offset].chars().filter(|&c| c == '\u{c}').count() + 1);
            field.anchor_method = if at.is_some() { "first_exact_match" } else { "unavailable" }.into();
        }
    }
    // Carry user metadata across a refresh. Source observations and failure
    // state belong to the new extraction and must never be copied forward.
    for key in ["field_evidence", "index_write_generation", "extraction_failure", "extraction_skip", "failed_source_hash",
        "mtime_unix", "fs_size", "fs_mtime", "fs_ctime", "images_indexed", "volume_id", "parent_dir"] {
        prior.as_object_mut().expect("checked above").remove(key);
    }
    for (key, value) in current.as_object().expect("checked above") { prior[key] = value.clone(); }
    prior["source_identity_version"] = super::super::source_identity::SOURCE_IDENTITY_VERSION.into();
    prior["field_evidence"] = serde_json::json!(fields);
    chunk.metadata_json = Some(prior.to_string());
    if chunk.doc_status.is_none() {
        let status = string_at(previous, "doc_status", row);
        if !status.is_empty() { chunk.doc_status = Some(status); }
    }
    let old_tags = list_str_col_val(previous, "tags", row);
    super::super::ingest::merge_tags(&mut chunk.tags, old_tags.into_iter().filter(|t| !t.starts_with("field:")).collect());
    chunk.tags.retain(|tag| !tag.starts_with("field:"));
    super::super::ingest::merge_tags(&mut chunk.tags,
        fields.iter().filter(|field| field.review_status != "rejected").map(super::super::fields::tag).collect());
    Ok(())
}

fn copied_row(batch: &RecordBatch, row: usize, doc_id: &str, generation: &str) -> Result<RecordBatch> {
    let mut result = batch.slice(row, 1);
    let mut columns = result.columns().to_vec();
    let mut metadata = metadata_at(batch, row)?;
    rekey_fields(&mut metadata, doc_id);
    metadata["source_identity_version"] = super::super::source_identity::SOURCE_IDENTITY_VERSION.into();
    metadata["index_write_generation"] = generation.into();
    for (name, value) in [("id", chunk_row_id(doc_id, number_at(batch, "chunk_index", row))),
        ("doc_id", doc_id.to_owned()), ("metadata_json", metadata.to_string())] {
        let index = result.schema().index_of(name)?;
        columns[index] = Arc::new(StringArray::from(vec![value]));
    }
    result = RecordBatch::try_new(result.schema(), columns)?;
    Ok(result)
}

fn fts_from_rows(batches: &[RecordBatch]) -> Result<Vec<TantivyInputOwned>> {
    let mut groups = BTreeMap::<String, Vec<FtsPiece>>::new();
    for batch in batches {
        for row in 0..batch.num_rows() {
            let optional = |name| { let value = string_at(batch, name, row); (!value.is_empty()).then_some(value) };
            let number = |name| batch.column_by_name(name).and_then(|c| c.as_any().downcast_ref::<Int32Array>())
                .filter(|c| !c.is_null(row)).map(|c| c.value(row));
            let doc_id = string_at(batch, "doc_id", row);
            groups.entry(doc_id.clone()).or_default().push(FtsPiece {
                id: string_at(batch, "id", row), doc_id, owner: string_at(batch, "owner_id", row),
                language: string_at(batch, "language", row), title: string_at(batch, "title", row),
                headings: string_at(batch, "headings_text", row), body: string_at(batch, "full_text", row),
                translated: optional("text_translated"), chunk_index: number_at(batch, "chunk_index", row),
                start: number("chunk_start_char"), end: number("chunk_end_char"),
            });
        }
    }
    let mut inputs = Vec::new();
    for (doc_id, pieces) in groups {
        if let Some(input) = fts_input_from_pieces(doc_id, pieces)? { inputs.push(input); }
    }
    Ok(inputs)
}

impl LocalIndex {
    pub async fn document_ids_for_source(&self, owner_id: &str, location_uri: &str) -> Result<Vec<String>> {
        let rows: Vec<RecordBatch> = self.table.query().only_if(format!("owner_id = '{}' AND location_uri = '{}'",
            owner_id.replace('\'', "''"), location_uri.replace('\'', "''")))
            .select(lancedb::query::Select::Columns(vec!["doc_id".into()])).execute().await?.try_collect().await?;
        let ids = rows.iter().flat_map(|batch| (0..batch.num_rows()).map(|row| string_at(batch, "doc_id", row))).collect::<BTreeSet<_>>();
        Ok(ids.into_iter().collect())
    }

    /// Enumerate identity scopes without taking another gate. Mutation callers
    /// must already hold the paired index guard through their final write.
    pub async fn document_occurrences(&self, doc_id: &str) -> Result<Vec<(String, String)>> {
        let rows: Vec<RecordBatch> = self.table.query().only_if(format!("doc_id = '{}'", doc_id.replace('\'', "''")))
            .select(lancedb::query::Select::Columns(vec!["owner_id".into(), "location_uri".into()]))
            .execute().await?.try_collect().await?;
        let mut occurrences = BTreeSet::new();
        for batch in rows { for row in 0..batch.num_rows() {
            occurrences.insert((string_at(&batch, "owner_id", row), string_at(&batch, "location_uri", row)));
        } }
        Ok(occurrences.into_iter().collect())
    }

    pub(crate) async fn occurrences_absent(&self, occurrences: &[SourceOccurrence]) -> Result<bool> {
        self.table.checkout_latest().await?;
        if occurrences.is_empty() { return Ok(true); }
        let filter = occurrences.iter().map(SourceOccurrence::filter).collect::<Vec<_>>().join(" OR ");
        Ok(self.table.count_rows(Some(filter)).await? == 0)
    }

    pub(crate) async fn metadata_occurrence(&self, chunk: &DocumentChunk) -> Result<DocumentChunk> {
        let mut prepared = chunk.clone();
        let mut filter = format!("owner_id = '{}' AND location_uri = '{}'", chunk.owner_id.replace('\'', "''"), chunk.location_uri.replace('\'', "''"));
        if chunk.location_uri.is_empty() { filter.push_str(&format!(" AND doc_id = '{}'", chunk.doc_id.replace('\'', "''"))); }
        let rows: Vec<RecordBatch> = self.table.query().only_if(filter)
            .select(lancedb::query::Select::Columns(vec!["doc_id".into()])).execute().await?.try_collect().await?;
        let ids: BTreeSet<String> = rows.iter().flat_map(|batch| (0..batch.num_rows()).map(|row| string_at(batch, "doc_id", row))).collect();
        let canonical = super::super::source_identity::occurrence_id(&chunk.owner_id, &chunk.location_uri, &chunk.source_hash);
        let id = if ids.contains(&chunk.doc_id) { chunk.doc_id.clone() }
            else if ids.contains(&canonical) { canonical.clone() }
            else if ids.len() == 1 { ids.into_iter().next().expect("one identity") }
            else if ids.len() > 1 { anyhow::bail!("Multiple stored identities share this source; select the intended occurrence before updating metadata"); }
            else if self.document_occurrences(&chunk.doc_id).await?.is_empty() { chunk.doc_id.clone() }
            else { self.available_occurrence_id(&canonical, &mut BTreeSet::new()).await? };
        prepared.doc_id = id;
        prepared.id = chunk_row_id(&prepared.doc_id, prepared.chunk_index);
        Ok(prepared)
    }

    pub(crate) async fn annotate_occurrence_failure(&self, occurrence: &SourceOccurrence, failure: &serde_json::Value, source_hash: &str) -> Result<()> {
        let filter = occurrence.filter();
        let mut batches: Vec<RecordBatch> = self.table.query().only_if(&filter).execute().await?.try_collect().await?;
        for batch in &mut batches {
            let mut metadata = Vec::new();
            for row in 0..batch.num_rows() {
                let mut value = metadata_at(batch, row)?;
                value["extraction_failure"] = failure.clone();
                value["failed_source_hash"] = source_hash.into();
                metadata.push(value.to_string());
            }
            let mut columns = batch.columns().to_vec();
            columns[batch.schema().index_of("metadata_json")?] = Arc::new(StringArray::from(metadata));
            *batch = RecordBatch::try_new(batch.schema(), columns)?;
        }
        if batches.is_empty() { return Ok(()); }
        let reader = arrow_array::RecordBatchIterator::new(batches.into_iter().map(Ok), self.schema.clone());
        let mut merge = self.table.merge_insert(&["id", "metadata_json"]);
        merge.when_not_matched_insert_all().when_not_matched_by_source_delete(Some(filter));
        merge.execute(Box::new(reader)).await?;
        Ok(())
    }

    async fn available_occurrence_id(&self, candidate: &str, reserved: &mut BTreeSet<String>) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut id = candidate.to_owned();
        loop {
            if !reserved.contains(&id) && self.document_occurrences(&id).await?.is_empty() {
                reserved.insert(id.clone());
                return Ok(id);
            }
            id = format!("{:x}", Sha256::digest(uuid::Uuid::new_v4().as_bytes()));
        }
    }

    /// Resolve ordinary per-source writes. Explicit/imported IDs use the
    /// separate import API and are never silently reinterpreted here.
    pub(crate) async fn prepare_occurrence_write(&self, chunks: &mut [DocumentChunk], inputs: &mut [TantivyInputOwned], generation: &str) -> Result<OccurrencePlan> {
        self.prepare_occurrence_change(chunks, inputs, generation, None).await
    }

    pub(crate) async fn prepare_occurrence_delete(&self, occurrence: SourceOccurrence, generation: &str) -> Result<OccurrencePlan> {
        self.prepare_occurrence_change(&mut [], &mut [], generation, Some(occurrence)).await
    }

    async fn prepare_occurrence_change(&self, chunks: &mut [DocumentChunk], inputs: &mut [TantivyInputOwned], generation: &str, deletion: Option<SourceOccurrence>) -> Result<OccurrencePlan> {
        self.table.checkout_latest().await?;
        let representatives: BTreeMap<String, &DocumentChunk> = chunks.iter().filter(|c| c.chunk_index == 0)
            .map(|c| (c.doc_id.clone(), c)).collect();
        let input_order: Vec<String> = inputs.iter().map(|input| input.doc_id.clone()).collect();
        let full_bodies: BTreeMap<String, &str> = inputs.iter().map(|input| (input.doc_id.clone(), input.body.as_str())).collect();
        let mut selected = BTreeMap::<(String, Scope), String>::new();
        let mut resolved = BTreeMap::<String, String>::new();
        let mut affected = BTreeSet::<String>::new();
        let mut reserved = BTreeSet::<String>::new();
        for (canonical, chunk) in &representatives {
            let scope = (chunk.owner_id.clone(), chunk.location_uri.clone());
            let mut filter = format!("owner_id = '{}' AND location_uri = '{}'", scope.0.replace('\'', "''"), scope.1.replace('\'', "''"));
            if scope.1.is_empty() { filter.push_str(&format!(" AND doc_id = '{}'", canonical.replace('\'', "''"))); }
            let rows: Vec<RecordBatch> = self.table.query().only_if(filter)
                .select(lancedb::query::Select::Columns(vec!["doc_id".into()]))
                .execute().await?.try_collect().await?;
            let mut ids = BTreeSet::new();
            for batch in rows { for row in 0..batch.num_rows() { ids.insert(string_at(&batch, "doc_id", row)); } }
            let chosen = if ids.contains(canonical) { Some(canonical.clone()) }
                else if ids.len() == 1 { ids.into_iter().next() }
                else if ids.is_empty() { None }
                else { anyhow::bail!("Multiple stored identities share this owner and source URI; select the intended occurrence before refreshing {}", chunk.location_uri); };
            if let Some(old_id) = chosen {
                selected.insert((old_id.clone(), scope), canonical.clone());
                affected.insert(old_id);
            } else {
                resolved.insert(canonical.clone(), self.available_occurrence_id(canonical, &mut reserved).await?);
            }
        }
        if let Some(occurrence) = &deletion { affected.insert(occurrence.doc_id.clone()); }
        let mut preserved = Vec::new();
        let mut prior_rows = BTreeMap::<String, RecordBatch>::new();
        let mut absent_occurrences = Vec::new();
        let mut replace_ids = affected.clone();
        for old_id in &affected {
            let batches: Vec<RecordBatch> = self.table.query().only_if(format!("doc_id = '{}'", old_id.replace('\'', "''")))
                .execute().await?.try_collect().await?;
            let mut groups = BTreeMap::<Scope, BTreeMap<i32, (usize, usize, i64)>>::new();
            for (batch_index, batch) in batches.iter().enumerate() { for row in 0..batch.num_rows() {
                let scope = (string_at(batch, "owner_id", row), string_at(batch, "location_uri", row));
                let indexed = batch.column_by_name("indexed_at").and_then(|c| c.as_any().downcast_ref::<TimestampMillisecondArray>())
                    .filter(|c| !c.is_null(row)).map(|c| c.value(row)).unwrap_or(0);
                let slot = groups.entry(scope).or_default().entry(number_at(batch, "chunk_index", row)).or_insert((batch_index, row, indexed));
                if indexed >= slot.2 { *slot = (batch_index, row, indexed); }
            } }
            let is_deleted = |scope: &Scope| deletion.as_ref().is_some_and(|d| d.doc_id == *old_id && d.owner_id == scope.0 && d.location_uri == scope.1);
            let surviving: Vec<Scope> = groups.keys().filter(|scope| !is_deleted(scope)).cloned().collect();
            // Keep one surviving stored identity stable. Other colliding
            // scopes receive independent IDs in this same atomic transaction.
            let keeper = surviving.iter().find(|scope| super::super::source_identity::occurrence_id(&scope.0, &scope.1, "") == *old_id)
                .or_else(|| surviving.iter().find(|scope| !selected.contains_key(&(old_id.clone(), (*scope).clone()))))
                .or_else(|| surviving.first()).cloned();
            for (scope, rows) in groups {
                let old_occurrence = SourceOccurrence { doc_id: old_id.clone(), owner_id: scope.0.clone(), location_uri: scope.1.clone() };
                if is_deleted(&scope) { absent_occurrences.push(old_occurrence); continue; }
                let id = if keeper.as_ref() == Some(&scope) {
                    reserved.insert(old_id.clone()); old_id.clone()
                } else {
                    let content = rows.values().next().map(|(batch, row, _)| string_at(&batches[*batch], "source_hash", *row)).unwrap_or_default();
                    let candidate = super::super::source_identity::occurrence_id(&scope.0, &scope.1, &content);
                    self.available_occurrence_id(&candidate, &mut reserved).await?
                };
                replace_ids.insert(id.clone());
                if id != *old_id { absent_occurrences.push(old_occurrence); }
                if let Some(canonical) = selected.get(&(old_id.clone(), scope)) {
                    resolved.insert(canonical.clone(), id);
                    if let Some((batch, row, _)) = rows.get(&0).or_else(|| rows.get(&-1)).or_else(|| rows.values().next()) {
                        prior_rows.insert(canonical.clone(), batches[*batch].slice(*row, 1));
                    }
                } else {
                    for (_, (batch, row, _)) in rows { preserved.push(copied_row(&batches[batch], row, &id, generation)?); }
                }
            }
        }
        for chunk in chunks.iter_mut() {
            let canonical = chunk.doc_id.clone();
            chunk.doc_id = resolved.get(&canonical).cloned().context("Missing resolved source identity")?;
            chunk.id = chunk_row_id(&chunk.doc_id, chunk.chunk_index);
            if let Some(previous) = prior_rows.get(&canonical) {
                retain_reviews(chunk, previous, 0, full_bodies.get(&canonical).copied().unwrap_or_default())?;
            }
            let mut metadata: serde_json::Value = chunk.metadata_json.as_deref().map(serde_json::from_str).transpose()?.unwrap_or_else(|| serde_json::json!({}));
            anyhow::ensure!(metadata.is_object(), "Document metadata must be an object");
            rekey_fields(&mut metadata, &chunk.doc_id);
            metadata["source_identity_version"] = super::super::source_identity::SOURCE_IDENTITY_VERSION.into();
            metadata["index_write_generation"] = generation.into();
            chunk.metadata_json = Some(metadata.to_string());
            replace_ids.insert(chunk.doc_id.clone());
        }
        for input in inputs { input.doc_id = resolved.get(&input.doc_id).cloned().context("Missing resolved lexical identity")?; }
        let mut receipts: BTreeSet<String> = chunks.iter().map(|c| c.id.clone()).collect();
        for batch in &preserved { for row in 0..batch.num_rows() { receipts.insert(string_at(batch, "id", row)); } }
        let mut filters = affected.iter().map(|id| format!("doc_id = '{}'", id.replace('\'', "''"))).collect::<Vec<_>>();
        filters.extend(chunks.iter().map(|chunk| SourceOccurrence::from_chunk(chunk).filter()));
        Ok(OccurrencePlan {
            doc_ids: input_order.iter().map(|id| resolved.get(id).cloned().context("Missing source result identity")).collect::<Result<_>>()?,
            replace_ids: replace_ids.into_iter().collect(), receipt_rows: receipts.into_iter().collect(), absent_occurrences,
            preserved_inputs: fts_from_rows(&preserved)?, preserved,
            filter: if filters.is_empty() { "false".into() } else { format!("({})", filters.join(" OR ")) },
        })
    }

    pub(crate) async fn commit_occurrence_plan(&self, chunks: &[DocumentChunk], plan: OccurrencePlan) -> Result<()> {
        let mut batches = plan.preserved;
        if !chunks.is_empty() {
            replacement_filter(chunks)?;
            batches.push(chunks_to_record_batch(chunks, self.dims, &self.schema)?);
        }
        if batches.is_empty() { self.table.delete(&plan.filter).await?; }
        else {
            let reader = arrow_array::RecordBatchIterator::new(batches.into_iter().map(Ok), self.schema.clone());
            let mut merge = self.table.merge_insert(&["id", "metadata_json"]);
            merge.when_not_matched_insert_all().when_not_matched_by_source_delete(Some(plan.filter));
            merge.execute(Box::new(reader)).await.context("committing source occurrences")?;
        }
        super::super::result_cache::invalidate();
        Ok(())
    }

    pub async fn delete_occurrence_with_fts(&self, doc_id: &str, owner_id: &str, location_uri: &str, fts: &super::super::fts_index::FtsIndex) -> Result<()> {
        super::super::ingest::apply_occurrence_deletion(self, fts, SourceOccurrence { doc_id: doc_id.into(), owner_id: owner_id.into(), location_uri: location_uri.into() }).await
    }
}
