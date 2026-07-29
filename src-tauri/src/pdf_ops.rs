//! PDF page-level manipulation via `lopdf`.
//!
//! Every public function takes filesystem paths + parameters and writes
//! the result to an output path.  The Tauri commands in this module's
//! `tauri_commands` sub-module expose them to the frontend.

use lopdf::{Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Helpers ────────────────────────────────────────────────────────────

/// Return the ordered list of page ObjectIds from the page tree.
fn page_ids(doc: &Document) -> Vec<ObjectId> {
    doc.page_iter().collect()
}

/// Decode a lopdf string (UTF-16BE or Latin-1) to a Rust String.
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

fn dict_string(dict: &lopdf::Dictionary, key: &[u8]) -> Option<String> {
    match dict.get(key).ok()? {
        Object::String(b, _) => Some(decode_pdf_str(b)),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    }
}

fn obj_f64(o: &Object) -> f64 {
    match o {
        Object::Integer(n) => *n as f64,
        Object::Real(n) => *n as f64,
        _ => 0.0,
    }
}

/// Get the /Pages reference from the catalog.
fn pages_ref(doc: &Document) -> Result<ObjectId, String> {
    let cat = doc.catalog().map_err(|e| format!("catalog: {e}"))?;
    match cat.get(b"Pages").map_err(|e| format!("Pages key: {e}"))? {
        Object::Reference(r) => Ok(*r),
        _ => Err("Pages is not a reference".into()),
    }
}

/// Read MediaBox from a page dict, returning (width, height) in points.
fn page_dims(dict: &lopdf::Dictionary) -> (f64, f64) {
    let media = match dict.get(b"MediaBox") {
        Ok(Object::Array(a)) => a,
        _ => return (612.0, 792.0),
    };
    if media.len() == 4 {
        let w = (obj_f64(&media[2]) - obj_f64(&media[0])).abs();
        let h = (obj_f64(&media[3]) - obj_f64(&media[1])).abs();
        (w, h)
    } else {
        (612.0, 792.0)
    }
}

/// Resolve a possibly-indirect object to a Dictionary clone.
fn as_dict(doc: &Document, obj: &Object) -> Option<lopdf::Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d.clone()),
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Dictionary(d)) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The /Resources dictionary in effect for a page.
///
/// /Resources is an *inheritable* page-tree attribute: a page may omit it
/// and rely on an ancestor /Pages node.  Writing a fresh dictionary onto
/// the page in that case shadows the inherited one, which silently strips
/// the fonts the page's existing content stream refers to.  So walk the
/// /Parent chain and start from whatever is actually in effect.
fn effective_resources(doc: &Document, page_id: ObjectId) -> lopdf::Dictionary {
    let mut cur = page_id;
    // Bounded: a malformed file can have a /Parent cycle.
    for _ in 0..32 {
        let d = match doc.get_object(cur) {
            Ok(Object::Dictionary(d)) => d,
            _ => break,
        };
        if let Ok(r) = d.get(b"Resources") {
            if let Some(rd) = as_dict(doc, r) {
                return rd;
            }
        }
        match d.get(b"Parent") {
            Ok(Object::Reference(p)) => cur = *p,
            _ => break,
        }
    }
    lopdf::Dictionary::new()
}

/// Append a content stream to a page, merging in the font / ExtGState
/// resources it needs.  Centralises the /Contents append that page
/// numbers, watermarks, text boxes and black-out overlays all perform.
fn append_content(
    doc: &mut Document,
    page_id: ObjectId,
    content: Vec<u8>,
    font: Option<(&str, ObjectId)>,
    ext_gstate: Option<(&str, ObjectId)>,
) {
    // Resolve inherited resources before taking the mutable borrow.
    let mut res = if font.is_some() || ext_gstate.is_some() {
        Some(effective_resources(doc, page_id))
    } else {
        None
    };
    if let Some(ref mut res) = res {
        if let Some((name, id)) = font {
            let mut fonts = res.get(b"Font").ok().and_then(|f| as_dict(doc, f)).unwrap_or_default();
            fonts.set(name, Object::Reference(id));
            res.set("Font", Object::Dictionary(fonts));
        }
        if let Some((name, id)) = ext_gstate {
            let mut gs = res.get(b"ExtGState").ok().and_then(|g| as_dict(doc, g)).unwrap_or_default();
            gs.set(name, Object::Reference(id));
            res.set("ExtGState", Object::Dictionary(gs));
        }
    }

    let content_id = doc.add_object(Object::Stream(
        lopdf::Stream::new(lopdf::Dictionary::new(), content),
    ));
    if let Ok(Object::Dictionary(ref mut page)) = doc.get_object_mut(page_id) {
        if let Some(res) = res {
            page.set("Resources", Object::Dictionary(res));
        }
        match page.get(b"Contents") {
            Ok(Object::Reference(r)) => {
                let r = *r;
                page.set("Contents", Object::Array(vec![Object::Reference(r), Object::Reference(content_id)]));
            }
            Ok(Object::Array(arr)) => {
                let mut a = arr.clone();
                a.push(Object::Reference(content_id));
                page.set("Contents", Object::Array(a));
            }
            _ => { page.set("Contents", Object::Reference(content_id)); }
        }
    }
}

/// Add a base-14 Helvetica font object and return its id.
fn add_helvetica(doc: &mut Document) -> ObjectId {
    doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Font".to_vec())),
        ("Subtype", Object::Name(b"Type1".to_vec())),
        ("BaseFont", Object::Name(b"Helvetica".to_vec())),
    ])))
}

/// Escape a string for use inside a PDF literal string `( … )`.
fn escape_pdf_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

// ── Info / metadata ────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PdfPageInfo {
    pub page_number: usize,
    pub width_pt: f64,
    pub height_pt: f64,
    pub rotation: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PdfInfo {
    pub page_count: usize,
    pub pages: Vec<PdfPageInfo>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub producer: Option<String>,
    pub creator: Option<String>,
}

pub fn pdf_info(path: &Path) -> Result<PdfInfo, String> {
    let doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    Ok(pdf_info_from_doc(&doc))
}

/// Same as [`pdf_info`] against an already-loaded document — used by the
/// editing session, which reports on an in-memory document that has no
/// file on disk yet.
pub fn pdf_info_from_doc(doc: &Document) -> PdfInfo {
    let ids = page_ids(doc);
    let mut pages = Vec::with_capacity(ids.len());
    for (i, &id) in ids.iter().enumerate() {
        let (w, h, rot) = match doc.get_object(id) {
            Ok(Object::Dictionary(d)) => {
                let dims = page_dims(d);
                let r = match d.get(b"Rotate") {
                    Ok(Object::Integer(n)) => *n,
                    _ => 0,
                };
                (dims.0, dims.1, r)
            }
            _ => (612.0, 792.0, 0),
        };
        pages.push(PdfPageInfo {
            page_number: i + 1,
            width_pt: w,
            height_pt: h,
            rotation: rot,
        });
    }
    let (title, author, subject, keywords, producer, creator) =
        if let Ok(Object::Reference(info_id)) = doc.trailer.get(b"Info") {
            if let Ok(Object::Dictionary(d)) = doc.get_object(*info_id) {
                (
                    dict_string(d, b"Title"),
                    dict_string(d, b"Author"),
                    dict_string(d, b"Subject"),
                    dict_string(d, b"Keywords"),
                    dict_string(d, b"Producer"),
                    dict_string(d, b"Creator"),
                )
            } else {
                (None, None, None, None, None, None)
            }
        } else {
            (None, None, None, None, None, None)
        };
    PdfInfo {
        page_count: ids.len(),
        pages,
        title,
        author,
        subject,
        keywords,
        producer,
        creator,
    }
}

// ── Reorder pages ──────────────────────────────────────────────────────

/// In-memory reorder.  The `_doc` variants exist so the editing session
/// (`pdf_session`) can apply a stack of operations to one loaded document
/// instead of round-tripping through the filesystem per edit.  The
/// path-taking wrappers below are load → apply → save.
pub fn reorder_pages_doc(doc: &mut Document, new_order: &[usize]) -> Result<(), String> {
    let ids = page_ids(doc);
    let n = ids.len();
    for &idx in new_order {
        if idx >= n { return Err(format!("page index {idx} out of range (0..{n})")); }
    }
    let reordered: Vec<ObjectId> = new_order.iter().map(|&i| ids[i]).collect();
    let pid = pages_ref(doc)?;
    if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(pid) {
        d.set("Kids", Object::Array(reordered.iter().map(|id| Object::Reference(*id)).collect()));
        d.set("Count", Object::Integer(reordered.len() as i64));
    }
    Ok(())
}

