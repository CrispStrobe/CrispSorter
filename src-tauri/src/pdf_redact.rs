//! True redaction — removing text from the content stream (P32.7).
//!
//! [`crate::pdf_ops::black_out_regions`] paints black rectangles over a
//! region. That is a *black-out*, not a redaction: the text objects
//! survive underneath and come straight back out through copy/paste or
//! `pdf-extract`. This module removes the glyphs themselves.
//!
//! ## How
//!
//! Page content is decoded to operations and replayed through a text
//! state machine (CTM via `q`/`Q`/`cm`, text matrices via `BT`/`Tm`/`Td`/
//! `TD`/`T*`, and `Tf`/`Tc`/`Tw`/`Tz`/`TL`/`Ts`). Every glyph's advance
//! is computed from the font's `/Widths`, giving its box in device
//! space. Glyphs whose box meets a redaction rectangle are dropped.
//!
//! Dropped glyphs are replaced by an equivalent `TJ` displacement rather
//! than simply deleted, so the surviving text stays exactly where it
//! was. A `TJ` number `n` displaces by `-n/1000 × Tfs × Th`, and a
//! glyph advances by `(w0/1000 × Tfs + Tc + Tw) × Th`, so the
//! replacement is `n = -(w0 + 1000 × (Tc + Tw) / Tfs)`.
//!
//! ## Limits — read these before trusting it
//!
//! * **Composite (Type0/CID) fonts** are not decomposed into glyphs.
//!   Multi-byte encodings vary per font and mis-splitting them would
//!   corrupt the text. When a Type0 run meets a rectangle the *whole
//!   run* is dropped — over-removal, which is the safe direction, but it
//!   can take neighbouring words with it. Reported in [`RedactReport`].
//! * **Form XObjects** (`Do`) are not descended into. Text inside them is
//!   not scrubbed; each occurrence is reported as a warning.
//! * **Raster content** is not touched. A photograph of text under the
//!   rectangle is covered, not removed.
//! * Type3 fonts, vertical writing and clipping paths are not modelled.
//!
//! Because of these, [`redact_regions_hard`] always *also* draws the
//! black rectangle: whatever the scrub could not reach stays visually
//! covered.

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::pdf_ops::RedactionSpec;

// ── Report ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RedactReport {
    /// Glyphs removed from content streams.
    pub glyphs_removed: usize,
    /// Whole show-text runs dropped because their font could not be
    /// decomposed into glyphs (composite fonts).
    pub runs_dropped: usize,
    /// Pages whose content was rewritten.
    pub pages_changed: usize,
    /// Conditions the caller should surface to the user — content the
    /// scrub could not reach.
    pub warnings: Vec<String>,
}

// ── Matrices ───────────────────────────────────────────────────────────

/// A PDF matrix `[a b c d e f]`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Matrix([f64; 6]);

impl Matrix {
    const IDENTITY: Matrix = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    /// `self × other` in PDF order (self applied first).
    fn mul(&self, other: &Matrix) -> Matrix {
        let [a1, b1, c1, d1, e1, f1] = self.0;
        let [a2, b2, c2, d2, e2, f2] = other.0;
        Matrix([
            a1 * a2 + b1 * c2,
            a1 * b2 + b1 * d2,
            c1 * a2 + d1 * c2,
            c1 * b2 + d1 * d2,
            e1 * a2 + f1 * c2 + e2,
            e1 * b2 + f1 * d2 + f2,
        ])
    }

    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    fn translation(tx: f64, ty: f64) -> Matrix {
        Matrix([1.0, 0.0, 0.0, 1.0, tx, ty])
    }
}

// ── Fonts ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct FontInfo {
    /// Glyph widths in 1/1000 em, keyed by single-byte code.
    pub(crate) widths: HashMap<u8, f64>,
    pub(crate) default_width: f64,
    /// Type0/CID — multi-byte codes we decline to split.
    pub(crate) composite: bool,
}

