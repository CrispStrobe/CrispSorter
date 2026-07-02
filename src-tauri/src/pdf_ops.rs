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
    let ids = page_ids(&doc);
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
    Ok(PdfInfo {
        page_count: ids.len(),
        pages,
        title,
        author,
        subject,
        keywords,
        producer,
        creator,
    })
}

// ── Reorder pages ──────────────────────────────────────────────────────

pub fn reorder_pages(path: &Path, new_order: &[usize], out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let ids = page_ids(&doc);
    let n = ids.len();
    for &idx in new_order {
        if idx >= n { return Err(format!("page index {idx} out of range (0..{n})")); }
    }
    let reordered: Vec<ObjectId> = new_order.iter().map(|&i| ids[i]).collect();
    let pid = pages_ref(&doc)?;
    if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(pid) {
        d.set("Kids", Object::Array(reordered.iter().map(|id| Object::Reference(*id)).collect()));
        d.set("Count", Object::Integer(reordered.len() as i64));
    }
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Extract pages ──────────────────────────────────────────────────────

pub fn extract_pages(path: &Path, page_indices: &[usize], out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let ids = page_ids(&doc);
    let n = ids.len();
    let keep: Vec<ObjectId> = page_indices
        .iter()
        .map(|&i| if i >= n { Err(format!("page {i} out of range")) } else { Ok(ids[i]) })
        .collect::<Result<_, _>>()?;
    let pid = pages_ref(&doc)?;
    if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(pid) {
        d.set("Kids", Object::Array(keep.iter().map(|id| Object::Reference(*id)).collect()));
        d.set("Count", Object::Integer(keep.len() as i64));
    }
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Remove pages ───────────────────────────────────────────────────────

pub fn remove_pages(path: &Path, page_indices: &[usize], out_path: &Path) -> Result<(), String> {
    let doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let n = page_ids(&doc).len();
    let remove: std::collections::HashSet<usize> = page_indices.iter().copied().collect();
    for &idx in &remove { if idx >= n { return Err(format!("page {idx} out of range")); } }
    let keep: Vec<usize> = (0..n).filter(|i| !remove.contains(i)).collect();
    if keep.is_empty() { return Err("Cannot remove all pages".into()); }
    extract_pages(path, &keep, out_path)
}

// ── Rotate pages ───────────────────────────────────────────────────────

pub fn rotate_pages(path: &Path, page_indices: &[usize], degrees: i64, out_path: &Path) -> Result<(), String> {
    if ![0, 90, 180, 270].contains(&degrees) {
        return Err(format!("degrees must be 0/90/180/270, got {degrees}"));
    }
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let ids = page_ids(&doc);
    let n = ids.len();
    for &idx in page_indices {
        if idx >= n { return Err(format!("page {idx} out of range")); }
        if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(ids[idx]) {
            let cur = match d.get(b"Rotate") {
                Ok(Object::Integer(v)) => *v,
                _ => 0,
            };
            d.set("Rotate", Object::Integer((cur + degrees) % 360));
        }
    }
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Crop pages ─────────────────────────────────────────────────────────

pub fn crop_pages(path: &Path, page_indices: &[usize], x: f64, y: f64, w: f64, h: f64, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let ids = page_ids(&doc);
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

pub fn add_page_numbers(path: &Path, config: &PageNumberConfig, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let ids = page_ids(&doc);
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

        let content = format!("BT /F1 {fs} Tf {adj_x:.1} {y:.1} Td ({label}) Tj ET");
        let font_dict = lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Helvetica".to_vec())),
        ]);
        let font_id = doc.add_object(Object::Dictionary(font_dict));
        let content_id = doc.add_object(Object::Stream(
            lopdf::Stream::new(lopdf::Dictionary::new(), content.into_bytes()),
        ));

        if let Ok(Object::Dictionary(ref mut page)) = doc.get_object_mut(id) {
            // Build resources with font
            let mut res = match page.get(b"Resources") {
                Ok(Object::Dictionary(d)) => d.clone(),
                _ => lopdf::Dictionary::new(),
            };
            let mut fonts = match res.get(b"Font") {
                Ok(Object::Dictionary(d)) => d.clone(),
                _ => lopdf::Dictionary::new(),
            };
            fonts.set("F1", Object::Reference(font_id));
            res.set("Font", Object::Dictionary(fonts));
            page.set("Resources", Object::Dictionary(res));

            // Append content stream
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

pub fn add_watermark(path: &Path, config: &WatermarkConfig, page_indices: Option<&[usize]>, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let ids = page_ids(&doc);
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
        let esc = config.text.replace('(', "\\(").replace(')', "\\)");
        let aw = config.text.len() as f64 * fs * 0.5;

        let content = format!(
            "q /GS0 gs {r:.3} {g:.3} {b:.3} rg BT /F1 {fs} Tf {:.4} {:.4} {:.4} {:.4} {:.1} {:.1} Tm ({esc}) Tj ET Q",
            cos, sin, -sin, cos,
            cx - aw / 2.0 * cos + fs / 2.0 * sin,
            cy - aw / 2.0 * sin - fs / 2.0 * cos,
        );

        let gs_dict = lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"ExtGState".to_vec())),
            ("ca", Object::Real(config.opacity as f32)),
            ("CA", Object::Real(config.opacity as f32)),
        ]);
        let gs_id = doc.add_object(Object::Dictionary(gs_dict));
        let font_dict = lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Helvetica".to_vec())),
        ]);
        let font_id = doc.add_object(Object::Dictionary(font_dict));
        let content_id = doc.add_object(Object::Stream(
            lopdf::Stream::new(lopdf::Dictionary::new(), content.into_bytes()),
        ));

        if let Ok(Object::Dictionary(ref mut page)) = doc.get_object_mut(id) {
            let mut res = match page.get(b"Resources") {
                Ok(Object::Dictionary(d)) => d.clone(),
                _ => lopdf::Dictionary::new(),
            };
            let mut fonts = match res.get(b"Font") {
                Ok(Object::Dictionary(d)) => d.clone(),
                _ => lopdf::Dictionary::new(),
            };
            fonts.set("F1", Object::Reference(font_id));
            res.set("Font", Object::Dictionary(fonts));
            let mut gs = match res.get(b"ExtGState") {
                Ok(Object::Dictionary(d)) => d.clone(),
                _ => lopdf::Dictionary::new(),
            };
            gs.set("GS0", Object::Reference(gs_id));
            res.set("ExtGState", Object::Dictionary(gs));
            page.set("Resources", Object::Dictionary(res));

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
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(())
}