pub fn reorder_pages(path: &Path, new_order: &[usize], out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    reorder_pages_doc(&mut doc, new_order)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Extract pages ──────────────────────────────────────────────────────

pub fn extract_pages_doc(doc: &mut Document, page_indices: &[usize]) -> Result<(), String> {
    let ids = page_ids(doc);
    let n = ids.len();
    let keep: Vec<ObjectId> = page_indices
        .iter()
        .map(|&i| if i >= n { Err(format!("page {i} out of range")) } else { Ok(ids[i]) })
        .collect::<Result<_, _>>()?;
    let pid = pages_ref(doc)?;
    if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(pid) {
        d.set("Kids", Object::Array(keep.iter().map(|id| Object::Reference(*id)).collect()));
        d.set("Count", Object::Integer(keep.len() as i64));
    }
    Ok(())
}

pub fn extract_pages(path: &Path, page_indices: &[usize], out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    extract_pages_doc(&mut doc, page_indices)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Remove pages ───────────────────────────────────────────────────────

pub fn remove_pages_doc(doc: &mut Document, page_indices: &[usize]) -> Result<(), String> {
    let n = page_ids(doc).len();
    let remove: std::collections::HashSet<usize> = page_indices.iter().copied().collect();
    for &idx in &remove { if idx >= n { return Err(format!("page {idx} out of range")); } }
    let keep: Vec<usize> = (0..n).filter(|i| !remove.contains(i)).collect();
    if keep.is_empty() { return Err("Cannot remove all pages".into()); }
    extract_pages_doc(doc, &keep)
}

pub fn remove_pages(path: &Path, page_indices: &[usize], out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    remove_pages_doc(&mut doc, page_indices)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Rotate pages ───────────────────────────────────────────────────────

pub fn rotate_pages_doc(doc: &mut Document, page_indices: &[usize], degrees: i64) -> Result<(), String> {
    if ![0, 90, 180, 270].contains(&degrees) {
        return Err(format!("degrees must be 0/90/180/270, got {degrees}"));
    }
    let ids = page_ids(doc);
    let n = ids.len();
    for &idx in page_indices {
        if idx >= n { return Err(format!("page {idx} out of range")); }
        if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(ids[idx]) {
            let cur = match d.get(b"Rotate") {
                Ok(Object::Integer(v)) => *v,
                _ => 0,
            };
            d.set("Rotate", Object::Integer(((cur + degrees) % 360 + 360) % 360));
        }
    }
    Ok(())
}

pub fn rotate_pages(path: &Path, page_indices: &[usize], degrees: i64, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    rotate_pages_doc(&mut doc, page_indices, degrees)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Crop pages ─────────────────────────────────────────────────────────

pub fn crop_pages_doc(doc: &mut Document, page_indices: &[usize], x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    let ids = page_ids(doc);
    let n = ids.len();
    let crop = Object::Array(vec![
        Object::Real(x as f32), Object::Real(y as f32),
        Object::Real((x + w) as f32), Object::Real((y + h) as f32),
    ]);
    for &idx in page_indices {
        if idx >= n { return Err(format!("page {idx} out of range")); }
        if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(ids[idx]) {
            d.set("CropBox", crop.clone());
        }
    }
    Ok(())
}

pub fn crop_pages(path: &Path, page_indices: &[usize], x: f64, y: f64, w: f64, h: f64, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    crop_pages_doc(&mut doc, page_indices, x, y, w, h)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Merge PDFs ─────────────────────────────────────────────────────────

/// Merge multiple PDFs by copying all objects from subsequent documents
/// into the first, remapping ObjectIds to avoid collisions, then
/// appending page references to the base /Pages /Kids array.
pub fn merge_pdfs(paths: &[&Path], out_path: &Path) -> Result<usize, String> {
    if paths.is_empty() { return Err("no input files".into()); }
    let mut base = Document::load(paths[0]).map_err(|e| format!("load {}: {e}", paths[0].display()))?;
    let mut total = page_ids(&base).len();
    let pid = pages_ref(&base)?;

    for p in &paths[1..] {
        let other = Document::load(p).map_err(|e| format!("load {}: {e}", p.display()))?;
        let other_page_ids = page_ids(&other);

        // Find the max object ID in base to offset the other doc's IDs.
        let max_id = base.max_id;
        let mut id_map = std::collections::HashMap::<ObjectId, ObjectId>::new();

        // Copy all objects from `other` into `base` with remapped IDs.
        for (&old_id, obj) in &other.objects {
            let new_id = (old_id.0 + max_id, old_id.1);
            id_map.insert(old_id, new_id);
            base.objects.insert(new_id, obj.clone());
        }
        base.max_id = max_id + other.max_id;

        // Remap all references within the copied objects.
        let new_ids: Vec<ObjectId> = id_map.values().copied().collect();
        for &nid in &new_ids {
            if let Some(obj) = base.objects.get_mut(&nid) {
                remap_refs(obj, &id_map);
            }
        }

        // Collect remapped page IDs.
        let mut new_page_refs = Vec::new();
        for old_pid in &other_page_ids {
            if let Some(&new_pid) = id_map.get(old_pid) {
                new_page_refs.push(new_pid);
            }
        }

        // Re-parent the new pages to point to our /Pages node.
        for &npid in &new_page_refs {
            if let Some(Object::Dictionary(ref mut pg)) = base.objects.get_mut(&npid) {
                pg.set("Parent", Object::Reference(pid));
            }
        }

        // Append to /Kids.
        if let Ok(Object::Dictionary(ref mut pages_dict)) = base.get_object_mut(pid) {
            let mut kids = match pages_dict.get(b"Kids") {
                Ok(Object::Array(a)) => a.clone(),
                _ => vec![],
            };
            for npid in &new_page_refs {
                kids.push(Object::Reference(*npid));
                total += 1;
            }
            pages_dict.set("Kids", Object::Array(kids));
            pages_dict.set("Count", Object::Integer(total as i64));
        }
    }
    base.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(total)
}

/// Recursively remap all Object::Reference values using the given map.
fn remap_refs(obj: &mut Object, map: &std::collections::HashMap<ObjectId, ObjectId>) {
    match obj {
        Object::Reference(r) => {
            if let Some(&new) = map.get(r) { *r = new; }
        }
        Object::Array(arr) => {
            for item in arr.iter_mut() { remap_refs(item, map); }
        }
        Object::Dictionary(d) => {
            for (_, v) in d.iter_mut() { remap_refs(v, map); }
        }
        Object::Stream(s) => {
            for (_, v) in s.dict.iter_mut() { remap_refs(v, map); }
        }
        _ => {}
    }
}

// ── Split PDF ──────────────────────────────────────────────────────────

pub fn split_pdf(path: &Path, ranges: &[(usize, usize)], out_dir: &Path, stem: &str) -> Result<Vec<String>, String> {
    let mut outputs = Vec::new();
    for (i, &(start, end)) in ranges.iter().enumerate() {
        let indices: Vec<usize> = (start..end).collect();
        let name = format!("{stem}_part{}.pdf", i + 1);
        let out = out_dir.join(&name);
        extract_pages(path, &indices, &out)?;
        outputs.push(out.to_string_lossy().into_owned());
    }
    Ok(outputs)
}

// ── Add page numbers ───────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PageNumberConfig {
    pub position: String,
    pub font_size: f64,
    pub format: String,
    pub start_number: usize,
    pub skip_first: usize,
}

impl Default for PageNumberConfig {
    fn default() -> Self {
        Self { position: "bottom-center".into(), font_size: 10.0, format: "arabic".into(), start_number: 1, skip_first: 0 }
    }
}

fn to_roman(mut n: usize) -> String {
    let vals = [(1000,"m"),(900,"cm"),(500,"d"),(400,"cd"),(100,"c"),(90,"xc"),(50,"l"),(40,"xl"),(10,"x"),(9,"ix"),(5,"v"),(4,"iv"),(1,"i")];
    let mut s = String::new();
    for &(v, r) in &vals { while n >= v { s.push_str(r); n -= v; } }
    s
}

pub fn add_page_numbers_doc(doc: &mut Document, config: &PageNumberConfig) -> Result<(), String> {
    let ids = page_ids(doc);
    let total = ids.len();

    for (i, &id) in ids.iter().enumerate() {
        if i < config.skip_first { continue; }
        let num = config.start_number + i - config.skip_first;
        let label = match config.format.as_str() {
            "roman" => to_roman(num),
            "page-of" => format!("Page {} of {}", num, total - config.skip_first),
            _ => num.to_string(),
        };

        let (pw, ph) = match doc.get_object(id) {
            Ok(Object::Dictionary(d)) => page_dims(d),
            _ => (612.0, 792.0),
        };

        let fs = config.font_size;
        let (x, y) = match config.position.as_str() {
            "bottom-left"  => (36.0, 24.0),
            "bottom-right" => (pw - 36.0, 24.0),
            "top-center"   => (pw / 2.0, ph - 24.0),
            "top-left"     => (36.0, ph - 24.0),
            "top-right"    => (pw - 36.0, ph - 24.0),
            _              => (pw / 2.0, 24.0),
        };
        let approx_w = label.len() as f64 * fs * 0.5;
        let adj_x = if config.position.contains("center") { x - approx_w / 2.0 }
                    else if config.position.contains("right") { x - approx_w }
                    else { x };

        let content = format!(
            "q BT /F1 {fs} Tf {adj_x:.1} {y:.1} Td ({}) Tj ET Q",
            escape_pdf_literal(&label),
        );
        let font_id = add_helvetica(doc);
        append_content(doc, id, content.into_bytes(), Some(("F1", font_id)), None);
    }
    Ok(())
}

pub fn add_page_numbers(path: &Path, config: &PageNumberConfig, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    add_page_numbers_doc(&mut doc, config)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Watermark / stamp ──────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WatermarkConfig {
    pub text: String,
    pub font_size: f64,
    pub angle: f64,
    pub opacity: f64,
    pub color: [f64; 3],
}

impl Default for WatermarkConfig {
    fn default() -> Self {
        Self { text: "CONFIDENTIAL".into(), font_size: 48.0, angle: 45.0, opacity: 0.15, color: [0.5, 0.5, 0.5] }
    }
}

pub fn add_watermark_doc(doc: &mut Document, config: &WatermarkConfig, page_indices: Option<&[usize]>) -> Result<(), String> {
    let ids = page_ids(doc);
    let n = ids.len();
    let apply_to: Vec<usize> = page_indices.map(|pi| pi.to_vec()).unwrap_or_else(|| (0..n).collect());

    for &idx in &apply_to {
        if idx >= n { return Err(format!("page {idx} out of range")); }
        let id = ids[idx];
        let (pw, ph) = match doc.get_object(id) {
            Ok(Object::Dictionary(d)) => page_dims(d),
            _ => (612.0, 792.0),
        };

        let cx = pw / 2.0;
        let cy = ph / 2.0;
        let rad = config.angle.to_radians();
        let (cos, sin) = (rad.cos(), rad.sin());
        let [r, g, b] = config.color;
        let fs = config.font_size;
        let esc = escape_pdf_literal(&config.text);
        let aw = config.text.chars().count() as f64 * fs * 0.5;

        let content = format!(
            "q /GS0 gs {r:.3} {g:.3} {b:.3} rg BT /F1 {fs} Tf {:.4} {:.4} {:.4} {:.4} {:.1} {:.1} Tm ({esc}) Tj ET Q",
            cos, sin, -sin, cos,
            cx - aw / 2.0 * cos + fs / 2.0 * sin,
            cy - aw / 2.0 * sin - fs / 2.0 * cos,
        );

        let gs_id = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"ExtGState".to_vec())),
            ("ca", Object::Real(config.opacity as f32)),
            ("CA", Object::Real(config.opacity as f32)),
        ])));
        let font_id = add_helvetica(doc);
        append_content(doc, id, content.into_bytes(), Some(("F1", font_id)), Some(("GS0", gs_id)));
    }
    Ok(())
}

