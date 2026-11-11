//! In-PDF annotation round-trip (P32.3).
//!
//! `index::annotations` stores highlights and notes in SQLite keyed by
//! `doc_id` + page + bounding box.  Nothing wrote them into the PDF, so
//! our markup was invisible to every other reader and did not survive
//! export; and markup made elsewhere was invisible to us.
//!
//! This module bridges both directions against real `/Annot` objects.
//! Reading is the sleeper win: pulling a PDF's existing `/Annots` into
//! the annotation tables makes other people's comments searchable
//! through the same FTS index as our own.
//!
//! ## Coordinates
//!
//! `/Rect` is `[x0 y0 x1 y1]` in unrotated user space, origin bottom-left,
//! and the pairs are *not* guaranteed to be ordered — the spec requires
//! readers to normalise. We store `x`/`y` as the lower-left corner with
//! positive `w`/`h`, matching the annotations table.
//!
//! ## Appearance streams
//!
//! The editor emits `/AP` appearance streams for highlights, rectangles,
//! ink, notes and free text so its preview and exported PDF show the same
//! drawing. Free text also carries `/DA`. Other supported markup subtypes
//! retain their standard PDF fields for viewers to render. The free-text
//! appearance uses Helvetica/WinAnsi; its Unicode `/Contents` is preserved.

use lopdf::{Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Types ──────────────────────────────────────────────────────────────

/// One annotation, in the shape the annotations table wants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PdfAnnotation {
    /// 0-based page index.
    pub page: usize,
    /// Lower-left corner and size, in points.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// `highlight` | `note` | `rectangle` | `freetext` | `underline` |
    /// `strikeout` | `squiggly` | `stamp` | `ink`.
    pub ann_type: String,
    /// `/Contents` — the note body.
    pub text: String,
    /// `#rrggbb`.
    pub color: String,
    /// `/T` — the annotation's author, when the producer recorded one.
    pub author: Option<String>,
    /// `/QuadPoints` for text markup, in groups of 8. Empty for others.
    pub quads: Vec<f64>,
    /// Freehand strokes for `ink`: each inner list is `x0 y0 x1 y1 …` in
    /// points. Empty for every other kind.
    #[serde(default)]
    pub ink: Vec<Vec<f64>>,
    /// Border / stroke width in points (rectangle, ink). `None` = 1.5.
    #[serde(default)]
    pub stroke_width: Option<f64>,
    /// Font size for `freetext`. `None` = 11.
    #[serde(default)]
    pub font_size: Option<f64>,
}

impl PdfAnnotation {
    /// A highlight covering a single rectangle.
    pub fn highlight(page: usize, x: f64, y: f64, w: f64, h: f64, text: &str, color: &str) -> Self {
        Self {
            page,
            x,
            y,
            w,
            h,
            ann_type: "highlight".into(),
            text: text.to_string(),
            color: color.to_string(),
            author: None,
            // QuadPoints order is x1 y1 x2 y2 x3 y3 x4 y4 =
            // upper-left, upper-right, lower-left, lower-right.
            quads: vec![x, y + h, x + w, y + h, x, y, x + w, y],
            ink: Vec::new(),
            stroke_width: None,
            font_size: None,
        }
    }
}

// ── Colour helpers ─────────────────────────────────────────────────────

/// The default highlight colour, as bytes.  Kept in 8-bit form and
/// divided on use so it round-trips exactly through [`rgb_to_hex`] —
/// writing the components as decimal literals does not (0.08 × 255
/// rounds to 20, giving `#facc14`).
pub const DEFAULT_COLOR_RGB8: [u8; 3] = [0xfa, 0xcc, 0x15];

fn default_rgb() -> [f64; 3] {
    [
        DEFAULT_COLOR_RGB8[0] as f64 / 255.0,
        DEFAULT_COLOR_RGB8[1] as f64 / 255.0,
        DEFAULT_COLOR_RGB8[2] as f64 / 255.0,
    ]
}

/// `#rrggbb` (or `rrggbb`) → 0.0–1.0 components. Falls back to yellow,
/// which is what an annotation with an unparseable colour should look
/// like rather than an invisible one.
pub fn hex_to_rgb(hex: &str) -> [f64; 3] {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 || !h.is_char_boundary(2) || !h.is_char_boundary(4) {
        return default_rgb();
    }
    match (
        u8::from_str_radix(&h[0..2], 16),
        u8::from_str_radix(&h[2..4], 16),
        u8::from_str_radix(&h[4..6], 16),
    ) {
        (Ok(r), Ok(g), Ok(b)) => [r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0],
        _ => default_rgb(),
    }
}

pub fn rgb_to_hex(rgb: [f64; 3]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(rgb[0]), b(rgb[1]), b(rgb[2]))
}

// ── Subtype mapping ────────────────────────────────────────────────────

/// PDF `/Subtype` → our `ann_type`. `None` for subtypes we deliberately
/// skip: links and popups are structure, not markup, and importing them
/// would clutter the reading list with navigation artefacts.
fn subtype_to_kind(subtype: &str) -> Option<&'static str> {
    Some(match subtype {
        "Highlight" => "highlight",
        "Text" => "note",
        "Square" => "rectangle",
        "FreeText" => "freetext",
        "Underline" => "underline",
        "StrikeOut" => "strikeout",
        "Squiggly" => "squiggly",
        "Stamp" => "stamp",
        "Ink" => "ink",
        _ => return None,
    })
}