impl Default for FontInfo {
    fn default() -> Self {
        // 500/1000 em is the conventional stand-in when a font declares
        // no widths. It makes boxes approximate, not wrong-by-orders.
        Self { widths: HashMap::new(), default_width: 500.0, composite: false }
    }
}

impl FontInfo {
    pub(crate) fn width(&self, code: u8) -> f64 {
        self.widths.get(&code).copied().unwrap_or(self.default_width)
    }
}

fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Object> {
    match obj {
        Object::Reference(r) => doc.get_object(*r).ok(),
        other => Some(other),
    }
}

pub(crate) fn obj_num(doc: &Document, o: &Object) -> f64 {
    match resolve(doc, o) {
        Some(Object::Integer(n)) => *n as f64,
        Some(Object::Real(n)) => *n as f64,
        _ => 0.0,
    }
}

/// Read the `/Font` entries reachable from a page's effective resources.
pub(crate) fn load_fonts(doc: &Document, page_id: ObjectId) -> HashMap<Vec<u8>, FontInfo> {
    let mut out = HashMap::new();

    // Walk the /Parent chain: /Resources is inheritable.
    let mut cur = page_id;
    let mut resources = None;
    for _ in 0..32 {
        let d = match doc.get_object(cur) {
            Ok(Object::Dictionary(d)) => d,
            _ => break,
        };
        if let Ok(r) = d.get(b"Resources") {
            match resolve(doc, r) {
                Some(Object::Dictionary(rd)) => {
                    resources = Some(rd.clone());
                    break;
                }
                _ => {}
            }
        }
        match d.get(b"Parent") {
            Ok(Object::Reference(p)) => cur = *p,
            _ => break,
        }
    }

    let resources = match resources {
        Some(r) => r,
        None => return out,
    };
    let fonts = match resources.get(b"Font").ok().and_then(|f| resolve(doc, f)) {
        Some(Object::Dictionary(d)) => d.clone(),
        _ => return out,
    };

    for (name, font_ref) in fonts.iter() {
        let fd = match resolve(doc, font_ref) {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => continue,
        };
        let mut info = FontInfo::default();

        if let Ok(Object::Name(sub)) = fd.get(b"Subtype") {
            if sub.as_slice() == b"Type0" {
                info.composite = true;
            }
        }

        let first_char = fd.get(b"FirstChar").map(|o| obj_num(doc, o)).unwrap_or(0.0) as i64;
        if let Some(Object::Array(widths)) = fd.get(b"Widths").ok().and_then(|w| resolve(doc, w)) {
            for (i, w) in widths.iter().enumerate() {
                let code = first_char + i as i64;
                if (0..=255).contains(&code) {
                    info.widths.insert(code as u8, obj_num(doc, w));
                }
            }
        }
        if let Some(Object::Dictionary(desc)) =
            fd.get(b"FontDescriptor").ok().and_then(|d| resolve(doc, d))
        {
            if let Ok(mw) = desc.get(b"MissingWidth") {
                info.default_width = obj_num(doc, mw);
            }
        }

        out.insert(name.to_vec(), info);
    }
    out
}

// ── Text state ─────────────────────────────────────────────────────────

struct TextState {
    tm: Matrix,
    tlm: Matrix,
    font: Vec<u8>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    /// Horizontal scaling, as a fraction (Tz 100 → 1.0).
    h_scale: f64,
    leading: f64,
    rise: f64,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            font: Vec::new(),
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
        }
    }
}

