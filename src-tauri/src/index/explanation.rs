//! Inspect current lexical proof and semantic passage evidence without
//! substituting literal find matches for the executed query's semantics.
use crate::AppState;
use crisp_index_protocol::{QueryExplanation, QueryExplanationRequest, QUERY_EXPLANATION_VERSION};
use crisp_search_query::query_plan::{prepare_query, QueryIntent};
use tauri::State;
#[path = "../../../shared/query_explanation.rs"]
mod wire;

#[tauri::command]
pub async fn index_explain_passage(
    state: State<'_, AppState>,
    doc_id: String,
    owner_id: Option<String>,
    chunk_index: i32,
    query: String,
    mode: String,
    query_intent: Option<String>,
    fuzzy: Option<bool>,
    synonyms: Option<bool>,
) -> Result<QueryExplanation, String> {
    let query_intent = query_intent.unwrap_or_else(|| "advanced".into());
    let intent = QueryIntent::parse(&query_intent).map_err(|error| error.to_string())?;
    let fuzzy = fuzzy.unwrap_or(false);
    let synonyms = synonyms.unwrap_or(false);
    let lexical_query = if mode == "vector" {
        String::new()
    } else {
        prepare_query(&query, intent, fuzzy, synonyms)
            .map_err(|error| error.to_string())?
            .lexical_query
    };
    let mut request = QueryExplanationRequest {
        explanation_version: QUERY_EXPLANATION_VERSION,
        doc_id,
        owner_id: owner_id.clone().unwrap_or_default(),
        chunk_index,
        query,
        lexical_query,
        query_intent,
        mode,
        fuzzy,
        synonyms,
    };
    request.validate()?;
    let lock = state.index.lock().await;
    if !lock.config.enabled {
        return Err("Enable the search index to explain stored passages".into());
    }
    let config = lock.config.clone();
    let local = lock.local.clone();
    let fts = lock.fts.clone();
    drop(lock);
    if config.backend_type == super::BackendType::Remote {
        if owner_id.is_none() {
            return Err(
                "Select a search result with its exact owner to inspect remote evidence".into(),
            );
        }
        let proxy = crate::sync::tauri_commands::proxy_config_for_app_state(&state).await?;
        let remote = super::remote_client::RemoteClient::new_with_proxy(
            config.remote_url.ok_or("Remote index URL is missing")?,
            config.remote_api_key.unwrap_or_default(),
            &proxy,
        )
        .map_err(|error| error.to_string())?;
        return remote
            .explain_passage(&request)
            .await
            .map_err(|error| error.to_string());
    }
    let local = local.ok_or("The local index is not ready")?;
    let fts = fts.ok_or("The local text index is not ready")?;
    let _consistent = local
        .read_guard(&fts)
        .await
        .map_err(|error| error.to_string())?;
    let passage = super::evidence::passage::read_passage_scoped(
        local.table_ref(),
        &request.doc_id,
        owner_id.as_deref(),
        chunk_index,
    )
    .await
    .map_err(|error| error.to_string())?;
    request.owner_id = passage.owner_id.clone();
    let evidence = if request.mode == "vector" {
        None
    } else {
        Some(
            fts.explain_passage(
                &request.lexical_query,
                &request.owner_id,
                &request.doc_id,
                &passage.text,
                crisp_search_query::explanation::PassageEdges {
                    start: passage.start_char == Some(0),
                    end: passage.next_chunk_index.is_none() && !passage.truncated,
                },
            )
            .map_err(|error| error.to_string())?,
        )
    };
    wire::response(&request, passage, evidence).map_err(|error| error.to_string())
}
