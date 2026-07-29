//! AcroForm fields: read, fill, flatten (P32.5).
//!
//! Nothing in the tree touched interactive forms before this beyond a
//! `/SigFlags` check in signature detection.
//!
//! ## Appearances
//!
//! A filled field renders from its `/AP` appearance stream, not from
//! `/V`. Generating appearances means laying out text inside the widget
//! rectangle with the field's own `/DA` font and quadding — a lot of
//! drawing code, and wrong in a different way for every field type.
//!
//! Instead we set `/NeedAppearances true` on the form dictionary, which
//! tells the viewer to build them itself. Every mainstream viewer honours
//! it. The exception is a viewer that renders `/AP` only and ignores the
//! flag, where a filled field can look empty while carrying the right
//! value — which is why [`flatten_form_doc`] exists: it draws the values
//! as page content and removes the fields, so the result is correct
//! everywhere.

use lopdf::{Document, Object, ObjectId};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Checkbox,
    Radio,
    Choice,
    Button,
    Signature,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormField {
    /// Fully qualified name — parent names joined with `.`, as the PDF
    /// spec defines it. This is what a filler should key on: partial
    /// names are only unique within their parent.
    pub name: String,
    pub kind: FieldKind,
    /// Current value as text. Checkboxes report their on/off state name.
    pub value: Option<String>,
    /// For checkboxes and radios, the value that means "on".
    pub on_value: Option<String>,
    /// Choice-field options.
    pub options: Vec<String>,
    pub read_only: bool,
    pub required: bool,
    /// 0-based page the widget sits on, when it could be located.
    pub page: Option<usize>,
    /// Widget rectangle (x, y, w, h), for flattening and for a UI overlay.
    pub rect: Option<(f64, f64, f64, f64)>,
}

// ── Helpers ────────────────────────────────────────────────────────────

fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Object> {
    match obj {
        Object::Reference(r) => doc.get_object(*r).ok(),
        other => Some(other),
    }
}

