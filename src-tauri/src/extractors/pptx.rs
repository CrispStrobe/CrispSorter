//! Native PowerPoint reader — visual-order text, speaker notes, comments.
//!
//! A Rust equivalent of the `ppt-xtract` / `pptx_to_docx` tools, reading the
//! OOXML package directly with `zip` + `quick-xml` (both already in the tree).
//!
//! ## Why this exists next to [`anydoc_conv`](super::anydoc_conv)
//!
//! anydoc converts `.pptx` competently and keeps speaker notes, but three
//! things it cannot do are exactly the ones a presentation needs:
//!
//! 1. **Visual order.** anydoc emits shapes in XML document order. A deck
//!    author who drags a box upward does not rewrite the XML, so the reading
//!    order on screen and the order in the file routinely disagree. This
//!    module sorts by the shape's `<a:off>` offset instead — top-to-bottom,
//!    then left-to-right, which is how a reader actually scans a slide.
//! 2. **Comments.** anydoc has no comment support at all. Both the classic
//!    (`p:cm`) and the modern (`p188:cm`) parts are read here — newer
//!    PowerPoint writes the latter, so handling only one silently loses
//!    comments depending on which version authored the deck.
//! 3. **Slide boundaries.** anydoc concatenates an untitled slide onto the
//!    previous one, which loses per-slide provenance. Every slide here gets
//!    a numbered heading whether or not it has a title.
//!
//! anydoc keeps the legacy binary `.ppt` path and the other twelve formats;
//! this module owns `.pptx` specifically.
//!
//! ## What is deliberately not read
//!
//! Images, charts and embedded media. Text extraction is the job here; an
//! image on a slide needs OCR, which is the `ocr` verb's business.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

// ── Model ────────────────────────────────────────────────────────────────

/// One deck.
#[derive(Debug, Clone, Default)]
pub struct Deck {
    pub slides: Vec<Slide>,
}

/// One slide, with its text already ordered.
#[derive(Debug, Clone, Default)]
pub struct Slide {
    /// 1-based position in the presentation's own slide order (which is not
    /// necessarily the `slideN.xml` filename order — decks get reordered).
    pub number: usize,
    /// Text of the title placeholder, when the slide has one.
    pub title: Option<String>,
    /// Non-title text blocks, in reading order.
    pub blocks: Vec<TextBlock>,
    /// Speaker notes, newline-joined.
    pub notes: Option<String>,
    pub comments: Vec<Comment>,
}

/// One shape's worth of text.
#[derive(Debug, Clone, Default)]
pub struct TextBlock {
    /// Paragraphs within the shape. A soft line break (`<a:br/>`) splits a
    /// paragraph here too, so line structure inside a text box survives.
    pub paragraphs: Vec<String>,
    /// EMU offset from the slide's top-left, when the shape declares one.
    /// Placeholders that inherit geometry from the layout have none.
    pub offset: Option<(i64, i64)>,
}

#[derive(Debug, Clone, Default)]
pub struct Comment {
    pub author: Option<String>,
    pub text: String,
}

/// How to order the shapes on a slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShapeOrder {
    /// Sort by `<a:off>` — top-to-bottom, then left-to-right. Matches what a
    /// reader sees rather than what the file happens to store.
    #[default]
    Visual,
    /// Raw XML document order, i.e. what anydoc and most converters emit.
    Xml,
}

/// Reader knobs, mirroring the `ppt-xtract` flags.
#[derive(Debug, Clone)]
pub struct ReadOptions {
    pub include_notes: bool,
    pub include_comments: bool,
    pub order: ShapeOrder,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            include_notes: true,
            include_comments: true,
            order: ShapeOrder::default(),
        }
    }
}

/// Render knobs.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Wrap body paragraphs at this width. `0` (default) leaves them alone.
    pub wrap_width: usize,
}

