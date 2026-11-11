// Root/member replacement plans share the existing paired-index transaction.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use crisp_index_protocol::container::ContainerMemberLocator;
use super::super::container_ingest::stored_locator;
use super::super::ingest::TantivyInputOwned;

const MAX_STORED_MEMBER_ROWS: usize = 65_536;

fn value(batch: &RecordBatch, name: &str, row: usize) -> String {
    str_col_opt(batch, name).filter(|column| !column.is_null(row)).map(|column| column.value(row).to_owned()).unwrap_or_default()
}

fn checked_locator(batch: &RecordBatch, row: usize) -> Result<Option<ContainerMemberLocator>> {
    let metadata = str_col_opt(batch, "metadata_json").filter(|column| !column.is_null(row)).map(|column| column.value(row));
    let locator = stored_locator(metadata)?;
    if let Some(locator) = &locator {
        locator.validate_identity(&value(batch,"owner_id",row), &value(batch,"doc_id",row), &value(batch,"location_uri",row))
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(locator.entries.last().is_some_and(|entry| entry.sha256.eq_ignore_ascii_case(&value(batch,"source_hash",row))),
            "Stored member fingerprint differs from its locator receipt");
    } else {
        anyhow::ensure!(!value(batch,"location_uri",row).starts_with("crisp+member:"),
            "Stored member source is missing its typed locator");
    }
    Ok(locator)
}

fn combine(mut root: OccurrencePlan, child: OccurrencePlan) -> OccurrencePlan {
    root.filter = format!("({}) OR ({})", root.filter, child.filter);
    root.replace_ids.extend(child.replace_ids);
    root.replace_ids.sort(); root.replace_ids.dedup();
    root.receipt_rows.extend(child.receipt_rows);
    root.receipt_rows.sort(); root.receipt_rows.dedup();
    root.absent_occurrences.extend(child.absent_occurrences);
    root.absent_occurrences.sort(); root.absent_occurrences.dedup();
    root.preserved_inputs.extend(child.preserved_inputs);
    root.preserved.extend(child.preserved);
    root.identity_map.extend(child.identity_map);
    root.member_roots.extend(child.member_roots);
    root
}

impl LocalIndex {
    pub(super) async fn prepare_member_location_rows(&self,updates:&BTreeMap<SourceOccurrence,String>,local_path:Option<(&str,Option<&str>)>)->Result<Vec<RecordBatch>> {
        let mut result=Vec::new();
        for (root,destination) in updates {
            for member in self.members_for_root(root).await? {
                let batches:Vec<RecordBatch>=self.table.query().only_if(member.filter()).select(lancedb::query::Select::Columns(
                    ["id","doc_id","owner_id","location_uri","source_hash","metadata_json","filename","ext","parent_dir","volume_id","indexed_at"]
                    .into_iter().map(str::to_owned).collect())).limit(MAX_STORED_MEMBER_ROWS+1).execute().await?.try_collect().await?;
                for batch in batches {for row in 0..batch.num_rows(){
                    anyhow::ensure!(result.len()<MAX_STORED_MEMBER_ROWS,"Container move exceeds the stored member row limit");
                    let mut locator=checked_locator(&batch,row)?.context("Stored member locator is missing")?;
                    locator.root_location_uri=destination.clone();locator.validate().map_err(anyhow::Error::msg)?;
                    let mut metadata:serde_json::Value=serde_json::from_str(&value(&batch,"metadata_json",row))?;
                    metadata["container_member"]=serde_json::json!(locator);
                    let selected=batch.slice(row,1);let mut columns=selected.columns().to_vec();
                    if let Some((path,volume))=local_path {
                        let parent=Path::new(path).parent().map(|parent|parent.to_string_lossy().into_owned());
                        metadata["parent_dir"]=serde_json::json!(parent);
                        metadata.as_object_mut().context("Stored member metadata is not an object")?.remove("volume_id");
                        if let Some(volume)=volume {metadata["volume_id"]=volume.into();}
                        columns[selected.schema().index_of("parent_dir")?]=Arc::new(StringArray::from(vec![parent]));
                        columns[selected.schema().index_of("volume_id")?]=Arc::new(StringArray::from(vec![volume]));
                    }
                    columns[selected.schema().index_of("metadata_json")?]=Arc::new(StringArray::from(vec![metadata.to_string()]));
                    result.push(RecordBatch::try_new(selected.schema(),columns)?);
                }}
            }
        }
        Ok(result)
    }

