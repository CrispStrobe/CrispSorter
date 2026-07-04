//! Auto-classify + auto-file logic for WatchMode::AutoFile (P26.2).
//!
//! When a file is detected in a watched folder with AutoFile mode:
//! 1. Extract text (quick pass — first 1000 chars)
//! 2. Classify via P26.1 doctype heuristic
//! 3. Look up the sort-rule template for that doctype
//! 4. Build the destination path from the template
//! 5. Move/copy the file

use crate::index::doctype::classify;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Sort-rule template: maps a doctype to a destination path pattern.
/// Pattern variables: `{doctype}`, `{year}`, `{filename}`, `{ext}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortRule {
    pub doctype: String,
    pub pattern: String,
}

/// Default sort rules — used when no custom rules are configured.
pub fn default_rules() -> Vec<SortRule> {
    vec![
        SortRule { doctype: "invoice".into(), pattern: "Invoices/{year}/{filename}".into() },
        SortRule { doctype: "receipt".into(), pattern: "Receipts/{year}/{filename}".into() },
        SortRule { doctype: "contract".into(), pattern: "Contracts/{filename}".into() },
        SortRule { doctype: "letter".into(), pattern: "Correspondence/{year}/{filename}".into() },
        SortRule { doctype: "memo".into(), pattern: "Correspondence/{year}/{filename}".into() },
        SortRule { doctype: "report".into(), pattern: "Reports/{year}/{filename}".into() },
        SortRule { doctype: "article".into(), pattern: "Articles/{year}/{filename}".into() },
        SortRule { doctype: "form".into(), pattern: "Forms/{filename}".into() },
        SortRule { doctype: "specification".into(), pattern: "Specifications/{filename}".into() },
        SortRule { doctype: "email".into(), pattern: "Email/{year}/{filename}".into() },
        SortRule { doctype: "ebook".into(), pattern: "Books/{filename}".into() },
        SortRule { doctype: "image".into(), pattern: "Images/{year}/{filename}".into() },
        SortRule { doctype: "audio".into(), pattern: "Media/Audio/{filename}".into() },
        SortRule { doctype: "video".into(), pattern: "Media/Video/{filename}".into() },
        SortRule { doctype: "code".into(), pattern: "Code/{filename}".into() },
        SortRule { doctype: "presentation".into(), pattern: "Presentations/{filename}".into() },
        SortRule { doctype: "spreadsheet".into(), pattern: "Spreadsheets/{filename}".into() },
    ]
}

/// Build a rule lookup map from a list of rules.
pub fn rule_map(rules: &[SortRule]) -> HashMap<String, String> {
    rules.iter().map(|r| (r.doctype.clone(), r.pattern.clone())).collect()
}

/// Determine the destination path for a file based on its doctype.
/// Returns `None` if no rule matches (file stays in place).
pub fn resolve_destination(
    file_path: &Path,
    base_dir: &Path,
    ext: &str,
    text: &str,
    rules: &HashMap<String, String>,
) -> Option<PathBuf> {
    let doctype = classify(ext, text, None, None);
    let pattern = rules.get(doctype.as_str())?;

    let filename = file_path.file_name()?.to_string_lossy();
    let year = chrono_year();

    let resolved = pattern
        .replace("{doctype}", doctype.as_str())
        .replace("{year}", &year)
        .replace("{filename}", &filename)
        .replace("{ext}", ext);

    Some(base_dir.join(resolved))
}

fn chrono_year() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Approximate year from epoch seconds
    let year = 1970 + (now / 31_557_600); // ~365.25 days
    year.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rules_cover_common_types() {
        let rules = default_rules();
        assert!(rules.iter().any(|r| r.doctype == "invoice"));
        assert!(rules.iter().any(|r| r.doctype == "contract"));
        assert!(rules.iter().any(|r| r.doctype == "email"));
        assert!(rules.len() >= 15);
    }

    #[test]
    fn rule_map_lookup() {
        let rules = default_rules();
        let map = rule_map(&rules);
        assert!(map.contains_key("invoice"));
        assert!(map.get("invoice").unwrap().contains("Invoices"));
    }

    #[test]
    fn resolve_destination_invoice() {
        let rules = rule_map(&default_rules());
        let text = format!("INVOICE\n\nInvoice Number: 12345\nBill To: Acme Corp\nDue Date: 2026-01-15\nSubtotal: $500.00\nTax: $50.00\nTotal Amount: $550.00\nPayment due within 30 days. Please remit payment to billing.\n\n{}", "Terms and conditions apply. ".repeat(15));
        let dest = resolve_destination(
            Path::new("/inbox/bill.pdf"),
            Path::new("/sorted"),
            "pdf",
            &text,
            &rules,
        );
        assert!(dest.is_some());
        let d = dest.unwrap();
        assert!(d.to_string_lossy().contains("Invoices"));
        assert!(d.to_string_lossy().contains("bill.pdf"));
    }

    #[test]
    fn resolve_destination_no_match() {
        let rules = rule_map(&default_rules());
        let dest = resolve_destination(
            Path::new("/inbox/random.pdf"),
            Path::new("/sorted"),
            "pdf",
            "short",
            &rules,
        );
        // "unknown" doctype has no rule → None
        assert!(dest.is_none());
    }

    #[test]
    fn resolve_destination_by_extension() {
        let rules = rule_map(&default_rules());
        let dest = resolve_destination(
            Path::new("/inbox/photo.jpg"),
            Path::new("/sorted"),
            "jpg",
            "",
            &rules,
        );
        assert!(dest.is_some());
        assert!(dest.unwrap().to_string_lossy().contains("Images"));
    }

    #[test]
    fn resolve_destination_email() {
        let rules = rule_map(&default_rules());
        let dest = resolve_destination(
            Path::new("/inbox/msg.eml"),
            Path::new("/sorted"),
            "eml",
            "",
            &rules,
        );
        assert!(dest.is_some());
        assert!(dest.unwrap().to_string_lossy().contains("Email"));
    }

    #[test]
    fn year_is_reasonable() {
        let y: u64 = chrono_year().parse().unwrap();
        assert!(y >= 2025 && y <= 2030);
    }

    #[test]
    fn resolve_destination_filename_with_spaces() {
        let rules = rule_map(&default_rules());
        let dest = resolve_destination(
            Path::new("/inbox/my contract.pdf"),
            Path::new("/sorted"),
            "eml",
            "",
            &rules,
        );
        assert!(dest.is_some());
        assert!(dest.unwrap().to_string_lossy().contains("my contract.pdf"));
    }

    #[test]
    fn rule_map_duplicate_last_wins() {
        let rules = vec![
            SortRule { doctype: "invoice".into(), pattern: "First/{filename}".into() },
            SortRule { doctype: "invoice".into(), pattern: "Second/{filename}".into() },
        ];
        let map = rule_map(&rules);
        assert_eq!(map.get("invoice").unwrap(), "Second/{filename}");
    }

    #[test]
    fn resolve_destination_code_by_ext() {
        let rules = rule_map(&default_rules());
        let dest = resolve_destination(
            Path::new("/inbox/main.rs"),
            Path::new("/sorted"),
            "rs",
            "",
            &rules,
        );
        assert!(dest.is_some());
        assert!(dest.unwrap().to_string_lossy().contains("Code"));
    }
}