fn rects_intersect(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

/// Redaction rectangles as (x0, y0, x1, y1).
fn spec_bounds(specs: &[&RedactionSpec]) -> Vec<(f64, f64, f64, f64)> {
    specs.iter().map(|r| (r.x, r.y, r.x + r.w, r.y + r.h)).collect()
}

// ── The scrub ──────────────────────────────────────────────────────────

/// Remove text glyphs intersecting `rects` from one page's content.
fn scrub_page(
    doc: &mut Document,
    page_id: ObjectId,
    rects: &[(f64, f64, f64, f64)],
    report: &mut RedactReport,
) -> Result<bool, String> {
    let fonts = load_fonts(doc, page_id);
    let content = match doc.get_and_decode_page_content(page_id) {
        Ok(c) => c,
        // A page whose content will not decode is left alone; the caller
        // still draws the covering rectangle over it.
        Err(e) => {
            report.warnings.push(format!("page content could not be decoded: {e}"));
            return Ok(false);
        }
    };

    let mut ctm_stack: Vec<Matrix> = Vec::new();
    let mut ctm = Matrix::IDENTITY;
    let mut ts = TextState::default();
    let mut out: Vec<Operation> = Vec::with_capacity(content.operations.len());
    let mut changed = false;

    for op in content.operations.iter() {
        let operands = &op.operands;
        match op.operator.as_str() {
            "q" => {
                ctm_stack.push(ctm);
                out.push(op.clone());
            }
            "Q" => {
                ctm = ctm_stack.pop().unwrap_or(Matrix::IDENTITY);
                out.push(op.clone());
            }
            "cm" => {
                if operands.len() >= 6 {
                    let m = Matrix([
                        obj_num(doc, &operands[0]),
                        obj_num(doc, &operands[1]),
                        obj_num(doc, &operands[2]),
                        obj_num(doc, &operands[3]),
                        obj_num(doc, &operands[4]),
                        obj_num(doc, &operands[5]),
                    ]);
                    ctm = m.mul(&ctm);
                }
                out.push(op.clone());
            }
            "BT" => {
                ts.tm = Matrix::IDENTITY;
                ts.tlm = Matrix::IDENTITY;
                out.push(op.clone());
            }
            "ET" => out.push(op.clone()),
            "Tf" => {
                if operands.len() >= 2 {
                    if let Object::Name(n) = &operands[0] {
                        ts.font = n.clone();
                    }
                    ts.size = obj_num(doc, &operands[1]);
                }
                out.push(op.clone());
            }
            "Tc" => {
                ts.char_spacing = operands.first().map(|o| obj_num(doc, o)).unwrap_or(0.0);
                out.push(op.clone());
            }
            "Tw" => {
                ts.word_spacing = operands.first().map(|o| obj_num(doc, o)).unwrap_or(0.0);
                out.push(op.clone());
            }
            "Tz" => {
                ts.h_scale = operands.first().map(|o| obj_num(doc, o)).unwrap_or(100.0) / 100.0;
                out.push(op.clone());
            }
            "TL" => {
                ts.leading = operands.first().map(|o| obj_num(doc, o)).unwrap_or(0.0);
                out.push(op.clone());
            }
            "Ts" => {
                ts.rise = operands.first().map(|o| obj_num(doc, o)).unwrap_or(0.0);
                out.push(op.clone());
            }
            "Tm" => {
                if operands.len() >= 6 {
                    ts.tlm = Matrix([
                        obj_num(doc, &operands[0]),
                        obj_num(doc, &operands[1]),
                        obj_num(doc, &operands[2]),
                        obj_num(doc, &operands[3]),
                        obj_num(doc, &operands[4]),
                        obj_num(doc, &operands[5]),
                    ]);
                    ts.tm = ts.tlm;
                }
                out.push(op.clone());
            }
            "Td" => {
                if operands.len() >= 2 {
                    let t = Matrix::translation(obj_num(doc, &operands[0]), obj_num(doc, &operands[1]));
                    ts.tlm = t.mul(&ts.tlm);
                    ts.tm = ts.tlm;
                }
                out.push(op.clone());
            }
            "TD" => {
                if operands.len() >= 2 {
                    ts.leading = -obj_num(doc, &operands[1]);
                    let t = Matrix::translation(obj_num(doc, &operands[0]), obj_num(doc, &operands[1]));
                    ts.tlm = t.mul(&ts.tlm);
                    ts.tm = ts.tlm;
                }
                out.push(op.clone());
            }
            "T*" => {
                let t = Matrix::translation(0.0, -ts.leading);
                ts.tlm = t.mul(&ts.tlm);
                ts.tm = ts.tlm;
                out.push(op.clone());
            }
            "Do" => {
                if let Some(Object::Name(n)) = operands.first() {
                    report.warnings.push(format!(
                        "form XObject /{} not descended into; any text inside it is covered but not removed",
                        String::from_utf8_lossy(n)
                    ));
                }
                out.push(op.clone());
            }
            "Tj" | "TJ" | "'" | "\"" => {
                // ' and " move to the next line first.
                if op.operator == "'" || op.operator == "\"" {
                    if op.operator == "\"" && operands.len() >= 2 {
                        ts.word_spacing = obj_num(doc, &operands[0]);
                        ts.char_spacing = obj_num(doc, &operands[1]);
                    }
                    let t = Matrix::translation(0.0, -ts.leading);
                    ts.tlm = t.mul(&ts.tlm);
                    ts.tm = ts.tlm;
                }

                let font = fonts.get(&ts.font).cloned().unwrap_or_default();
                let items = show_items(op, doc);

                if font.composite {
                    // Cannot split multi-byte codes safely. Measure the run
                    // as a whole; if it meets a rectangle, drop all of it.
                    let (hit, advance) = composite_run_hits(&items, &ts, &ctm, rects, &font);
                    if hit {
                        report.runs_dropped += 1;
                        report.warnings.push(
                            "a composite-font run was removed whole; neighbouring text in the same run may have gone with it".into(),
                        );
                        changed = true;
                        ts.tm = Matrix::translation(advance, 0.0).mul(&ts.tm);
                        continue;
                    }
                    ts.tm = Matrix::translation(advance, 0.0).mul(&ts.tm);
                    out.push(op.clone());
                    continue;
                }

                let (new_items, removed) = scrub_run(&items, &mut ts, &ctm, rects, &font);
                if removed > 0 {
                    report.glyphs_removed += removed;
                    changed = true;
                    if !new_items.is_empty() {
                        out.push(Operation::new("TJ", vec![Object::Array(new_items)]));
                    }
                } else {
                    out.push(op.clone());
                }
            }
            _ => out.push(op.clone()),
        }
    }

    if !changed {
        return Ok(false);
    }

    let encoded = Content { operations: out }
        .encode()
        .map_err(|e| format!("re-encode content: {e}"))?;
    // Replace the page's content wholesale. A page's /Contents array is
    // defined to behave as one concatenated stream, so collapsing it to a
    // single stream is faithful.
    let new_stream = doc.add_object(Object::Stream(lopdf::Stream::new(
        lopdf::Dictionary::new(),
        encoded,
    )));
    if let Ok(Object::Dictionary(ref mut page)) = doc.get_object_mut(page_id) {
        page.set("Contents", Object::Reference(new_stream));
    }
    Ok(true)
}

/// Normalise any show-text operator to a `TJ`-style item list.
fn show_items(op: &Operation, _doc: &Document) -> Vec<Object> {
    match op.operator.as_str() {
        "TJ" => match op.operands.first() {
            Some(Object::Array(a)) => a.clone(),
            _ => Vec::new(),
        },
        // For " the string is the third operand.
        "\"" => op.operands.get(2).cloned().into_iter().collect(),
        _ => op.operands.first().cloned().into_iter().collect(),
    }
}

/// Advance produced by one glyph, in unscaled text space.
fn glyph_advance(w0: f64, code: u8, ts: &TextState, font: &FontInfo) -> f64 {
    let _ = font;
    let word = if code == b' ' { ts.word_spacing } else { 0.0 };
    (w0 / 1000.0 * ts.size + ts.char_spacing + word) * ts.h_scale
}

/// Device-space box of a glyph starting at text-space offset `tx`.
fn glyph_box(tx: f64, advance: f64, ts: &TextState, ctm: &Matrix) -> (f64, f64, f64, f64) {
    // Approximate vertical extent: most Latin fonts sit within
    // -0.25em..0.9em around the baseline. Exact ascent/descent would need
    // the FontDescriptor, and erring large only makes redaction keener.
    let y0 = ts.rise - 0.25 * ts.size;
    let y1 = ts.rise + 0.9 * ts.size;
    let m = ts.tm.mul(ctm);
    let corners = [
        m.apply(tx, y0),
        m.apply(tx + advance, y0),
        m.apply(tx, y1),
        m.apply(tx + advance, y1),
    ];
    let xs: Vec<f64> = corners.iter().map(|c| c.0).collect();
    let ys: Vec<f64> = corners.iter().map(|c| c.1).collect();
    (
        xs.iter().cloned().fold(f64::INFINITY, f64::min),
        ys.iter().cloned().fold(f64::INFINITY, f64::min),
        xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    )
}

/// Walk a run glyph by glyph, dropping those that meet a rectangle.
///
/// Returns the rebuilt `TJ` item list and the number of glyphs removed.
/// `ts.tm` is advanced past the whole run.
fn scrub_run(
    items: &[Object],
    ts: &mut TextState,
    ctm: &Matrix,
    rects: &[(f64, f64, f64, f64)],
    font: &FontInfo,
) -> (Vec<Object>, usize) {
    let mut out: Vec<Object> = Vec::new();
    let mut kept: Vec<u8> = Vec::new();
    let mut removed = 0usize;
    // Offset within the run, in unscaled text space.
    let mut tx = 0.0f64;

    let flush = |kept: &mut Vec<u8>, out: &mut Vec<Object>| {
        if !kept.is_empty() {
            out.push(Object::String(
                std::mem::take(kept),
                lopdf::StringFormat::Literal,
            ));
        }
    };

    for item in items {
        match item {
            Object::String(bytes, _) => {
                for &code in bytes.iter() {
                    let w0 = font.width(code);
                    let adv = glyph_advance(w0, code, ts, font);
                    let bbox = glyph_box(tx, adv, ts, ctm);
                    let hit = rects.iter().any(|r| rects_intersect(bbox, *r));
                    if hit {
                        flush(&mut kept, &mut out);
                        // Replace the glyph with an equal displacement so
                        // everything after it stays put.
                        let n = if ts.size.abs() > f64::EPSILON {
                            -(w0 + 1000.0 * (ts.char_spacing
                                + if code == b' ' { ts.word_spacing } else { 0.0 })
                                / ts.size)
                        } else {
                            -w0
                        };
                        out.push(Object::Real(n as f32));
                        removed += 1;
                    } else {
                        kept.push(code);
                    }
                    tx += adv;
                }
            }
            // A pre-existing TJ displacement: keep it and account for it.
            other => {
                let n = match other {
                    Object::Integer(v) => *v as f64,
                    Object::Real(v) => *v as f64,
                    _ => 0.0,
                };
                flush(&mut kept, &mut out);
                out.push(other.clone());
                tx += -n / 1000.0 * ts.size * ts.h_scale;
            }
        }
    }
    flush(&mut kept, &mut out);
    ts.tm = Matrix::translation(tx, 0.0).mul(&ts.tm);
    (out, removed)
}

/// Whether a composite-font run meets a rectangle, plus its advance.
fn composite_run_hits(
    items: &[Object],
    ts: &TextState,
    ctm: &Matrix,
    rects: &[(f64, f64, f64, f64)],
    font: &FontInfo,
) -> (bool, f64) {
    // Assume two-byte codes, the overwhelmingly common CID case, purely
    // to estimate the run's extent — no glyph is split out of it.
    let mut tx = 0.0;
    for item in items {
        match item {
            Object::String(bytes, _) => {
                let glyphs = bytes.len().div_ceil(2);
                tx += glyphs as f64 * (font.default_width / 1000.0 * ts.size + ts.char_spacing)
                    * ts.h_scale;
            }
            Object::Integer(v) => tx += -(*v as f64) / 1000.0 * ts.size * ts.h_scale,
            Object::Real(v) => tx += -(*v as f64) / 1000.0 * ts.size * ts.h_scale,
            _ => {}
        }
    }
    let bbox = glyph_box(0.0, tx, ts, ctm);
    (rects.iter().any(|r| rects_intersect(bbox, *r)), tx)
}

// ── Public API ─────────────────────────────────────────────────────────

/// Redact regions for real: scrub the text, then cover what remains.
///
/// The covering rectangle is not decoration — see the module limits. It
/// is what stands between the caller and the content this pass cannot
/// reach (raster images, form XObjects).
pub fn redact_regions_hard(
    path: &Path,
    regions: &[RedactionSpec],
    out_path: &Path,
) -> Result<RedactReport, String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let report = redact_regions_hard_doc(&mut doc, regions)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(report)
}