fn decode_text(bytes: &[u8]) -> String {
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

fn obj_text(doc: &Document, obj: &Object) -> Option<String> {
    match resolve(doc, obj)? {
        Object::String(b, _) => Some(decode_text(b)),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    }
}

fn obj_f64(doc: &Document, o: &Object) -> f64 {
    match resolve(doc, o) {
        Some(Object::Integer(n)) => *n as f64,
        Some(Object::Real(n)) => *n as f64,
        _ => 0.0,
    }
}

/// The `/AcroForm` dictionary's object id, when it is stored indirectly.
///
/// `None` does **not** mean the document has no form: the catalog may hold
/// `/AcroForm` as a direct dictionary, which is legal and is what MuPDF
/// writes. Use [`acroform_dict`] to read it and
/// [`set_acroform_flag`] to modify it.
fn acroform_id(doc: &Document) -> Option<ObjectId> {
    let cat = doc.catalog().ok()?;
    match cat.get(b"AcroForm").ok()? {
        Object::Reference(r) => Some(*r),
        _ => None,
    }
}

/// The `/AcroForm` dictionary, however the catalog stores it.
fn acroform_dict(doc: &Document) -> Option<lopdf::Dictionary> {
    let cat = doc.catalog().ok()?;
    match cat.get(b"AcroForm").ok()? {
        Object::Dictionary(d) => Some(d.clone()),
        Object::Reference(r) => match doc.get_object(*r) {
            Ok(Object::Dictionary(d)) => Some(d.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Set a boolean on the form dictionary, wherever it lives.
fn set_acroform_flag(doc: &mut Document, key: &str, value: bool) {
    if let Some(id) = acroform_id(doc) {
        if let Ok(Object::Dictionary(ref mut f)) = doc.get_object_mut(id) {
            f.set(key, Object::Boolean(value));
        }
        return;
    }
    // Direct dictionary: mutate it in place on the catalog.
    if let Ok(cat) = doc.catalog_mut() {
        if let Ok(Object::Dictionary(ref mut f)) = cat.get_mut(b"AcroForm") {
            f.set(key, Object::Boolean(value));
        }
    }
}

/// Field type, inherited from an ancestor when the node omits it.
fn field_kind(doc: &Document, dict: &lopdf::Dictionary, flags: i64) -> FieldKind {
    let ft = dict
        .get(b"FT")
        .ok()
        .and_then(|o| resolve(doc, o))
        .and_then(|o| match o {
            Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
            _ => None,
        });
    match ft.as_deref() {
        Some("Tx") => FieldKind::Text,
        Some("Btn") => {
            // Bit 16 (1-based) = pushbutton, bit 15 = radio.
            if flags & (1 << 16) != 0 {
                FieldKind::Button
            } else if flags & (1 << 15) != 0 {
                FieldKind::Radio
            } else {
                FieldKind::Checkbox
            }
        }
        Some("Ch") => FieldKind::Choice,
        Some("Sig") => FieldKind::Signature,
        _ => FieldKind::Unknown,
    }
}

/// The "on" state of a checkbox or radio widget: the `/AP` `/N` key that
/// is not `/Off`.
fn on_state(doc: &Document, dict: &lopdf::Dictionary) -> Option<String> {
    let ap = match dict.get(b"AP").ok().and_then(|o| resolve(doc, o))? {
        Object::Dictionary(d) => d.clone(),
        _ => return None,
    };
    let n = match ap.get(b"N").ok().and_then(|o| resolve(doc, o))? {
        Object::Dictionary(d) => d.clone(),
        _ => return None,
    };
    n.iter()
        .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
        .find(|k| k != "Off")
}

// ── Reading ────────────────────────────────────────────────────────────

/// Walk the field tree and return every terminal field.
///
/// Intermediate nodes exist only to namespace their children, so they are
/// not reported; their names are folded into the qualified names below.
pub fn read_fields(doc: &Document) -> Vec<FormField> {
    let mut out = Vec::new();
    let form = match acroform_dict(doc) {
        Some(f) => f,
        None => return out,
    };
    let roots = match form.get(b"Fields").ok().and_then(|o| resolve(doc, o)) {
        Some(Object::Array(a)) => a.clone(),
        _ => return out,
    };

    // Widget -> page, so fields can report where they live.
    let mut widget_page = std::collections::HashMap::new();
    for (idx, page_id) in doc.page_iter().enumerate() {
        if let Ok(Object::Dictionary(p)) = doc.get_object(page_id) {
            if let Some(Object::Array(annots)) = p.get(b"Annots").ok().and_then(|a| resolve(doc, a))
            {
                for a in annots {
                    if let Object::Reference(r) = a {
                        widget_page.insert(*r, idx);
                    }
                }
            }
        }
    }

    for root in &roots {
        walk_field(doc, root, "", None, 0, &widget_page, &mut out, 0);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk_field(
    doc: &Document,
    node: &Object,
    prefix: &str,
    inherited_kind: Option<FieldKind>,
    inherited_flags: i64,
    widget_page: &std::collections::HashMap<ObjectId, usize>,
    out: &mut Vec<FormField>,
    depth: usize,
) {
    // A malformed file can cycle through /Kids.
    if depth > 32 {
        return;
    }
    let node_id = match node {
        Object::Reference(r) => Some(*r),
        _ => None,
    };
    let dict = match resolve(doc, node) {
        Some(Object::Dictionary(d)) => d.clone(),
        _ => return,
    };

    let partial = dict.get(b"T").ok().and_then(|o| obj_text(doc, o));
    let name = match (&partial, prefix.is_empty()) {
        (Some(p), true) => p.clone(),
        (Some(p), false) => format!("{prefix}.{p}"),
        (None, _) => prefix.to_string(),
    };

    let flags = dict
        .get(b"Ff")
        .ok()
        .map(|o| obj_f64(doc, o) as i64)
        .unwrap_or(inherited_flags);
    let kind = match dict.get(b"FT") {
        Ok(_) => field_kind(doc, &dict, flags),
        Err(_) => inherited_kind.unwrap_or(FieldKind::Unknown),
    };

    let kids = match dict.get(b"Kids").ok().and_then(|o| resolve(doc, o)) {
        Some(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    };

    // A node with /Kids that are themselves fields (they have /T) is an
    // intermediate node. Kids without /T are just widgets for this field.
    let kids_are_fields = kids.iter().any(|k| match resolve(doc, k) {
        Some(Object::Dictionary(d)) => d.has(b"T"),
        _ => false,
    });

    if kids_are_fields {
        for kid in &kids {
            walk_field(doc, kid, &name, Some(kind), flags, widget_page, out, depth + 1);
        }
        return;
    }

    // Terminal field. Its widget is either merged into this dict or is a
    // single kid.
    let widget_dict = if kids.is_empty() {
        Some((node_id, dict.clone()))
    } else {
        kids.first().and_then(|k| match (k, resolve(doc, k)) {
            (Object::Reference(r), Some(Object::Dictionary(d))) => Some((Some(*r), d.clone())),
            (_, Some(Object::Dictionary(d))) => Some((None, d.clone())),
            _ => None,
        })
    };

    let (page, rect) = match &widget_dict {
        Some((id, wd)) => {
            let page = id.and_then(|i| widget_page.get(&i).copied());
            let rect = match wd.get(b"Rect").ok().and_then(|o| resolve(doc, o)) {
                Some(Object::Array(a)) if a.len() == 4 => {
                    let (x0, y0, x1, y1) = (
                        obj_f64(doc, &a[0]),
                        obj_f64(doc, &a[1]),
                        obj_f64(doc, &a[2]),
                        obj_f64(doc, &a[3]),
                    );
                    Some((
                        x0.min(x1),
                        y0.min(y1),
                        (x1 - x0).abs(),
                        (y1 - y0).abs(),
                    ))
                }
                _ => None,
            };
            (page, rect)
        }
        None => (None, None),
    };

    let value = dict.get(b"V").ok().and_then(|o| obj_text(doc, o));
    let on_value = widget_dict.as_ref().and_then(|(_, wd)| on_state(doc, wd));

    let options = match dict.get(b"Opt").ok().and_then(|o| resolve(doc, o)) {
        Some(Object::Array(a)) => a
            .iter()
            .filter_map(|o| match resolve(doc, o) {
                // An option may be [export, display]; the display string
                // is what a user picks from.
                Some(Object::Array(pair)) => pair.last().and_then(|p| obj_text(doc, p)),
                Some(other) => obj_text(doc, other),
                None => None,
            })
            .collect(),
        _ => Vec::new(),
    };

    out.push(FormField {
        name,
        kind,
        value,
        on_value,
        options,
        // Bit 1 = read-only, bit 2 = required (1-based in the spec).
        read_only: flags & 1 != 0,
        required: flags & 2 != 0,
        page,
        rect,
    });
}

pub fn read_fields_from_path(path: &Path) -> Result<Vec<FormField>, String> {
    let doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    Ok(read_fields(&doc))
}

// ── Filling ────────────────────────────────────────────────────────────

fn text_object(s: &str) -> Object {
    let mut bytes = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        bytes.extend_from_slice(&u.to_be_bytes());
    }
    Object::String(bytes, lopdf::StringFormat::Hexadecimal)
}

/// Set field values by qualified name. Returns how many were set.
///
/// Unknown names are an error rather than a silent no-op: a filler that
/// quietly drops a value produces a form that looks filled and is not.
pub fn fill_fields_doc(
    doc: &mut Document,
    values: &std::collections::HashMap<String, String>,
) -> Result<usize, String> {
    let fields = read_fields(doc);
    let known: std::collections::HashSet<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    for name in values.keys() {
        if !known.contains(name.as_str()) {
            return Err(format!(
                "no such form field: {name:?} (document has: {})",
                fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
    }

    // Locate the object id backing each named field.
    let ids = field_object_ids(doc);
    let mut set = 0;
    for (name, value) in values {
        let Some(&id) = ids.get(name) else { continue };
        let kind = fields.iter().find(|f| &f.name == name).map(|f| f.kind);
        let on = fields.iter().find(|f| &f.name == name).and_then(|f| f.on_value.clone());

        if let Ok(Object::Dictionary(ref mut d)) = doc.get_object_mut(id) {
            match kind {
                Some(FieldKind::Checkbox) | Some(FieldKind::Radio) => {
                    // Checkbox state is a *name*, and the "on" name is
                    // whatever the widget's /AP declares — not "Yes".
                    let truthy = matches!(
                        value.to_lowercase().as_str(),
                        "true" | "yes" | "on" | "1"
                    );
                    let state = if truthy {
                        on.unwrap_or_else(|| "Yes".to_string())
                    } else {
                        "Off".to_string()
                    };
                    d.set("V", Object::Name(state.clone().into_bytes()));
                    d.set("AS", Object::Name(state.into_bytes()));
                }
                _ => {
                    d.set("V", text_object(value));
                }
            }
            set += 1;
        }
    }

    // Without this the viewer renders the old (or empty) appearance.
    set_acroform_flag(doc, "NeedAppearances", true);
    Ok(set)
}

/// Map qualified field name → the object id holding its `/V`.
fn field_object_ids(doc: &Document) -> std::collections::HashMap<String, ObjectId> {
    let mut map = std::collections::HashMap::new();
    let Some(form) = acroform_dict(doc) else { return map };
    let Some(Object::Array(roots)) = form.get(b"Fields").ok().and_then(|o| resolve(doc, o)) else {
        return map;
    };
    let roots = roots.clone();
    for r in &roots {
        collect_ids(doc, r, "", &mut map, 0);
    }
    map
}

fn collect_ids(
    doc: &Document,
    node: &Object,
    prefix: &str,
    map: &mut std::collections::HashMap<String, ObjectId>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    let Object::Reference(id) = node else { return };
    let Ok(Object::Dictionary(dict)) = doc.get_object(*id) else { return };
    let dict = dict.clone();

    let partial = dict.get(b"T").ok().and_then(|o| obj_text(doc, o));
    let name = match (&partial, prefix.is_empty()) {
        (Some(p), true) => p.clone(),
        (Some(p), false) => format!("{prefix}.{p}"),
        (None, _) => prefix.to_string(),
    };

    let kids = match dict.get(b"Kids").ok().and_then(|o| resolve(doc, o)) {
        Some(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let kids_are_fields = kids.iter().any(|k| match resolve(doc, k) {
        Some(Object::Dictionary(d)) => d.has(b"T"),
        _ => false,
    });
    if kids_are_fields {
        for kid in &kids {
            collect_ids(doc, kid, &name, map, depth + 1);
        }
    } else {
        map.insert(name, *id);
    }
}

pub fn fill_fields(
    path: &Path,
    values: &std::collections::HashMap<String, String>,
    out_path: &Path,
) -> Result<usize, String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let n = fill_fields_doc(&mut doc, values)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(n)
}

// ── Flattening ─────────────────────────────────────────────────────────

/// Draw field values as page content and remove the interactive form.
///
/// This is what makes a filled form portable: no viewer has to honour
/// `/NeedAppearances`, and the values can no longer be edited away.
/// Returns the number of fields flattened.
pub fn flatten_form_doc(doc: &mut Document) -> Result<usize, String> {
    let fields = read_fields(doc);
    if fields.is_empty() {
        return Ok(0);
    }
    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let mut drawn = 0;

    for f in &fields {
        let (Some(page_idx), Some((x, y, _w, h))) = (f.page, f.rect) else { continue };
        let Some(&page_id) = page_ids.get(page_idx) else { continue };

        let text = match f.kind {
            FieldKind::Checkbox | FieldKind::Radio => {
                let on = f.value.as_deref().unwrap_or("Off");
                if on.eq_ignore_ascii_case("off") || on.is_empty() {
                    continue;
                }
                "X".to_string()
            }
            _ => match f.value.as_deref() {
                Some(v) if !v.trim().is_empty() => v.to_string(),
                _ => continue,
            },
        };

        // Sit the baseline a little above the widget's bottom edge, with a
        // font size that fits the box rather than a fixed guess.
        let size = (h * 0.6).clamp(6.0, 14.0);
        let baseline = y + (h - size) / 2.0 + size * 0.2;
        let content = format!(
            "q BT /F1 {size:.1} Tf 0 0 0 rg {:.2} {:.2} Td ({}) Tj ET Q",
            x + 2.0,
            baseline,
            crate::pdf_ops::escape_pdf_literal(&text),
        );
        let font_id = crate::pdf_ops::add_helvetica(doc);
        crate::pdf_ops::append_content(doc, page_id, content.into_bytes(), Some(("F1", font_id)), None);
        drawn += 1;
    }

    // Drop the widgets, then the form itself.
    for page_id in &page_ids {
        let annots = match doc.get_object(*page_id) {
            Ok(Object::Dictionary(p)) => match p.get(b"Annots").ok().and_then(|a| resolve(doc, a)) {
                Some(Object::Array(a)) => a.clone(),
                _ => continue,
            },
            _ => continue,
        };
        let kept: Vec<Object> = annots
            .into_iter()
            .filter(|a| {
                let d = match resolve(doc, a) {
                    Some(Object::Dictionary(d)) => d,
                    _ => return true,
                };
                !matches!(d.get(b"Subtype"), Ok(Object::Name(n)) if n.as_slice() == b"Widget")
            })
            .collect();
        if let Ok(Object::Dictionary(ref mut p)) = doc.get_object_mut(*page_id) {
            if kept.is_empty() {
                p.remove(b"Annots");
            } else {
                p.set("Annots", Object::Array(kept));
            }
        }
    }
    if let Ok(cat) = doc.catalog_mut() {
        cat.remove(b"AcroForm");
    }
    Ok(drawn)
}

pub fn flatten_form(path: &Path, out_path: &Path) -> Result<usize, String> {
    let mut doc = Document::load(path).map_err(|e| format!("load: {e}"))?;
    let n = flatten_form_doc(&mut doc)?;
    doc.save(out_path).map_err(|e| format!("save: {e}"))?;
    Ok(n)
}

pub mod tauri_commands {
    use super::*;
    use std::collections::HashMap;

    #[tauri::command]
    pub async fn pdf_read_form_fields(path: String) -> Result<Vec<FormField>, String> {
        tokio::task::spawn_blocking(move || read_fields_from_path(Path::new(&path)))
            .await
            .map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_fill_form(
        path: String,
        values: HashMap<String, String>,
        out_path: String,
    ) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || {
            fill_fields(Path::new(&path), &values, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))?
    }

    #[tauri::command]
    pub async fn pdf_flatten_form(path: String, out_path: String) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || flatten_form(Path::new(&path), Path::new(&out_path)))
            .await
            .map_err(|e| format!("join: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A one-page document with an AcroForm: a text field, a checkbox
    /// whose "on" state is /Ja (deliberately not /Yes), and a two-level
    /// nested field to exercise qualified naming.
    fn form_doc() -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();

        let text_field = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Annot".to_vec())),
            ("Subtype", Object::Name(b"Widget".to_vec())),
            ("FT", Object::Name(b"Tx".to_vec())),
            ("T", Object::String(b"fullName".to_vec(), lopdf::StringFormat::Literal)),
            ("Rect", Object::Array(vec![
                Object::Integer(100), Object::Integer(700),
                Object::Integer(300), Object::Integer(720),
            ])),
        ])));

        let mut ap_n = lopdf::Dictionary::new();
        ap_n.set("Ja", Object::Null);
        ap_n.set("Off", Object::Null);
        let mut ap = lopdf::Dictionary::new();
        ap.set("N", Object::Dictionary(ap_n));
        let check = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Annot".to_vec())),
            ("Subtype", Object::Name(b"Widget".to_vec())),
            ("FT", Object::Name(b"Btn".to_vec())),
            ("T", Object::String(b"agree".to_vec(), lopdf::StringFormat::Literal)),
            ("AP", Object::Dictionary(ap)),
            ("Rect", Object::Array(vec![
                Object::Integer(100), Object::Integer(650),
                Object::Integer(112), Object::Integer(662),
            ])),
        ])));

        // address.city — a child under an intermediate node.
        let city = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Annot".to_vec())),
            ("Subtype", Object::Name(b"Widget".to_vec())),
            ("T", Object::String(b"city".to_vec(), lopdf::StringFormat::Literal)),
            ("Rect", Object::Array(vec![
                Object::Integer(100), Object::Integer(600),
                Object::Integer(300), Object::Integer(620),
            ])),
        ])));
        let address = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("FT", Object::Name(b"Tx".to_vec())),
            ("T", Object::String(b"address".to_vec(), lopdf::StringFormat::Literal)),
            ("Kids", Object::Array(vec![Object::Reference(city)])),
        ])));

        let form = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Fields", Object::Array(vec![
                Object::Reference(text_field),
                Object::Reference(check),
                Object::Reference(address),
            ])),
        ])));

        doc.objects.insert(page_id, Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Page".to_vec())),
            ("Parent", Object::Reference(pages_id)),
            ("MediaBox", Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(612), Object::Integer(792),
            ])),
            ("Annots", Object::Array(vec![
                Object::Reference(text_field),
                Object::Reference(check),
                Object::Reference(city),
            ])),
        ])));
        doc.objects.insert(pages_id, Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Count", Object::Integer(1)),
            ("Kids", Object::Array(vec![Object::Reference(page_id)])),
        ])));
        let cat = doc.add_object(Object::Dictionary(lopdf::Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Catalog".to_vec())),
            ("Pages", Object::Reference(pages_id)),
            ("AcroForm", Object::Reference(form)),
        ])));
        doc.trailer.set("Root", Object::Reference(cat));
        doc
    }

    fn field<'a>(fs: &'a [FormField], name: &str) -> &'a FormField {
        fs.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("no field {name:?} in {:?}", fs.iter().map(|f| &f.name).collect::<Vec<_>>()))
    }

    /// Rebuild the fixture with /AcroForm stored *directly* in the
    /// catalog rather than as a reference.
    fn form_doc_direct_acroform() -> Document {
        let mut doc = form_doc();
        let form = acroform_dict(&doc).expect("fixture should have a form");
        if let Ok(cat) = doc.catalog_mut() {
            cat.set("AcroForm", Object::Dictionary(form));
        }
        doc
    }

    #[test]
    fn reads_a_form_stored_as_a_direct_dictionary() {
        // The catalog may hold /AcroForm either way; MuPDF writes it
        // direct, and only handling the reference made such forms
        // invisible — found by verifying against a MuPDF-authored file.
        let doc = form_doc_direct_acroform();
        let fs = read_fields(&doc);
        assert_eq!(fs.len(), 3, "direct /AcroForm was not read");
        assert_eq!(field(&fs, "fullName").kind, FieldKind::Text);
    }

    #[test]
    fn filling_a_direct_acroform_still_requests_appearances() {
        let mut doc = form_doc_direct_acroform();
        let mut vals = HashMap::new();
        vals.insert("fullName".to_string(), "Grace Hopper".to_string());
        assert_eq!(fill_fields_doc(&mut doc, &vals).unwrap(), 1);
        let form = acroform_dict(&doc).unwrap();
        assert!(
            matches!(form.get(b"NeedAppearances"), Ok(Object::Boolean(true))),
            "NeedAppearances not set on a direct /AcroForm"
        );
    }

    #[test]
    fn flattening_a_direct_acroform_removes_it() {
        let mut doc = form_doc_direct_acroform();
        let mut vals = HashMap::new();
        vals.insert("fullName".to_string(), "Grace Hopper".to_string());
        fill_fields_doc(&mut doc, &vals).unwrap();
        flatten_form_doc(&mut doc).unwrap();
        assert!(read_fields(&doc).is_empty());
        assert!(doc.catalog().unwrap().get(b"AcroForm").is_err());
    }

    #[test]
    fn reads_fields_with_kinds_and_pages() {
        let fs = read_fields(&form_doc());
        assert_eq!(fs.len(), 3, "{:?}", fs.iter().map(|f| &f.name).collect::<Vec<_>>());
        assert_eq!(field(&fs, "fullName").kind, FieldKind::Text);
        assert_eq!(field(&fs, "agree").kind, FieldKind::Checkbox);
        assert_eq!(field(&fs, "fullName").page, Some(0));
        assert!(field(&fs, "fullName").rect.is_some());
    }

    #[test]
    fn nested_fields_get_qualified_names() {
        // Partial names are only unique within their parent, so a filler
        // keying on "city" alone would be ambiguous in a real form.
        let fs = read_fields(&form_doc());
        assert!(fs.iter().any(|f| f.name == "address.city"));
        assert!(!fs.iter().any(|f| f.name == "city"));
    }

    #[test]
    fn intermediate_nodes_are_not_reported_as_fields() {
        let fs = read_fields(&form_doc());
        assert!(!fs.iter().any(|f| f.name == "address"), "namespace node leaked into the field list");
    }

    #[test]
    fn checkbox_on_state_is_read_from_the_appearance_dict() {
        let fs = read_fields(&form_doc());
        // /Ja, not the /Yes everyone assumes.
        assert_eq!(field(&fs, "agree").on_value.as_deref(), Some("Ja"));
    }

    #[test]
    fn filling_sets_values_and_requests_appearances() {
        let mut doc = form_doc();
        let mut vals = HashMap::new();
        vals.insert("fullName".to_string(), "Ada Lovelace".to_string());
        vals.insert("address.city".to_string(), "London".to_string());
        assert_eq!(fill_fields_doc(&mut doc, &vals).unwrap(), 2);

        let fs = read_fields(&doc);
        assert_eq!(field(&fs, "fullName").value.as_deref(), Some("Ada Lovelace"));
        assert_eq!(field(&fs, "address.city").value.as_deref(), Some("London"));

        let form = acroform_dict(&doc).unwrap();
        assert!(matches!(form.get(b"NeedAppearances"), Ok(Object::Boolean(true))),
            "viewers will render the old appearance without this");
    }

    #[test]
    fn checkbox_uses_the_documents_own_on_state_not_yes() {
        let mut doc = form_doc();
        let mut vals = HashMap::new();
        vals.insert("agree".to_string(), "true".to_string());
        fill_fields_doc(&mut doc, &vals).unwrap();
        let fs = read_fields(&doc);
        assert_eq!(field(&fs, "agree").value.as_deref(), Some("Ja"));
    }

    #[test]
    fn unchecking_writes_off() {
        let mut doc = form_doc();
        let mut vals = HashMap::new();
        vals.insert("agree".to_string(), "false".to_string());
        fill_fields_doc(&mut doc, &vals).unwrap();
        assert_eq!(field(&read_fields(&doc), "agree").value.as_deref(), Some("Off"));
    }

    #[test]
    fn an_unknown_field_name_is_an_error_not_a_silent_noop() {
        // A filler that quietly drops a value yields a form that looks
        // filled and is not.
        let mut doc = form_doc();
        let mut vals = HashMap::new();
        vals.insert("nosuchfield".to_string(), "x".to_string());
        let err = fill_fields_doc(&mut doc, &vals).unwrap_err();
        assert!(err.contains("nosuchfield"), "{err}");
    }

    #[test]
    fn non_ascii_values_survive() {
        let mut doc = form_doc();
        let mut vals = HashMap::new();
        vals.insert("fullName".to_string(), "Ærøskøbing — 東京".to_string());
        fill_fields_doc(&mut doc, &vals).unwrap();
        assert_eq!(field(&read_fields(&doc), "fullName").value.as_deref(), Some("Ærøskøbing — 東京"));
    }

    #[test]
    fn flattening_removes_the_form_and_its_widgets() {
        let mut doc = form_doc();
        let mut vals = HashMap::new();
        vals.insert("fullName".to_string(), "Ada Lovelace".to_string());
        fill_fields_doc(&mut doc, &vals).unwrap();

        let drawn = flatten_form_doc(&mut doc).unwrap();
        assert!(drawn >= 1, "nothing was drawn");
        assert!(read_fields(&doc).is_empty(), "fields survived flattening");
        assert!(doc.catalog().unwrap().get(b"AcroForm").is_err(), "/AcroForm survived");

        let page_id = doc.page_iter().next().unwrap();
        if let Ok(Object::Dictionary(p)) = doc.get_object(page_id) {
            if let Ok(Object::Array(a)) = p.get(b"Annots") {
                assert!(a.is_empty(), "widget annotations survived");
            }
        }
    }

    #[test]
    fn flattening_draws_the_value_onto_the_page() {
        let mut doc = form_doc();
        let mut vals = HashMap::new();
        vals.insert("fullName".to_string(), "Ada Lovelace".to_string());
        fill_fields_doc(&mut doc, &vals).unwrap();
        flatten_form_doc(&mut doc).unwrap();

        let page_id = doc.page_iter().next().unwrap();
        let content = doc.get_and_decode_page_content(page_id).unwrap();
        let mut drawn = String::new();
        for op in &content.operations {
            if op.operator == "Tj" {
                if let Some(Object::String(b, _)) = op.operands.first() {
                    drawn.push_str(&String::from_utf8_lossy(b));
                }
            }
        }
        assert!(drawn.contains("Ada Lovelace"), "value not drawn: {drawn:?}");
    }

    #[test]
    fn an_empty_field_is_not_drawn() {
        let mut doc = form_doc();
        // Nothing filled at all.
        assert_eq!(flatten_form_doc(&mut doc).unwrap(), 0);
    }

    #[test]
    fn a_document_without_a_form_reads_as_empty() {
        let mut doc = form_doc();
        if let Ok(cat) = doc.catalog_mut() { cat.remove(b"AcroForm"); }
        assert!(read_fields(&doc).is_empty());
        assert_eq!(flatten_form_doc(&mut doc).unwrap(), 0);
    }
}