impl Slide {
    /// Heading for this slide — its title, or a numbered stand-in so an
    /// untitled slide still starts a visible section.
    pub fn heading(&self) -> String {
        match self.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => format!("Slide {}: {t}", self.number),
            None => format!("Slide {}", self.number),
        }
    }

    /// Every body paragraph, blocks flattened, blank ones dropped.
    pub fn body_paragraphs(&self) -> Vec<&str> {
        self.blocks
            .iter()
            .flat_map(|b| b.paragraphs.iter())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

// ── Reading ──────────────────────────────────────────────────────────────

type Archive = zip::ZipArchive<std::fs::File>;

/// Read a `.pptx` into a [`Deck`].
pub fn read_deck(path: &Path, opts: &ReadOptions) -> Result<Deck> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a readable PPTX package", path.display()))?;

    let authors = read_comment_authors(&mut zip);
    let slide_parts = slide_parts_in_presentation_order(&mut zip);

    let mut slides = Vec::with_capacity(slide_parts.len());
    for (i, part) in slide_parts.iter().enumerate() {
        let Some(xml) = read_part(&mut zip, part) else {
            continue;
        };
        let (title, mut blocks) = parse_slide_shapes(&xml);
        if opts.order == ShapeOrder::Visual {
            sort_visually(&mut blocks);
        }

        let rels = read_rels_for(&mut zip, part);
        let notes = if opts.include_notes {
            rels.iter()
                .find(|(ty, _)| ty.ends_with("/notesSlide"))
                .and_then(|(_, target)| read_part(&mut zip, target))
                .and_then(|xml| {
                    let text = parse_notes(&xml);
                    (!text.trim().is_empty()).then_some(text)
                })
        } else {
            None
        };

        let comments = if opts.include_comments {
            rels.iter()
                .filter(|(ty, target)| ty.ends_with("/comments") || target.contains("omment"))
                .filter_map(|(_, target)| read_part(&mut zip, target))
                .flat_map(|xml| parse_comments(&xml, &authors))
                .collect()
        } else {
            Vec::new()
        };

        slides.push(Slide {
            number: i + 1,
            title,
            blocks,
            notes,
            comments,
        });
    }

    if slides.is_empty() {
        anyhow::bail!("no slides found in {}", path.display());
    }
    Ok(Deck { slides })
}