pub fn redact_regions_hard_doc(
    doc: &mut Document,
    regions: &[RedactionSpec],
) -> Result<RedactReport, String> {
    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let n = page_ids.len();
    let mut report = RedactReport::default();

    let mut by_page: HashMap<usize, Vec<&RedactionSpec>> = HashMap::new();
    for r in regions {
        if r.page >= n {
            return Err(format!("page {} out of range (0..{n})", r.page));
        }
        by_page.entry(r.page).or_default().push(r);
    }

    for (page_idx, specs) in &by_page {
        let rects = spec_bounds(specs);
        if scrub_page(doc, page_ids[*page_idx], &rects, &mut report)? {
            report.pages_changed += 1;
        }
    }

    // Cover afterwards, so the rectangles are not themselves scrubbed.
    let owned: Vec<RedactionSpec> = regions.to_vec();
    crate::pdf_ops::black_out_regions_doc(doc, &owned)?;

    report.warnings.sort();
    report.warnings.dedup();
    Ok(report)
}

pub mod tauri_commands {
    use super::*;

    /// True redaction: remove the text, then cover the region.
    #[tauri::command]
    pub async fn pdf_redact_hard(
        path: String,
        regions: Vec<RedactionSpec>,
        out_path: String,
    ) -> Result<RedactReport, String> {
        tokio::task::spawn_blocking(move || {
            redact_regions_hard(Path::new(&path), &regions, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page document whose content draws `text` with Helvetica at
    /// (x, y), size 12.
    fn doc_with_text(text: &str, x: f64, y: f64) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Helvetica".to_vec())),
            ("FirstChar", Object::Integer(0)),
            // Uniform 500/1000 em keeps the arithmetic checkable by hand.
            ("Widths", Object::Array((0..256).map(|_| Object::Integer(500)).collect())),
        ])));
        let content = format!("BT /F1 12 Tf {x} {y} Td ({text}) Tj ET");
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content.into_bytes(),
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

