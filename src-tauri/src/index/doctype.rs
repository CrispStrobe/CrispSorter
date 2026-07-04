//! Document-type classification at ingest (P26.1).
//!
//! Heuristic classifier that assigns a document type based on file
//! extension, page count, and text content patterns.  Stores the
//! result as a `doctype:<class>` tag on the document.
//!
//! Classes: letter, invoice, form, email, report, specification,
//! presentation, spreadsheet, image, audio, video, ebook, code,
//! article, contract, receipt, memo, unknown.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum DocType {
    Letter,
    Invoice,
    Receipt,
    Form,
    Email,
    Report,
    Specification,
    Presentation,
    Spreadsheet,
    Image,
    Audio,
    Video,
    Ebook,
    Code,
    Article,
    Contract,
    Memo,
    Unknown,
}

impl DocType {
    pub fn as_tag(&self) -> String {
        format!("doctype:{}", self.as_str())
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Letter => "letter",
            Self::Invoice => "invoice",
            Self::Receipt => "receipt",
            Self::Form => "form",
            Self::Email => "email",
            Self::Report => "report",
            Self::Specification => "specification",
            Self::Presentation => "presentation",
            Self::Spreadsheet => "spreadsheet",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Ebook => "ebook",
            Self::Code => "code",
            Self::Article => "article",
            Self::Contract => "contract",
            Self::Memo => "memo",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify a document based on its extension, text content, and metadata.
pub fn classify(
    ext: &str,
    text: &str,
    _title: Option<&str>,
    page_count: Option<usize>,
) -> DocType {
    let ext_lower = ext.to_lowercase();
    let text_len = text.len();

    // 1. Extension-based classification (high confidence)
    // These early-returns avoid the expensive text.to_lowercase() below.
    match ext_lower.as_str() {
        "eml" | "msg" | "mbox" => return DocType::Email,
        "epub" | "mobi" | "azw3" | "fb2" => return DocType::Ebook,
        "pptx" | "ppt" | "odp" | "key" => return DocType::Presentation,
        "xlsx" | "xls" | "ods" | "numbers" => return DocType::Spreadsheet,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tiff" | "tif" |
        "heic" | "heif" | "ico" | "avif" => return DocType::Image,
        "mp3" | "wav" | "flac" | "ogg" | "opus" | "m4a" | "aac" | "wma" => return DocType::Audio,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => return DocType::Video,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "kt" | "swift" |
        "c" | "cpp" | "h" | "hpp" | "rb" | "php" | "lua" | "sh" | "bash" | "zsh" |
        "sql" | "graphql" | "svelte" | "vue" => return DocType::Code,
        _ => {}
    }

    // 2. Content-based classification for PDFs and text documents
    if text_len < 50 {
        return DocType::Unknown;
    }

    // Deferred: only allocate the lowercase copy when we actually need
    // content-based signal matching (skipped for all extension-matched types).
    let text_lower = text.to_lowercase();

    // Invoice / receipt detection
    let invoice_signals = [
        "invoice", "rechnung", "faktura", "bill to", "due date",
        "payment", "total amount", "subtotal", "tax", "vat",
        "invoice number", "rechnungsnummer", "fällig",
    ];
    let invoice_score: usize = invoice_signals.iter()
        .filter(|s| text_lower.contains(*s))
        .count();
    if invoice_score >= 3 {
        // Distinguish invoice from receipt by length
        if text_len < 500 || text_lower.contains("receipt") || text_lower.contains("quittung") {
            return DocType::Receipt;
        }
        return DocType::Invoice;
    }

    // Contract detection
    let contract_signals = [
        "agreement", "contract", "vertrag", "vereinbarung",
        "party", "parties", "whereas", "hereby", "obligations",
        "termination", "governing law", "jurisdiction",
        "shall", "binding", "effective date",
    ];
    let contract_score: usize = contract_signals.iter()
        .filter(|s| text_lower.contains(*s))
        .count();
    if contract_score >= 4 {
        return DocType::Contract;
    }

    // Letter / memo detection
    let letter_signals = [
        "dear ", "sincerely", "regards", "yours", "to whom",
        "re:", "subject:", "sehr geehrte", "mit freundlichen",
        "hochachtungsvoll",
    ];
    let letter_score: usize = letter_signals.iter()
        .filter(|s| text_lower.contains(*s))
        .count();
    if letter_score >= 2 {
        if text_len < 2000 || text_lower.contains("memo") || text_lower.contains("memorandum") {
            return DocType::Memo;
        }
        return DocType::Letter;
    }

    // Form detection
    let form_signals = [
        "please fill", "check box", "checkbox", "signature:",
        "date:", "name:", "address:", "applicant", "application form",
        "formular", "ausfüllen", "unterschrift",
    ];
    let form_score: usize = form_signals.iter()
        .filter(|s| text_lower.contains(*s))
        .count();
    if form_score >= 2 {
        return DocType::Form;
    }

    // Specification / technical document
    let spec_signals = [
        "specification", "requirements", "scope", "revision",
        "version history", "table of contents", "appendix",
        "spezifikation", "anforderung", "lastenheft", "pflichtenheft",
    ];
    let spec_score: usize = spec_signals.iter()
        .filter(|s| text_lower.contains(*s))
        .count();
    if spec_score >= 3 {
        return DocType::Specification;
    }

    // Report (long document with structured content)
    let pages = page_count.unwrap_or(0);
    if pages >= 5 || text_len > 10000 {
        let report_signals = [
            "executive summary", "conclusion", "findings",
            "methodology", "results", "analysis", "recommendation",
            "zusammenfassung", "ergebnis", "empfehlung",
        ];
        let report_score: usize = report_signals.iter()
            .filter(|s| text_lower.contains(*s))
            .count();
        if report_score >= 2 {
            return DocType::Report;
        }
    }

    // Article (academic / news)
    let article_signals = [
        "abstract", "introduction", "references", "bibliography",
        "doi:", "issn", "journal", "published", "peer-reviewed",
    ];
    let article_score: usize = article_signals.iter()
        .filter(|s| text_lower.contains(*s))
        .count();
    if article_score >= 2 {
        return DocType::Article;
    }

    // Default: report for long docs, unknown for short
    if pages >= 3 || text_len > 5000 {
        DocType::Report
    } else {
        DocType::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_extension() {
        assert_eq!(classify("eml", "", None, None), DocType::Email);
        assert_eq!(classify("epub", "", None, None), DocType::Ebook);
        assert_eq!(classify("pptx", "", None, None), DocType::Presentation);
        assert_eq!(classify("xlsx", "", None, None), DocType::Spreadsheet);
        assert_eq!(classify("jpg", "", None, None), DocType::Image);
        assert_eq!(classify("mp3", "", None, None), DocType::Audio);
        assert_eq!(classify("mp4", "", None, None), DocType::Video);
        assert_eq!(classify("rs", "", None, None), DocType::Code);
        assert_eq!(classify("py", "", None, None), DocType::Code);
    }

    #[test]
    fn classify_invoice() {
        let text = format!("INVOICE\n\nInvoice Number: 12345\nBill To: Acme Corp\n123 Business Ave\nDue Date: 2026-01-15\n\nItem 1: Widget A - $200.00\nItem 2: Widget B - $300.00\n\nSubtotal: $500.00\nTax (10%): $50.00\nTotal Amount: $550.00\n\nPayment Terms: Net 30\nPayment due within 30 days. Please remit payment to our billing department.\n\n{}", "Additional terms and conditions apply. ".repeat(10));
        assert_eq!(classify("pdf", &text, None, Some(1)), DocType::Invoice);
    }

    #[test]
    fn classify_receipt() {
        let text = "Receipt for your purchase. Invoice #99. Total: $12.50. Tax: $1.00. Payment received. Thank you for your business. Have a great day!";
        assert_eq!(classify("pdf", text, None, Some(1)), DocType::Receipt);
    }

    #[test]
    fn classify_contract() {
        let text = "SERVICE AGREEMENT\nThis Agreement is entered into by and between the parties. Whereas the parties agree to the following obligations. This contract shall be binding. Termination may occur under governing law. The effective date is January 1, 2026. Jurisdiction is Germany.";
        assert_eq!(classify("pdf", text, None, Some(3)), DocType::Contract);
    }

    #[test]
    fn classify_letter() {
        // Must be > 2000 chars to not be classified as memo
        let text = format!("Dear Mr. Smith,\n\nI am writing to inform you about the upcoming changes to our company policy. Please review the attached documents at your earliest convenience and let us know if you have any questions.\n\n{}\n\nSincerely,\nJane Doe\nDirector of Operations", "This is an important matter that requires your attention. ".repeat(40));
        assert_eq!(classify("pdf", &text, None, Some(1)), DocType::Letter);
    }

    #[test]
    fn classify_memo() {
        let text = "MEMORANDUM\n\nDear Team,\n\nThis is a memo to inform you about the upcoming office renovations scheduled for next month. Please clear your desks by Friday.\n\nRegards,\nManagement";
        assert_eq!(classify("pdf", text, None, Some(1)), DocType::Memo);
    }

    #[test]
    fn classify_article() {
        let text = "Abstract: This paper presents a novel approach to machine learning. Introduction: We review related work. The DOI: 10.1234/example. See references and bibliography at the end.";
        assert_eq!(classify("pdf", text, None, Some(10)), DocType::Article);
    }

    #[test]
    fn classify_report() {
        let long_text = "Executive Summary: This report presents the findings of our analysis. The methodology involved surveys. Results show significant improvement. Our recommendation is to proceed. Conclusion: The project was successful.".to_string() + &" more text".repeat(500);
        assert_eq!(classify("pdf", &long_text, None, Some(20)), DocType::Report);
    }

    #[test]
    fn classify_short_unknown() {
        assert_eq!(classify("pdf", "hello", None, Some(1)), DocType::Unknown);
    }

    #[test]
    fn classify_form() {
        let text = "Application Form\nPlease fill in all fields.\nName: ___________\nAddress: ___________\nDate: ___________\nSignature: ___________";
        assert_eq!(classify("pdf", text, None, Some(1)), DocType::Form);
    }

    #[test]
    fn as_tag_format() {
        assert_eq!(DocType::Invoice.as_tag(), "doctype:invoice");
        assert_eq!(DocType::Contract.as_tag(), "doctype:contract");
    }

    #[test]
    fn classify_specification() {
        let text = "Technical Specification v2.3\n\nRevision History\n\nTable of Contents\n1. Scope\n2. Requirements\n3. Appendix\n\nThis specification defines the requirements for the new system architecture.";
        assert_eq!(classify("pdf", text, None, Some(5)), DocType::Specification);
    }

    #[test]
    fn classify_german_invoice() {
        let text = format!("Rechnung\n\nRechnungsnummer: DE-2026-001\nFällig: 15.01.2026\nZwischensumme: 500,00 EUR\nMehrwertsteuer: 95,00 EUR\nGesamtbetrag: 595,00 EUR\n\n{}", "Zahlungsbedingungen gelten. ".repeat(15));
        assert_eq!(classify("pdf", &text, None, Some(1)), DocType::Invoice);
    }

    #[test]
    fn classify_german_letter() {
        let text = format!("Sehr geehrte Frau Müller,\n\nhiermit möchte ich Ihnen mitteilen, dass wir Ihren Antrag geprüft haben. {}\n\nMit freundlichen Grüßen,\nDr. Schmidt", "Bitte beachten Sie die beigefügten Unterlagen. ".repeat(40));
        assert_eq!(classify("pdf", &text, None, Some(1)), DocType::Letter);
    }

    #[test]
    fn classify_empty_text() {
        assert_eq!(classify("pdf", "", None, None), DocType::Unknown);
    }

    #[test]
    fn classify_all_extensions_covered() {
        // Verify all media extensions produce expected types
        assert_eq!(classify("msg", "", None, None), DocType::Email);
        assert_eq!(classify("mbox", "", None, None), DocType::Email);
        assert_eq!(classify("mobi", "", None, None), DocType::Ebook);
        assert_eq!(classify("ppt", "", None, None), DocType::Presentation);
        assert_eq!(classify("xls", "", None, None), DocType::Spreadsheet);
        assert_eq!(classify("wav", "", None, None), DocType::Audio);
        assert_eq!(classify("mkv", "", None, None), DocType::Video);
        assert_eq!(classify("go", "", None, None), DocType::Code);
        assert_eq!(classify("java", "", None, None), DocType::Code);
        assert_eq!(classify("svg", "", None, None), DocType::Image);
    }

    #[test]
    fn as_str_roundtrip() {
        // Verify all variants have valid str representations
        let types = [
            DocType::Letter, DocType::Invoice, DocType::Receipt, DocType::Form,
            DocType::Email, DocType::Report, DocType::Specification,
            DocType::Presentation, DocType::Spreadsheet, DocType::Image,
            DocType::Audio, DocType::Video, DocType::Ebook, DocType::Code,
            DocType::Article, DocType::Contract, DocType::Memo, DocType::Unknown,
        ];
        for t in &types {
            assert!(!t.as_str().is_empty());
            assert!(t.as_tag().starts_with("doctype:"));
        }
    }
}