fn kind_to_subtype(kind: &str) -> &'static str {
    match kind {
        "highlight" => "Highlight",
        "rectangle" => "Square",
        "freetext" => "FreeText",
        "underline" => "Underline",
        "strikeout" => "StrikeOut",
        "squiggly" => "Squiggly",
        "stamp" => "Stamp",
        "ink" => "Ink",
        // Everything else — including "note" — becomes a sticky note.
        _ => "Text",
    }
}

/// Text-markup subtypes carry `/QuadPoints`; the others do not.
fn is_text_markup(kind: &str) -> bool {
    matches!(kind, "highlight" | "underline" | "strikeout" | "squiggly")
}

// ── Reading ────────────────────────────────────────────────────────────

fn obj_f64(doc: &Document, o: &Object) -> f64 {
    match o {
        Object::Integer(n) => *n as f64,
        Object::Real(n) => *n as f64,
        Object::Reference(r) => doc.get_object(*r).map(|v| obj_f64(doc, v)).unwrap_or(0.0),
        _ => 0.0,
    }
}

fn decode_pdf_str(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

fn dict_text(doc: &Document, dict: &lopdf::Dictionary, key: &[u8]) -> Option<String> {
    match dict.get(key).ok()? {
        Object::String(b, _) => Some(decode_pdf_str(b)),
        Object::Reference(r) => match doc.get_object(*r).ok()? {
            Object::String(b, _) => Some(decode_pdf_str(b)),
            _ => None,
        },
        _ => None,
    }
}

/// Resolve an object that may be given indirectly, to an array.
fn as_array(doc: &Document, obj: &Object) -> Option<Vec<Object>> {
    match obj {
        Object::Array(a) => Some(a.clone()),
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Array(a)) => Some(a.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Read every markup annotation in the document, in page order.
pub fn read_annotations(doc: &Document) -> Vec<PdfAnnotation> {
    let mut out = Vec::new();
    for (page_idx, page_id) in doc.page_iter().enumerate() {
        let page = match doc.get_object(page_id) {
            Ok(Object::Dictionary(d)) => d.clone(),
            _ => continue,
        };
        let annots = match page.get(b"Annots").ok().and_then(|a| as_array(doc, a)) {
            Some(a) => a,
            None => continue,
        };
        for entry in annots {
            let dict = match &entry {
                Object::Dictionary(d) => d.clone(),
                Object::Reference(r) => match doc.get_object(*r) {
                    Ok(Object::Dictionary(d)) => d.clone(),
                    _ => continue,
                },
                _ => continue,
            };
            if let Some(a) = read_one(doc, &dict, page_idx) {
                out.push(a);
            }
        }
    }
    out
}

fn read_one(doc: &Document, dict: &lopdf::Dictionary, page: usize) -> Option<PdfAnnotation> {
    let subtype = match dict.get(b"Subtype").ok()? {
        Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
        _ => return None,
    };
    let kind = subtype_to_kind(&subtype)?;

    let rect = dict.get(b"Rect").ok().and_then(|r| as_array(doc, r))?;
    if rect.len() != 4 {
        return None;
    }
    let (a, b, c, d) = (
        obj_f64(doc, &rect[0]),
        obj_f64(doc, &rect[1]),
        obj_f64(doc, &rect[2]),
        obj_f64(doc, &rect[3]),
    );
    // The spec does not guarantee x0<x1 / y0<y1; normalise.
    let (x0, x1) = if a <= c { (a, c) } else { (c, a) };
    let (y0, y1) = if b <= d { (b, d) } else { (d, b) };

    let color = dict
        .get(b"C")
        .ok()
        .and_then(|c| as_array(doc, c))
        .map(|arr| {
            let v: Vec<f64> = arr.iter().map(|o| obj_f64(doc, o)).collect();
            match v.len() {
                // DeviceGray
                1 => rgb_to_hex([v[0], v[0], v[0]]),
                3 => rgb_to_hex([v[0], v[1], v[2]]),
                // DeviceCMYK
                4 => rgb_to_hex([
                    (1.0 - v[0]) * (1.0 - v[3]),
                    (1.0 - v[1]) * (1.0 - v[3]),
                    (1.0 - v[2]) * (1.0 - v[3]),
                ]),
                _ => "#facc15".to_string(),
            }
        })
        .unwrap_or_else(|| "#facc15".to_string());

    let quads = dict
        .get(b"QuadPoints")
        .ok()
        .and_then(|q| as_array(doc, q))
        .map(|arr| arr.iter().map(|o| obj_f64(doc, o)).collect())
        .unwrap_or_default();

    Some(PdfAnnotation {
        page,
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
        ann_type: kind.to_string(),
        text: dict_text(doc, dict, b"Contents").unwrap_or_default(),
        color,
        author: dict_text(doc, dict, b"T"),
        quads,
        ink: read_ink_list(doc, dict),
        stroke_width: None,
        font_size: None,
    })
}

/// `/InkList` → list of strokes (flat `x y` pairs). Empty when absent.
fn read_ink_list(doc: &Document, dict: &lopdf::Dictionary) -> Vec<Vec<f64>> {
    dict.get(b"InkList")
        .ok()
        .and_then(|o| as_array(doc, o))
        .map(|strokes| {
            strokes
                .iter()
                .filter_map(|s| as_array(doc, s))
                .map(|pts| pts.iter().map(|o| obj_f64(doc, o)).collect())
                .collect()
        })
        .unwrap_or_default()
}

pub fn read_annotations_from_path(path: &Path) -> Result<Vec<PdfAnnotation>, String> {
    crate::pdf_ops::with_operation("pdf.annotations.read", |log| {
        log.step("reading", serde_json::Value::Null);
        let doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
        let annotations = read_annotations(&doc);
        log.step("read", serde_json::json!({"items":annotations.len()}));
        Ok(annotations)
    })
}

// ── Writing ────────────────────────────────────────────────────────────

fn pdf_text_obj(s: &str) -> Object {
    // UTF-16BE with a BOM: the only encoding that covers non-Latin-1 text
    // in a PDF text string. ASCII would round-trip as Literal, but always
    // taking this branch keeps behaviour uniform.
    let mut bytes = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        bytes.extend_from_slice(&u.to_be_bytes());
    }
    Object::String(bytes, lopdf::StringFormat::Hexadecimal)
}

fn build_annot_dict(a: &PdfAnnotation) -> lopdf::Dictionary {
    let [r, g, b] = hex_to_rgb(&a.color);
    let mut d = lopdf::Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Annot".to_vec())),
        ("Subtype", Object::Name(kind_to_subtype(&a.ann_type).as_bytes().to_vec())),
        ("Rect", Object::Array(vec![
            Object::Real(a.x as f32),
            Object::Real(a.y as f32),
            Object::Real((a.x + a.w) as f32),
            Object::Real((a.y + a.h) as f32),
        ])),
        ("C", Object::Array(vec![
            Object::Real(r as f32),
            Object::Real(g as f32),
            Object::Real(b as f32),
        ])),
        ("Contents", pdf_text_obj(&a.text)),
        // Printable, so the markup survives a print or a flatten.
        ("F", Object::Integer(4)),
    ]);

    if let Some(ref author) = a.author {
        d.set("T", pdf_text_obj(author));
    }

    if is_text_markup(&a.ann_type) {
        // Fall back to the rectangle's own corners when the caller did not
        // supply quads — a markup annotation without /QuadPoints is
        // malformed and viewers may drop it entirely.
        let quads = if a.quads.len() >= 8 {
            a.quads.clone()
        } else {
            vec![a.x, a.y + a.h, a.x + a.w, a.y + a.h, a.x, a.y, a.x + a.w, a.y]
        };
        d.set(
            "QuadPoints",
            Object::Array(quads.iter().map(|v| Object::Real(*v as f32)).collect()),
        );
    }

    if a.ann_type == "freetext" {
        // FreeText renders nothing without a default-appearance string.
        let size = a.font_size.unwrap_or(11.0);
        d.set(
            "DA",
            Object::String(
                format!("{r:.3} {g:.3} {b:.3} rg /Helv {size:.1} Tf").into_bytes(),
                lopdf::StringFormat::Literal,
            ),
        );
    }

    if a.ann_type == "rectangle" || a.ann_type == "ink" {
        d.set(
            "BS",
            Object::Dictionary(lopdf::Dictionary::from_iter(vec![
                ("Type", Object::Name(b"Border".to_vec())),
                ("W", Object::Real(stroke_width(a) as f32)),
                ("S", Object::Name(b"S".to_vec())),
            ])),
        );
    }

    if a.ann_type == "ink" {
        d.set(
            "InkList",
            Object::Array(
                a.ink
                    .iter()
                    .map(|stroke| {
                        Object::Array(stroke.iter().map(|v| Object::Real(*v as f32)).collect())
                    })
                    .collect(),
            ),
        );
    }

    d
}