    /// Caller holds the paired read lease through materialization. Every stored
    /// chunk must agree; neither chunk zero nor an arbitrary representative wins.
    pub async fn container_member_locator(&self, owner_id: &str, doc_id: &str) -> Result<Option<ContainerMemberLocator>> {
        anyhow::ensure!(!owner_id.trim().is_empty() && !doc_id.trim().is_empty(), "An exact owner and document are required");
        let rows: Vec<RecordBatch> = self.table.query()
            .only_if(document_keys_sql(&[(owner_id.to_owned(),doc_id.to_owned())]))
            .select(lancedb::query::Select::Columns(["doc_id","owner_id","location_uri","source_hash","metadata_json"]
                .into_iter().map(str::to_owned).collect()))
            .limit(MAX_STORED_MEMBER_ROWS + 1).execute().await?.try_collect().await?;
        let count = rows.iter().map(RecordBatch::num_rows).sum::<usize>();
        anyhow::ensure!(count > 0, "The selected indexed document no longer exists");
        anyhow::ensure!(count <= MAX_STORED_MEMBER_ROWS, "The selected member has too many stored chunks to inspect safely");
        let mut selected = None;
        let mut first = true;
        for batch in rows { for row in 0..batch.num_rows() {
            let locator = checked_locator(&batch,row)?;
            if first { selected = locator; first = false; }
            else { anyhow::ensure!(selected == locator, "Stored member chunks disagree about their source locator; refresh the root source"); }
        } }
        Ok(selected)
    }

    async fn members_for_root(&self, root: &SourceOccurrence) -> Result<Vec<SourceOccurrence>> {
        // Legacy ordinary rows may have identities outside the member wire
        // bounds; no valid member locator can refer to those roots.
        let Ok(prefix) = crisp_index_protocol::container::root_member_uri_prefix(&root.owner_id,&root.doc_id) else { return Ok(Vec::new()); };
        let rows: Vec<RecordBatch> = self.table.query()
            .only_if(format!("owner_id = '{}' AND starts_with(location_uri, '{}')",root.owner_id.replace('\'',"''"),prefix))
            .select(lancedb::query::Select::Columns(["doc_id","owner_id","location_uri","source_hash","metadata_json"]
                .into_iter().map(str::to_owned).collect()))
            .limit(MAX_STORED_MEMBER_ROWS+1).execute().await?.try_collect().await?;
        anyhow::ensure!(rows.iter().map(RecordBatch::num_rows).sum::<usize>() <= MAX_STORED_MEMBER_ROWS,
            "Container has too many stored member chunks to replace safely");
        let mut members = BTreeSet::new();
        let mut locators = BTreeMap::new();
        for batch in rows { for row in 0..batch.num_rows() {
            let locator = checked_locator(&batch,row)?.context("Stored container member has no locator")?;
            if locator.root_location_uri != root.location_uri { continue; }
            anyhow::ensure!(locator.root_owner_id == root.owner_id && locator.root_doc_id == root.doc_id,
                "Stored container member belongs to a different root");
            let occurrence = SourceOccurrence { doc_id:value(&batch,"doc_id",row),owner_id:value(&batch,"owner_id",row),location_uri:value(&batch,"location_uri",row) };
            if let Some(previous) = locators.insert(occurrence.clone(),locator.clone()) {
                anyhow::ensure!(previous == locator,"Stored member chunks disagree about their root receipt");
            }
            members.insert(occurrence);
        } }
        Ok(members.into_iter().collect())
    }

