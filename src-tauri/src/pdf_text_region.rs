//! Text regions: draw a rectangle, fill it with wrapped, formatted text
//! (P32.9).
//!
//! [`crate::pdf_ops::add_text_box_doc`] places text at a point and breaks
//! only where the caller put a newline. That is fine for a stamp and
//! useless for a paragraph: you have to know in advance where every line
//! ends. A region takes a box and lays the text out inside it.
//!
//! ## Why the width tables exist
//!
//! Wrapping requires knowing how wide each glyph is. An average-width
//! guess produces lines that either overflow the box or stop well short of
//! it, which is the one thing a text box must not do. So
//! [`crate::pdf_base14`] carries real per-glyph widths for the base-14
//! faces, generated from metrically-compatible AFM metrics.
//!
//! ## Limits
//!
//! * Base-14 faces only, so the character repertoire is Latin-1. A
//!   character the face has no glyph for is reported rather than dropped
//!   silently — see [`LayoutReport::unsupported_chars`].
//! * No hyphenation. A word longer than the box is broken at the last
//!   glyph that fits, because the alternative is drawing outside the box.
//! * Text that does not fit is *not* drawn, and the overflow is reported.
//!   Silently spilling past the rectangle would defeat the point.

use crate::pdf_base14::Base14;
use lopdf::{Document, Object};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    /// Stretch inter-word gaps so both edges line up. The last line of a
    /// paragraph is left-aligned, as convention requires.
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum VerticalAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextRegion {
    /// 0-based page index.
    pub page: usize,
    /// Box in points, origin bottom-left.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub text: String,
    #[serde(default)]
    pub font: Base14,
    pub font_size: f64,
    /// RGB, 0.0–1.0.
    #[serde(default)]
    pub color: [f64; 3],
    #[serde(default)]
    pub align: Align,
    #[serde(default)]
    pub vertical_align: VerticalAlign,
    /// Baseline-to-baseline distance as a multiple of the font size.
    #[serde(default)]
    pub line_height: Option<f64>,
    /// Inset from the box edges, in points.
    #[serde(default)]
    pub padding: Option<f64>,
    /// Stroke the rectangle — useful while positioning it.
    #[serde(default)]
    pub draw_border: bool,
}

