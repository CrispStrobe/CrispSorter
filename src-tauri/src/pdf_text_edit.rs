//! On-page text editing (P32.8, tiers 1–2).
//!
//! Editing text that is already on a page is the hardest thing in this
//! area, so it is deliberately split:
//!
//! * **Tier 1 — overprint.** Cover a region with a filled rectangle and
//!   draw new text on top. Crude, but exact and font-independent: it
//!   works on any PDF, including scanned ones, because it does not care
//!   what is underneath. [`overprint_doc`].
//! * **Tier 2 — substitution.** Find a string in the content stream and
//!   replace it in place, keeping the original font. The surrounding
//!   text does not move, because the width difference is absorbed into a
//!   `TJ` displacement. [`substitute_text_doc`].
//! * **Tier 3 — reflow.** Re-laying out a paragraph after an edit, with
//!   line breaking and font subsetting for characters the embedded font
//!   lacks. Not attempted; it is a substantially larger project than the
//!   two above put together.
//!
//! ## What tier 2 will and will not do
//!
//! Matches are searched across a whole show-text run, not per operand, so
//! a word broken up by kerning — `[(W) -80 (ord)] TJ`, which is how most
//! typeset PDFs look — is still found. Kerning adjustments that fall
//! *inside* a replaced span are dropped and accounted for in the width
//! compensation.
//!
//! It will not:
//!
//! * cross a show-text operator boundary — a phrase split across two
//!   separate `Tj` operations is not matched;
//! * touch composite (Type0/CID) fonts, whose multi-byte encodings vary
//!   per font — those runs are skipped and reported;
//! * write a character the font has no width for, since the result would
//!   be a blank or a wrong glyph. Such substitutions are refused.
//!
//! Everything it declines to do is counted in [`TextEditReport`] rather
//! than passed over silently.

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::pdf_redact::{load_fonts, obj_num, FontInfo};

// ── Reports ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TextEditReport {
    /// Occurrences replaced.
    pub replacements: usize,
    /// Pages whose content stream was rewritten.
    pub pages_changed: usize,
    /// Occurrences found but skipped, with the reason in `warnings`.
    pub skipped: usize,
    pub warnings: Vec<String>,
}

// ── Tier 1: overprint ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverprintSpec {
    /// 0-based page index.
    pub page: usize,
    /// Region to cover, in points from the bottom-left.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Replacement text. Empty simply blanks the region.
    pub text: String,
    pub font_size: f64,
    /// Text colour, RGB 0.0–1.0.
    pub color: [f64; 3],
    /// Fill colour for the covering rectangle. White by default — the
    /// point is to match the page, and most pages are white.
    pub background: [f64; 3],
}

impl Default for OverprintSpec {
    fn default() -> Self {
        Self {
            page: 0,
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            text: String::new(),
            font_size: 11.0,
            color: [0.0, 0.0, 0.0],
            background: [1.0, 1.0, 1.0],
        }
    }
}

/// Cover a region and draw replacement text over it.
///
/// The original text is still in the content stream underneath — this is
/// a visual replacement, not a removal. When the old text must actually
/// go, run [`crate::pdf_redact::redact_regions_hard`] over the same
/// region first.
pub fn overprint_doc(doc: &mut Document, specs: &[OverprintSpec]) -> Result<usize, String> {
    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let n = page_ids.len();
    let mut done = 0;

    for s in specs {
        if s.page >= n {
            return Err(format!("page {} out of range (0..{n})", s.page));
        }
        let page_id = page_ids[s.page];
        let [br, bg, bb] = s.background;
        let [tr, tg, tb] = s.color;

        let mut content = format!(
            "q {br:.3} {bg:.3} {bb:.3} rg {:.2} {:.2} {:.2} {:.2} re f Q",
            s.x, s.y, s.w, s.h
        );
        if !s.text.is_empty() {
            let size = s.font_size;
            // Baseline sits inside the covered box rather than on its
            // bottom edge, so descenders are not clipped by the region.
            let baseline = s.y + (s.h - size) / 2.0 + size * 0.2;
            content.push_str(&format!(
                " q {tr:.3} {tg:.3} {tb:.3} rg BT /F1 {size} Tf {:.2} {:.2} Td ({}) Tj ET Q",
                s.x + 1.0,
                baseline,
                crate::pdf_ops::escape_pdf_literal(&s.text),
            ));
        }

        let font_id = crate::pdf_ops::add_helvetica(doc);
        crate::pdf_ops::append_content(
            doc,
            page_id,
            content.into_bytes(),
            Some(("F1", font_id)),
            None,
        );
        done += 1;
    }
    Ok(done)
}

