//! Size reduction: stream compression and object streams (P32.6).
//!
//! This was the other thing the qpdf plan was for, and like AES-256 it
//! turned out to be available in pure Rust — lopdf ships both halves:
//!
//! * **Stream compression.** `Stream::compress()` deflates any content
//!   stream that has no `/Filter` yet. PDF writers that emit uncompressed
//!   content are common (ours included, and MuPDF's `insert_text` output),
//!   so this alone often takes a large bite.
//! * **Object streams** (`/ObjStm`, PDF 1.5+). Non-stream objects — the
//!   dictionaries and arrays that make up the page tree, annotations,
//!   outlines — get packed into a single deflated stream instead of
//!   sitting in the file one per `obj`/`endobj` pair. On a document with
//!   many small objects this is the bigger win of the two.
//!
//! ## What is deliberately not touched
//!
//! * **Images.** Recompressing them means decoding and re-encoding, which
//!   is either lossless-and-pointless or lossy-and-surprising. A caller
//!   who wants smaller images should downsample them on purpose.
//! * **Already-filtered streams.** If a stream declares a `/Filter` we
//!   leave it: re-deflating deflated data grows it.
//! * **Objects that must stay reachable directly.** The catalog, the
//!   `/Encrypt` dictionary, anything referenced from the trailer, and
//!   streams themselves cannot live in an object stream; lopdf's
//!   `can_be_compressed` decides, and we defer to it rather than
//!   second-guessing the rules.

use lopdf::{Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressOptions {
    /// Deflate content streams that carry no `/Filter`.
    pub compress_streams: bool,
    /// Pack eligible non-stream objects into `/ObjStm` object streams.
    ///
    /// Requires a PDF 1.5 reader. Every mainstream viewer since ~2003
    /// qualifies, but it does raise the version floor, so it is a choice
    /// rather than a default-on.
    pub use_object_streams: bool,
    /// Objects per object stream. lopdf's default is a reasonable
    /// trade-off between compression and the cost of decoding one stream
    /// to reach a single object.
    pub max_objects_per_stream: Option<usize>,
}

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            compress_streams: true,
            use_object_streams: true,
            max_objects_per_stream: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompressReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Content streams deflated.
    pub streams_compressed: usize,
    /// Whether the output was written with object + xref streams.
    pub used_object_streams: bool,
}

impl CompressReport {
    /// Saved fraction, 0.0–1.0. Zero when the file grew.
    pub fn savings_ratio(&self) -> f64 {
        if self.bytes_before == 0 || self.bytes_after >= self.bytes_before {
            return 0.0;
        }
        1.0 - (self.bytes_after as f64 / self.bytes_before as f64)
    }
}