// ── Insert blank pages ─────────────────────────────────────────────────

pub fn insert_blank_page(path: &Path, position: usize, width: f64, height: f64, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let n = page_ids(&doc).len();
    if position > n { return Err(format!("position {position} out of range (0..={n})")); }
    let pid = pages_ref(&doc)?;
    let page_dict = lopdf::Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Page".to_vec())),
        ("Parent", Object::Reference(pid)),
        ("MediaBox", Object::Array(vec![
            Object::Integer(0), Object::Integer(0),
            Object::Real(width as f32), Object::Real(height as f32),
        ])),
    ]);
    let new_page_id = doc.add_object(Object::Dictionary(page_dict));
    if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(pid) {
        let mut kids = match d.get(b"Kids") {
            Ok(Object::Array(a)) => a.clone(),
            _ => vec![],
        };
        kids.insert(position, Object::Reference(new_page_id));
        d.set("Kids", Object::Array(kids));
        d.set("Count", Object::Integer((n + 1) as i64));
    }
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

pub fn edit_metadata(path: &Path, edits: &MetadataEdit, out_path: &Path) -> Result<(), String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_roman() {
        assert_eq!(to_roman(1), "i");
        assert_eq!(to_roman(4), "iv");
        assert_eq!(to_roman(9), "ix");
        assert_eq!(to_roman(42), "xlii");
        assert_eq!(to_roman(1999), "mcmxcix");
    }

    #[test]
    fn test_page_number_config_default() {
        let c = PageNumberConfig::default();
        assert_eq!(c.position, "bottom-center");
        assert_eq!(c.format, "arabic");
    }
}