// ── Tier 2: in-place substitution ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Substitution {
    pub find: String,
    pub replace: String,
}

/// Per-run text state. Only what affects glyph advance — substitution
/// does not need the full matrix stack that redaction does, because it
/// never has to know *where* on the page a glyph sits.
struct RunState {
    font: Vec<u8>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
}

impl Default for RunState {
    fn default() -> Self {
        Self {
            font: Vec::new(),
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
        }
    }
}

/// Advance of one glyph in unscaled text space.
fn advance(code: u8, font: &FontInfo, st: &RunState) -> f64 {
    let word = if code == b' ' { st.word_spacing } else { 0.0 };
    (font.width(code) / 1000.0 * st.size + st.char_spacing + word) * st.h_scale
}

/// Encode a replacement string to single-byte codes.
///
/// Returns `None` when a character cannot be represented, rather than
/// substituting a placeholder: a silently wrong glyph in a document
/// someone is about to sign is worse than a refused edit.
fn encode_simple(s: &str, font: &FontInfo) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let cp = ch as u32;
        if cp > 0xFF {
            return None;
        }
        let b = cp as u8;
        // A code the font declares no width for will not render usefully.
        if !font.widths.is_empty() && !font.widths.contains_key(&b) {
            return None;
        }
        out.push(b);
    }
    Some(out)
}

/// Flatten a show-text run's operands into a byte string plus a record of
/// where the numeric kerning adjustments sat.
fn flatten_run(items: &[Object]) -> (Vec<u8>, Vec<(usize, f64)>) {
    let mut bytes = Vec::new();
    // (flat index the adjustment precedes, value)
    let mut adjustments = Vec::new();
    for item in items {
        match item {
            Object::String(b, _) => bytes.extend_from_slice(b),
            Object::Integer(v) => adjustments.push((bytes.len(), *v as f64)),
            Object::Real(v) => adjustments.push((bytes.len(), *v as f64)),
            _ => {}
        }
    }
    (bytes, adjustments)
}

fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            hits.push(i);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    hits
}