impl Default for TextRegion {
    fn default() -> Self {
        Self {
            page: 0,
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 100.0,
            text: String::new(),
            font: Base14::default(),
            font_size: 11.0,
            color: [0.0, 0.0, 0.0],
            align: Align::default(),
            vertical_align: VerticalAlign::default(),
            line_height: None,
            padding: None,
            draw_border: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LayoutReport {
    /// Lines drawn.
    pub lines: usize,
    /// Lines that did not fit vertically and were not drawn.
    pub lines_dropped: usize,
    /// True when anything was dropped.
    pub overflow: bool,
    /// Characters the chosen face cannot render, deduplicated.
    pub unsupported_chars: Vec<char>,
}

/// One laid-out line.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    /// Byte codes to draw.
    codes: Vec<u8>,
    /// Width in points.
    width: f64,
    /// Inter-word gaps, for justification.
    gaps: usize,
    /// Last line of a paragraph — never justified.
    ends_paragraph: bool,
}

/// Map a char to its single-byte code, if the face can render it.
fn code_of(ch: char, font: Base14) -> Option<u8> {
    let cp = ch as u32;
    if cp > 0xFF {
        return None;
    }
    let b = cp as u8;
    // Width 0 means "no glyph" in the tables; space is the one legitimate
    // exception we look up directly.
    if b != b' ' && font.width(b) == 0 {
        return None;
    }
    Some(b)
}

fn text_width(codes: &[u8], font: Base14, size: f64) -> f64 {
    codes.iter().map(|&c| font.width(c) as f64).sum::<f64>() / 1000.0 * size
}

/// Greedy word wrap into `max_width`.
///
/// Hard newlines end a paragraph; everything else flows. A word wider than
/// the box is split at the last glyph that fits rather than drawn outside
/// it.
fn wrap(
    text: &str,
    font: Base14,
    size: f64,
    max_width: f64,
    unsupported: &mut Vec<char>,
) -> Vec<Line> {
    let mut lines = Vec::new();
    let space_w = font.width(b' ') as f64 / 1000.0 * size;

    for (pi, para) in text.split('\n').enumerate() {
        let _ = pi;
        let mut cur: Vec<u8> = Vec::new();
        let mut cur_w = 0.0f64;
        let mut gaps = 0usize;

        let words: Vec<&str> = para.split(' ').collect();
        for (wi, word) in words.iter().enumerate() {
            let mut codes: Vec<u8> = Vec::new();
            for ch in word.chars() {
                match code_of(ch, font) {
                    Some(b) => codes.push(b),
                    None => {
                        if !unsupported.contains(&ch) {
                            unsupported.push(ch);
                        }
                    }
                }
            }
            let ww = text_width(&codes, font, size);

            let need = if cur.is_empty() { ww } else { cur_w + space_w + ww };
            if need <= max_width || cur.is_empty() {
                if !cur.is_empty() {
                    cur.push(b' ');
                    cur_w += space_w;
                    gaps += 1;
                }
                // A single word too wide for the box: split it.
                if ww > max_width && cur_w == 0.0 {
                    let mut chunk: Vec<u8> = Vec::new();
                    for b in codes {
                        let next = text_width(&[b], font, size);
                        if text_width(&chunk, font, size) + next > max_width && !chunk.is_empty() {
                            let w = text_width(&chunk, font, size);
                            lines.push(Line { codes: std::mem::take(&mut chunk), width: w, gaps: 0, ends_paragraph: false });
                        }
                        chunk.push(b);
                    }
                    cur = chunk;
                    cur_w = text_width(&cur, font, size);
                } else {
                    cur.extend_from_slice(&codes);
                    cur_w += ww;
                }
            } else {
                lines.push(Line { codes: std::mem::take(&mut cur), width: cur_w, gaps, ends_paragraph: false });
                gaps = 0;
                cur = codes;
                cur_w = ww;
            }
            let _ = wi;
        }
        // Whatever is left ends this paragraph.
        lines.push(Line { codes: cur, width: cur_w, gaps, ends_paragraph: true });
    }
    lines
}

/// Lay out and draw a text region.
pub fn draw_region_doc(doc: &mut Document, region: &TextRegion) -> Result<LayoutReport, String> {
    let page_ids: Vec<lopdf::ObjectId> = doc.page_iter().collect();
    let page_id = *page_ids
        .get(region.page)
        .ok_or_else(|| format!("page {} out of range (0..{})", region.page, page_ids.len()))?;

    if region.w <= 0.0 || region.h <= 0.0 {
        return Err("region must have a positive width and height".into());
    }
    if region.font_size <= 0.0 {
        return Err("font size must be positive".into());
    }

    let pad = region.padding.unwrap_or(2.0);
    let inner_w = (region.w - 2.0 * pad).max(1.0);
    let inner_h = (region.h - 2.0 * pad).max(1.0);
    let leading = region.font_size * region.line_height.unwrap_or(1.2);

    let mut report = LayoutReport::default();
    let lines = wrap(
        &region.text,
        region.font,
        region.font_size,
        inner_w,
        &mut report.unsupported_chars,
    );

    // How many lines fit. Ascent of ~0.75em keeps the first baseline
    // inside the box rather than on its top edge.
    let max_lines = ((inner_h + (leading - region.font_size)) / leading).floor() as usize;
    let fitting = lines.len().min(max_lines.max(0));
    report.lines = fitting;
    report.lines_dropped = lines.len().saturating_sub(fitting);
    report.overflow = report.lines_dropped > 0;

    let block_h = fitting as f64 * leading;
    let top = match region.vertical_align {
        VerticalAlign::Top => region.y + region.h - pad,
        VerticalAlign::Middle => region.y + (region.h + block_h) / 2.0,
        VerticalAlign::Bottom => region.y + pad + block_h,
    };

    let [r, g, b] = region.color;
    let mut content = String::new();

    if region.draw_border {
        content.push_str(&format!(
            "q 0.6 0.6 0.6 RG 0.5 w {:.2} {:.2} {:.2} {:.2} re S Q\n",
            region.x, region.y, region.w, region.h
        ));
    }

    if fitting > 0 {
        content.push_str(&format!("q {r:.3} {g:.3} {b:.3} rg BT /RF {} Tf\n", region.font_size));
        for (i, line) in lines.iter().take(fitting).enumerate() {
            let baseline = top - region.font_size * 0.8 - i as f64 * leading;
            let (x, word_spacing) = match region.align {
                Align::Left => (region.x + pad, 0.0),
                Align::Center => (region.x + pad + (inner_w - line.width) / 2.0, 0.0),
                Align::Right => (region.x + pad + (inner_w - line.width), 0.0),
                Align::Justify => {
                    // Stretch the gaps, except on a paragraph's last line.
                    if line.ends_paragraph || line.gaps == 0 {
                        (region.x + pad, 0.0)
                    } else {
                        let extra = (inner_w - line.width) / line.gaps as f64;
                        (region.x + pad, extra.max(0.0))
                    }
                }
            };
            // Tw applies to every space in the string, which is exactly
            // what justification needs.
            content.push_str(&format!("{:.3} Tw 1 0 0 1 {:.2} {:.2} Tm ({}) Tj\n",
                word_spacing, x, baseline,
                escape_codes(&line.codes)));
        }
        content.push_str("0 Tw ET Q\n");
    }

    let font_id = add_base14(doc, region.font);
    crate::pdf_ops::append_content(
        doc,
        page_id,
        content.into_bytes(),
        Some(("RF", font_id)),
        None,
    );
    Ok(report)
}

/// Escape a byte string for a PDF literal.
fn escape_codes(codes: &[u8]) -> String {
    let mut out = String::with_capacity(codes.len() + 8);
    for &c in codes {
        match c {
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            b'\\' => out.push_str("\\\\"),
            // Non-ASCII bytes are legal in a literal string but easier to
            // read (and safer through any transport) as octal escapes.
            0x20..=0x7E => out.push(c as char),
            _ => out.push_str(&format!("\\{c:03o}")),
        }
    }
    out
}

fn add_base14(doc: &mut Document, font: Base14) -> lopdf::ObjectId {
    doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Font".to_vec())),
        ("Subtype", Object::Name(b"Type1".to_vec())),
        ("BaseFont", Object::Name(font.base_font().as_bytes().to_vec())),
        // WinAnsi so the Latin-1 codes above map to the glyphs we measured.
        ("Encoding", Object::Name(b"WinAnsiEncoding".to_vec())),
    ])))
}