    /// All text recoverable from the page content, as an extractor would.
    fn visible_text(doc: &Document) -> String {
        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        let mut s = String::new();
        for op in &content.operations {
            let strings: Vec<&Object> = match op.operator.as_str() {
                "Tj" | "'" => op.operands.iter().collect(),
                "TJ" => match op.operands.first() {
                    Some(Object::Array(a)) => a.iter().collect(),
                    _ => vec![],
                },
                _ => vec![],
            };
            for o in strings {
                if let Object::String(b, _) = o {
                    s.push_str(&String::from_utf8_lossy(b));
                }
            }
        }
        s
    }

    fn spec(page: usize, x: f64, y: f64, w: f64, h: f64) -> RedactionSpec {
        RedactionSpec { page, x, y, w, h }
    }

    #[test]
    fn text_under_the_rectangle_is_actually_gone() {
        // 12pt, 500/1000 em → each glyph advances 6pt. "SECRET" spans
        // x = 100..136 at y = 700.
        let mut doc = doc_with_text("SECRET", 100.0, 700.0);
        assert!(visible_text(&doc).contains("SECRET"));

        let report = redact_regions_hard_doc(&mut doc, &[spec(0, 95.0, 690.0, 50.0, 20.0)]).unwrap();
        assert!(report.glyphs_removed >= 6, "got {report:?}");
        let after = visible_text(&doc);
        assert!(!after.contains("SECRET"), "text survived redaction: {after:?}");
    }