    pub(crate) async fn prepare_occurrence_write(&self, chunks: &mut [DocumentChunk], inputs: &mut [TantivyInputOwned], generation: &str) -> Result<OccurrencePlan> {
        let mut root_chunks = Vec::new(); let mut child_chunks = Vec::new();
        for chunk in chunks.iter() {
            if stored_locator(chunk.metadata_json.as_deref())?.is_some() { child_chunks.push(chunk.clone()); }
            else { root_chunks.push(chunk.clone()); }
        }
        let root_keys: BTreeSet<_> = root_chunks.iter().map(|chunk|chunk.doc_id.clone()).collect();
        let mut root_inputs: Vec<_> = inputs.iter().filter(|input|root_keys.contains(&input.doc_id)).cloned().collect();
        let mut child_inputs: Vec<_> = inputs.iter().filter(|input|!root_keys.contains(&input.doc_id)).cloned().collect();
        let original_roots: Vec<_> = root_chunks.iter().map(SourceOccurrence::from_chunk).collect();
        let mut plan = self.prepare_occurrence_change(&mut root_chunks,&mut root_inputs,generation,&[]).await?;
        let resolved_roots: BTreeMap<_,_> = original_roots.iter().zip(&root_chunks).map(|(old,new)|
            ((old.owner_id.clone(),old.doc_id.clone(),old.location_uri.clone()),new.doc_id.clone())).collect();
        let mut child_ids = BTreeMap::new();
        for chunk in &mut child_chunks {
            let old_id = chunk.doc_id.clone();
            let mut locator = stored_locator(chunk.metadata_json.as_deref())?.context("Member locator disappeared")?;
            let key = (locator.root_owner_id.clone(),locator.root_doc_id.clone(),locator.root_location_uri.clone());
            locator.root_doc_id = resolved_roots.get(&key).cloned().context("Member was supplied without its root source")?;
            let mut metadata: serde_json::Value = serde_json::from_str(chunk.metadata_json.as_deref().context("Member metadata is missing")?)?;
            chunk.doc_id = locator.member_id().map_err(anyhow::Error::msg)?;
            chunk.id = chunk_row_id(&chunk.doc_id,chunk.chunk_index);
            chunk.location_uri = locator.member_uri().map_err(anyhow::Error::msg)?;
            metadata["container_member"] = serde_json::json!(locator);
            chunk.metadata_json = Some(metadata.to_string());
            child_ids.insert(old_id,chunk.doc_id.clone());
        }
        for input in &mut child_inputs { input.doc_id = child_ids.get(&input.doc_id).cloned().context("Missing member lexical identity")?; }
        let fresh: BTreeSet<_> = child_chunks.iter().map(SourceOccurrence::from_chunk).collect();
        let refreshed: BTreeSet<_> = root_chunks.iter().map(SourceOccurrence::from_chunk).collect();
        let mut roots = BTreeSet::new();
        roots.extend(plan.identity_map.iter().filter(|(old,new)|refreshed.contains(new)&&plan.member_roots.contains(old)).map(|(old,_)|old.clone()));
        roots.extend(root_chunks.iter().filter(|chunk|chunk.metadata_json.as_deref().and_then(|metadata|serde_json::from_str::<serde_json::Value>(metadata).ok())
            .is_some_and(|metadata|metadata.get("container_source").is_some())).map(SourceOccurrence::from_chunk));
        let mut deletions = BTreeSet::new();
        for root in roots { for member in self.members_for_root(&root).await? {
            if !fresh.contains(&member) { deletions.insert(member); }
        } }
        let mut child_plan = self.prepare_occurrence_change(&mut child_chunks,&mut child_inputs,generation,&deletions.into_iter().collect::<Vec<_>>()).await?;
        for chunk in &child_chunks {
            stored_locator(chunk.metadata_json.as_deref())?.context("Missing committed member locator")?
                .validate_identity(&chunk.owner_id,&chunk.doc_id,&chunk.location_uri).map_err(anyhow::Error::msg)?;
        }
        // Legacy root collisions can rekey a surviving, unselected occurrence.
        // Its children retain their text and reviews while following that root.
        let fresh_roots: BTreeSet<_> = root_chunks.iter().map(SourceOccurrence::from_chunk).collect();
        for (old,new) in &plan.identity_map {
            if old.doc_id != new.doc_id && !fresh_roots.contains(new) && plan.member_roots.contains(old) {
                child_plan = self.rebind_preserved_members(child_plan,old,new,generation).await?;
            }
        }
        plan = combine(plan,child_plan);
        for (target,prepared) in chunks.iter_mut().zip(root_chunks.into_iter().chain(child_chunks)) { *target = prepared; }
        for (target,prepared) in inputs.iter_mut().zip(root_inputs.into_iter().chain(child_inputs)) { *target = prepared; }
        Ok(plan)
    }

