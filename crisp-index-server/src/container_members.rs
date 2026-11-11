//! Atomic owner-scoped publication and inspection of root/member groups.
use super::*;
use crisp_index_protocol::{ContainerMemberLocator,ContainerMemberRequest,ContainerMemberResponse,
    CONTAINER_MEMBERS_VERSION,MAX_CONTAINER_MEMBERS,MAX_CONTAINER_TEXT_BYTES};

fn container_present(chunk:&IngestChunk)->bool {
    chunk.container_member.is_some()||chunk.container_source.is_some()||chunk.container_coverage.is_some()
}

pub fn validate_container_batch(batch:&IngestBatch)->Result<()> {
    let grouped=batch.chunks.iter().any(container_present);
    if batch.container_group_version.is_none() {
        ensure!(!grouped,"Container metadata requires the versioned complete-group endpoint");
        return Ok(());
    }
    ensure!(batch.container_group_version==Some(CONTAINER_MEMBERS_VERSION),"Unsupported container group version");
    ensure!(grouped,"A container group must include a captured root source");
    let mut roots=BTreeMap::new();let mut docs=BTreeMap::<DocumentKey,Vec<&IngestChunk>>::new();
    for chunk in &batch.chunks {
        validate_container_chunk(chunk)?;
        let key=DocumentKey {owner_id:chunk.owner_id.clone(),doc_id:chunk.doc_id.clone()};
        docs.entry(key.clone()).or_default().push(chunk);
        if chunk.container_source.is_some() { roots.entry(key).or_insert(chunk); }
    }
    let mut members=BTreeMap::<DocumentKey,BTreeSet<String>>::new();
    let mut text_bytes=BTreeMap::<DocumentKey,usize>::new();
    for (key,chunks) in &docs {
        let first=chunks[0];
        if !container_present(first) { continue; }
        let total=first.chunk_total.context("Container documents require complete declared chunk sequences")?;
        ensure!(total>0 && chunks.len()==total as usize && chunks.iter().all(|chunk|chunk.chunk_total==Some(total))
            && chunks.iter().map(|chunk|chunk.chunk_index).collect::<BTreeSet<_>>()==(0..total).collect(),
            "Container publication must contain every chunk of every supplied document");
        let root_key=if let Some(locator)=&first.container_member {
            let root_key=DocumentKey {owner_id:locator.root_owner_id.clone(),doc_id:locator.root_doc_id.clone()};
            let root=roots.get(&root_key).context("Container member was supplied without its root document")?;
            ensure!(root.location_uri==locator.root_location_uri && root.source_hash.eq_ignore_ascii_case(&locator.root_sha256),
                "Member locator does not match the supplied root source receipt");
            members.entry(root_key.clone()).or_default().insert(key.doc_id.clone());root_key
        } else {
            ensure!(first.source_identity_version==Some(1),"Container roots require owner/source occurrence resolution");
            key.clone()
        };
        let bytes=text_bytes.entry(root_key).or_default();
        for chunk in chunks {
            *bytes=bytes.checked_add(chunk.full_text.len()).and_then(|bytes|bytes.checked_add(chunk.full_text_md.as_ref().map_or(0,String::len)))
                .context("Container indexed text size overflow")?;
            // Chunk overlap and one aggregate Markdown preview can coexist with
            // the extractor's 16 MiB plain-text budget.
            ensure!(*bytes<=MAX_CONTAINER_TEXT_BYTES*3,"Container indexed text exceeds its bounded upload budget");
        }
    }
    for (key,root) in roots {
        let count=members.get(&key).map_or(0,BTreeSet::len);
        ensure!(count<=MAX_CONTAINER_MEMBERS,"Container member count exceeds the shared limit");
        ensure!(root.container_coverage.as_ref().is_some_and(|coverage|coverage.extracted as usize>=count),
            "Container coverage is missing extracted member documents");
    }
    Ok(())
}