    #[test]
    fn text_outside_the_rectangle_is_untouched() {
        let mut doc = doc_with_text("KEEPME", 400.0, 700.0);
        let report = redact_regions_hard_doc(&mut doc, &[spec(0, 50.0, 100.0, 60.0, 20.0)]).unwrap();
        assert_eq!(report.glyphs_removed, 0);
        assert!(visible_text(&doc).contains("KEEPME"));
    }

    #[test]
    fn only_the_covered_glyphs_are_removed() {
        // "ABCDEFGH" from x=100, 6pt per glyph: A..H at 100,106,…,142.
        // Cover x = 100..118 → A, B, C.
        let mut doc = doc_with_text("ABCDEFGH", 100.0, 700.0);
        redact_regions_hard_doc(&mut doc, &[spec(0, 99.0, 690.0, 19.0, 20.0)]).unwrap();
        let after = visible_text(&doc);
        assert!(!after.contains('A') && !after.contains('B'), "leading glyphs survived: {after:?}");
        assert!(after.contains('G') && after.contains('H'), "trailing glyphs lost: {after:?}");
    }

    #[test]
    fn surviving_text_keeps_its_position() {
        // Removed glyphs become TJ displacements rather than vanishing, so
        // the remaining text does not shift left.
        let mut doc = doc_with_text("ABCDEFGH", 100.0, 700.0);
        redact_regions_hard_doc(&mut doc, &[spec(0, 99.0, 690.0, 19.0, 20.0)]).unwrap();
        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        let tj = content
            .operations
            .iter()
            .find(|o| o.operator == "TJ")
            .expect("expected a TJ operation");
        let arr = match tj.operands.first() {
            Some(Object::Array(a)) => a.clone(),
            other => panic!("TJ operand was not an array: {other:?}"),
        };
        let has_displacement = arr.iter().any(|o| matches!(o, Object::Real(_) | Object::Integer(_)));
        assert!(has_displacement, "no displacement compensating the removed glyphs: {arr:?}");
    }