fn read_part(zip: &mut Archive, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Slide parts in the order the *presentation* declares, not the order the
/// filenames imply. Reordering a deck in PowerPoint rewrites `sldIdLst` and
/// leaves `slideN.xml` names untouched, so sorting by filename silently
/// scrambles any deck that has ever been rearranged.
fn slide_parts_in_presentation_order(zip: &mut Archive) -> Vec<String> {
    let ordered = (|| {
        let pres = read_part(zip, "ppt/presentation.xml")?;
        let ids = parse_slide_id_list(&pres);
        if ids.is_empty() {
            return None;
        }
        let rels = read_part(zip, "ppt/_rels/presentation.xml.rels")?;
        let map = parse_relationships(&rels);
        let parts: Vec<String> = ids
            .iter()
            .filter_map(|rid| map.get(rid))
            .map(|(_, target)| resolve_relative("ppt/", target))
            .collect();
        (!parts.is_empty()).then_some(parts)
    })();

    if let Some(parts) = ordered {
        return parts;
    }

    // Fallback: numeric filename order. Correct for decks never reordered,
    // and better than returning nothing.
    let mut found: Vec<(usize, String)> = zip
        .file_names()
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .map(|n| {
            let num = n
                .trim_start_matches("ppt/slides/slide")
                .trim_end_matches(".xml")
                .parse::<usize>()
                .unwrap_or(usize::MAX);
            (num, n.to_string())
        })
        .collect();
    found.sort_by_key(|(n, _)| *n);
    found.into_iter().map(|(_, n)| n).collect()
}

fn read_rels_for(zip: &mut Archive, part: &str) -> Vec<(String, String)> {
    let (dir, file) = match part.rfind('/') {
        Some(i) => (&part[..=i], &part[i + 1..]),
        None => ("", part),
    };
    let rels_path = format!("{dir}_rels/{file}.rels");
    let Some(xml) = read_part(zip, &rels_path) else {
        return Vec::new();
    };
    parse_relationships(&xml)
        .into_values()
        .map(|(ty, target)| (ty, resolve_relative(dir, &target)))
        .collect()
}

/// Resolve a relationship target against the part's directory, collapsing
/// the `../` that OOXML uses to reach sibling folders.
fn resolve_relative(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut segments: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

// ── XML parsing ──────────────────────────────────────────────────────────

/// `<p:sldIdLst><p:sldId r:id="rId2"/>…` → the r:ids, in order.
fn parse_slide_id_list(xml: &[u8]) -> Vec<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut in_list = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = e.local_name().as_ref().to_vec();
                if local == b"sldIdLst" {
                    in_list = true;
                } else if in_list && local == b"sldId" {
                    // The id we want is `r:id`; a plain `id` attribute also
                    // exists on the same element and is NOT a relationship.
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"id"
                            && attr.key.prefix().is_some_and(|p| p.as_ref() == b"r")
                        {
                            out.push(String::from_utf8_lossy(&attr.value).into_owned());
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.local_name().as_ref() == b"sldIdLst" {
                    in_list = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// `.rels` → `{ Id: (Type, Target) }`.
fn parse_relationships(xml: &[u8]) -> BTreeMap<String, (String, String)> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out = BTreeMap::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.local_name().as_ref() != b"Relationship" {
                    buf.clear();
                    continue;
                }
                let (mut id, mut ty, mut target) = (None, None, None);
                for attr in e.attributes().flatten() {
                    let v = String::from_utf8_lossy(&attr.value).into_owned();
                    match attr.key.local_name().as_ref() {
                        b"Id" => id = Some(v),
                        b"Type" => ty = Some(v),
                        b"Target" => target = Some(v),
                        _ => {}
                    }
                }
                if let (Some(id), Some(ty), Some(target)) = (id, ty, target) {
                    out.insert(id, (ty, target));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Walk a slide's shape tree: `(title text, body blocks in XML order)`.
fn parse_slide_shapes(xml: &[u8]) -> (Option<String>, Vec<TextBlock>) {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut title: Option<String> = None;
    let mut blocks: Vec<TextBlock> = Vec::new();

    // Per-shape accumulators.
    let mut in_shape = false;
    let mut is_title = false;
    let mut is_slide_number = false;
    let mut offset: Option<(i64, i64)> = None;
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    let mut in_body = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                match e.local_name().as_ref() {
                    b"sp" => {
                        in_shape = true;
                        is_title = false;
                        is_slide_number = false;
                        offset = None;
                        paragraphs.clear();
                        current.clear();
                    }
                    b"ph" if in_shape => {
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"type" {
                                match attr.value.as_ref() {
                                    b"title" | b"ctrTitle" => is_title = true,
                                    b"sldNum" | b"dt" | b"ftr" => is_slide_number = true,
                                    _ => {}
                                }
                            }
                        }
                    }
                    // `<a:off x=".." y="..">` — EMU from the slide origin.
                    // Only the first one in a shape is the shape's own; any
                    // later ones belong to nested geometry.
                    b"off" if in_shape && offset.is_none() => {
                        let (mut x, mut y) = (None, None);
                        for attr in e.attributes().flatten() {
                            let v = String::from_utf8_lossy(&attr.value)
                                .parse::<i64>()
                                .ok();
                            match attr.key.local_name().as_ref() {
                                b"x" => x = v,
                                b"y" => y = v,
                                _ => {}
                            }
                        }
                        if let (Some(x), Some(y)) = (x, y) {
                            offset = Some((x, y));
                        }
                    }
                    b"txBody" if in_shape => in_body = true,
                    b"t" if in_body => in_text = true,
                    // A soft line break inside a paragraph. Treated as a
                    // paragraph split so line structure inside a text box
                    // survives the round trip.
                    b"br" if in_body => {
                        paragraphs.push(std::mem::take(&mut current));
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) if in_text => {
                if let Ok(decoded) = e.xml_content() {
                    current.push_str(&decoded);
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" if in_body => {
                    paragraphs.push(std::mem::take(&mut current));
                }
                b"txBody" => in_body = false,
                b"sp" if in_shape => {
                    in_shape = false;
                    let text: Vec<String> = paragraphs
                        .drain(..)
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect();
                    if text.is_empty() || is_slide_number {
                        // Slide-number / date / footer placeholders are
                        // furniture, not content.
                    } else if is_title && title.is_none() {
                        title = Some(text.join(" "));
                    } else {
                        blocks.push(TextBlock {
                            paragraphs: text,
                            offset,
                        });
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    (title, blocks)
}

/// Sort shapes into reading order.
///
/// Shapes that declare no offset inherit their geometry from the slide
/// layout, which this reader does not resolve. Rather than guess a position
/// for them, they keep XML order and go last — a stable sort, so ties and
/// the unpositioned group both stay in their original sequence.
fn sort_visually(blocks: &mut [TextBlock]) {
    blocks.sort_by_key(|b| match b.offset {
        Some((x, y)) => (0, y, x),
        None => (1, 0, 0),
    });
}

/// Speaker notes: every text run in the notes slide except the slide-number
/// placeholder, which PowerPoint puts in every notes page.
fn parse_notes(xml: &[u8]) -> String {
    let (_, blocks) = parse_slide_shapes(xml);
    blocks
        .iter()
        .flat_map(|b| b.paragraphs.iter())
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `ppt/commentAuthors.xml` (classic) and `ppt/authors.xml` (modern) →
/// `{ author id: name }`. Modern ids are GUIDs, classic ones small integers,
/// so one map serves both without collisions.
fn read_comment_authors(zip: &mut Archive) -> BTreeMap<String, String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut out = BTreeMap::new();
    for part in ["ppt/commentAuthors.xml", "ppt/authors.xml"] {
        let Some(xml) = read_part(zip, part) else {
            continue;
        };
        let mut reader = Reader::from_reader(xml.as_slice());
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let local = e.local_name().as_ref().to_vec();
                    if local == b"cmAuthor" || local == b"author" {
                        let (mut id, mut name) = (None, None);
                        for attr in e.attributes().flatten() {
                            let v = String::from_utf8_lossy(&attr.value).into_owned();
                            match attr.key.local_name().as_ref() {
                                b"id" => id = Some(v),
                                b"name" => name = Some(v),
                                _ => {}
                            }
                        }
                        if let (Some(id), Some(name)) = (id, name) {
                            out.insert(id, name);
                        }
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }
    out
}

/// Comments from either part shape.
///
/// Classic: `<p:cm authorId="1"><p:text>…</p:text></p:cm>`.
/// Modern:  `<p188:cm authorId="{guid}"><p188:txBody><a:t>…</a:t></p188:txBody></p188:cm>`.
/// Matching on local names covers both without hard-coding either prefix.
fn parse_comments(xml: &[u8], authors: &BTreeMap<String, String>) -> Vec<Comment> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out: Vec<Comment> = Vec::new();

    let mut in_comment = false;
    let mut author_id: Option<String> = None;
    let mut text = String::new();
    let mut capture = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"cm" => {
                    in_comment = true;
                    author_id = None;
                    text.clear();
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"authorId" {
                            author_id =
                                Some(String::from_utf8_lossy(&attr.value).into_owned());
                        }
                    }
                }
                // `text` is the classic body; `t` is a run inside a modern
                // `txBody`. Both only count inside a comment element.
                b"text" | b"t" if in_comment => capture = true,
                _ => {}
            },
            Ok(Event::Text(e)) if capture => {
                if let Ok(decoded) = e.xml_content() {
                    if !text.is_empty() && !text.ends_with(' ') {
                        text.push(' ');
                    }
                    text.push_str(decoded.trim());
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"text" | b"t" => capture = false,
                b"cm" if in_comment => {
                    in_comment = false;
                    let body = text.trim().to_string();
                    if !body.is_empty() {
                        out.push(Comment {
                            author: author_id
                                .as_ref()
                                .and_then(|id| authors.get(id).cloned()),
                            text: body,
                        });
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

// ── Rendering ────────────────────────────────────────────────────────────

/// Wrap on whitespace at `width`. `0` disables.
fn wrap(text: &str, width: usize) -> String {
    if width == 0 || text.chars().count() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut line = 0usize;
    for word in text.split_whitespace() {
        let w = word.chars().count();
        if line > 0 && line + 1 + w > width {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += w;
    }
    out
}

/// GitHub-Flavored Markdown, one `##` section per slide.
pub fn render_markdown(deck: &Deck, opts: &RenderOptions) -> String {
    let mut out = String::new();
    for slide in &deck.slides {
        out.push_str(&format!("## {}\n\n", slide.heading()));
        for para in slide.body_paragraphs() {
            out.push_str(&wrap(para, opts.wrap_width));
            out.push_str("\n\n");
        }
        if let Some(notes) = &slide.notes {
            out.push_str("### Notes\n\n");
            for line in notes.lines().filter(|l| !l.trim().is_empty()) {
                out.push_str(&wrap(line, opts.wrap_width));
                out.push_str("\n\n");
            }
        }
        if !slide.comments.is_empty() {
            out.push_str("### Comments\n\n");
            for c in &slide.comments {
                match &c.author {
                    Some(a) => out.push_str(&format!("- **{a}**: {}\n", c.text)),
                    None => out.push_str(&format!("- {}\n", c.text)),
                }
            }
            out.push('\n');
        }
    }
    out
}

/// Plain text — same structure, no markup.
pub fn render_text(deck: &Deck, opts: &RenderOptions) -> String {
    let mut out = String::new();
    for slide in &deck.slides {
        out.push_str(&slide.heading());
        out.push_str("\n\n");
        for para in slide.body_paragraphs() {
            out.push_str(&wrap(para, opts.wrap_width));
            out.push('\n');
        }
        if let Some(notes) = &slide.notes {
            out.push_str("\nNotes:\n");
            out.push_str(notes);
            out.push('\n');
        }
        for c in &slide.comments {
            match &c.author {
                Some(a) => out.push_str(&format!("\nComment ({a}): {}\n", c.text)),
                None => out.push_str(&format!("\nComment: {}\n", c.text)),
            }
        }
        out.push('\n');
    }
    out
}

/// Escape a string for an RTF body.
///
/// RTF is 7-bit: `\`, `{` and `}` are syntax, and anything above ASCII has to
/// go out as a signed 16-bit `\uN?` escape (the `?` is the fallback glyph for
/// readers that cannot handle the code point). Without this, a German umlaut
/// in a deck corrupts the rest of the document.
fn rtf_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '{' => out.push_str(r"\{"),
            '}' => out.push_str(r"\}"),
            '\n' => out.push_str("\\par\n"),
            c if (c as u32) < 128 => out.push(c),
            c => {
                // Astral-plane code points need a surrogate pair, which is
                // what `encode_utf16` yields; each unit goes out separately.
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{}?", *unit as i16));
                }
            }
        }
    }
    out
}

/// Rich Text Format, readable by Word, Pages and LibreOffice.
pub fn render_rtf(deck: &Deck, opts: &RenderOptions) -> String {
    let mut out = String::from(
        r"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss Helvetica;}}\fs24",
    );
    out.push('\n');
    for slide in &deck.slides {
        out.push_str(&format!(
            "\\pard\\sa180\\b\\fs32 {}\\b0\\fs24\\par\n",
            rtf_escape(&slide.heading())
        ));
        for para in slide.body_paragraphs() {
            out.push_str(&format!(
                "\\pard\\sa120 {}\\par\n",
                rtf_escape(&wrap(para, opts.wrap_width))
            ));
        }
        if let Some(notes) = &slide.notes {
            out.push_str("\\pard\\sa120\\i Notes\\i0\\par\n");
            out.push_str(&format!("\\pard\\sa120 {}\\par\n", rtf_escape(notes)));
        }
        for c in &slide.comments {
            let line = match &c.author {
                Some(a) => format!("Comment ({a}): {}", c.text),
                None => format!("Comment: {}", c.text),
            };
            out.push_str(&format!("\\pard\\sa120\\i {}\\i0\\par\n", rtf_escape(&line)));
        }
    }
    out.push('}');
    out
}

/// Word document, one Heading1 per slide.
///
/// Built directly rather than through `export::export_to_docx` because that
/// helper splits a flat string on blank lines and cannot mark headings — the
/// per-slide structure is the entire point here.
#[cfg(feature = "desktop")]
pub fn write_docx(deck: &Deck, opts: &RenderOptions, out_path: &Path) -> Result<()> {
    use docx_rs::*;

    // `Docx::new()` ships a styles part containing only `Normal`, so a bare
    // `.style("Heading1")` is a dangling reference: Word falls back to body
    // text and the slide titles never reach the navigation pane or a table
    // of contents. Defining the style is what makes them real headings.
    // (`export::export_to_docx` has the same dangling reference — separate
    // fix, same cause.)
    let mut docx = Docx::new().add_style(
        Style::new("Heading1", StyleType::Paragraph)
            .name("heading 1")
            .based_on("Normal")
            .next("Normal")
            .bold()
            .size(32)
            .outline_lvl(0)
            .q_format(true),
    );
    for slide in &deck.slides {
        docx = docx.add_paragraph(
            Paragraph::new()
                .add_run(Run::new().add_text(slide.heading()).bold())
                .style("Heading1"),
        );
        for para in slide.body_paragraphs() {
            docx = docx.add_paragraph(
                Paragraph::new().add_run(Run::new().add_text(wrap(para, opts.wrap_width))),
            );
        }
        if let Some(notes) = &slide.notes {
            docx = docx.add_paragraph(
                Paragraph::new().add_run(Run::new().add_text("Notes").italic()),
            );
            for line in notes.lines().filter(|l| !l.trim().is_empty()) {
                docx = docx
                    .add_paragraph(Paragraph::new().add_run(Run::new().add_text(line)));
            }
        }
        for c in &slide.comments {
            let line = match &c.author {
                Some(a) => format!("Comment ({a}): {}", c.text),
                None => format!("Comment: {}", c.text),
            };
            docx = docx
                .add_paragraph(Paragraph::new().add_run(Run::new().add_text(line).italic()));
        }
    }

    let file = std::fs::File::create(out_path)
        .with_context(|| format!("creating {}", out_path.display()))?;
    docx.build()
        .pack(file)
        .with_context(|| format!("writing DOCX to {}", out_path.display()))?;
    Ok(())
}

#[cfg(not(feature = "desktop"))]
pub fn write_docx(_deck: &Deck, _opts: &RenderOptions, _out_path: &Path) -> Result<()> {
    Err(anyhow::anyhow!(
        "DOCX output needs the `desktop` feature (docx-rs)"
    ))
}

// ── Extractor entry point ────────────────────────────────────────────────

/// Read a `.pptx` for the index: Markdown body plus per-slide headings.
pub fn extract(path: &Path) -> Result<super::ExtractedDocument> {
    let deck = read_deck(path, &ReadOptions::default())?;
    let headings = deck.slides.iter().map(Slide::heading).collect();
    Ok(super::ExtractedDocument {
        full_text: render_markdown(&deck, &RenderOptions::default()),
        headings,
        ext: "pptx".to_string(),
        ..Default::default()
    })
}

// ── Test support ─────────────────────────────────────────────────────────

/// Fixture builder shared with the CLI tests, which need a real deck to
/// prove `--engine` actually reroutes.
#[cfg(test)]
pub(crate) mod tests_support {
    use std::path::Path;

    /// Build a real PPTX package on disk.
    ///
    /// Deliberately adversarial: the presentation declares the slides in
    /// the reverse of their filename order, and the first slide's shapes
    /// are stored bottom-first. A reader that trusts filenames or XML
    /// order gets both wrong.
    /// OOXML namespace URIs. The fixture used placeholder prefixes at first
    /// (`xmlns:p="p"`), which this reader tolerates because it matches on
    /// local names — but a stricter parser rejects the package outright, so
    /// the fixture would only ever have exercised half the story.
    const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
    const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const NS_PKG: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

    pub(crate) fn write_sample_deck(dir: &Path) -> std::path::PathBuf {
        use std::io::Write;
        let path = dir.join("deck.pptx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let mut put = |name: &str, body: String| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        };

        put(
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
<Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
<Override PartName="/ppt/notesSlides/notesSlide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml"/>
</Types>"#
                .to_string(),
        );
        put(
            "_rels/.rels",
            format!(
                r#"<Relationships xmlns="{NS_PKG}"><Relationship Id="rId1" Type="{NS_R}/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#
            ),
        );

        // rId2 → slide2.xml FIRST, rId3 → slide1.xml second.
        put(
            "ppt/presentation.xml",
            format!(
                r#"<p:presentation xmlns:p="{NS_P}" xmlns:r="{NS_R}"><p:sldIdLst>
<p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/>
</p:sldIdLst></p:presentation>"#
            ),
        );
        put(
            "ppt/_rels/presentation.xml.rels",
            format!(
                r#"<Relationships xmlns="{NS_PKG}">
<Relationship Id="rId2" Type="{NS_R}/slide" Target="slides/slide2.xml"/>
<Relationship Id="rId3" Type="{NS_R}/slide" Target="slides/slide1.xml"/>
</Relationships>"#
            ),
        );

        // slide2.xml is presented FIRST; its shapes are stored bottom-first.
        put(
            "ppt/slides/slide2.xml",
            format!(
                r#"<p:sld xmlns:p="{NS_P}" xmlns:a="{NS_A}"><p:cSld><p:spTree>
<p:sp><p:spPr><a:xfrm><a:off x="1000" y="8000"/></a:xfrm></p:spPr>
  <p:txBody><a:p><a:r><a:t>BOTTOM stored first</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
  <p:spPr><a:xfrm><a:off x="1000" y="500"/></a:xfrm></p:spPr>
  <p:txBody><a:p><a:r><a:t>Erste Folie</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:spPr><a:xfrm><a:off x="1000" y="3000"/></a:xfrm></p:spPr>
  <p:txBody><a:p><a:r><a:t>MIDDLE stored last</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#
            ),
        );
        put(
            "ppt/slides/_rels/slide2.xml.rels",
            format!(
                r#"<Relationships xmlns="{NS_PKG}">
<Relationship Id="rId1" Type="{NS_R}/notesSlide" Target="../notesSlides/notesSlide1.xml"/>
<Relationship Id="rId2" Type="{NS_R}/comments" Target="../comments/comment1.xml"/>
</Relationships>"#
            ),
        );
        put(
            "ppt/notesSlides/notesSlide1.xml",
            format!(
                r#"<p:notes xmlns:p="{NS_P}" xmlns:a="{NS_A}"><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr>
  <p:txBody><a:p><a:r><a:t>1</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:txBody><a:p><a:r><a:t>Nicht zu schnell sprechen</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:notes>"#
            ),
        );
        put(
            "ppt/comments/comment1.xml",
            format!(
                r#"<p:cmLst xmlns:p="{NS_P}"><p:cm authorId="1" idx="1">
<p:text>Diese Folie kuerzen</p:text></p:cm></p:cmLst>"#
            ),
        );
        put(
            "ppt/commentAuthors.xml",
            format!(
                r#"<p:cmAuthorLst xmlns:p="{NS_P}"><p:cmAuthor id="1" name="Jana"/></p:cmAuthorLst>"#
            ),
        );

        // Presented SECOND despite the lower filename number.
        put(
            "ppt/slides/slide1.xml",
            format!(
                r#"<p:sld xmlns:p="{NS_P}" xmlns:a="{NS_A}"><p:cSld><p:spTree>
<p:sp><p:txBody><a:p><a:r><a:t>Zweite Folie ohne Titel</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:sld>"#
            ),
        );

        zip.finish().unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_targets_resolve_through_parent_hops() {
        assert_eq!(
            resolve_relative("ppt/slides/", "../notesSlides/notesSlide1.xml"),
            "ppt/notesSlides/notesSlide1.xml"
        );
        assert_eq!(resolve_relative("ppt/", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve_relative("ppt/slides/", "/ppt/x.xml"), "ppt/x.xml");
        assert_eq!(resolve_relative("ppt/slides/", "./slide2.xml"), "ppt/slides/slide2.xml");
    }

    #[test]
    fn slide_id_list_takes_the_relationship_id_not_the_slide_id() {
        // `<p:sldId id="256" r:id="rId2"/>` — picking the wrong attribute
        // yields "256", which resolves to no relationship at all.
        let xml = br#"<p:presentation xmlns:p="p" xmlns:r="r">
            <p:sldIdLst>
              <p:sldId id="256" r:id="rId2"/>
              <p:sldId id="257" r:id="rId3"/>
            </p:sldIdLst>
          </p:presentation>"#;
        assert_eq!(parse_slide_id_list(xml), vec!["rId2", "rId3"]);
    }

    #[test]
    fn relationships_parse_into_id_type_target() {
        let xml = br#"<Relationships>
            <Relationship Id="rId2" Type="http://x/notesSlide" Target="../notesSlides/n1.xml"/>
          </Relationships>"#;
        let map = parse_relationships(xml);
        let (ty, target) = map.get("rId2").unwrap();
        assert!(ty.ends_with("/notesSlide"));
        assert_eq!(target, "../notesSlides/n1.xml");
    }

    fn shape(y: i64, x: i64, text: &str) -> TextBlock {
        TextBlock {
            paragraphs: vec![text.to_string()],
            offset: Some((x, y)),
        }
    }

    #[test]
    fn visual_sort_is_top_to_bottom_then_left_to_right() {
        let mut blocks = vec![
            shape(5000, 1000, "bottom"),
            shape(1000, 9000, "top right"),
            shape(1000, 1000, "top left"),
        ];
        sort_visually(&mut blocks);
        let order: Vec<&str> = blocks
            .iter()
            .map(|b| b.paragraphs[0].as_str())
            .collect();
        assert_eq!(order, vec!["top left", "top right", "bottom"]);
    }

    #[test]
    fn unpositioned_shapes_keep_xml_order_and_go_last() {
        let mut blocks = vec![
            TextBlock { paragraphs: vec!["inherited a".into()], offset: None },
            shape(9000, 0, "positioned"),
            TextBlock { paragraphs: vec!["inherited b".into()], offset: None },
        ];
        sort_visually(&mut blocks);
        let order: Vec<&str> = blocks.iter().map(|b| b.paragraphs[0].as_str()).collect();
        assert_eq!(order, vec!["positioned", "inherited a", "inherited b"]);
    }

    #[test]
    fn slide_heading_falls_back_to_the_number() {
        let untitled = Slide { number: 3, ..Default::default() };
        assert_eq!(untitled.heading(), "Slide 3");
        let titled = Slide {
            number: 1,
            title: Some("  Intro  ".into()),
            ..Default::default()
        };
        assert_eq!(titled.heading(), "Slide 1: Intro");
        // A whitespace-only title must not produce "Slide 2: ".
        let blank = Slide { number: 2, title: Some("   ".into()), ..Default::default() };
        assert_eq!(blank.heading(), "Slide 2");
    }

    #[test]
    fn rtf_escapes_syntax_characters_and_non_ascii() {
        assert_eq!(rtf_escape(r"a\b{c}"), r"a\\b\{c\}");
        // ü = U+00FC = 252, which fits i16 unchanged.
        assert_eq!(rtf_escape("ü"), r"\u252?");
        // Beyond the BMP goes out as a surrogate pair, each as a signed unit.
        assert_eq!(rtf_escape("😀"), r"\u-10179?\u-8704?");
        assert_eq!(rtf_escape("a\nb"), "a\\par\nb");
    }

    #[test]
    fn wrap_breaks_on_whitespace_and_zero_disables() {
        assert_eq!(wrap("one two three", 0), "one two three");
        assert_eq!(wrap("one two three", 7), "one two\nthree");
        // A word longer than the width is not broken mid-word.
        assert_eq!(wrap("supercalifragilistic", 5), "supercalifragilistic");
    }

    #[test]
    fn comments_parse_from_the_classic_part() {
        let xml = br#"<p:cmLst xmlns:p="p">
            <p:cm authorId="1" idx="1"><p:text>Erste Anmerkung</p:text></p:cm>
            <p:cm authorId="2" idx="2"><p:text>Second note</p:text></p:cm>
          </p:cmLst>"#;
        let mut authors = BTreeMap::new();
        authors.insert("1".to_string(), "Jana".to_string());
        let comments = parse_comments(xml, &authors);
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author.as_deref(), Some("Jana"));
        assert_eq!(comments[0].text, "Erste Anmerkung");
        // Unknown author id → no name, but the comment still survives.
        assert_eq!(comments[1].author, None);
        assert_eq!(comments[1].text, "Second note");
    }

    #[test]
    fn comments_parse_from_the_modern_part() {
        // Newer PowerPoint writes this shape instead; handling only the
        // classic one loses every comment in a modern deck.
        let xml = br#"<p188:cmLst xmlns:p188="p188" xmlns:a="a">
            <p188:cm id="{GUID}" authorId="{AUTH}">
              <p188:txBody><a:p><a:r><a:t>Modern note</a:t></a:r></a:p></p188:txBody>
            </p188:cm>
          </p188:cmLst>"#;
        let mut authors = BTreeMap::new();
        authors.insert("{AUTH}".to_string(), "Kim".to_string());
        let comments = parse_comments(xml, &authors);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author.as_deref(), Some("Kim"));
        assert_eq!(comments[0].text, "Modern note");
    }

    #[test]
    fn shapes_parse_with_text_offsets_and_placeholder_roles() {
        let xml = br#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
              <p:spPr><a:xfrm><a:off x="100" y="100"/></a:xfrm></p:spPr>
              <p:txBody><a:p><a:r><a:t>Der Titel</a:t></a:r></a:p></p:txBody>
            </p:sp>
            <p:sp>
              <p:spPr><a:xfrm><a:off x="200" y="900"/></a:xfrm></p:spPr>
              <p:txBody>
                <a:p><a:r><a:t>Zeile eins</a:t></a:r><a:br/><a:r><a:t>Zeile zwei</a:t></a:r></a:p>
              </p:txBody>
            </p:sp>
            <p:sp>
              <p:nvSpPr><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr>
              <p:txBody><a:p><a:r><a:t>7</a:t></a:r></a:p></p:txBody>
            </p:sp>
          </p:spTree></p:cSld></p:sld>"#;

        let (title, blocks) = parse_slide_shapes(xml);
        assert_eq!(title.as_deref(), Some("Der Titel"));
        // The slide-number placeholder is furniture and must not appear.
        assert_eq!(blocks.len(), 1, "got {blocks:?}");
        assert_eq!(blocks[0].offset, Some((200, 900)));
        // `<a:br/>` splits the paragraph, preserving the line structure.
        assert_eq!(blocks[0].paragraphs, vec!["Zeile eins", "Zeile zwei"]);
    }

    use super::tests_support::write_sample_deck as write_test_deck;


    #[test]
    fn reads_a_real_package_in_presentation_order_with_notes_and_comments() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_test_deck(tmp.path());

        let deck = read_deck(&path, &ReadOptions::default()).expect("deck should read");
        assert_eq!(deck.slides.len(), 2);

        // Presentation order, not filename order: slide2.xml comes first.
        let first = &deck.slides[0];
        assert_eq!(first.title.as_deref(), Some("Erste Folie"));

        // Visual order, not XML order.
        assert_eq!(
            first.body_paragraphs(),
            vec!["MIDDLE stored last", "BOTTOM stored first"]
        );

        assert_eq!(first.notes.as_deref(), Some("Nicht zu schnell sprechen"));
        assert_eq!(first.comments.len(), 1);
        assert_eq!(first.comments[0].author.as_deref(), Some("Jana"));
        assert_eq!(first.comments[0].text, "Diese Folie kuerzen");

        // An untitled slide still gets its own numbered heading.
        let second = &deck.slides[1];
        assert!(second.title.is_none());
        assert_eq!(second.heading(), "Slide 2");
        assert_eq!(second.body_paragraphs(), vec!["Zweite Folie ohne Titel"]);
    }

    #[test]
    fn xml_order_and_the_opt_outs_are_honoured() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_test_deck(tmp.path());

        let deck = read_deck(
            &path,
            &ReadOptions {
                include_notes: false,
                include_comments: false,
                order: ShapeOrder::Xml,
            },
        )
        .unwrap();

        let first = &deck.slides[0];
        assert_eq!(
            first.body_paragraphs(),
            vec!["BOTTOM stored first", "MIDDLE stored last"],
            "ShapeOrder::Xml must preserve document order"
        );
        assert!(first.notes.is_none());
        assert!(first.comments.is_empty());
    }

    #[test]
    fn extract_produces_a_heading_per_slide() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_test_deck(tmp.path());
        let doc = extract(&path).unwrap();
        assert_eq!(doc.ext, "pptx");
        assert_eq!(doc.headings, vec!["Slide 1: Erste Folie", "Slide 2"]);
        assert!(doc.full_text.contains("Nicht zu schnell sprechen"));
        assert!(doc.full_text.contains("Jana"));
    }

    #[test]
    fn a_package_with_no_slides_is_an_error_not_an_empty_deck() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("empty.pptx");
        let file = std::fs::File::create(&path).unwrap();
        zip::ZipWriter::new(file).finish().unwrap();
        let err = read_deck(&path, &ReadOptions::default()).unwrap_err().to_string();
        assert!(err.contains("no slides"), "got: {err}");
    }

    #[test]
    fn rendered_markdown_carries_slides_notes_and_comments() {
        let deck = Deck {
            slides: vec![Slide {
                number: 1,
                title: Some("Intro".into()),
                blocks: vec![shape(0, 0, "Body line")],
                notes: Some("Remember to breathe".into()),
                comments: vec![Comment {
                    author: Some("Jana".into()),
                    text: "tighten this".into(),
                }],
            }],
        };
        let md = render_markdown(&deck, &RenderOptions::default());
        assert!(md.contains("## Slide 1: Intro"), "{md}");
        assert!(md.contains("Body line"), "{md}");
        assert!(md.contains("### Notes"), "{md}");
        assert!(md.contains("Remember to breathe"), "{md}");
        assert!(md.contains("- **Jana**: tighten this"), "{md}");
    }
}