/// Rewrite one show-text run, applying whichever substitutions match.
///
/// Returns the new operand list and how many replacements happened.
fn rewrite_run(
    items: &[Object],
    subs: &[(Vec<u8>, Vec<u8>)],
    font: &FontInfo,
    st: &RunState,
) -> (Vec<Object>, usize) {
    let (flat, adjustments) = flatten_run(items);

    // Collect non-overlapping matches, earliest first.
    let mut spans: Vec<(usize, usize, &[u8])> = Vec::new();
    for (find, replace) in subs {
        for at in find_all(&flat, find) {
            let end = at + find.len();
            if spans.iter().any(|(s, e, _)| at < *e && *s < end) {
                continue; // overlaps an earlier match
            }
            spans.push((at, end, replace.as_slice()));
        }
    }
    if spans.is_empty() {
        return (items.to_vec(), 0);
    }
    spans.sort_by_key(|(s, _, _)| *s);

    let mut out: Vec<Object> = Vec::new();
    let mut cursor = 0usize;
    let mut count = 0usize;

    let flush_adjustments = |out: &mut Vec<Object>, from: usize, to: usize| {
        for (at, v) in &adjustments {
            if *at > from && *at < to {
                out.push(Object::Real(*v as f32));
            }
        }
    };

    for (start, end, replacement) in &spans {
        if cursor < *start {
            out.push(Object::String(
                flat[cursor..*start].to_vec(),
                lopdf::StringFormat::Literal,
            ));
            // Kerning that sat inside the kept text must be kept too.
            flush_adjustments(&mut out, cursor, *start);
        }

        // Width of what is going away, including any kerning inside it.
        let old_advance: f64 = flat[*start..*end].iter().map(|&c| advance(c, font, st)).sum();
        let old_kerning: f64 = adjustments
            .iter()
            .filter(|(at, _)| *at > *start && *at < *end)
            .map(|(_, v)| -v / 1000.0 * st.size * st.h_scale)
            .sum();
        let new_advance: f64 = replacement.iter().map(|&c| advance(c, font, st)).sum();

        out.push(Object::String(
            replacement.to_vec(),
            lopdf::StringFormat::Literal,
        ));

        // Absorb the width difference so everything after this stays put.
        let delta = new_advance - (old_advance + old_kerning);
        if st.size.abs() > f64::EPSILON && st.h_scale.abs() > f64::EPSILON && delta.abs() > 1e-6 {
            let n = delta * 1000.0 / (st.size * st.h_scale);
            out.push(Object::Real(n as f32));
        }

        count += 1;
        cursor = *end;
    }

    if cursor < flat.len() {
        out.push(Object::String(
            flat[cursor..].to_vec(),
            lopdf::StringFormat::Literal,
        ));
        flush_adjustments(&mut out, cursor, flat.len());
    }

    (out, count)
}

