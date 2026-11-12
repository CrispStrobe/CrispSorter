//! Path resolution proves a unique current parent/name match, not historical
//! identity. Duplicate undo needs a durable deleted-object receipt separately.

use anyhow::{anyhow, Result};

pub(super) fn path_query(parent_id: &str, name: &str, trashed: bool, folder: bool) -> String {
    let parent = parent_id.replace('\\', "\\\\").replace('\'', "\\'");
    let name = name.replace('\\', "\\\\").replace('\'', "\\'");
    let folder_filter = if folder {
        " and mimeType = 'application/vnd.google-apps.folder'"
    } else {
        ""
    };
    format!("'{parent}' in parents and name = '{name}' and trashed = {trashed}{folder_filter}")
}

pub(super) fn unique_file_id(body: &serde_json::Value) -> Result<String> {
    let files = body["files"]
        .as_array()
        .ok_or_else(|| anyhow!("Google Drive resolution: missing files"))?;
    let paginated = body
        .get("nextPageToken")
        .is_some_and(|token| !token.is_null() && token != "");
    anyhow::ensure!(
        !paginated && body["incompleteSearch"] != true,
        "Google Drive resolution is incomplete; refusing ambiguous identity"
    );
    anyhow::ensure!(
        files.len() <= 1,
        "Google Drive path is ambiguous; use its object ID in the provider client"
    );
    let file = files
        .first()
        .ok_or_else(|| anyhow!("Google Drive path not found"))?;
    let id = file["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("Google Drive resolution: missing object ID"))?;
    Ok(id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn live_ancestors_are_folder_only_and_trashed_leaf_retains_exact_parent() {
        assert_eq!(path_query("root", "Docs", false, true),
            "'root' in parents and name = 'Docs' and trashed = false and mimeType = 'application/vnd.google-apps.folder'");
        assert_eq!(
            path_query("folder-id", "report.pdf", true, false),
            "'folder-id' in parents and name = 'report.pdf' and trashed = true"
        );
    }

    #[test]
    fn query_literals_escape_quotes_and_backslashes() {
        assert_eq!(
            path_query("parent'id", "a\\b'c", false, false),
            "'parent\\'id' in parents and name = 'a\\\\b\\'c' and trashed = false"
        );
    }

    #[test]
    fn a_unique_complete_match_preserves_object_id() {
        assert_eq!(
            unique_file_id(&json!({"files":[{"id":"exact-object"}]})).unwrap(),
            "exact-object"
        );
    }

    #[test]
    fn duplicate_names_cannot_select_the_first_object() {
        let error = unique_file_id(&json!({"files":[{"id":"one"},{"id":"two"}]})).unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn incomplete_results_cannot_prove_uniqueness() {
        for body in [
            json!({"files":[{"id":"one"}],"nextPageToken":"more"}),
            json!({"files":[{"id":"one"}],"incompleteSearch":true}),
        ] {
            assert!(unique_file_id(&body)
                .unwrap_err()
                .to_string()
                .contains("incomplete"));
        }
    }

    #[test]
    fn missing_or_malformed_identity_is_rejected() {
        for body in [
            json!({}),
            json!({"files":[]}),
            json!({"files":[{}]}),
            json!({"files":[{"id":""}]}),
        ] {
            assert!(unique_file_id(&body).is_err());
        }
    }
}