fn stroke_width(a: &PdfAnnotation) -> f64 {
    a.stroke_width.filter(|w| w.is_finite() && *w > 0.0).unwrap_or(1.5)
}

/// Encode text for a base-14 font with /WinAnsiEncoding. Characters it
/// cannot express become `?` rather than corrupting the string.
fn win_ansi_bytes(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| {
            let u = c as u32;
            if (0x20..0x7f).contains(&u) || (0xa0..=0xff).contains(&u) {
                u as u8
            } else {
                b'?'
            }
        })
        .collect()
}

/// Escape bytes for a PDF literal string inside a content stream.
fn pdf_literal(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('(');
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out.push(')');
    out
}

/// Greedy word wrap using an average Helvetica glyph width — good enough
/// to keep a free-text note inside its box without a full metrics pass.
fn wrap_lines(text: &str, size: f64, width: f64) -> Vec<String> {
    let max_chars = ((width / (size * 0.5)).floor() as usize).max(1);
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if candidate.chars().count() > max_chars && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

/// Build a `/N` appearance stream for the kinds the editor draws.
///
/// The form's `/BBox` equals the annotation's `/Rect` and its matrix is
/// the identity, so the content can be written in page coordinates.
pub(crate) fn appearance_stream(a: &PdfAnnotation) -> Option<lopdf::Stream> {
    let [r, g, b] = hex_to_rgb(&a.color);
    let (x, y, w, h) = (a.x, a.y, a.w.max(0.0), a.h.max(0.0));
    let mut resources = lopdf::Dictionary::new();
    let content = match a.ann_type.as_str() {
        "highlight" => {
            resources.set(
                "ExtGState",
                Object::Dictionary(lopdf::Dictionary::from_iter(vec![(
                    "GH",
                    Object::Dictionary(lopdf::Dictionary::from_iter(vec![
                        ("Type", Object::Name(b"ExtGState".to_vec())),
                        ("BM", Object::Name(b"Multiply".to_vec())),
                        ("ca", Object::Real(0.45)),
                        ("CA", Object::Real(0.45)),
                    ])),
                )])),
            );
            format!("/GH gs {r:.3} {g:.3} {b:.3} rg {x:.2} {y:.2} {w:.2} {h:.2} re f")
        }
        "rectangle" => {
            let lw = stroke_width(a);
            let half = lw / 2.0;
            format!(
                "{r:.3} {g:.3} {b:.3} RG {lw:.2} w {:.2} {:.2} {:.2} {:.2} re S",
                x + half,
                y + half,
                (w - lw).max(0.0),
                (h - lw).max(0.0)
            )
        }
        "ink" => {
            let lw = stroke_width(a);
            let mut c = format!("{r:.3} {g:.3} {b:.3} RG {lw:.2} w 1 J 1 j");
            for stroke in &a.ink {
                let pts: Vec<(f64, f64)> = stroke.chunks_exact(2).map(|p| (p[0], p[1])).collect();
                let Some(first) = pts.first() else { continue };
                c.push_str(&format!(" {:.2} {:.2} m", first.0, first.1));
                if pts.len() == 1 {
                    // A dot: a zero-length segment with round caps.
                    c.push_str(&format!(" {:.2} {:.2} l", first.0 + 0.01, first.1));
                }
                for p in pts.iter().skip(1) {
                    c.push_str(&format!(" {:.2} {:.2} l", p.0, p.1));
                }
                c.push_str(" S");
            }
            c
        }
        "freetext" => {
            let size = a.font_size.unwrap_or(11.0).clamp(4.0, 144.0);
            resources.set(
                "Font",
                Object::Dictionary(lopdf::Dictionary::from_iter(vec![(
                    "Helv",
                    Object::Dictionary(lopdf::Dictionary::from_iter(vec![
                        ("Type", Object::Name(b"Font".to_vec())),
                        ("Subtype", Object::Name(b"Type1".to_vec())),
                        ("BaseFont", Object::Name(b"Helvetica".to_vec())),
                        ("Encoding", Object::Name(b"WinAnsiEncoding".to_vec())),
                    ])),
                )])),
            );
            let leading = size * 1.2;
            let mut c = format!(
                "q {x:.2} {y:.2} {w:.2} {h:.2} re W n BT /Helv {size:.2} Tf {r:.3} {g:.3} {b:.3} rg {leading:.2} TL {:.2} {:.2} Td",
                x + 2.0,
                y + h - size
            );
            for (i, line) in wrap_lines(&a.text, size, (w - 4.0).max(size)).iter().enumerate() {
                if i > 0 {
                    c.push_str(" T*");
                }
                c.push_str(&format!(" {} Tj", pdf_literal(&win_ansi_bytes(line))));
            }
            c.push_str(" ET Q");
            c
        }
        "note" => {
            // A small folded-note glyph, so the note is visible where
            // a viewer does not draw its own icon.
            let s = w.min(h).max(1.0);
            format!(
                "{r:.3} {g:.3} {b:.3} rg 0.25 0.25 0.25 RG 0.8 w {x:.2} {y:.2} {s:.2} {s:.2} re B \
                 {:.2} {:.2} m {:.2} {:.2} l {:.2} {:.2} m {:.2} {:.2} l {:.2} {:.2} m {:.2} {:.2} l S",
                x + s * 0.2, y + s * 0.7, x + s * 0.8, y + s * 0.7,
                x + s * 0.2, y + s * 0.5, x + s * 0.8, y + s * 0.5,
                x + s * 0.2, y + s * 0.3, x + s * 0.6, y + s * 0.3,
            )
        }
        _ => return None,
    };
    let dict = lopdf::Dictionary::from_iter(vec![
        ("Type", Object::Name(b"XObject".to_vec())),
        ("Subtype", Object::Name(b"Form".to_vec())),
        (
            "BBox",
            Object::Array(vec![
                Object::Real(x as f32),
                Object::Real(y as f32),
                Object::Real((x + w) as f32),
                Object::Real((y + h) as f32),
            ]),
        ),
        ("Resources", Object::Dictionary(resources)),
    ]);
    Some(lopdf::Stream::new(dict, content.into_bytes()))
}

/// Remove the topmost markup annotation on `page` whose rectangle contains
/// the point (`x`, `y`, in points). Links, form widgets and popups are
/// structure rather than markup and are never erased. Errors when nothing
/// is hit, so the caller can tell the user instead of recording a no-op.
pub fn erase_annotation_at(doc: &mut Document, page: usize, x: f64, y: f64) -> Result<(), String> {
    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let page_id = *page_ids
        .get(page)
        .ok_or_else(|| format!("page {page} out of range (0..{})", page_ids.len()))?;
    let list: Vec<Object> = match doc.get_object(page_id) {
        Ok(Object::Dictionary(d)) => d.get(b"Annots").ok().and_then(|a| as_array(doc, a)).unwrap_or_default(),
        _ => Vec::new(),
    };
    const TOLERANCE: f64 = 3.0;
    let mut hit: Option<usize> = None;
    for (i, entry) in list.iter().enumerate().rev() {
        let dict = match entry {
            Object::Dictionary(d) => d.clone(),
            Object::Reference(r) => match doc.get_object(*r) {
                Ok(Object::Dictionary(d)) => d.clone(),
                _ => continue,
            },
            _ => continue,
        };
        let subtype = match dict.get(b"Subtype") {
            Ok(Object::Name(n)) => String::from_utf8_lossy(n).into_owned(),
            _ => continue,
        };
        if subtype_to_kind(&subtype).is_none() {
            continue;
        }
        let Some(rect) = dict.get(b"Rect").ok().and_then(|r| as_array(doc, r)) else { continue };
        if rect.len() != 4 {
            continue;
        }
        let v: Vec<f64> = rect.iter().map(|o| obj_f64(doc, o)).collect();
        let (x0, x1) = (v[0].min(v[2]) - TOLERANCE, v[0].max(v[2]) + TOLERANCE);
        let (y0, y1) = (v[1].min(v[3]) - TOLERANCE, v[1].max(v[3]) + TOLERANCE);
        if x >= x0 && x <= x1 && y >= y0 && y <= y1 {
            hit = Some(i);
            break;
        }
    }
    let i = hit.ok_or_else(|| "no annotation at that point".to_string())?;
    let mut list = list;
    list.remove(i);
    if let Ok(Object::Dictionary(ref mut p)) = doc.get_object_mut(page_id) {
        p.set("Annots", Object::Array(list));
    }
    Ok(())
}

/// Append annotations to a document's pages.
///
/// Existing `/Annots` are preserved — this adds, it does not replace, so
/// importing our markup into a PDF that already carries someone else's
/// does not destroy theirs. Returns how many were written.
pub fn write_annotations_doc(doc: &mut Document, annots: &[PdfAnnotation]) -> Result<usize, String> {
    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let n = page_ids.len();

    // Create the annotation objects first: adding objects while holding a
    // mutable borrow of a page is not possible.
    let mut per_page: std::collections::HashMap<usize, Vec<ObjectId>> = std::collections::HashMap::new();
    for a in annots {
        if a.page >= n {
            return Err(format!("annotation page {} out of range (0..{n})", a.page));
        }
        let mut dict = build_annot_dict(a);
        // Viewers are supposed to synthesise appearances, but pdf.js (our
        // own preview) and several others draw nothing for an annotation
        // without /AP, so the markup the user just drew would vanish.
        if let Some(ap) = appearance_stream(a) {
            let ap_id = doc.add_object(Object::Stream(ap));
            dict.set(
                "AP",
                Object::Dictionary(lopdf::Dictionary::from_iter(vec![("N", Object::Reference(ap_id))])),
            );
        }
        let id = doc.add_object(Object::Dictionary(dict));
        per_page.entry(a.page).or_default().push(id);
    }

    let mut written = 0;
    for (page_idx, new_ids) in per_page {
        let page_id = page_ids[page_idx];
        // Resolve any existing /Annots before the mutable borrow.
        let existing: Vec<Object> = match doc.get_object(page_id) {
            Ok(Object::Dictionary(d)) => {
                d.get(b"Annots").ok().and_then(|a| as_array(doc, a)).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        if let Ok(Object::Dictionary(ref mut page)) = doc.get_object_mut(page_id) {
            let mut list = existing;
            for id in &new_ids {
                list.push(Object::Reference(*id));
                written += 1;
            }
            page.set("Annots", Object::Array(list));
        }
    }
    Ok(written)
}

pub fn write_annotations(
    path: &Path,
    annots: &[PdfAnnotation],
    out_path: &Path,
) -> Result<usize, String> {
    crate::pdf_ops::with_operation("pdf.annotations.write", |log| {
        log.step("reading", serde_json::Value::Null);
        let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
        log.step("annotating", serde_json::json!({"items":annots.len()}));
        let n = write_annotations_doc(&mut doc, annots)?;
        log.step("publishing", serde_json::json!({"items":n}));
        crate::pdf_ops::save_document(&mut doc, out_path)?;
        Ok(n)
    })
}

pub fn export_annotations(path: &Path, format: &str, out_path: &Path) -> Result<usize, String> {
    crate::pdf_ops::with_operation("pdf.annotations.export", |log| {
        log.step("reading", serde_json::json!({"format":format}));
        let annots = read_annotations_from_path(path)?;
        let title = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        log.step("rendering", serde_json::json!({"items":annots.len()}));
        let body = match format {
            "markdown" | "md" => to_markdown(&annots, &title),
            "csv" => to_csv(&annots),
            "json" => serde_json::to_string_pretty(&annots).map_err(|e| format!("serialise: {e}"))?,
            other => return Err(format!("unknown export format: {other}")),
        };
        log.step("publishing", serde_json::json!({"bytes":body.len()}));
        crate::pdf_ops::publish_bytes(out_path, body.as_bytes())?;
        Ok(annots.len())
    })
}

// ── Export ─────────────────────────────────────────────────────────────

/// Render annotations as Markdown, grouped by page.
pub fn to_markdown(annots: &[PdfAnnotation], title: &str) -> String {
    let mut out = String::new();
    if !title.is_empty() {
        out.push_str(&format!("# {title}\n\n"));
    }
    if annots.is_empty() {
        out.push_str("_No annotations._\n");
        return out;
    }
    let mut current_page = usize::MAX;
    for a in annots {
        if a.page != current_page {
            current_page = a.page;
            out.push_str(&format!("\n## Page {}\n\n", a.page + 1));
        }
        let body = a.text.trim();
        if body.is_empty() {
            out.push_str(&format!("- _{}_ ", a.ann_type));
        } else {
            out.push_str(&format!("- {body}"));
        }
        if let Some(ref who) = a.author {
            out.push_str(&format!(" — {who}"));
        }
        out.push('\n');
    }
    out
}

/// Render annotations as CSV. Fields are quoted and internal quotes are
/// doubled, per RFC 4180.
pub fn to_csv(annots: &[PdfAnnotation]) -> String {
    fn q(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
    let mut out = String::from("page,type,x,y,w,h,color,author,text\n");
    for a in annots {
        out.push_str(&format!(
            "{},{},{:.2},{:.2},{:.2},{:.2},{},{},{}\n",
            a.page + 1,
            q(&a.ann_type),
            a.x,
            a.y,
            a.w,
            a.h,
            q(&a.color),
            q(a.author.as_deref().unwrap_or("")),
            q(&a.text),
        ));
    }
    out
}

// ── Bridge to the annotation store ─────────────────────────────────────

/// Dedup key for an imported annotation.
///
/// Coordinates are quantised to a tenth of a point before hashing: the
/// same annotation re-read after a save/load cycle comes back with tiny
/// float differences (we serialise `/Rect` as `Real`, i.e. f32), and an
/// exact-float key would let every re-import duplicate the whole set.
fn dedup_key(page: i32, x: f64, y: f64, w: f64, h: f64, kind: &str, text: &str) -> String {
    let q = |v: f64| (v * 10.0).round() as i64;
    format!("{page}|{}|{}|{}|{}|{kind}|{text}", q(x), q(y), q(w), q(h))
}

/// Import annotations read from a PDF into the annotation store.
///
/// Idempotent: re-importing the same document adds nothing. Returns the
/// number of rows actually inserted, so the caller can tell the user
/// "12 imported" rather than "12 found, 12 of which you already had".
pub fn import_into_store(
    store: &crate::index::annotations::AnnotationStore,
    doc_id: &str,
    annots: &[PdfAnnotation],
) -> Result<usize, String> {
    let existing = store.get_annotations(doc_id).map_err(|e| e.to_string())?;
    let seen: std::collections::HashSet<String> = existing
        .iter()
        .map(|a| dedup_key(a.page, a.x, a.y, a.w, a.h, &a.ann_type, &a.text))
        .collect();

    let mut inserted = 0;
    for a in annots {
        let key = dedup_key(a.page as i32, a.x, a.y, a.w, a.h, &a.ann_type, &a.text);
        if seen.contains(&key) {
            continue;
        }
        store
            .add_annotation(
                doc_id,
                a.page as i32,
                a.x,
                a.y,
                a.w,
                a.h,
                &a.ann_type,
                &a.text,
                &a.color,
            )
            .map_err(|e| e.to_string())?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Export a document's stored annotations back into a PDF.
pub fn export_from_store(
    store: &crate::index::annotations::AnnotationStore,
    doc_id: &str,
    path: &Path,
    out_path: &Path,
) -> Result<usize, String> {
    let rows = store.get_annotations(doc_id).map_err(|e| e.to_string())?;
    let annots: Vec<PdfAnnotation> = rows
        .into_iter()
        .map(|a| PdfAnnotation {
            page: a.page.max(0) as usize,
            x: a.x,
            y: a.y,
            w: a.w,
            h: a.h,
            ann_type: a.ann_type,
            text: a.text,
            color: a.color,
            author: None,
            // Regenerated from the rectangle by `build_annot_dict`.
            quads: Vec::new(),
            ink: Vec::new(),
            stroke_width: None,
            font_size: None,
        })
        .collect();
    write_annotations(path, &annots, out_path)
}

// ── Tauri commands ─────────────────────────────────────────────────────

pub mod tauri_commands {
    use super::*;
    use crate::index::annotations::AnnotationStore;
    use crate::AppState;
    use tauri::State;

    async fn get_store(state: &State<'_, AppState>) -> Result<AnnotationStore, String> {
        let data_dir = state.data_dir.lock().await;
        let dir = data_dir.as_ref().ok_or("App data dir not set")?;
        AnnotationStore::open_or_create(dir).map_err(|e| e.to_string())
    }

    /// Read a PDF's `/Annots` into the annotation store, where FTS can
    /// reach them. Idempotent — re-importing adds nothing.
    #[tauri::command]
    pub async fn pdf_import_annotations(
        state: State<'_, AppState>,
        path: String,
        doc_id: String,
    ) -> Result<usize, String> {
        let store = get_store(&state).await?;
        tokio::task::spawn_blocking(move || {
            let annots = read_annotations_from_path(Path::new(&path))?;
            import_into_store(&store, &doc_id, &annots)
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }

    /// Write the store's annotations for `doc_id` into a PDF, so markup
    /// made in the app survives export and is visible to other readers.
    #[tauri::command]
    pub async fn pdf_stamp_annotations(
        state: State<'_, AppState>,
        path: String,
        doc_id: String,
        out_path: String,
    ) -> Result<usize, String> {
        let store = get_store(&state).await?;
        tokio::task::spawn_blocking(move || {
            export_from_store(&store, &doc_id, Path::new(&path), Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_read_annotations(path: String) -> Result<Vec<PdfAnnotation>, String> {
        tokio::task::spawn_blocking(move || read_annotations_from_path(Path::new(&path)))
            .await
            .map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_write_annotations(
        path: String,
        annotations: Vec<PdfAnnotation>,
        out_path: String,
    ) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || {
            write_annotations(Path::new(&path), &annotations, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_export_annotations(
        path: String,
        format: String,
        out_path: String,
    ) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || {
            export_annotations(Path::new(&path), &format, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_doc(pages: usize) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let kids: Vec<Object> = (0..pages)
            .map(|_| {
                let page = lopdf::Dictionary::from_iter(vec![
                    ("Type", Object::Name(b"Page".to_vec())),
                    ("Parent", Object::Reference(pages_id)),
                    ("MediaBox", Object::Array(vec![
                        Object::Integer(0), Object::Integer(0),
                        Object::Integer(612), Object::Integer(792),
                    ])),
                ]);
                Object::Reference(doc.add_object(Object::Dictionary(page)))
            })
            .collect();
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

    #[test]
    fn hex_colour_round_trips() {
        assert_eq!(rgb_to_hex(hex_to_rgb("#facc15")), "#facc15");
        assert_eq!(rgb_to_hex(hex_to_rgb("60a5fa")), "#60a5fa");
    }

    #[test]
    fn unparseable_colour_falls_back_to_yellow_not_black() {
        // Black would render as an invisible highlight on dark text.
        assert_eq!(rgb_to_hex(hex_to_rgb("nonsense")), "#facc15");
        assert_eq!(rgb_to_hex(hex_to_rgb("#zzzzzz")), "#facc15");
        assert_eq!(rgb_to_hex(hex_to_rgb("")), "#facc15");
    }

    #[test]
    fn multibyte_colour_string_does_not_panic() {
        // "üüü" is 3 chars but 6 bytes, so a naive length check passes and
        // slicing &h[0..2] would land mid-character and panic.
        assert_eq!(rgb_to_hex(hex_to_rgb("üüü")), "#facc15");
        assert_eq!(rgb_to_hex(hex_to_rgb("#日本語")), "#facc15");
    }

    #[test]
    fn write_then_read_round_trips_an_annotation() {
        let mut doc = blank_doc(2);
        let a = PdfAnnotation::highlight(1, 72.0, 100.0, 200.0, 14.0, "important bit", "#facc15");
        assert_eq!(write_annotations_doc(&mut doc, &[a.clone()]).unwrap(), 1);

        let back = read_annotations(&doc);
        assert_eq!(back.len(), 1);
        let b = &back[0];
        assert_eq!(b.page, 1);
        assert_eq!(b.ann_type, "highlight");
        assert_eq!(b.text, "important bit");
        assert_eq!(b.color, "#facc15");
        assert!((b.x - 72.0).abs() < 0.01);
        assert!((b.w - 200.0).abs() < 0.01);
        assert_eq!(b.quads.len(), 8);
    }

    #[test]
    fn non_ascii_text_survives_the_round_trip() {
        let mut doc = blank_doc(1);
        let a = PdfAnnotation::highlight(0, 10.0, 10.0, 50.0, 12.0, "Übermäßig — 日本語", "#60a5fa");
        write_annotations_doc(&mut doc, &[a]).unwrap();
        let back = read_annotations(&doc);
        assert_eq!(back[0].text, "Übermäßig — 日本語");
    }

    #[test]
    fn author_is_preserved_when_present_and_absent() {
        let mut doc = blank_doc(1);
        let mut with = PdfAnnotation::highlight(0, 0.0, 0.0, 10.0, 10.0, "x", "#facc15");
        with.author = Some("Ada".into());
        let without = PdfAnnotation::highlight(0, 20.0, 0.0, 10.0, 10.0, "y", "#facc15");
        write_annotations_doc(&mut doc, &[with, without]).unwrap();
        let back = read_annotations(&doc);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].author.as_deref(), Some("Ada"));
        assert_eq!(back[1].author, None);
    }

    #[test]
    fn writing_preserves_existing_annotations() {
        let mut doc = blank_doc(1);
        write_annotations_doc(&mut doc, &[PdfAnnotation::highlight(0, 0.0, 0.0, 10.0, 10.0, "first", "#facc15")]).unwrap();
        write_annotations_doc(&mut doc, &[PdfAnnotation::highlight(0, 20.0, 0.0, 10.0, 10.0, "second", "#facc15")]).unwrap();
        let back = read_annotations(&doc);
        assert_eq!(back.len(), 2, "second write must not clobber the first");
        let texts: Vec<&str> = back.iter().map(|a| a.text.as_str()).collect();
        assert!(texts.contains(&"first") && texts.contains(&"second"));
    }

    #[test]
    fn inverted_rect_is_normalised() {
        // Producers may emit [x1 y1 x0 y0]; readers must cope.
        let mut doc = blank_doc(1);
        let annot = lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Annot".to_vec())),
            ("Subtype", Object::Name(b"Square".to_vec())),
            ("Rect", Object::Array(vec![
                Object::Real(300.0), Object::Real(400.0),
                Object::Real(100.0), Object::Real(200.0),
            ])),
        ]);
        let id = doc.add_object(Object::Dictionary(annot));
        let page_id = doc.page_iter().next().unwrap();
        if let Ok(Object::Dictionary(ref mut p)) = doc.get_object_mut(page_id) {
            p.set("Annots", Object::Array(vec![Object::Reference(id)]));
        }
        let back = read_annotations(&doc);
        assert_eq!(back.len(), 1);
        assert!((back[0].x - 100.0).abs() < 0.01);
        assert!((back[0].y - 200.0).abs() < 0.01);
        assert!(back[0].w > 0.0 && back[0].h > 0.0, "width/height must be positive");
    }

    #[test]
    fn links_and_popups_are_skipped() {
        let mut doc = blank_doc(1);
        let mut ids = Vec::new();
        for st in [&b"Link"[..], &b"Popup"[..], &b"Highlight"[..]] {
            let d = lopdf::Dictionary::from_iter(vec![
                ("Type", Object::Name(b"Annot".to_vec())),
                ("Subtype", Object::Name(st.to_vec())),
                ("Rect", Object::Array(vec![
                    Object::Real(0.0), Object::Real(0.0),
                    Object::Real(10.0), Object::Real(10.0),
                ])),
            ]);
            ids.push(Object::Reference(doc.add_object(Object::Dictionary(d))));
        }
        let page_id = doc.page_iter().next().unwrap();
        if let Ok(Object::Dictionary(ref mut p)) = doc.get_object_mut(page_id) {
            p.set("Annots", Object::Array(ids));
        }
        let back = read_annotations(&doc);
        assert_eq!(back.len(), 1, "only the Highlight is markup");
        assert_eq!(back[0].ann_type, "highlight");
    }

    #[test]
    fn eraser_preserves_unsupported_annotations_and_indirect_lists() {
        let mut doc = blank_doc(1);
        write_annotations_doc(&mut doc, &[PdfAnnotation::highlight(0, 0.0, 0.0, 20.0, 20.0, "editable", "#facc15")]).unwrap();
        let page_id = doc.page_iter().next().unwrap();
        let mut list = doc.get_object(page_id).unwrap().as_dict().unwrap()
            .get(b"Annots").unwrap().as_array().unwrap().clone();
        for subtype in ["Link", "Widget", "Popup", "FileAttachment", "Screen"] {
            let annotation = lopdf::Dictionary::from_iter(vec![
                ("Type", Object::Name(b"Annot".to_vec())),
                ("Subtype", Object::Name(subtype.as_bytes().to_vec())),
                ("Rect", Object::Array(vec![0.into(), 0.into(), 20.into(), 20.into()])),
            ]);
            list.push(Object::Reference(doc.add_object(Object::Dictionary(annotation))));
        }
        let list_id = doc.add_object(Object::Array(list));
        doc.get_object_mut(page_id).unwrap().as_dict_mut().unwrap().set("Annots", Object::Reference(list_id));
        erase_annotation_at(&mut doc, 0, 10.0, 10.0).unwrap();
        let remaining = doc.get_object(page_id).unwrap().as_dict().unwrap().get(b"Annots").unwrap().as_array().unwrap();
        assert_eq!(remaining.len(), 5);
        assert!(read_annotations(&doc).is_empty());
        assert!(erase_annotation_at(&mut doc, 0, 10.0, 10.0).is_err(), "non-markup attachments and controls are not eraser targets");
    }

    #[test]
    fn out_of_range_page_is_rejected() {
        let mut doc = blank_doc(1);
        let a = PdfAnnotation::highlight(5, 0.0, 0.0, 10.0, 10.0, "x", "#facc15");
        assert!(write_annotations_doc(&mut doc, &[a]).is_err());
    }

    #[test]
    fn markdown_export_groups_by_page() {
        let annots = vec![
            PdfAnnotation::highlight(0, 0.0, 0.0, 1.0, 1.0, "first note", "#facc15"),
            PdfAnnotation::highlight(0, 0.0, 0.0, 1.0, 1.0, "second note", "#facc15"),
            PdfAnnotation::highlight(2, 0.0, 0.0, 1.0, 1.0, "later note", "#facc15"),
        ];
        let md = to_markdown(&annots, "Doc");
        assert!(md.starts_with("# Doc"));
        assert_eq!(md.matches("## Page 1").count(), 1);
        assert_eq!(md.matches("## Page 3").count(), 1);
        assert!(md.contains("- first note"));
    }

    #[test]
    fn csv_export_escapes_quotes_and_commas() {
        let mut a = PdfAnnotation::highlight(0, 0.0, 0.0, 1.0, 1.0, "he said \"hi\", loudly", "#facc15");
        a.author = Some("Ada".into());
        let csv = to_csv(&[a]);
        assert!(csv.contains("\"he said \"\"hi\"\", loudly\""));
        // Header plus exactly one record — an unescaped comma would split it.
        assert_eq!(csv.lines().count(), 2);
    }

    #[test]
    fn dedup_key_tolerates_f32_serialisation_drift() {
        // /Rect is written as Real (f32), so a value that survives a
        // save/load cycle comes back slightly changed. An exact-float key
        // would make every re-import duplicate the whole annotation set.
        let a = dedup_key(0, 72.0, 100.0, 200.0, 14.0, "highlight", "x");
        let b = dedup_key(0, 72.000004, 99.999996, 200.00001, 14.000001, "highlight", "x");
        assert_eq!(a, b);
    }

    #[test]
    fn dedup_key_still_separates_genuinely_different_annotations() {
        let base = dedup_key(0, 72.0, 100.0, 200.0, 14.0, "highlight", "x");
        assert_ne!(base, dedup_key(1, 72.0, 100.0, 200.0, 14.0, "highlight", "x"));
        assert_ne!(base, dedup_key(0, 92.0, 100.0, 200.0, 14.0, "highlight", "x"));
        assert_ne!(base, dedup_key(0, 72.0, 100.0, 200.0, 14.0, "note", "x"));
        assert_ne!(base, dedup_key(0, 72.0, 100.0, 200.0, 14.0, "highlight", "y"));
        // A tenth of a point apart is a different annotation, not drift.
        assert_ne!(base, dedup_key(0, 72.2, 100.0, 200.0, 14.0, "highlight", "x"));
    }

    #[test]
    fn empty_export_is_not_an_error() {
        assert!(to_markdown(&[], "Doc").contains("No annotations"));
        assert_eq!(to_csv(&[]).lines().count(), 1);
    }
}