/// Replace text throughout a document, keeping the original font.
pub fn substitute_text_doc(
    doc: &mut Document,
    subs: &[Substitution],
) -> Result<TextEditReport, String> {
    let mut report = TextEditReport::default();
    if subs.iter().all(|s| s.find.is_empty()) {
        return Ok(report);
    }
    let page_ids: Vec<ObjectId> = doc.page_iter().collect();

    for page_id in page_ids {
        let fonts = load_fonts(doc, page_id);
        let content = match doc.get_and_decode_page_content(page_id) {
            Ok(c) => c,
            Err(e) => {
                report
                    .warnings
                    .push(format!("page content could not be decoded: {e}"));
                continue;
            }
        };

        let mut st = RunState::default();
        let mut out: Vec<Operation> = Vec::with_capacity(content.operations.len());
        let mut changed = false;

        for op in content.operations.iter() {
            let operands = &op.operands;
            match op.operator.as_str() {
                "Tf" => {
                    if operands.len() >= 2 {
                        if let Object::Name(n) = &operands[0] {
                            st.font = n.clone();
                        }
                        st.size = obj_num(doc, &operands[1]);
                    }
                    out.push(op.clone());
                }
                "Tc" => {
                    st.char_spacing = operands.first().map(|o| obj_num(doc, o)).unwrap_or(0.0);
                    out.push(op.clone());
                }
                "Tw" => {
                    st.word_spacing = operands.first().map(|o| obj_num(doc, o)).unwrap_or(0.0);
                    out.push(op.clone());
                }
                "Tz" => {
                    st.h_scale =
                        operands.first().map(|o| obj_num(doc, o)).unwrap_or(100.0) / 100.0;
                    out.push(op.clone());
                }
                "Tj" | "TJ" | "'" | "\"" => {
                    let font = fonts.get(&st.font).cloned().unwrap_or_default();
                    let items: Vec<Object> = match op.operator.as_str() {
                        "TJ" => match operands.first() {
                            Some(Object::Array(a)) => a.clone(),
                            _ => Vec::new(),
                        },
                        "\"" => operands.get(2).cloned().into_iter().collect(),
                        _ => operands.first().cloned().into_iter().collect(),
                    };

                    if font.composite {
                        // Only report a skip if this run actually contains
                        // something we were asked to change.
                        let (flat, _) = flatten_run(&items);
                        let touched = subs.iter().any(|s| {
                            !s.find.is_empty() && !find_all(&flat, s.find.as_bytes()).is_empty()
                        });
                        if touched {
                            report.skipped += 1;
                            report.warnings.push(
                                "a match sits in a composite (Type0) font and was left alone; \
                                 its multi-byte encoding cannot be rewritten safely"
                                    .into(),
                            );
                        }
                        out.push(op.clone());
                        continue;
                    }

                    // Encode replacements in this run's font; refuse any
                    // the font cannot render.
                    let mut encoded: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
                    for s in subs {
                        if s.find.is_empty() {
                            continue;
                        }
                        match (encode_simple(&s.find, &font), encode_simple(&s.replace, &font)) {
                            (Some(f), Some(r)) => encoded.push((f, r)),
                            _ => {
                                let (flat, _) = flatten_run(&items);
                                if !find_all(&flat, s.find.as_bytes()).is_empty() {
                                    report.skipped += 1;
                                    report.warnings.push(format!(
                                        "{:?} cannot be written in this run's font; left alone",
                                        s.replace
                                    ));
                                }
                            }
                        }
                    }
                    if encoded.is_empty() {
                        out.push(op.clone());
                        continue;
                    }

                    let (new_items, n) = rewrite_run(&items, &encoded, &font, &st);
                    if n > 0 {
                        report.replacements += n;
                        changed = true;
                        // ' and " carry line-positioning side effects, so
                        // they cannot simply become TJ. Emit the movement
                        // first, then the rewritten text.
                        match op.operator.as_str() {
                            "'" => out.push(Operation::new("T*", vec![])),
                            "\"" => {
                                if operands.len() >= 2 {
                                    out.push(Operation::new("Tw", vec![operands[0].clone()]));
                                    out.push(Operation::new("Tc", vec![operands[1].clone()]));
                                }
                                out.push(Operation::new("T*", vec![]));
                            }
                            _ => {}
                        }
                        out.push(Operation::new("TJ", vec![Object::Array(new_items)]));
                    } else {
                        out.push(op.clone());
                    }
                }
                _ => out.push(op.clone()),
            }
        }

        if changed {
            let encoded = Content { operations: out }
                .encode()
                .map_err(|e| format!("re-encode content: {e}"))?;
            let new_stream = doc.add_object(Object::Stream(lopdf::Stream::new(
                lopdf::Dictionary::new(),
                encoded,
            )));
            if let Ok(Object::Dictionary(ref mut page)) = doc.get_object_mut(page_id) {
                page.set("Contents", Object::Reference(new_stream));
            }
            report.pages_changed += 1;
        }
    }

    report.warnings.sort();
    report.warnings.dedup();
    Ok(report)
}

// ── Path wrappers ──────────────────────────────────────────────────────

pub fn overprint(path: &Path, specs: &[OverprintSpec], out_path: &Path) -> Result<usize, String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let n = overprint_doc(&mut doc, specs)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(n)
}

pub fn substitute_text(
    path: &Path,
    subs: &[Substitution],
    out_path: &Path,
) -> Result<TextEditReport, String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let report = substitute_text_doc(&mut doc, subs)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(report)
}

pub mod tauri_commands {
    use super::*;