    #[test]
    fn the_covering_rectangle_is_still_drawn() {
        // Belt and braces: whatever the scrub cannot reach stays covered.
        let mut doc = doc_with_text("SECRET", 100.0, 700.0);
        redact_regions_hard_doc(&mut doc, &[spec(0, 95.0, 690.0, 50.0, 20.0)]).unwrap();
        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        let has_fill = content.operations.iter().any(|o| o.operator == "re")
            && content.operations.iter().any(|o| o.operator == "f");
        assert!(has_fill, "no covering rectangle was drawn");
    }

    #[test]
    fn out_of_range_page_is_rejected() {
        let mut doc = doc_with_text("X", 10.0, 10.0);
        assert!(redact_regions_hard_doc(&mut doc, &[spec(7, 0.0, 0.0, 10.0, 10.0)]).is_err());
    }

    #[test]
    fn no_regions_changes_nothing() {
        let mut doc = doc_with_text("KEEPME", 100.0, 700.0);
        let report = redact_regions_hard_doc(&mut doc, &[]).unwrap();
        assert_eq!(report.glyphs_removed, 0);
        assert_eq!(report.pages_changed, 0);
        assert!(visible_text(&doc).contains("KEEPME"));
    }

    #[test]
    fn matrix_multiplication_matches_the_pdf_convention() {
        // Translate then scale: the translation must be scaled too.
        let t = Matrix::translation(10.0, 20.0);
        let s = Matrix([2.0, 0.0, 0.0, 2.0, 0.0, 0.0]);
        let m = t.mul(&s);
        assert_eq!(m.apply(0.0, 0.0), (20.0, 40.0));
    }

    #[test]
    fn rect_intersection_excludes_bare_touching() {
        assert!(rects_intersect((0.0, 0.0, 10.0, 10.0), (5.0, 5.0, 15.0, 15.0)));
        assert!(!rects_intersect((0.0, 0.0, 10.0, 10.0), (10.0, 0.0, 20.0, 10.0)));
        assert!(!rects_intersect((0.0, 0.0, 10.0, 10.0), (20.0, 20.0, 30.0, 30.0)));
    }
}