    pub(crate) async fn prepare_occurrence_delete(&self, root: SourceOccurrence, generation: &str) -> Result<OccurrencePlan> {
        let mut deletions = self.members_for_root(&root).await?;
        deletions.push(root);
        let mut plan = self.prepare_occurrence_change(&mut [],&mut [],generation,&deletions).await?;
        for (old,new) in plan.identity_map.clone() {
            if old.doc_id != new.doc_id && plan.member_roots.contains(&old) { plan = self.rebind_preserved_members(plan,&old,&new,generation).await?; }
        }
        Ok(plan)
    }

    async fn rebind_preserved_members(&self, mut plan: OccurrencePlan, old: &SourceOccurrence, new: &SourceOccurrence, generation: &str) -> Result<OccurrencePlan> {
        let members = self.members_for_root(old).await?;
        if members.is_empty() { return Ok(plan); }
        let mut preserved = Vec::new();
        for member in &members {
            let batches: Vec<RecordBatch> = self.table.query().only_if(member.filter()).limit(MAX_STORED_MEMBER_ROWS+1)
                .execute().await?.try_collect().await?;
            for batch in batches { for row in 0..batch.num_rows() {
                let mut locator = checked_locator(&batch,row)?.context("Stored member locator is missing")?;
                locator.root_doc_id = new.doc_id.clone(); locator.root_location_uri = new.location_uri.clone();
                let id = locator.member_id().map_err(anyhow::Error::msg)?;
                anyhow::ensure!(self.document_occurrences(&id).await?.is_empty(),
                    "Repaired member identity collides with an existing source; no container rows were changed");
                let mut copied = occurrence::copied_row(&batch,row,&id,generation)?;
                let mut metadata: serde_json::Value = serde_json::from_str(&value(&copied,"metadata_json",0))?;
                metadata["container_member"] = serde_json::json!(locator);
                let mut columns = copied.columns().to_vec();
                columns[copied.schema().index_of("metadata_json")?] = Arc::new(StringArray::from(vec![metadata.to_string()]));
                columns[copied.schema().index_of("location_uri")?] = Arc::new(StringArray::from(vec![locator.member_uri().map_err(anyhow::Error::msg)?]));
                copied = RecordBatch::try_new(copied.schema(),columns)?;
                plan.receipt_rows.push(value(&copied,"id",0));
                plan.replace_ids.push(id);
                preserved.push(copied);
            } }
        }
        let deletion = self.prepare_occurrence_change(&mut [],&mut [],generation,&members).await?;
        plan = combine(plan,deletion);
        plan.preserved_inputs.extend(occurrence::fts_from_rows(&preserved)?);
        plan.preserved.extend(preserved);
        Ok(plan)
    }
}