    #[tauri::command]
    pub async fn pdf_overprint(
        path: String,
        specs: Vec<OverprintSpec>,
        out_path: String,
    ) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || {
            overprint(Path::new(&path), &specs, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_substitute_text(
        path: String,
        substitutions: Vec<Substitution>,
        out_path: String,
    ) -> Result<TextEditReport, String> {
        tokio::task::spawn_blocking(move || {
            substitute_text(Path::new(&path), &substitutions, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One page drawing `content_ops` with a uniform-width Helvetica.
    fn doc_with_ops(content: &str) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Helvetica".to_vec())),
            ("FirstChar", Object::Integer(0)),
            ("Widths", Object::Array((0..256).map(|_| Object::Integer(500)).collect())),
        ])));
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content.as_bytes().to_vec(),
        )));
        let mut fonts = lopdf::Dictionary::new();
        fonts.set("F1", Object::Reference(font_id));
        let mut res = lopdf::Dictionary::new();
        res.set("Font", Object::Dictionary(fonts));
        let page_id = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Page".to_vec())),
            ("Parent", Object::Reference(pages_id)),
            ("MediaBox", Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(612), Object::Integer(792),
            ])),
            ("Resources", Object::Dictionary(res)),
            ("Contents", Object::Reference(content_id)),
        ])));
        doc.objects.insert(pages_id, Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Count", Object::Integer(1)),
            ("Kids", Object::Array(vec![Object::Reference(page_id)])),
        ])));
        let cat = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Catalog".to_vec())),
            ("Pages", Object::Reference(pages_id)),
        ])));
        doc.trailer.set("Root", Object::Reference(cat));
        doc
    }

    /// Everything a text extractor would recover from the page.
    fn text_of(doc: &Document) -> String {
        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        let mut s = String::new();
        for op in &content.operations {
            let items: Vec<Object> = match op.operator.as_str() {
                "Tj" | "'" => op.operands.iter().cloned().collect(),
                "TJ" => match op.operands.first() {
                    Some(Object::Array(a)) => a.clone(),
                    _ => vec![],
                },
                _ => vec![],
            };
            for o in items {
                if let Object::String(b, _) = o {
                    s.push_str(&String::from_utf8_lossy(&b));
                }
            }
        }
        s
    }

    fn sub(find: &str, replace: &str) -> Substitution {
        Substitution { find: find.into(), replace: replace.into() }
    }

    #[test]
    fn replaces_a_simple_string() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (Hello World) Tj ET");
        let r = substitute_text_doc(&mut doc, &[sub("World", "There")]).unwrap();
        assert_eq!(r.replacements, 1);
        assert_eq!(r.pages_changed, 1);
        assert_eq!(text_of(&doc), "Hello There");
    }

    #[test]
    fn finds_a_match_split_by_kerning() {
        // How a typeset PDF actually looks. Searching per-operand would
        // miss this entirely.
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td [(W) -80 (orld)] TJ ET");
        let r = substitute_text_doc(&mut doc, &[sub("World", "Earth")]).unwrap();
        assert_eq!(r.replacements, 1, "kerned word not matched");
        assert_eq!(text_of(&doc), "Earth");
    }

    #[test]
    fn text_after_a_shorter_replacement_does_not_shift() {
        // The width difference must be absorbed into a TJ displacement,
        // or everything after the edit slides left.
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (AAAA tail) Tj ET");
        substitute_text_doc(&mut doc, &[sub("AAAA", "BB")]).unwrap();
        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        let tj = content.operations.iter().find(|o| o.operator == "TJ").unwrap();
        let arr = match tj.operands.first() {
            Some(Object::Array(a)) => a.clone(),
            other => panic!("expected TJ array, got {other:?}"),
        };
        let adj: Vec<f64> = arr.iter().filter_map(|o| match o {
            Object::Real(v) => Some(*v as f64),
            Object::Integer(v) => Some(*v as f64),
            _ => None,
        }).collect();
        assert!(!adj.is_empty(), "no compensating displacement: {arr:?}");
        // Two 500/1000-em glyphs removed at 12 pt = 12 pt narrower, which
        // is -1000 TJ units.
        assert!((adj[0] - (-1000.0)).abs() < 1.0, "wrong compensation: {adj:?}");
    }

    #[test]
    fn replaces_every_occurrence() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (cat cat cat) Tj ET");
        let r = substitute_text_doc(&mut doc, &[sub("cat", "dog")]).unwrap();
        assert_eq!(r.replacements, 3);
        assert_eq!(text_of(&doc), "dog dog dog");
    }

    #[test]
    fn a_string_that_is_absent_changes_nothing() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (Hello World) Tj ET");
        let r = substitute_text_doc(&mut doc, &[sub("Absent", "X")]).unwrap();
        assert_eq!(r.replacements, 0);
        assert_eq!(r.pages_changed, 0);
        assert_eq!(text_of(&doc), "Hello World");
    }

    #[test]
    fn an_empty_replacement_deletes_the_text() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (keep DROP keep) Tj ET");
        let r = substitute_text_doc(&mut doc, &[sub("DROP", "")]).unwrap();
        assert_eq!(r.replacements, 1);
        assert_eq!(text_of(&doc), "keep  keep");
    }

    #[test]
    fn several_substitutions_apply_in_one_pass() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (red and blue) Tj ET");
        let r = substitute_text_doc(&mut doc, &[sub("red", "one"), sub("blue", "two")]).unwrap();
        assert_eq!(r.replacements, 2);
        assert_eq!(text_of(&doc), "one and two");
    }

    #[test]
    fn overlapping_matches_do_not_double_replace() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (aaaa) Tj ET");
        let r = substitute_text_doc(&mut doc, &[sub("aa", "b")]).unwrap();
        // "aaaa" holds two non-overlapping "aa".
        assert_eq!(r.replacements, 2);
        assert_eq!(text_of(&doc), "bb");
    }

    #[test]
    fn a_character_the_font_cannot_render_is_refused_not_mangled() {
        // The test font declares widths for 0..255 only, so a CJK
        // character has no code — writing one would produce a wrong glyph.
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let r = substitute_text_doc(&mut doc, &[sub("Hello", "日本語")]).unwrap();
        assert_eq!(r.replacements, 0);
        assert_eq!(r.skipped, 1);
        assert!(r.warnings.iter().any(|w| w.contains("cannot be written")), "{:?}", r.warnings);
        assert_eq!(text_of(&doc), "Hello", "text must be left intact");
    }

    #[test]
    fn empty_find_is_ignored_rather_than_matching_everywhere() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let r = substitute_text_doc(&mut doc, &[sub("", "X")]).unwrap();
        assert_eq!(r.replacements, 0);
        assert_eq!(text_of(&doc), "Hello");
    }

    #[test]
    fn overprint_covers_the_region_and_draws_the_new_text() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (old value) Tj ET");
        let spec = OverprintSpec {
            page: 0, x: 70.0, y: 695.0, w: 120.0, h: 16.0,
            text: "new value".into(), ..Default::default()
        };
        assert_eq!(overprint_doc(&mut doc, &[spec]).unwrap(), 1);

        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        assert!(content.operations.iter().any(|o| o.operator == "re"));
        assert!(text_of(&doc).contains("new value"));
        // Tier 1 is a cover-up, not a removal — the original is still there.
        assert!(text_of(&doc).contains("old value"));
    }

    #[test]
    fn overprint_rejects_a_page_out_of_range() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td (x) Tj ET");
        let spec = OverprintSpec { page: 9, ..Default::default() };
        assert!(overprint_doc(&mut doc, &[spec]).is_err());
    }

    #[test]
    fn kerning_outside_a_replacement_is_preserved() {
        let mut doc = doc_with_ops("BT /F1 12 Tf 72 700 Td [(AB) -50 (CD target)] TJ ET");
        substitute_text_doc(&mut doc, &[sub("target", "x")]).unwrap();
        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        let tj = content.operations.iter().find(|o| o.operator == "TJ").unwrap();
        let arr = match tj.operands.first() {
            Some(Object::Array(a)) => a.clone(),
            _ => panic!("expected array"),
        };
        // An integral Real survives the encode/decode round trip as an
        // Integer, so match on the value rather than the variant.
        let nums: Vec<f64> = arr.iter().filter_map(|o| match o {
            Object::Real(v) => Some(*v as f64),
            Object::Integer(v) => Some(*v as f64),
            _ => None,
        }).collect();
        assert!(
            nums.iter().any(|v| (v + 50.0).abs() < 0.01),
            "kerning between AB and CD was lost: {arr:?}"
        );
    }
}