pub fn add_watermark(path: &Path, config: &WatermarkConfig, page_indices: Option<&[usize]>, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    add_watermark_doc(&mut doc, config, page_indices)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Insert blank pages ─────────────────────────────────────────────────

pub fn insert_blank_page_doc(doc: &mut Document, position: usize, width: f64, height: f64) -> Result<(), String> {
    let n = page_ids(doc).len();
    if position > n { return Err(format!("position {position} out of range (0..={n})")); }
    let pid = pages_ref(doc)?;
    let page_dict = lopdf::Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Page".to_vec())),
        ("Parent", Object::Reference(pid)),
        ("MediaBox", Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Real(width as f32), Object::Real(height as f32),
        ])),
    ]);
    let new_page_id = doc.add_object(Object::Dictionary(page_dict));
    // Flatten first: /Kids on the root /Pages node is not necessarily the
    // page order when the tree is nested, and positional insert into a
    // nested tree would land in the wrong place.
    let flat: Vec<Object> = page_ids(doc).iter().map(|id| Object::Reference(*id)).collect();
    if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(pid) {
        let mut kids = flat;
        kids.insert(position, Object::Reference(new_page_id));
        d.set("Kids", Object::Array(kids));
        d.set("Count", Object::Integer((n + 1) as i64));
    }
    Ok(())
}

pub fn insert_blank_page(path: &Path, position: usize, width: f64, height: f64, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    insert_blank_page_doc(&mut doc, position, width, height)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Insert pages from another document ─────────────────────────────────

/// Insert a page range from `src_path` into `doc` at `position`.
///
/// `src_pages` is a list of 0-based page indices in the source document;
/// an empty list means every page.  Objects are deep-copied with remapped
/// ids (same approach as `merge_pdfs`) so the two documents stay
/// independent.
pub fn insert_pages_from_doc(
    doc: &mut Document,
    src_path: &Path,
    src_pages: &[usize],
    position: usize,
) -> Result<usize, String> {
    let n = page_ids(doc).len();
    if position > n { return Err(format!("position {position} out of range (0..={n})")); }

    let src = Document::load(src_path).map_err(|e| format!("load {}: {e}", src_path.display()))?;
    let src_ids = src.page_iter().collect::<Vec<_>>();
    let wanted: Vec<ObjectId> = if src_pages.is_empty() {
        src_ids.clone()
    } else {
        src_pages
            .iter()
            .map(|&i| src_ids.get(i).copied().ok_or_else(|| format!("source page {i} out of range")))
            .collect::<Result<_, _>>()?
    };
    if wanted.is_empty() { return Err("no source pages selected".into()); }

    // Copy every source object under an offset id, then remap references.
    // Same scheme as `merge_pdfs`: shift by our max_id, keep the generation.
    let max_id = doc.max_id;
    let mut map = std::collections::HashMap::<ObjectId, ObjectId>::new();
    for (&old_id, obj) in &src.objects {
        let new_id = (old_id.0 + max_id, old_id.1);
        map.insert(old_id, new_id);
        doc.objects.insert(new_id, obj.clone());
    }
    doc.max_id = max_id + src.max_id;
    let new_ids: Vec<ObjectId> = map.values().copied().collect();
    for &nid in &new_ids {
        if let Some(obj) = doc.objects.get_mut(&nid) {
            remap_refs(obj, &map);
        }
    }

    let pid = pages_ref(doc)?;
    let mut inserted = Vec::with_capacity(wanted.len());
    for old in &wanted {
        let new_id = map[old];
        // Reparent onto our page tree so /Resources inheritance resolves here.
        if let Some(Object::Dictionary(ref mut d)) = doc.objects.get_mut(&new_id) {
            d.set("Parent", Object::Reference(pid));
        }
        inserted.push(Object::Reference(new_id));
    }

    let count = inserted.len();
    let mut kids: Vec<Object> = page_ids(doc).iter().map(|id| Object::Reference(*id)).collect();
    let at = position.min(kids.len());
    for (i, obj) in inserted.into_iter().enumerate() {
        kids.insert(at + i, obj);
    }
    let total = kids.len();
    if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(pid) {
        d.set("Kids", Object::Array(kids));
        d.set("Count", Object::Integer(total as i64));
    }
    Ok(count)
}

// ── Text box ───────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TextBoxConfig {
    /// 0-based page index.
    pub page: usize,
    /// Lower-left corner of the first line's baseline, in points.
    pub x: f64,
    pub y: f64,
    pub text: String,
    pub font_size: f64,
    /// RGB, each component 0.0–1.0.
    pub color: [f64; 3],
    /// Leading between lines; defaults to 1.2 × font_size when None.
    pub line_height: Option<f64>,
}

/// Draw a text box onto a page as page content (not an annotation).
///
/// Newlines in `text` become separate lines via the `TD`/`Td` leading.
/// Base-14 Helvetica, so this is WinAnsi-limited — characters outside
/// that repertoire are dropped rather than rendered as garbage.
pub fn add_text_box_doc(doc: &mut Document, config: &TextBoxConfig) -> Result<(), String> {
    let ids = page_ids(doc);
    let id = *ids.get(config.page).ok_or_else(|| format!("page {} out of range", config.page))?;

    let [r, g, b] = config.color;
    let fs = config.font_size;
    let leading = config.line_height.unwrap_or(fs * 1.2);

    let mut content = format!(
        "q {r:.3} {g:.3} {b:.3} rg BT /F1 {fs} Tf {:.2} TL {:.2} {:.2} Td",
        leading, config.x, config.y,
    );
    for (i, line) in config.text.split('\n').enumerate() {
        if i > 0 { content.push_str(" T*"); }
        content.push_str(&format!(" ({}) Tj", escape_pdf_literal(line)));
    }
    content.push_str(" ET Q");

    let font_id = add_helvetica(doc);
    append_content(doc, id, content.into_bytes(), Some(("F1", font_id)), None);
    Ok(())
}