pub(super) fn validate_container_chunk(chunk:&IngestChunk)->Result<()> {
    if let Some(locator)=&chunk.container_member {
        ensure!(chunk.container_source.is_none()&&chunk.source_identity_version.is_none(),
            "A member cannot also claim a root source identity");
        locator.validate_identity(&chunk.owner_id,&chunk.doc_id,&chunk.location_uri).map_err(anyhow::Error::msg)?;
        ensure!(locator.entries.last().is_some_and(|entry|entry.sha256.eq_ignore_ascii_case(&chunk.source_hash)),
            "Member source fingerprint differs from the decoded entry receipt");
    } else {
        ensure!(!chunk.location_uri.starts_with("crisp+member:"),"Virtual member sources require typed provenance");
    }
    if let Some(source)=&chunk.container_source {
        source.validate().map_err(anyhow::Error::msg)?;
        ensure!(source.sha256.eq_ignore_ascii_case(&chunk.source_hash),"Container root fingerprint differs from its source receipt");
        ensure!(chunk.container_coverage.is_some(),"Container roots require explicit extraction coverage");
    }
    if let Some(coverage)=&chunk.container_coverage { coverage.validate().map_err(anyhow::Error::msg)?; }
    ensure!(chunk.container_coverage.is_none()||chunk.container_member.is_some()||chunk.container_source.is_some(),
        "Container coverage requires a root or member receipt");
    Ok(())
}

fn row_string<'a>(batch:&'a RecordBatch,name:&str,row:usize)->Result<&'a str> {
    let column=string_column(batch,name)?;
    ensure!(!column.is_null(row),"Stored member identity is incomplete");Ok(column.value(row))
}

fn row_locator(batch:&RecordBatch,row:usize)->Result<Option<ContainerMemberLocator>> {
    let column=string_column(batch,"container_member")?;
    let locator:Option<ContainerMemberLocator>=if column.is_null(row){None}else{Some(serde_json::from_str(column.value(row))?)};
    if let Some(locator)=&locator {
        locator.validate_identity(row_string(batch,"owner_id",row)?,row_string(batch,"doc_id",row)?,row_string(batch,"location_uri",row)?)
            .map_err(anyhow::Error::msg)?;
        ensure!(locator.entries.last().is_some_and(|entry|entry.sha256.eq_ignore_ascii_case(row_string(batch,"source_hash",row).unwrap_or_default())),
            "Stored member fingerprint differs from its locator");
    } else { ensure!(!row_string(batch,"location_uri",row)?.starts_with("crisp+member:"),"Stored member locator is missing"); }
    Ok(locator)
}

impl VectorStore {
    async fn member_metadata_rows(&self,filter:String)->Result<Vec<RecordBatch>> {
        let rows:Vec<RecordBatch>=self.table.query().only_if(filter).select(Select::columns(&[
            "id","doc_id","owner_id","location_uri","source_hash","container_member","indexed_at","parent_dir"]))
            .limit(MAX_AFFECTED_CHUNKS+1).execute().await?.try_collect().await?;
        ensure!(rows.iter().map(RecordBatch::num_rows).sum::<usize>()<=MAX_AFFECTED_CHUNKS,
            "Stored container metadata exceeds the 65536-row limit");Ok(rows)
    }

