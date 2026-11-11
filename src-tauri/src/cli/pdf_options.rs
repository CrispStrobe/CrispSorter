//! Bounded CLI recipes that execute the PDF editor's production edit operations.
use crate::operation_log::OperationLog;
use crate::pdf_session::{EditOp, PdfEditSession};
use serde::Serialize;
use serde_json::json;
use std::fs::File;
use std::io::Read;
use std::path::Path;

const MAX_RECIPE_BYTES: u64 = 1024 * 1024;
const MAX_OPERATIONS: usize = 1024;

fn read_recipe(reader: impl Read) -> Result<Vec<EditOp>, String> {
    let mut bytes = Vec::new();
    reader.take(MAX_RECIPE_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|error| format!("Read PDF edit recipe: {error}"))?;
    if bytes.len() as u64 > MAX_RECIPE_BYTES {
        return Err("PDF edit recipe exceeds the 1 MiB limit.".into());
    }
    let operations: Vec<EditOp> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid PDF edit recipe: {error}"))?;
    if operations.is_empty() || operations.len() > MAX_OPERATIONS {
        return Err("PDF edit recipe must contain between 1 and 1,024 operations.".into());
    }
    Ok(operations)
}

#[derive(Debug, Serialize)]
pub(super) struct ApplyReport {
    pub pages: usize,
    pub operations: usize,
    pub output: String,
}

/// Apply every edit in memory before publishing the complete output. A rejected
/// later operation cannot leave an earlier edit in the destination file.
pub(super) fn apply_recipe(input: &Path, recipe: &Path, output: &Path) -> Result<ApplyReport, String> {
    let log = OperationLog::start("pdf.recipe", json!({"surface": "cli"}));
    let result = (|| {
        log.step("loading_recipe", json!({}));
        let operations = read_recipe(File::open(recipe)
            .map_err(|error| format!("Open PDF edit recipe: {error}"))?)?;
        let count = operations.len();
        log.step("loading_document", json!({"count": count}));
        let mut session = PdfEditSession::open(input)?;
        for (index, operation) in operations.into_iter().enumerate() {
            log.step("applying", json!({"count": index + 1, "total": count,
                "mode": format!("pdf.{}", operation.label())}));
            session.apply(operation)?;
        }
        let pages = session.state("cli-recipe")?.info.page_count;
        log.step("publishing", json!({"pages": pages}));
        let target = session.save(Some(output))?;
        Ok(ApplyReport { pages, operations: count, output: target.to_string_lossy().into_owned() })
    })();
    match &result {
        Ok(report) => log.succeed(json!({"pages": report.pages, "count": report.operations, "saved": true})),
        Err(_) => log.fail("pdf_recipe_failed"),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Document, Object};

    fn document(path: &Path) {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Count" => 1, "Kids" => vec![Object::Reference(page_id)],
        }));
        let catalog = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
    }

    #[test]
    fn recipe_rejects_empty_unknown_and_oversized_input() {
        for bytes in [b"[]".as_slice(), br#"[{"kind":"unknown"}]"#, b"{}", b"[null]"] {
            assert!(read_recipe(bytes).is_err());
        }
        assert!(read_recipe(std::io::repeat(b' ')).unwrap_err().contains("1 MiB"));
        let too_many = vec![json!({"kind":"rotate", "pages":[0], "degrees":90}); MAX_OPERATIONS + 1];
        assert!(read_recipe(serde_json::to_vec(&too_many).unwrap().as_slice()).unwrap_err().contains("1,024"));
    }

    #[test]
    fn recipe_executes_editor_annotations_and_preserves_input() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("private-source.pdf");
        let target = root.path().join("private-output.pdf");
        let recipe = root.path().join("private-recipe.json");
        document(&source);
        let original = std::fs::read(&source).unwrap();
        std::fs::write(&recipe, serde_json::to_vec(&json!([
            {"kind":"rotate", "pages":[0], "degrees":90},
            {"kind":"annotate", "annotations":[{
                "page":0, "x":10, "y":10, "w":40, "h":20,
                "ann_type":"rectangle", "text":"private-note-sentinel",
                "color":"#ff0000", "author":null, "quads":[]
            }]}
        ])).unwrap()).unwrap();
        let (result, events) = crate::operation_log::capture(|| apply_recipe(&source, &recipe, &target));
        let report = result.unwrap();
        assert_eq!((report.pages, report.operations), (1, 2));
        assert_eq!(crate::pdf_ops::pdf_info(&target).unwrap().pages[0].rotation, 90);
        let annotations = crate::pdf_annots::read_annotations_from_path(&target).unwrap();
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].text, "private-note-sentinel");
        assert_eq!(std::fs::read(&source).unwrap(), original);
        let receipts: Vec<_> = events.iter().filter(|event| event.operation == "pdf.recipe").collect();
        assert_eq!(receipts.first().unwrap().phase, "started");
        assert_eq!(receipts.last().unwrap().phase, "completed");
        assert_eq!(receipts.iter().filter(|event| ["completed", "failed", "cancelled"].contains(&event.phase.as_str())).count(), 1);
        let encoded = serde_json::to_string(&events).unwrap();
        for private in ["private-source", "private-output", "private-recipe", "private-note-sentinel"] {
            assert!(!encoded.contains(private), "receipt disclosed {private}");
        }
    }

    #[test]
    fn rejected_later_edit_preserves_existing_output_and_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.pdf");
        let target = root.path().join("output.pdf");
        let recipe = root.path().join("recipe.json");
        document(&source);
        let original = std::fs::read(&source).unwrap();
        std::fs::write(&target, b"previous complete output").unwrap();
        std::fs::write(&recipe, br#"[
            {"kind":"rotate","pages":[0],"degrees":90},
            {"kind":"remove_pages","pages":[0]}
        ]"#).unwrap();
        let (result, events) = crate::operation_log::capture(|| apply_recipe(&source, &recipe, &target));
        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"previous complete output");
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(!events.iter().any(|event| event.operation == "output.publish"));
        let receipt = events.iter().rev().find(|event| event.operation == "pdf.recipe").unwrap();
        assert_eq!(receipt.phase, "failed");
        assert_eq!(receipt.code.as_deref(), Some("pdf_recipe_failed"));
    }
}