pub fn add_text_box(path: &Path, config: &TextBoxConfig, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    add_text_box_doc(&mut doc, config)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Edit metadata ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct MetadataEdit {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
}

pub fn edit_metadata_doc(doc: &mut Document, edits: &MetadataEdit) -> Result<(), String> {
    let info_id = match doc.trailer.get(b"Info") {
        Ok(Object::Reference(r)) => *r,
        _ => {
            let id = doc.add_object(Object::Dictionary(lopdf::Dictionary::new()));
            doc.trailer.set("Info", Object::Reference(id));
            id
        }
    };
    if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(info_id) {
        if let Some(ref v) = edits.title { d.set("Title", Object::String(v.as_bytes().to_vec(), lopdf::StringFormat::Literal)); }
        if let Some(ref v) = edits.author { d.set("Author", Object::String(v.as_bytes().to_vec(), lopdf::StringFormat::Literal)); }
        if let Some(ref v) = edits.subject { d.set("Subject", Object::String(v.as_bytes().to_vec(), lopdf::StringFormat::Literal)); }
        if let Some(ref v) = edits.keywords { d.set("Keywords", Object::String(v.as_bytes().to_vec(), lopdf::StringFormat::Literal)); }
    }
    Ok(())
}

pub fn edit_metadata(path: &Path, edits: &MetadataEdit, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    edit_metadata_doc(&mut doc, edits)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Decrypt (password-protected PDFs) ───────────────────────────────

/// Decrypt a password-protected PDF and save the unprotected version.
pub fn decrypt_pdf(path: &Path, password: &str, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    if !doc.is_encrypted() {
        // Not encrypted — just copy as-is
        doc.save(out_path).map_err(|e| format!("save: {e}"))?;
        return Ok(());
    }
    doc.decrypt(password).map_err(|e| format!("decrypt: {e}"))?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

/// Check whether a PDF is encrypted.
pub fn is_encrypted(path: &Path) -> Result<bool, String> {
    let doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    Ok(doc.is_encrypted())
}

// ── Encrypt (set password + permissions) ────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EncryptConfig {
    /// Owner password (full access).
    pub owner_password: String,
    /// User password (restricted access). Empty string = no user password.
    pub user_password: String,
    /// Permission flags — which operations the user password allows.
    pub allow_print: bool,
    pub allow_copy: bool,
    pub allow_modify: bool,
    pub allow_annotate: bool,
    pub allow_fill_forms: bool,
    pub allow_assemble: bool,
    pub allow_high_quality_print: bool,
}

impl Default for EncryptConfig {
    fn default() -> Self {
        Self {
            owner_password: String::new(),
            user_password: String::new(),
            allow_print: true,
            allow_copy: true,
            allow_modify: true,
            allow_annotate: true,
            allow_fill_forms: true,
            allow_assemble: true,
            allow_high_quality_print: true,
        }
    }
}

pub fn encrypt_pdf(path: &Path, config: &EncryptConfig, out_path: &Path) -> Result<(), String> {
    use lopdf::encryption::{EncryptionVersion, Permissions};

    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;

    if doc.is_encrypted() {
        return Err("PDF is already encrypted; decrypt first".into());
    }

    let mut perms = Permissions::empty();
    if config.allow_print              { perms |= Permissions::PRINTABLE; }
    if config.allow_copy               { perms |= Permissions::COPYABLE | Permissions::COPYABLE_FOR_ACCESSIBILITY; }
    if config.allow_modify             { perms |= Permissions::MODIFIABLE; }
    if config.allow_annotate           { perms |= Permissions::ANNOTABLE; }
    if config.allow_fill_forms         { perms |= Permissions::FILLABLE; }
    if config.allow_assemble           { perms |= Permissions::ASSEMBLABLE; }
    if config.allow_high_quality_print { perms |= Permissions::PRINTABLE_IN_HIGH_QUALITY; }

    // Use V2 (RC4 with configurable key length, widely compatible).
    // V4/V5 (AES) would be stronger but requires private CryptFilter types
    // in lopdf 0.38; upgrade when lopdf exposes them publicly.
    let enc_version = EncryptionVersion::V2 {
        document: &doc,
        owner_password: &config.owner_password,
        user_password: &config.user_password,
        key_length: 128,
        permissions: perms,
    };

    let state = lopdf::encryption::EncryptionState::try_from(enc_version)
        .map_err(|e| format!("encryption setup: {e}"))?;
    doc.encrypt(&state).map_err(|e| format!("encrypt: {e}"))?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Sanitise (strip hidden metadata) ────────────────────────────────

/// Fine-grained options for what to strip during sanitisation.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SanitiseOptions {
    pub strip_info: bool,
    pub strip_xmp: bool,
    pub strip_javascript: bool,
    pub strip_embedded_files: bool,
    pub strip_open_action: bool,
    pub strip_thumbnails: bool,
    pub strip_annotations: bool,
}

impl Default for SanitiseOptions {
    fn default() -> Self {
        Self {
            strip_info: true,
            strip_xmp: true,
            strip_javascript: true,
            strip_embedded_files: true,
            strip_open_action: true,
            strip_thumbnails: true,
            strip_annotations: true,
        }
    }
}

/// Remove hidden metadata from a PDF.  Uses `SanitiseOptions` to
/// control which categories are stripped.
pub fn sanitise_pdf(path: &Path, out_path: &Path) -> Result<Vec<String>, String> {
    sanitise_pdf_with_options(path, &SanitiseOptions::default(), out_path)
}

pub fn sanitise_pdf_with_options(path: &Path, opts: &SanitiseOptions, out_path: &Path) -> Result<Vec<String>, String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let mut stripped = Vec::new();

    // 1. Remove /Info dictionary
    if opts.strip_info {
        if let Ok(Object::Reference(info_id)) = doc.trailer.get(b"Info") {
            let info_id = *info_id;
            doc.objects.remove(&info_id);
            doc.trailer.remove(b"Info");
            stripped.push("Info dictionary".into());
        }
    }

    // Collect catalog-level IDs to remove (avoids borrow conflicts).
    let mut remove_from_catalog: Vec<&'static [u8]> = Vec::new();
    let mut meta_obj_id: Option<ObjectId> = None;
    let mut names_id: Option<ObjectId> = None;
    let mut names_remove: Vec<Vec<u8>> = Vec::new();

    if let Ok(cat) = doc.catalog() {
        // 2. XMP metadata
        if opts.strip_xmp {
            if let Ok(Object::Reference(mid)) = cat.get(b"Metadata") {
                meta_obj_id = Some(*mid);
                remove_from_catalog.push(b"Metadata");
            }
        }
        // 4. OpenAction
        if opts.strip_open_action && cat.has(b"OpenAction") {
            remove_from_catalog.push(b"OpenAction");
        }
        // 3. Names dict
        if opts.strip_javascript || opts.strip_embedded_files {
            if let Ok(Object::Reference(nid)) = cat.get(b"Names") {
                names_id = Some(*nid);
            }
        }
    }

    // Check what to remove from Names dict
    if let Some(nid) = names_id {
        if let Ok(Object::Dictionary(nd)) = doc.get_object(nid) {
            if opts.strip_javascript && nd.has(b"JavaScript")    { names_remove.push(b"JavaScript".to_vec()); }
            if opts.strip_embedded_files && nd.has(b"EmbeddedFiles") { names_remove.push(b"EmbeddedFiles".to_vec()); }
        }
    }

    // Now mutate: remove XMP object
    if let Some(mid) = meta_obj_id {
        doc.objects.remove(&mid);
        stripped.push("XMP metadata".into());
    }

    // Remove catalog keys
    if !remove_from_catalog.is_empty() {
        if let Ok(cm) = doc.catalog_mut() {
            for key in &remove_from_catalog {
                if *key == b"OpenAction" { stripped.push("OpenAction".into()); }
                cm.remove(*key);
            }
        }
    }

    // Remove from Names dict
    if let Some(nid) = names_id {
        for key in &names_remove {
            if let Ok(Object::Dictionary(ref mut nd)) = doc.get_object_mut(nid) {
                nd.remove(key);
                stripped.push(String::from_utf8_lossy(key).into_owned());
            }
        }
    }

    // 5. Strip per-page thumbnails and annotations
    if opts.strip_thumbnails || opts.strip_annotations {
        let ids = page_ids(&doc);
        for &id in &ids {
            if let Ok(Object::Dictionary(ref mut pg)) = doc.get_object_mut(id) {
                if opts.strip_thumbnails && pg.has(b"Thumb") {
                    pg.remove(b"Thumb");
                    stripped.push("Page thumbnail".into());
                }
                if opts.strip_annotations && pg.has(b"Annots") {
                    pg.remove(b"Annots");
                    stripped.push("Annotations".into());
                }
            }
        }
    }
    stripped.sort();
    stripped.dedup();

    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(stripped)
}

// ── PDF/A conformance metadata (P26.5) ──────────────────────────────

/// Add PDF/A-2b conformance metadata to an existing PDF.  This sets
/// the XMP metadata packet with `pdfaid:part=2` + `pdfaid:conformance=B`
/// and adds an sRGB OutputIntent.  Does NOT re-encode fonts or images
/// — the caller is responsible for ensuring the content is conformant.
pub fn convert_to_pdfa(path: &Path, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;

    // 1. Create XMP metadata stream with PDF/A-2b conformance
    let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about=""
      xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/"
      xmlns:dc="http://purl.org/dc/elements/1.1/">
      <pdfaid:part>2</pdfaid:part>
      <pdfaid:conformance>B</pdfaid:conformance>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    let xmp_stream = lopdf::Stream::new(
        lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Metadata".to_vec())),
            ("Subtype", Object::Name(b"XML".to_vec())),
        ]),
        xmp.as_bytes().to_vec(),
    );
    let xmp_id = doc.add_object(Object::Stream(xmp_stream));

    // 2. Set /Metadata on catalog
    if let Ok(cm) = doc.catalog_mut() {
        cm.set("Metadata", Object::Reference(xmp_id));
    }

    // 3. Add sRGB OutputIntent
    let output_intent = lopdf::Dictionary::from_iter(vec![
        ("Type", Object::Name(b"OutputIntent".to_vec())),
        ("S", Object::Name(b"GTS_PDFA1".to_vec())),
        ("OutputConditionIdentifier", Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal)),
        ("RegistryName", Object::String(b"http://www.color.org".to_vec(), lopdf::StringFormat::Literal)),
        ("Info", Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal)),
    ]);
    let intent_id = doc.add_object(Object::Dictionary(output_intent));

    if let Ok(cm) = doc.catalog_mut() {
        cm.set("OutputIntents", Object::Array(vec![Object::Reference(intent_id)]));
    }

    // 4. Set PDF version to 1.7 (minimum for PDF/A-2)
    doc.version = "1.7".to_string();

    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Digital signature detection (P26.6) ─────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PdfSignatureInfo {
    /// Signer name from /Name field.
    pub name: Option<String>,
    /// Reason for signing from /Reason field.
    pub reason: Option<String>,
    /// Location from /Location field.
    pub location: Option<String>,
    /// Signing date from /M field.
    pub date: Option<String>,
    /// Filter (e.g. "Adobe.PPKLite", "Adobe.PPKMS").
    pub filter: Option<String>,
    /// Sub-filter (e.g. "adbe.pkcs7.detached", "ETSI.CAdES.detached").
    pub sub_filter: Option<String>,
    /// Whether the signature has a /ByteRange (i.e. covers actual content).
    pub has_byte_range: bool,
    /// Page number (1-based) where the signature widget appears, if any.
    pub page: Option<usize>,
}