    async fn member_removals(&self,root:&DocumentKey,root_uri:&str)->Result<Vec<SourceRemoval>> {
        let Ok(prefix)=crisp_index_protocol::root_member_uri_prefix(&root.owner_id,&root.doc_id) else{return Ok(Vec::new());};
        let rows=self.member_metadata_rows(format!("owner_id='{}' AND starts_with(location_uri,'{}')",sql_escape(&root.owner_id),prefix)).await?;
        let mut selected=BTreeMap::<DocumentKey,(String,ContainerMemberLocator)>::new();
        for batch in rows {for row in 0..batch.num_rows(){
            let locator=row_locator(&batch,row)?.context("Stored member locator is missing")?;
            if locator.root_location_uri!=root_uri {continue;}
            ensure!(locator.root_owner_id==root.owner_id&&locator.root_doc_id==root.doc_id,"Stored member belongs to another root");
            let key=DocumentKey {owner_id:row_string(&batch,"owner_id",row)?.into(),doc_id:row_string(&batch,"doc_id",row)?.into()};
            let receipt=(row_string(&batch,"location_uri",row)?.to_owned(),locator);
            if let Some(previous)=selected.insert(key,receipt.clone()){ensure!(previous==receipt,"Stored member chunks disagree about their provenance");}
        }}
        Ok(selected.into_iter().map(|(key,(location_uri,_))|SourceRemoval{key,location_uri}).collect())
    }

    pub(super) async fn resolve_container_group(&self,batch:&IngestBatch)->Result<(Vec<IngestChunk>,Vec<SourceRemoval>)> {
        validate_container_batch(batch)?;
        let (mut normalized,mut removals)=self.resolve_source_identities(&batch.chunks).await?;
        let mut root_ids=BTreeMap::new();let mut old_roots=BTreeSet::new();
        for (original,resolved) in batch.chunks.iter().zip(&normalized) {
            if original.container_member.is_some()||original.source_identity_version!=Some(1){continue;}
            let original_key=(original.owner_id.clone(),original.doc_id.clone(),original.location_uri.clone());
            root_ids.insert(original_key,resolved.doc_id.clone());
            let key=DocumentKey {owner_id:original.owner_id.clone(),doc_id:resolved.doc_id.clone()};
            old_roots.insert((key,original.location_uri.clone()));
            for previous in self.keys_for_location(&original.location_uri).await? {
                if previous.owner_id==original.owner_id {old_roots.insert((previous,original.location_uri.clone()));}
            }
        }
        for chunk in &mut normalized {
            if let Some(locator)=&mut chunk.container_member {
                locator.root_doc_id=root_ids.get(&(locator.root_owner_id.clone(),locator.root_doc_id.clone(),locator.root_location_uri.clone()))
                    .cloned().context("Missing resolved container root identity")?;
                chunk.doc_id=locator.member_id().map_err(anyhow::Error::msg)?;
                chunk.location_uri=locator.member_uri().map_err(anyhow::Error::msg)?;
            }
        }
        let incoming:BTreeSet<_>=normalized.iter().filter(|chunk|chunk.container_member.is_some())
            .map(|chunk|(DocumentKey {owner_id:chunk.owner_id.clone(),doc_id:chunk.doc_id.clone()},chunk.location_uri.clone())).collect();
        for (key,uri) in old_roots {for old in self.member_removals(&key,&uri).await? {
            if !incoming.contains(&(old.key.clone(),old.location_uri.clone())) {removals.push(old);}
        }}
        Ok((normalized,removals))
    }

    pub(super) async fn expand_member_deletion_keys(&self,roots:&[DocumentKey])->Result<Vec<DocumentKey>> {
        let mut result:BTreeSet<_>=roots.iter().cloned().collect();
        let rows=self.member_metadata_rows(keys_sql(roots)).await?;
        let mut sources=BTreeSet::new();
        for batch in rows {for row in 0..batch.num_rows(){
            if row_locator(&batch,row)?.is_none() {
                sources.insert((DocumentKey {owner_id:row_string(&batch,"owner_id",row)?.into(),doc_id:row_string(&batch,"doc_id",row)?.into()},row_string(&batch,"location_uri",row)?.to_owned()));
            }
        }}
        for (root,uri) in sources {result.extend(self.member_removals(&root,&uri).await?.into_iter().map(|member|member.key));}
        Ok(result.into_iter().collect())
    }