/// Deflate every stream that does not already declare a `/Filter`.
///
/// Counts streams that came out deflated, not calls that returned `Ok`.
/// `Stream::compress()` is a no-op in *two* cases and reports success in
/// both: when a `/Filter` is already set, and — the one that is easy to
/// miss — when deflating would not save at least 19 bytes, which is the
/// normal outcome for the short content streams a one-line page has. An
/// earlier version counted the call and reported 12 streams compressed on
/// a document where it had compressed none.
fn compress_streams(doc: &mut Document) -> usize {
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    let mut n = 0;
    for id in ids {
        if let Some(Object::Stream(ref mut s)) = doc.objects.get_mut(&id) {
            if s.dict.get(b"Filter").is_ok() {
                continue;
            }
            // The cross-reference and object streams are rebuilt by the
            // writer, which throws away whatever we did to them — so
            // deflating one is work that never reaches the file, and
            // counting it is a report of something that did not happen.
            if matches!(
                s.dict.get(b"Type").and_then(|t| t.as_name()),
                Ok(b"XRef") | Ok(b"ObjStm")
            ) {
                continue;
            }
            let _ = s.compress();
            if s.dict.get(b"Filter").is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// Compress a PDF, reporting what actually changed.
///
/// The result is verified before being reported as a success: a smaller
/// file that no longer parses is not a win.
pub fn compress_pdf(
    path: &Path,
    options: &CompressOptions,
    out_path: &Path,
) -> Result<CompressReport, String> {
    let bytes_before = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .len();

    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let pages_before = doc.page_iter().count();

    let mut report = CompressReport { bytes_before, ..Default::default() };

    if options.compress_streams {
        report.streams_compressed = compress_streams(&mut doc);
    }
    // Object streams are lopdf's job, not ours. Moving objects into an
    // /ObjStm by hand and then saving with a classic xref table leaves
    // them unreachable — every page vanished, which the verification
    // below caught. `save_with_options` writes the xref *stream* that
    // makes packed objects findable.
    if options.use_object_streams {
        let mut b = lopdf::SaveOptions::builder();
        b = b.use_object_streams(true).use_xref_streams(true);
        if let Some(m) = options.max_objects_per_stream {
            b = b.max_objects_per_stream(m);
        }
        let opts = b.build();
        // Object streams and xref streams both require a PDF 1.5 reader.
        if doc.version.as_str() < "1.5" {
            doc.version = "1.5".to_string();
        }
        let mut f = std::fs::File::create(out_path)
            .map_err(|e| format!("create {}: {e}", out_path.display()))?;
        doc.save_with_options(&mut f, opts)
            .map_err(|e| format!("save: {e}"))?;
        report.used_object_streams = true;
    } else {
        doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    }

    // Verify: same page count, still parseable. Compression that quietly
    // drops content is worse than no compression.
    match Document::load(out_path) {
        Ok(check) => {
            let pages_after = check.page_iter().count();
            if pages_after != pages_before {
                let _ = std::fs::remove_file(out_path);
                return Err(format!(
                    "compression changed the page count ({pages_before} → {pages_after}); \
                     output discarded"
                ));
            }
            if check.catalog().is_err() {
                let _ = std::fs::remove_file(out_path);
                return Err("compressed output has no reachable catalog; discarded".into());
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(out_path);
            return Err(format!("compressed output does not parse ({e}); discarded"));
        }
    }

    report.bytes_after = std::fs::metadata(out_path)
        .map_err(|e| format!("stat {}: {e}", out_path.display()))?
        .len();
    Ok(report)
}

pub mod tauri_commands {
    use super::*;

    #[tauri::command]
    pub async fn pdf_compress(
        path: String,
        options: Option<CompressOptions>,
        out_path: String,
    ) -> Result<CompressReport, String> {
        let opts = options.unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            compress_pdf(Path::new(&path), &opts, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A document with several uncompressed content streams and enough
    /// small objects for object streams to be worth doing.
    fn make_doc(pages: usize) -> Document {
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Helvetica".to_vec())),
        ])));
        let mut kids = Vec::new();
        for i in 0..pages {
            // Long, highly repetitive content so deflate has something to do.
            let text = format!("(page {i} {}) Tj", "compressible ".repeat(40));
            let content = format!("BT /F1 12 Tf 72 700 Td {text} ET");
            let cid = doc.add_object(Object::Stream(lopdf::Stream::new(
                lopdf::Dictionary::new(),
                content.into_bytes(),
            )));
            let mut fonts = lopdf::Dictionary::new();
            fonts.set("F1", Object::Reference(font_id));
            let mut res = lopdf::Dictionary::new();
            res.set("Font", Object::Dictionary(fonts));
            let page = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(pages_id)),
                ("MediaBox", Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Integer(612), Object::Integer(792),
                ])),
                ("Resources", Object::Dictionary(res)),
                ("Contents", Object::Reference(cid)),
            ])));
            kids.push(Object::Reference(page));
        }
        doc.objects.insert(pages_id, Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Count", Object::Integer(pages as i64)),
            ("Kids", Object::Array(kids)),
        ])));
        let cat = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Catalog".to_vec())),
            ("Pages", Object::Reference(pages_id)),
        ])));
        doc.trailer.set("Root", Object::Reference(cat));
        doc
    }

    fn write(doc: &mut Document, dir: &TempDir, name: &str) -> std::path::PathBuf {
        let p = dir.path().join(name);
        doc.save(&p).unwrap();
        p
    }

    #[test]
    fn compression_makes_the_file_smaller() {
        let dir = TempDir::new().unwrap();
        let src = write(&mut make_doc(8), &dir, "in.pdf");
        let out = dir.path().join("out.pdf");
        let r = compress_pdf(&src, &CompressOptions::default(), &out).unwrap();
        assert!(r.bytes_after < r.bytes_before,
                "{} → {}", r.bytes_before, r.bytes_after);
        assert!(r.savings_ratio() > 0.0);
    }

    #[test]
    fn streams_are_deflated() {
        let dir = TempDir::new().unwrap();
        let src = write(&mut make_doc(4), &dir, "in.pdf");
        let out = dir.path().join("out.pdf");
        let opts = CompressOptions { use_object_streams: false, ..Default::default() };
        let r = compress_pdf(&src, &opts, &out).unwrap();
        assert!(r.streams_compressed >= 4, "{r:?}");
        // Every content stream should now declare a filter.
        let doc = Document::load(&out).unwrap();
        for page_id in doc.page_iter() {
            if let Ok(Object::Dictionary(p)) = doc.get_object(page_id) {
                if let Ok(Object::Reference(r)) = p.get(b"Contents") {
                    if let Ok(Object::Stream(s)) = doc.get_object(*r) {
                        assert!(s.dict.get(b"Filter").is_ok(), "stream left uncompressed");
                    }
                }
            }
        }
    }

    #[test]
    fn object_streams_are_created_and_bump_the_version() {
        let dir = TempDir::new().unwrap();
        let src = write(&mut make_doc(10), &dir, "in.pdf");
        let out = dir.path().join("out.pdf");
        let opts = CompressOptions { compress_streams: false, ..Default::default() };
        let r = compress_pdf(&src, &opts, &out).unwrap();
        assert!(r.used_object_streams, "{r:?}");
        let doc = Document::load(&out).unwrap();
        assert!(doc.version.as_str() >= "1.5", "version not raised: {}", doc.version);
    }

    #[test]
    fn the_document_still_reads_after_compression() {
        let dir = TempDir::new().unwrap();
        let src = write(&mut make_doc(6), &dir, "in.pdf");
        let out = dir.path().join("out.pdf");
        compress_pdf(&src, &CompressOptions::default(), &out).unwrap();
        let doc = Document::load(&out).unwrap();
        assert_eq!(doc.page_iter().count(), 6);
        assert!(doc.catalog().is_ok());
        // And the text is still there, via lopdf's own extractor.
        let pages: Vec<u32> = (1..=6).collect();
        let text = doc.extract_text(&pages).unwrap_or_default();
        assert!(text.contains("compressible"), "content lost: {:?}", &text[..text.len().min(60)]);
    }

    #[test]
    fn doing_nothing_is_allowed_and_reports_nothing() {
        let dir = TempDir::new().unwrap();
        let src = write(&mut make_doc(3), &dir, "in.pdf");
        let out = dir.path().join("out.pdf");
        let opts = CompressOptions {
            compress_streams: false,
            use_object_streams: false,
            max_objects_per_stream: None,
        };
        let r = compress_pdf(&src, &opts, &out).unwrap();
        assert_eq!(r.streams_compressed, 0);
        assert!(!r.used_object_streams);
    }

    #[test]
    fn a_stream_too_short_to_benefit_is_reported_as_untouched() {
        // lopdf refuses to deflate when the result would not be at least 19
        // bytes smaller — and says Ok. Counting the call rather than the
        // filter made this document report every stream as compressed while
        // the output stayed plaintext, which an independent reader caught.
        let dir = TempDir::new().unwrap();
        // 1.4 deliberately: at 1.5 lopdf saves an xref *stream*, which is
        // repetitive enough to deflate, and this test is about the short
        // content streams.
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for _ in 0..3 {
            let cid = doc.add_object(Object::Stream(lopdf::Stream::new(
                lopdf::Dictionary::new(),
                b"BT 72 700 Td (hi) Tj ET".to_vec(),
            )));
            kids.push(Object::Reference(doc.add_object(Object::Dictionary(
                lopdf::Dictionary::from_iter(vec![
                    ("Type", Object::Name(b"Page".to_vec())),
                    ("Parent", Object::Reference(pages_id)),
                    ("MediaBox", Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(612), Object::Integer(792),
                    ])),
                    ("Contents", Object::Reference(cid)),
                ]),
            ))));
        }
        doc.objects.insert(pages_id, Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Count", Object::Integer(3)),
            ("Kids", Object::Array(kids)),
        ])));
        let cat = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Catalog".to_vec())),
            ("Pages", Object::Reference(pages_id)),
        ])));
        doc.trailer.set("Root", Object::Reference(cat));
        let src = write(&mut doc, &dir, "short.pdf");

        let out = dir.path().join("short-out.pdf");
        let opts = CompressOptions { use_object_streams: false, ..Default::default() };
        let r = compress_pdf(&src, &opts, &out).unwrap();
        let check = Document::load(&out).unwrap();

        // Each page's content is ~23 bytes, far short of the 19-byte saving
        // lopdf requires, so every one of them must come out raw.
        for page_id in check.page_iter() {
            if let Ok(Object::Dictionary(p)) = check.get_object(page_id) {
                if let Ok(Object::Reference(cid)) = p.get(b"Contents") {
                    if let Ok(Object::Stream(s)) = check.get_object(*cid) {
                        assert!(
                            s.dict.get(b"Filter").is_err(),
                            "a 23-byte stream was marked filtered"
                        );
                    }
                }
            }
        }

        // And the report may not claim more than the file shows: the old code
        // counted every *attempt*, so it reported one per stream while the
        // page content stayed plaintext.
        let filtered = check.objects.values().filter(|o| {
            matches!(o, Object::Stream(st) if st.dict.get(b"Filter").is_ok())
        }).count();
        assert!(
            r.streams_compressed <= filtered,
            "reported {} deflated, output has {filtered} filtered stream(s): {r:?}",
            r.streams_compressed
        );
    }

    #[test]
    fn already_compressed_streams_are_left_alone() {
        // Running twice must not re-deflate: the second pass should find
        // nothing to do rather than growing the file.
        let dir = TempDir::new().unwrap();
        let src = write(&mut make_doc(5), &dir, "in.pdf");
        let once = dir.path().join("once.pdf");
        compress_pdf(&src, &CompressOptions::default(), &once).unwrap();
        let twice = dir.path().join("twice.pdf");
        let r2 = compress_pdf(&once, &CompressOptions::default(), &twice).unwrap();
        // The invariant that matters is that nothing gets deflated twice:
        // a /Filter array with two FlateDecode entries means the data was
        // compressed on top of itself. (A second pass may still find one
        // fresh stream that lopdf's object/xref-stream writer produced, so
        // asserting zero here would be asserting the wrong thing.)
        let doc = Document::load(&twice).unwrap();
        for (_, obj) in doc.objects.iter() {
            if let Object::Stream(st) = obj {
                if let Ok(Object::Array(filters)) = st.dict.get(b"Filter") {
                    let flate = filters.iter().filter(|f| {
                        matches!(f, Object::Name(n) if n.as_slice() == b"FlateDecode")
                    }).count();
                    assert!(flate <= 1, "stream deflated {flate} times");
                }
            }
        }
        assert_eq!(doc.page_iter().count(), 5, "pages lost on the second pass");
        let _ = r2;
    }

    #[test]
    fn savings_ratio_is_zero_when_nothing_shrank() {
        let r = CompressReport { bytes_before: 100, bytes_after: 120, ..Default::default() };
        assert_eq!(r.savings_ratio(), 0.0);
        let r = CompressReport { bytes_before: 0, bytes_after: 0, ..Default::default() };
        assert_eq!(r.savings_ratio(), 0.0);
    }
}