pub fn draw_region(
    path: &Path,
    regions: &[TextRegion],
    out_path: &Path,
) -> Result<Vec<LayoutReport>, String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let mut reports = Vec::with_capacity(regions.len());
    for r in regions {
        reports.push(draw_region_doc(&mut doc, r)?);
    }
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(reports)
}

/// Lay out without drawing — lets a GUI preview the fit before committing.
pub fn measure_region(region: &TextRegion) -> LayoutReport {
    let pad = region.padding.unwrap_or(2.0);
    let inner_w = (region.w - 2.0 * pad).max(1.0);
    let inner_h = (region.h - 2.0 * pad).max(1.0);
    let leading = region.font_size * region.line_height.unwrap_or(1.2);
    let mut report = LayoutReport::default();
    let lines = wrap(&region.text, region.font, region.font_size, inner_w,
                     &mut report.unsupported_chars);
    let max_lines = ((inner_h + (leading - region.font_size)) / leading).floor() as usize;
    let fitting = lines.len().min(max_lines);
    report.lines = fitting;
    report.lines_dropped = lines.len().saturating_sub(fitting);
    report.overflow = report.lines_dropped > 0;
    report
}

pub mod tauri_commands {
    use super::*;

    #[tauri::command]
    pub async fn pdf_draw_text_regions(
        path: String,
        regions: Vec<TextRegion>,
        out_path: String,
    ) -> Result<Vec<LayoutReport>, String> {
        tokio::task::spawn_blocking(move || {
            draw_region(Path::new(&path), &regions, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }

    /// Preview the layout without writing anything.
    #[tauri::command]
    pub async fn pdf_measure_text_region(region: TextRegion) -> Result<LayoutReport, String> {
        Ok(measure_region(&region))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(pages: usize) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let kids: Vec<Object> = (0..pages)
            .map(|_| {
                Object::Reference(doc.add_object(Object::Dictionary(
                    lopdf::Dictionary::from_iter(vec![
                        ("Type", Object::Name(b"Page".to_vec())),
                        ("Parent", Object::Reference(pages_id)),
                        ("MediaBox", Object::Array(vec![
                            Object::Integer(0), Object::Integer(0),
                            Object::Integer(612), Object::Integer(792),
                        ])),
                    ]),
                )))
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

    fn region(text: &str, w: f64, h: f64) -> TextRegion {
        TextRegion {
            page: 0, x: 72.0, y: 600.0, w, h,
            text: text.into(), font_size: 10.0, ..Default::default()
        }
    }

    #[test]
    fn widths_match_the_canonical_adobe_metrics() {
        // If these drift, every wrap decision is wrong.
        assert_eq!(Base14::Helvetica.width(b' '), 278);
        assert_eq!(Base14::Helvetica.width(b'A'), 667);
        assert_eq!(Base14::Helvetica.width(b'i'), 222);
        assert_eq!(Base14::Helvetica.width(b'W'), 944);
        assert_eq!(Base14::TimesRoman.width(b'A'), 722);
        // Courier is monospaced.
        for c in [b'i', b'W', b'.', b'M'] {
            assert_eq!(Base14::Courier.width(c), 600, "courier not monospaced at {c}");
        }
    }

    #[test]
    fn text_wraps_to_the_box_width() {
        let text = "the quick brown fox jumps over the lazy dog again and again";
        let narrow = measure_region(&region(text, 90.0, 300.0));
        let wide = measure_region(&region(text, 400.0, 300.0));
        assert!(narrow.lines > wide.lines,
                "narrow={} wide={}", narrow.lines, wide.lines);
        assert!(wide.lines >= 1);
    }

    #[test]
    fn no_line_exceeds_the_box() {
        let text = "the quick brown fox jumps over the lazy dog";
        let r = region(text, 120.0, 300.0);
        let mut unsupported = Vec::new();
        let pad = 2.0;
        let inner = r.w - 2.0 * pad;
        for line in wrap(&r.text, r.font, r.font_size, inner, &mut unsupported) {
            assert!(line.width <= inner + 0.01,
                    "line {:.2}pt exceeds {:.2}pt box", line.width, inner);
        }
    }

    #[test]
    fn hard_newlines_start_new_lines() {
        let r = measure_region(&region("one\ntwo\nthree", 400.0, 300.0));
        assert_eq!(r.lines, 3);
    }

    #[test]
    fn a_word_wider_than_the_box_is_split_not_spilled() {
        let long = "W".repeat(60);
        let r = region(&long, 60.0, 300.0);
        let mut unsupported = Vec::new();
        let lines = wrap(&r.text, r.font, r.font_size, 56.0, &mut unsupported);
        assert!(lines.len() > 1, "unsplittable word was not broken");
        for l in &lines {
            assert!(l.width <= 56.01, "split chunk {:.2} too wide", l.width);
        }
    }

    #[test]
    fn overflow_is_reported_not_drawn() {
        // A one-line-high box given ten lines of text.
        let text = (0..10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let r = measure_region(&region(&text, 200.0, 16.0));
        assert!(r.overflow, "{r:?}");
        assert!(r.lines_dropped > 0);
        assert!(r.lines < 10);
    }

    #[test]
    fn unsupported_characters_are_reported() {
        let r = measure_region(&region("hello 日本語 world", 400.0, 100.0));
        assert!(!r.unsupported_chars.is_empty(), "{r:?}");
        assert!(r.unsupported_chars.contains(&'日'));
        // Reported once each, not once per occurrence.
        let mut seen = r.unsupported_chars.clone();
        seen.dedup();
        assert_eq!(seen.len(), r.unsupported_chars.len());
    }

    #[test]
    fn accented_widths_are_winansi_not_adobe_standard() {
        // Regression: the width tables were first keyed by the AFM's own
        // `C` codes, which are Adobe-Standard-encoded. Every accented
        // character then looked unsupported even though the face has it.
        // These are the WinAnsi positions we declare in the font dict.
        assert_eq!(Base14::Helvetica.width(0xDC), 722, "Udieresis");
        assert_eq!(Base14::Helvetica.width(0xE4), 556, "adieresis");
        assert_eq!(Base14::Helvetica.width(0xDF), 611, "germandbls");
        assert_eq!(Base14::Helvetica.width(0xEF), 278, "idieresis");
    }

    #[test]
    fn latin1_accents_are_supported() {
        let r = measure_region(&region("Übermäßig café naïve", 400.0, 100.0));
        assert!(r.unsupported_chars.is_empty(), "{:?}", r.unsupported_chars);
    }

    #[test]
    fn drawing_appends_content_and_reports_lines() {
        let mut doc = blank(1);
        let rep = draw_region_doc(&mut doc, &region("hello wrapped world", 120.0, 100.0)).unwrap();
        assert!(rep.lines >= 1);
        assert!(!rep.overflow);
        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        let ops: Vec<&str> = content.operations.iter().map(|o| o.operator.as_str()).collect();
        assert!(ops.contains(&"BT") && ops.contains(&"Tj") && ops.contains(&"Tf"));
    }

    #[test]
    fn each_alignment_places_the_line_differently() {
        let mut xs = Vec::new();
        for align in [Align::Left, Align::Center, Align::Right] {
            let mut doc = blank(1);
            let mut r = region("short", 300.0, 100.0);
            r.align = align;
            draw_region_doc(&mut doc, &r).unwrap();
            let page_id = doc.page_iter().next().unwrap();
            let c = doc.get_and_decode_page_content(page_id).unwrap();
            let tm = c.operations.iter().find(|o| o.operator == "Tm").expect("Tm");
            let x = match tm.operands.get(4) {
                Some(Object::Real(v)) => *v as f64,
                Some(Object::Integer(v)) => *v as f64,
                _ => panic!("no x in Tm"),
            };
            xs.push(x);
        }
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "left/center/right not ordered: {xs:?}");
    }

    #[test]
    fn justify_sets_word_spacing_but_not_on_the_last_line() {
        let mut doc = blank(1);
        let mut r = region("aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk", 140.0, 200.0);
        r.align = Align::Justify;
        draw_region_doc(&mut doc, &r).unwrap();
        let page_id = doc.page_iter().next().unwrap();
        let c = doc.get_and_decode_page_content(page_id).unwrap();
        let tws: Vec<f64> = c.operations.iter()
            .filter(|o| o.operator == "Tw")
            .filter_map(|o| match o.operands.first() {
                Some(Object::Real(v)) => Some(*v as f64),
                Some(Object::Integer(v)) => Some(*v as f64),
                _ => None,
            })
            .collect();
        assert!(tws.iter().any(|v| *v > 0.0), "no line was justified: {tws:?}");
        assert!(tws.iter().any(|v| *v == 0.0), "last line should not be justified: {tws:?}");
    }

    #[test]
    fn vertical_alignment_moves_the_block() {
        let mut ys = Vec::new();
        for va in [VerticalAlign::Bottom, VerticalAlign::Middle, VerticalAlign::Top] {
            let mut doc = blank(1);
            let mut r = region("one line", 300.0, 200.0);
            r.vertical_align = va;
            draw_region_doc(&mut doc, &r).unwrap();
            let page_id = doc.page_iter().next().unwrap();
            let c = doc.get_and_decode_page_content(page_id).unwrap();
            let tm = c.operations.iter().find(|o| o.operator == "Tm").unwrap();
            let y = match tm.operands.get(5) {
                Some(Object::Real(v)) => *v as f64,
                Some(Object::Integer(v)) => *v as f64,
                _ => panic!(),
            };
            ys.push(y);
        }
        assert!(ys[0] < ys[1] && ys[1] < ys[2], "bottom/middle/top not ordered: {ys:?}");
    }

    #[test]
    fn a_border_can_be_drawn_for_positioning() {
        let mut doc = blank(1);
        let mut r = region("x", 100.0, 50.0);
        r.draw_border = true;
        draw_region_doc(&mut doc, &r).unwrap();
        let page_id = doc.page_iter().next().unwrap();
        let c = doc.get_and_decode_page_content(page_id).unwrap();
        let ops: Vec<&str> = c.operations.iter().map(|o| o.operator.as_str()).collect();
        assert!(ops.contains(&"re") && ops.contains(&"S"), "no stroked rectangle: {ops:?}");
    }

    #[test]
    fn degenerate_geometry_is_rejected() {
        let mut doc = blank(1);
        let mut r = region("x", 0.0, 100.0);
        assert!(draw_region_doc(&mut doc, &r).is_err());
        r.w = 100.0;
        r.font_size = 0.0;
        assert!(draw_region_doc(&mut doc, &r).is_err());
        r.font_size = 10.0;
        r.page = 9;
        assert!(draw_region_doc(&mut doc, &r).is_err());
    }

    #[test]
    fn a_bigger_font_needs_more_lines_for_the_same_text() {
        let text = "the quick brown fox jumps over the lazy dog";
        let mut small = region(text, 200.0, 400.0);
        small.font_size = 8.0;
        let mut big = region(text, 200.0, 400.0);
        big.font_size = 18.0;
        assert!(measure_region(&big).lines > measure_region(&small).lines);
    }
}