    pub(super) async fn update_container_locations(&self,roots:&[DocumentKey],new_uri:&str,indexed_at:i64)->Result<()> {
        ensure!(!new_uri.starts_with("crisp+member:"),"A root cannot be moved into a virtual member URI");
        let keys=self.expand_member_deletion_keys(roots).await?;
        let root_set:BTreeSet<_>=roots.iter().cloned().collect();
        let rows=self.member_metadata_rows(keys_sql(&keys)).await?;
        let mut ids=Vec::new();let mut owners=Vec::new();let mut docs=Vec::new();let mut locations=Vec::new();let mut locators=Vec::new();
        for batch in rows {for row in 0..batch.num_rows(){
            let key=DocumentKey {owner_id:row_string(&batch,"owner_id",row)?.into(),doc_id:row_string(&batch,"doc_id",row)?.into()};
            let mut locator=row_locator(&batch,row)?;
            if root_set.contains(&key) {ensure!(locator.is_none(),"Move the root source to relocate an indexed member");locations.push(new_uri.to_owned());}
            else {
                let member=locator.as_mut().context("Stored descendant locator is missing")?;
                ensure!(root_set.contains(&DocumentKey {owner_id:member.root_owner_id.clone(),doc_id:member.root_doc_id.clone()}),"Stored member belongs to another root");
                member.root_location_uri=new_uri.to_owned();member.validate().map_err(anyhow::Error::msg)?;
                locations.push(row_string(&batch,"location_uri",row)?.to_owned());
            }
            ids.push(row_string(&batch,"id",row)?.to_owned());owners.push(key.owner_id);docs.push(key.doc_id);
            locators.push(locator.as_ref().map(serde_json::to_string).transpose()?);
        }}
        let count=ids.len();if count==0{return Ok(());}
        let source_schema=build_lance_schema(self.dims);
        let names=["id","owner_id","doc_id","location_uri","container_member","parent_dir","indexed_at"];
        let projection=names.iter().map(|name|source_schema.index_of(name)).collect::<std::result::Result<Vec<_>,_>>()?;
        let schema=Arc::new(source_schema.project(&projection)?);
        let batch=RecordBatch::try_new(schema.clone(),vec![Arc::new(StringArray::from(ids)),Arc::new(StringArray::from(owners)),Arc::new(StringArray::from(docs)),
            Arc::new(StringArray::from(locations)),Arc::new(StringArray::from(locators)),Arc::new(StringArray::from(vec![parent_directory(new_uri);count])),
            Arc::new(TimestampMillisecondArray::from(vec![indexed_at;count]))])?;
        let reader=arrow_array::RecordBatchIterator::new(vec![Ok(batch)],schema);
        let mut merge=self.table.merge_insert(&["id","owner_id","doc_id"]);merge.when_matched_update_all(None);
        merge.execute(Box::new(reader)).await.context("Moving root and member source metadata atomically")?;Ok(())
    }
}

impl SearchIndex {
    pub async fn read_container_member(&self,request:&ContainerMemberRequest)->Result<ContainerMemberResponse> {
        request.validate().map_err(anyhow::Error::msg)?;
        let _guard=self.read_guard().await?;
        let key=DocumentKey {owner_id:request.owner_id.clone(),doc_id:request.doc_id.clone()};
        let rows=self.vector.member_metadata_rows(keys_sql(&[key])).await?;
        ensure!(rows.iter().any(|batch|batch.num_rows()>0),"The selected indexed document no longer exists");
        let mut locator=None;let mut first=true;
        for batch in rows {for row in 0..batch.num_rows(){
            let current=row_locator(&batch,row)?;
            if first {locator=current;first=false;}else{ensure!(locator==current,"Stored member chunks disagree about their provenance");}
        }}
        let response=ContainerMemberResponse{version:CONTAINER_MEMBERS_VERSION,owner_id:request.owner_id.clone(),doc_id:request.doc_id.clone(),locator};
        response.validate_for(request).map_err(anyhow::Error::msg)?;Ok(response)
    }
}