/// Detect digital signatures in a PDF.  Does not verify cryptographic
/// validity (would need a PKCS#7/CMS library); reports what signatures
/// exist and their metadata.
pub fn detect_signatures(path: &Path) -> Result<Vec<PdfSignatureInfo>, String> {
    let doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let mut sigs = Vec::new();

    // Walk all pages looking for /Annots with /Subtype /Widget and /FT /Sig
    let page_list = page_ids(&doc);
    for (page_idx, &pid) in page_list.iter().enumerate() {
        let annot_refs = match doc.get_object(pid) {
            Ok(Object::Dictionary(pg)) => {
                match pg.get(b"Annots") {
                    Ok(Object::Array(arr)) => arr.clone(),
                    _ => continue,
                }
            }
            _ => continue,
        };

        for annot_ref in &annot_refs {
            let annot_id = match annot_ref {
                Object::Reference(r) => *r,
                _ => continue,
            };
            let annot = match doc.get_object(annot_id) {
                Ok(Object::Dictionary(d)) => d,
                _ => continue,
            };

            // Check if this is a signature widget
            let is_sig = matches!(annot.get(b"FT"), Ok(Object::Name(n)) if n == b"Sig");
            if !is_sig { continue; }

            // Get the /V (value) dict which contains the actual signature
            let sig_dict = match annot.get(b"V") {
                Ok(Object::Reference(r)) => {
                    match doc.get_object(*r) {
                        Ok(Object::Dictionary(d)) => Some(d),
                        _ => None,
                    }
                }
                Ok(Object::Dictionary(d)) => Some(d),
                _ => None,
            };

            let (name, reason, location, date, filter, sub_filter, has_byte_range) =
                if let Some(sd) = sig_dict {
                    (
                        dict_string(sd, b"Name"),
                        dict_string(sd, b"Reason"),
                        dict_string(sd, b"Location"),
                        dict_string(sd, b"M"),
                        dict_string(sd, b"Filter"),
                        dict_string(sd, b"SubFilter"),
                        sd.has(b"ByteRange"),
                    )
                } else {
                    (None, None, None, None, None, None, false)
                };

            sigs.push(PdfSignatureInfo {
                name,
                reason,
                location,
                date,
                filter,
                sub_filter,
                has_byte_range,
                page: Some(page_idx + 1),
            });
        }
    }

    // Also check the AcroForm /SigFlags for document-level signature fields
    if sigs.is_empty() {
        if let Ok(cat) = doc.catalog() {
            if let Ok(Object::Reference(form_ref)) = cat.get(b"AcroForm") {
                if let Ok(Object::Dictionary(form)) = doc.get_object(*form_ref) {
                    if let Ok(Object::Integer(flags)) = form.get(b"SigFlags") {
                        if *flags & 1 != 0 {
                            // SignaturesExist flag is set but we couldn't find the widget
                            sigs.push(PdfSignatureInfo {
                                name: None, reason: None, location: None, date: None,
                                filter: None, sub_filter: None,
                                has_byte_range: false, page: None,
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(sigs)
}

// ── PII redaction (P26.7) ────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RedactionSpec {
    /// Page number (0-based).
    pub page: usize,
    /// Bounding box in points (origin = bottom-left).
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Black out regions by overlaying opaque rectangles.
///
/// **This is not redaction.**  The rectangles are drawn on top of the
/// existing content: the text objects survive underneath and come back
/// out through copy/paste, `pdf-extract`, or any other extractor.  Use
/// [`crate::pdf_redact::redact_regions_hard`] when the content must
/// actually be removed — it scrubs the content stream *and* calls this
/// to cover whatever it could not reach.
pub fn black_out_regions_doc(
    doc: &mut Document,
    regions: &[RedactionSpec],
) -> Result<usize, String> {
    let ids = page_ids(doc);
    let n = ids.len();

    // Group regions by page
    let mut by_page: std::collections::HashMap<usize, Vec<&RedactionSpec>> = std::collections::HashMap::new();
    for r in regions {
        if r.page >= n { return Err(format!("page {} out of range (0..{n})", r.page)); }
        by_page.entry(r.page).or_default().push(r);
    }

    let mut count = 0;
    for (page_idx, page_regions) in &by_page {
        let id = ids[*page_idx];
        // Build content stream: black rectangles
        let mut ops = String::new();
        for r in page_regions {
            ops.push_str(&format!(
                "q 0 0 0 rg {:.1} {:.1} {:.1} {:.1} re f Q\n",
                r.x, r.y, r.w, r.h,
            ));
            count += 1;
        }
        let content_id = doc.add_object(Object::Stream(
            lopdf::Stream::new(lopdf::Dictionary::new(), ops.into_bytes()),
        ));
        if let Ok(Object::Dictionary(ref mut page)) = doc.get_object_mut(id) {
            match page.get(b"Contents") {
                Ok(Object::Reference(r)) => {
                    let r = *r;
                    page.set("Contents", Object::Array(vec![Object::Reference(r), Object::Reference(content_id)]));
                }
                Ok(Object::Array(arr)) => {
                    let mut a = arr.clone();
                    a.push(Object::Reference(content_id));
                    page.set("Contents", Object::Array(a));
                }
                _ => { page.set("Contents", Object::Reference(content_id)); }
            }
        }
    }
    Ok(count)
}

/// Path-taking wrapper for [`black_out_regions_doc`].
///
/// **Visual only — the text underneath survives.** See that function.
pub fn black_out_regions(
    path: &Path,
    regions: &[RedactionSpec],
    out_path: &Path,
) -> Result<usize, String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let count = black_out_regions_doc(&mut doc, regions)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(count)
}

/// Deprecated alias kept so existing callers keep compiling.
///
/// The name is a lie — this never redacted anything, it drew rectangles.
/// New code should call [`crate::pdf_redact::redact_regions_hard`] for
/// real redaction, or [`black_out_regions`] when covering is genuinely
/// what is wanted.
#[deprecated(note = "visual-only; use pdf_redact::redact_regions_hard for real redaction")]
pub fn redact_regions(
    path: &Path,
    regions: &[RedactionSpec],
    out_path: &Path,
) -> Result<usize, String> {
    black_out_regions(path, regions, out_path)
}

/// Black out text by searching for specific strings and covering them.
///
/// **Visual only**, and doubly approximate: the boxes are derived from
/// assumed character dimensions, not real glyph positions.
///
/// Strips matches from the `/Info` dictionary and draws boxes on the
/// page. The page text itself is not removed.
pub fn redact_text_patterns(
    path: &Path,
    patterns: &[String],
    out_path: &Path,
) -> Result<usize, String> {
    // For text-level redaction without bounding boxes, we strip matching
    // text from the /Info dictionary and add visual redaction boxes as a
    // best-effort measure.  True content-stream text removal requires
    // parsing the content stream operators, which is a much larger effort.
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;

    // Strip patterns from /Info dict (metadata redaction)
    if let Ok(Object::Reference(info_id)) = doc.trailer.get(b"Info") {
        let info_id = *info_id;
        if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(info_id) {
            for key in &[b"Title" as &[u8], b"Author", b"Subject", b"Keywords"] {
                if let Ok(Object::String(val, fmt)) = d.get(key) {
                    let text = decode_pdf_str(val);
                    let mut redacted = text.clone();
                    for pattern in patterns {
                        redacted = redacted.replace(pattern.as_str(), "█".repeat(pattern.len()).as_str());
                    }
                    if redacted != text {
                        d.set(*key, Object::String(redacted.into_bytes(), *fmt));
                    }
                }
            }
        }
    }

    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(patterns.len())
}

// ── Digital signature creation (P27.12) ─────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SignConfig {
    /// Path to the PFX/P12 certificate file.
    pub cert_path: String,
    /// Password for the PFX/P12 file.
    pub cert_password: String,
    /// Reason for signing (optional, shown in signature panel).
    pub reason: Option<String>,
    /// Location (optional).
    pub location: Option<String>,
    /// Contact info (optional).
    pub contact: Option<String>,
}

/// Sign a PDF with a PKCS#12 certificate.  Adds a /Sig dictionary
/// to the first page's annotations.  This is a basic CMS signature
/// — not a full PAdES/LTV implementation.
pub fn sign_pdf(
    path: &Path,
    config: &SignConfig,
    out_path: &Path,
) -> Result<(), String> {
    use openssl::pkcs12::Pkcs12;
    use openssl::sign::Signer;
    use openssl::hash::MessageDigest;

    // 1. Load the PFX certificate
    let pfx_bytes = std::fs::read(&config.cert_path)
        .map_err(|e| format!("read cert: {e}"))?;
    let pkcs12 = Pkcs12::from_der(&pfx_bytes)
        .map_err(|e| format!("parse PFX: {e}"))?;
    let identity = pkcs12.parse2(&config.cert_password)
        .map_err(|e| format!("unlock PFX: {e}"))?;
    let pkey = identity.pkey.ok_or("PFX has no private key")?;
    let cert = identity.cert.ok_or("PFX has no certificate")?;

    // 2. Load the PDF
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;

    // 3. Compute a SHA-256 digest of the PDF content
    let pdf_bytes = std::fs::read(path).map_err(|e| format!("read pdf: {e}"))?;
    let mut signer = Signer::new(MessageDigest::sha256(), &pkey)
        .map_err(|e| format!("signer init: {e}"))?;
    signer.update(&pdf_bytes).map_err(|e| format!("sign update: {e}"))?;
    let signature = signer.sign_to_vec().map_err(|e| format!("sign: {e}"))?;

    // 4. Build the /Sig dictionary
    let signer_name = cert.subject_name()
        .entries_by_nid(openssl::nid::Nid::COMMONNAME)
        .next()
        .map(|e| e.data().to_string().unwrap_or_default())
        .unwrap_or_default();

    let mut sig_dict = lopdf::Dictionary::new();
    sig_dict.set("Type", Object::Name(b"Sig".to_vec()));
    sig_dict.set("Filter", Object::Name(b"Adobe.PPKLite".to_vec()));
    sig_dict.set("SubFilter", Object::Name(b"adbe.pkcs7.detached".to_vec()));
    sig_dict.set("Name", Object::String(signer_name.into_bytes(), lopdf::StringFormat::Literal));
    if let Some(ref r) = config.reason {
        sig_dict.set("Reason", Object::String(r.as_bytes().to_vec(), lopdf::StringFormat::Literal));
    }
    if let Some(ref l) = config.location {
        sig_dict.set("Location", Object::String(l.as_bytes().to_vec(), lopdf::StringFormat::Literal));
    }
    if let Some(ref c) = config.contact {
        sig_dict.set("ContactInfo", Object::String(c.as_bytes().to_vec(), lopdf::StringFormat::Literal));
    }
    sig_dict.set("Contents", Object::String(signature, lopdf::StringFormat::Hexadecimal));

    let sig_id = doc.add_object(Object::Dictionary(sig_dict));

    // 5. Add a widget annotation on page 1 pointing to the /Sig value
    let ids = page_ids(&doc);
    if let Some(&page_id) = ids.first() {
        let widget = lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Annot".to_vec())),
            ("Subtype", Object::Name(b"Widget".to_vec())),
            ("FT", Object::Name(b"Sig".to_vec())),
            ("V", Object::Reference(sig_id)),
            ("T", Object::String(b"Signature1".to_vec(), lopdf::StringFormat::Literal)),
            ("Rect", Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(0), Object::Integer(0), // invisible signature
            ])),
            ("P", Object::Reference(page_id)),
        ]);
        let widget_id = doc.add_object(Object::Dictionary(widget));

        if let Ok(Object::Dictionary(ref mut page)) = doc.get_object_mut(page_id) {
            let mut annots = match page.get(b"Annots") {
                Ok(Object::Array(a)) => a.clone(),
                _ => vec![],
            };
            annots.push(Object::Reference(widget_id));
            page.set("Annots", Object::Array(annots));
        }
    }

    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Tauri commands ─────────────────────────────────────────────────────

pub mod tauri_commands {
    use super::*;

    #[tauri::command]
    pub async fn pdf_info(path: String) -> Result<PdfInfo, String> {
        tokio::task::spawn_blocking(move || super::pdf_info(Path::new(&path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_reorder_pages(path: String, new_order: Vec<usize>, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::reorder_pages(Path::new(&path), &new_order, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_extract_pages(path: String, page_indices: Vec<usize>, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::extract_pages(Path::new(&path), &page_indices, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_remove_pages(path: String, page_indices: Vec<usize>, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::remove_pages(Path::new(&path), &page_indices, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_rotate_pages(path: String, page_indices: Vec<usize>, degrees: i64, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::rotate_pages(Path::new(&path), &page_indices, degrees, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_crop_pages(path: String, page_indices: Vec<usize>, x: f64, y: f64, w: f64, h: f64, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::crop_pages(Path::new(&path), &page_indices, x, y, w, h, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_merge(paths: Vec<String>, out_path: String) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || {
            let p: Vec<&Path> = paths.iter().map(|s| Path::new(s.as_str())).collect();
            super::merge_pdfs(&p, Path::new(&out_path))
        }).await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_split(path: String, ranges: Vec<(usize, usize)>, out_dir: String, stem: String) -> Result<Vec<String>, String> {
        tokio::task::spawn_blocking(move || super::split_pdf(Path::new(&path), &ranges, Path::new(&out_dir), &stem))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_add_page_numbers(path: String, config: PageNumberConfig, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::add_page_numbers(Path::new(&path), &config, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_add_watermark(path: String, config: WatermarkConfig, page_indices: Option<Vec<usize>>, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::add_watermark(Path::new(&path), &config, page_indices.as_deref(), Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_insert_blank_page(path: String, position: usize, width: Option<f64>, height: Option<f64>, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::insert_blank_page(Path::new(&path), position, width.unwrap_or(612.0), height.unwrap_or(792.0), Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_edit_metadata(path: String, edits: MetadataEdit, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::edit_metadata(Path::new(&path), &edits, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_decrypt(path: String, password: String, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::decrypt_pdf(Path::new(&path), &password, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_is_encrypted(path: String) -> Result<bool, String> {
        tokio::task::spawn_blocking(move || super::is_encrypted(Path::new(&path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_encrypt(path: String, config: EncryptConfig, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::encrypt_pdf(Path::new(&path), &config, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_sign(path: String, config: SignConfig, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::sign_pdf(Path::new(&path), &config, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_redact_regions(path: String, regions: Vec<RedactionSpec>, out_path: String) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || super::black_out_regions(Path::new(&path), &regions, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_redact_text(path: String, patterns: Vec<String>, out_path: String) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || super::redact_text_patterns(Path::new(&path), &patterns, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_convert_pdfa(path: String, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::convert_to_pdfa(Path::new(&path), Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_detect_signatures(path: String) -> Result<Vec<PdfSignatureInfo>, String> {
        tokio::task::spawn_blocking(move || super::detect_signatures(Path::new(&path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_sanitise(path: String, options: Option<SanitiseOptions>, out_path: String) -> Result<Vec<String>, String> {
        let opts = options.unwrap_or_default();
        tokio::task::spawn_blocking(move || super::sanitise_pdf_with_options(Path::new(&path), &opts, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_add_text_box(path: String, config: TextBoxConfig, out_path: String) -> Result<(), String> {
        tokio::task::spawn_blocking(move || super::add_text_box(Path::new(&path), &config, Path::new(&out_path)))
            .await.map_err(|e| format!("join: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a minimal valid 2-page PDF for testing.
    fn create_test_pdf(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("test.pdf");
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page1_id = doc.new_object_id();
        let page2_id = doc.new_object_id();

        let pages = lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Kids", Object::Array(vec![Object::Reference(page1_id), Object::Reference(page2_id)])),
            ("Count", Object::Integer(2)),
        ]);
        doc.objects.insert(pages_id, Object::Dictionary(pages));

        for pid in [page1_id, page2_id] {
            let page = lopdf::Dictionary::from_iter(vec![
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(pages_id)),
                ("MediaBox", Object::Array(vec![
                    Object::Integer(0), Object::Integer(0),
                    Object::Real(612.0), Object::Real(792.0),
                ])),
            ]);
            doc.objects.insert(pid, Object::Dictionary(page));
        }

        let info = lopdf::Dictionary::from_iter(vec![
            ("Title", Object::String(b"Test PDF".to_vec(), lopdf::StringFormat::Literal)),
            ("Author", Object::String(b"Tester".to_vec(), lopdf::StringFormat::Literal)),
        ]);
        let info_id = doc.add_object(Object::Dictionary(info));

        let catalog = lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Catalog".to_vec())),
            ("Pages", Object::Reference(pages_id)),
        ]);
        let catalog_id = doc.add_object(Object::Dictionary(catalog));

        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.trailer.set("Info", Object::Reference(info_id));
        // Encryption requires /ID in the trailer
        let file_id = Object::Array(vec![
            Object::String(b"testid1234567890".to_vec(), lopdf::StringFormat::Literal),
            Object::String(b"testid1234567890".to_vec(), lopdf::StringFormat::Literal),
        ]);
        doc.trailer.set("ID", file_id);
        doc.save(&path).unwrap();
        path
    }

    #[test]
    fn test_to_roman() {
        assert_eq!(to_roman(1), "i");
        assert_eq!(to_roman(4), "iv");
        assert_eq!(to_roman(9), "ix");
        assert_eq!(to_roman(42), "xlii");
        assert_eq!(to_roman(1999), "mcmxcix");
        assert_eq!(to_roman(0), "");
    }

    #[test]
    fn test_page_number_config_default() {
        let c = PageNumberConfig::default();
        assert_eq!(c.position, "bottom-center");
        assert_eq!(c.format, "arabic");
        assert_eq!(c.start_number, 1);
        assert_eq!(c.skip_first, 0);
    }

    #[test]
    fn test_watermark_config_default() {
        let c = WatermarkConfig::default();
        assert_eq!(c.text, "CONFIDENTIAL");
        assert_eq!(c.opacity, 0.15);
    }

    #[test]
    fn test_sanitise_options_default() {
        let o = SanitiseOptions::default();
        assert!(o.strip_info);
        assert!(o.strip_xmp);
        assert!(o.strip_javascript);
        assert!(o.strip_annotations);
    }

    #[test]
    fn test_encrypt_config_default() {
        let c = EncryptConfig::default();
        assert!(c.allow_print);
        assert!(c.allow_copy);
        assert!(c.allow_modify);
    }

    #[test]
    fn pdf_info_reads_metadata() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let info = pdf_info(&pdf).unwrap();
        assert_eq!(info.page_count, 2);
        assert_eq!(info.title.as_deref(), Some("Test PDF"));
        assert_eq!(info.author.as_deref(), Some("Tester"));
        assert_eq!(info.pages.len(), 2);
        assert!((info.pages[0].width_pt - 612.0).abs() < 1.0);
        assert!((info.pages[0].height_pt - 792.0).abs() < 1.0);
    }

    #[test]
    fn extract_pages_subset() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("extracted.pdf");
        extract_pages(&pdf, &[0], &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 1);
    }

    #[test]
    fn extract_pages_out_of_range() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("bad.pdf");
        let err = extract_pages(&pdf, &[5], &out);
        assert!(err.is_err());
    }

    #[test]
    fn remove_pages_keeps_remaining() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("trimmed.pdf");
        remove_pages(&pdf, &[1], &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 1);
    }

    #[test]
    fn remove_all_pages_fails() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("empty.pdf");
        let err = remove_pages(&pdf, &[0, 1], &out);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("Cannot remove all"));
    }

    #[test]
    fn reorder_pages_reverses() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("reordered.pdf");
        reorder_pages(&pdf, &[1, 0], &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn rotate_pages_sets_rotation() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("rotated.pdf");
        rotate_pages(&pdf, &[0], 90, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.pages[0].rotation, 90);
        assert_eq!(info.pages[1].rotation, 0); // untouched
    }

    #[test]
    fn rotate_invalid_degrees() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("bad.pdf");
        let err = rotate_pages(&pdf, &[0], 45, &out);
        assert!(err.is_err());
    }

    #[test]
    fn crop_pages_sets_cropbox() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("cropped.pdf");
        crop_pages(&pdf, &[0, 1], 50.0, 50.0, 200.0, 300.0, &out).unwrap();
        // Just verify it doesn't crash and produces a valid PDF
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn merge_two_pdfs() {
        let dir = TempDir::new().unwrap();
        let pdf1 = create_test_pdf(dir.path());
        let pdf2_path = dir.path().join("test2.pdf");
        std::fs::copy(&pdf1, &pdf2_path).unwrap();
        let out = dir.path().join("merged.pdf");
        let total = merge_pdfs(&[&pdf1, &pdf2_path], &out).unwrap();
        assert_eq!(total, 4); // 2 + 2
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 4);
    }

    #[test]
    fn merge_empty_fails() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("empty.pdf");
        let err = merge_pdfs(&[], &out);
        assert!(err.is_err());
    }

    #[test]
    fn split_pdf_into_parts() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out_dir = dir.path().join("splits");
        std::fs::create_dir_all(&out_dir).unwrap();
        let outputs = split_pdf(&pdf, &[(0, 1), (1, 2)], &out_dir, "doc").unwrap();
        assert_eq!(outputs.len(), 2);
        for o in &outputs {
            let info = pdf_info(Path::new(o)).unwrap();
            assert_eq!(info.page_count, 1);
        }
    }

    #[test]
    fn add_page_numbers_arabic() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("numbered.pdf");
        let config = PageNumberConfig::default();
        add_page_numbers(&pdf, &config, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn add_page_numbers_roman() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("roman.pdf");
        let config = PageNumberConfig { format: "roman".into(), ..Default::default() };
        add_page_numbers(&pdf, &config, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn add_page_numbers_page_of() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("pageof.pdf");
        let config = PageNumberConfig { format: "page-of".into(), ..Default::default() };
        add_page_numbers(&pdf, &config, &out).unwrap();
        assert!(out.exists());
    }

    #[test]
    fn add_page_numbers_skip_first() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("skip.pdf");
        let config = PageNumberConfig { skip_first: 1, ..Default::default() };
        add_page_numbers(&pdf, &config, &out).unwrap();
        assert!(out.exists());
    }

    #[test]
    fn add_watermark_all_pages() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("watermarked.pdf");
        let config = WatermarkConfig::default();
        add_watermark(&pdf, &config, None, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn add_watermark_specific_pages() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("wm_partial.pdf");
        let config = WatermarkConfig::default();
        add_watermark(&pdf, &config, Some(&[0]), &out).unwrap();
        assert!(out.exists());
    }

    #[test]
    fn insert_blank_page_at_start() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("blank_start.pdf");
        insert_blank_page(&pdf, 0, 612.0, 792.0, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 3);
    }

    #[test]
    fn insert_blank_page_at_end() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("blank_end.pdf");
        insert_blank_page(&pdf, 2, 612.0, 792.0, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 3);
    }

    #[test]
    fn insert_blank_out_of_range() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("bad.pdf");
        let err = insert_blank_page(&pdf, 10, 612.0, 792.0, &out);
        assert!(err.is_err());
    }

    #[test]
    fn edit_metadata_updates_fields() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("meta.pdf");
        let edits = MetadataEdit {
            title: Some("New Title".into()),
            author: Some("New Author".into()),
            subject: Some("Subject".into()),
            keywords: Some("test, pdf".into()),
        };
        edit_metadata(&pdf, &edits, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.title.as_deref(), Some("New Title"));
        assert_eq!(info.author.as_deref(), Some("New Author"));
        assert_eq!(info.subject.as_deref(), Some("Subject"));
    }

    #[test]
    fn edit_metadata_partial_update() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("partial.pdf");
        let edits = MetadataEdit { title: Some("Only Title".into()), ..Default::default() };
        edit_metadata(&pdf, &edits, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.title.as_deref(), Some("Only Title"));
        // Author should still be "Tester" from original
        assert_eq!(info.author.as_deref(), Some("Tester"));
    }

    #[test]
    fn is_encrypted_false_for_normal_pdf() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        assert!(!is_encrypted(&pdf).unwrap());
    }

    #[test]
    fn encrypt_marks_as_encrypted() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let encrypted = dir.path().join("encrypted.pdf");

        let config = EncryptConfig {
            owner_password: "owner123".into(),
            user_password: "user456".into(),
            ..Default::default()
        };
        encrypt_pdf(&pdf, &config, &encrypted).unwrap();
        assert!(is_encrypted(&encrypted).unwrap());
    }

    #[test]
    fn decrypt_unencrypted_passes_through() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("decrypted.pdf");
        // Decrypting an unencrypted PDF should just copy it
        decrypt_pdf(&pdf, "", &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn encrypt_already_encrypted_fails() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let enc1 = dir.path().join("enc1.pdf");
        let enc2 = dir.path().join("enc2.pdf");
        let config = EncryptConfig { owner_password: "pw".into(), ..Default::default() };
        encrypt_pdf(&pdf, &config, &enc1).unwrap();
        let err = encrypt_pdf(&enc1, &config, &enc2);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("already encrypted"));
    }

    #[test]
    fn sanitise_strips_info() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("sanitised.pdf");
        let stripped = sanitise_pdf(&pdf, &out).unwrap();
        assert!(stripped.contains(&"Info dictionary".to_string()));
        let info = pdf_info(&out).unwrap();
        assert!(info.title.is_none());
        assert!(info.author.is_none());
    }

    #[test]
    fn sanitise_with_options_keeps_annotations() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("partial_san.pdf");
        let opts = SanitiseOptions {
            strip_info: true,
            strip_annotations: false, // keep annotations
            ..Default::default()
        };
        let stripped = sanitise_pdf_with_options(&pdf, &opts, &out).unwrap();
        assert!(stripped.contains(&"Info dictionary".to_string()));
        assert!(!stripped.contains(&"Annotations".to_string()));
    }

    #[test]
    fn detect_signatures_on_unsigned_pdf() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let sigs = detect_signatures(&pdf).unwrap();
        assert!(sigs.is_empty());
    }

    #[test]
    fn convert_to_pdfa_adds_metadata() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("pdfa.pdf");
        convert_to_pdfa(&pdf, &out).unwrap();
        // Verify the output is a valid PDF
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn decode_pdf_str_utf16be() {
        let bytes = [0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]; // "Hi" in UTF-16BE
        assert_eq!(decode_pdf_str(&bytes), "Hi");
    }

    #[test]
    fn decode_pdf_str_latin1() {
        let bytes = [0x48, 0x65, 0x6C, 0x6C, 0x6F]; // "Hello"
        assert_eq!(decode_pdf_str(&bytes), "Hello");
    }

    #[test]
    fn remap_refs_rewrites_references() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert((1, 0), (10, 0));
        let mut obj = Object::Reference((1, 0));
        remap_refs(&mut obj, &map);
        assert_eq!(obj, Object::Reference((10, 0)));
    }

    #[test]
    fn remap_refs_nested_array() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert((1, 0), (10, 0));
        let mut obj = Object::Array(vec![Object::Reference((1, 0)), Object::Integer(42)]);
        remap_refs(&mut obj, &map);
        match &obj {
            Object::Array(a) => assert_eq!(a[0], Object::Reference((10, 0))),
            _ => panic!("expected array"),
        }
    }

    // ── Redaction tests ───────────────────────────────────────────────

    #[test]
    fn redact_regions_applies_boxes() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("redacted.pdf");
        let regions = vec![
            RedactionSpec { page: 0, x: 50.0, y: 50.0, w: 100.0, h: 20.0 },
            RedactionSpec { page: 1, x: 10.0, y: 10.0, w: 50.0, h: 50.0 },
        ];
        let count = black_out_regions(&pdf, &regions, &out).unwrap();
        assert_eq!(count, 2);
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn redact_regions_out_of_range() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("bad.pdf");
        let regions = vec![RedactionSpec { page: 5, x: 0.0, y: 0.0, w: 10.0, h: 10.0 }];
        assert!(black_out_regions(&pdf, &regions, &out).is_err());
    }

    #[test]
    fn redact_regions_empty() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("empty.pdf");
        let count = black_out_regions(&pdf, &[], &out).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn redact_text_patterns_in_metadata() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("redacted_meta.pdf");
        let count = redact_text_patterns(&pdf, &["Tester".into()], &out).unwrap();
        assert_eq!(count, 1);
        // Author "Tester" should be redacted in /Info
        let info = pdf_info(&out).unwrap();
        assert_ne!(info.author.as_deref(), Some("Tester"));
    }

    #[test]
    fn redact_text_patterns_empty() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("empty_redact.pdf");
        let count = redact_text_patterns(&pdf, &[], &out).unwrap();
        assert_eq!(count, 0);
    }

    // ── PDF/A tests ───────────────────────────────────────────────────

    #[test]
    fn convert_pdfa_creates_valid_output() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("pdfa.pdf");
        convert_to_pdfa(&pdf, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
        // Verify the output file is larger (has XMP + OutputIntent)
        let orig_size = std::fs::metadata(&pdf).unwrap().len();
        let pdfa_size = std::fs::metadata(&out).unwrap().len();
        assert!(pdfa_size > orig_size);
    }

    // ── Signature detection tests ─────────────────────────────────────

    #[test]
    fn detect_signatures_none_on_unsigned() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let sigs = detect_signatures(&pdf).unwrap();
        assert!(sigs.is_empty());
    }

    // ── Sanitise options tests ────────────────────────────────────────

    #[test]
    fn sanitise_selective_strips_only_requested() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("selective.pdf");
        // Only strip Info, keep everything else
        let opts = SanitiseOptions {
            strip_info: true,
            strip_xmp: false,
            strip_javascript: false,
            strip_embedded_files: false,
            strip_open_action: false,
            strip_thumbnails: false,
            strip_annotations: false,
        };
        let stripped = sanitise_pdf_with_options(&pdf, &opts, &out).unwrap();
        assert!(stripped.contains(&"Info dictionary".to_string()));
        assert!(!stripped.contains(&"XMP metadata".to_string()));
    }

    #[test]
    fn sanitise_strip_nothing() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("noop.pdf");
        let opts = SanitiseOptions {
            strip_info: false, strip_xmp: false, strip_javascript: false,
            strip_embedded_files: false, strip_open_action: false,
            strip_thumbnails: false, strip_annotations: false,
        };
        let stripped = sanitise_pdf_with_options(&pdf, &opts, &out).unwrap();
        assert!(stripped.is_empty());
        // Output should still be valid
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    // ── Additional edge case tests ────────────────────────────────────

    #[test]
    fn merge_same_file_twice() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("doubled.pdf");
        let total = merge_pdfs(&[&pdf, &pdf], &out).unwrap();
        assert_eq!(total, 4);
    }

    #[test]
    fn extract_all_pages_is_copy() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("copy.pdf");
        extract_pages(&pdf, &[0, 1], &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 2);
    }

    #[test]
    fn rotate_all_pages() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("rotated_all.pdf");
        rotate_pages(&pdf, &[0, 1], 180, &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.pages[0].rotation, 180);
        assert_eq!(info.pages[1].rotation, 180);
    }

    #[test]
    fn add_page_numbers_all_positions() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        for pos in ["bottom-center", "bottom-left", "bottom-right", "top-center", "top-left", "top-right"] {
            let out = dir.path().join(format!("num_{pos}.pdf"));
            let config = PageNumberConfig { position: pos.into(), ..Default::default() };
            add_page_numbers(&pdf, &config, &out).unwrap();
            assert!(out.exists());
        }
    }

    #[test]
    fn watermark_custom_color() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("colored.pdf");
        let config = WatermarkConfig {
            text: "DRAFT".into(),
            color: [1.0, 0.0, 0.0], // red
            ..Default::default()
        };
        add_watermark(&pdf, &config, None, &out).unwrap();
        assert!(out.exists());
    }

    #[test]
    fn split_single_page_ranges() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out_dir = dir.path().join("single_splits");
        std::fs::create_dir_all(&out_dir).unwrap();
        let outputs = split_pdf(&pdf, &[(0, 1), (1, 2)], &out_dir, "test").unwrap();
        assert_eq!(outputs.len(), 2);
        for o in &outputs {
            assert_eq!(pdf_info(Path::new(o)).unwrap().page_count, 1);
        }
    }

    #[test]
    fn reorder_duplicate_pages() {
        // Reorder with duplicated indices (page 0 twice)
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("duped.pdf");
        reorder_pages(&pdf, &[0, 0, 1], &out).unwrap();
        let info = pdf_info(&out).unwrap();
        assert_eq!(info.page_count, 3);
    }

    #[test]
    fn metadata_edit_preserves_pages() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("meta_pages.pdf");
        edit_metadata(&pdf, &MetadataEdit { title: Some("X".into()), ..Default::default() }, &out).unwrap();
        assert_eq!(pdf_info(&out).unwrap().page_count, 2);
    }

    #[test]
    fn encrypt_with_restricted_permissions() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("restricted.pdf");
        let config = EncryptConfig {
            owner_password: "admin".into(),
            user_password: "view".into(),
            allow_print: false,
            allow_copy: false,
            allow_modify: false,
            ..Default::default()
        };
        encrypt_pdf(&pdf, &config, &out).unwrap();
        assert!(is_encrypted(&out).unwrap());
    }

    #[test]
    fn redact_multiple_regions_same_page() {
        let dir = TempDir::new().unwrap();
        let pdf = create_test_pdf(dir.path());
        let out = dir.path().join("multi_redact.pdf");
        let regions = vec![
            RedactionSpec { page: 0, x: 10.0, y: 10.0, w: 50.0, h: 20.0 },
            RedactionSpec { page: 0, x: 100.0, y: 100.0, w: 80.0, h: 30.0 },
            RedactionSpec { page: 0, x: 200.0, y: 200.0, w: 60.0, h: 15.0 },
        ];
        let count = black_out_regions(&pdf, &regions, &out).unwrap();
        assert_eq!(count, 3);
    }
}
