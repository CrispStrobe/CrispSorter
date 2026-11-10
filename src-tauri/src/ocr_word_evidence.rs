//! Word evidence from an explicitly selected, installed Tesseract executable.
//!
//! This is independent of CrispEmbed's embedded Tesseract engine. One invocation
//! writes TSV and TXT; only exact, sequential matches against that same TXT can
//! receive word evidence. No region/token score becomes a character score.
//!
//! The caller owns image preparation and crop-to-page coordinate translation.
//! This module returns coordinates in the supplied single image's pixels.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

const TSV_HEADER: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";
const MAX_TSV_BYTES: usize = 32 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_WORDS: usize = 100_000;
const MAX_ALTERNATIVES_PER_WORD: usize = 50;
const MAX_ALTERNATIVE_UTF16: usize = 10_000;
const MAX_PAGE_PIXELS: u64 = 100_000_000;

/// Build permission only. Use `capabilities()` to report installed availability.
pub const SYSTEM_TESSERACT_AVAILABLE: bool = cfg!(all(
    feature = "sidecars",
    not(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))
));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NativeWordAlternative {
    pub text: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NativeWordEvidence {
    pub text: String,
    /// Half-open UTF-16 offsets within the containing region's original text.
    pub start: usize,
    pub end: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Native score normalized to 0..1, interpreted with its basis; not calibrated.
    pub confidence: Option<f32>,
    pub confidence_basis: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<Vec<NativeWordAlternative>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WordEvidenceEnvelope {
    pub version: u32,
    pub offset_unit: String,
    pub source: String,
    pub words: Vec<NativeWordEvidence>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemTesseractRegion {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub word_evidence: WordEvidenceEnvelope,
}

#[derive(Debug, Clone)]
pub struct SystemTesseractResult {
    pub width: u32,
    pub height: u32,
    pub full_text: String,
    pub regions: Vec<SystemTesseractRegion>,
    pub warnings: Vec<String>,
    pub diagnostics: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SystemTesseractCapabilities {
    pub available: bool,
    pub version: Option<String>,
    pub languages: Vec<String>,
    pub reason: Option<String>,
}

impl SystemTesseractCapabilities {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self { available: false, version: None, languages: Vec::new(), reason: Some(reason.into()) }
    }
}

/// Cached for the UI session. Restart after installing a runtime/language pack.
/// Recognition performs a fresh preflight so removing a pack cannot silently
/// reduce a multilingual request to the languages that still happen to load.
pub fn capabilities() -> SystemTesseractCapabilities {
    static CAPABILITIES: OnceLock<SystemTesseractCapabilities> = OnceLock::new();
    CAPABILITIES.get_or_init(probe).clone()
}

#[cfg(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))))]
fn probe() -> SystemTesseractCapabilities {
    system::probe()
}

#[cfg(not(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas")))))]
fn probe() -> SystemTesseractCapabilities {
    SystemTesseractCapabilities::unavailable("System Tesseract requires a desktop build with sidecars enabled; it is unavailable in sandboxed or mobile builds. The embedded OCR engines remain separate.")
}

#[derive(Debug)]
struct TsvWord {
    paragraph: (u32, u32),
    text: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    confidence: Option<f32>,
}

#[derive(Debug)]
struct Paragraph {
    key: (u32, u32),
    byte_start: usize,
    byte_end: usize,
    utf16_start: usize,
    utf16_end: usize,
    words: Vec<NativeWordEvidence>,
}

fn input_dimensions(width: u32, height: u32) -> Result<(), String> {
    // Integer geometry must also remain exact in the existing f32 region DTO.
    if width == 0 || height == 0 || width > 16_777_216 || height > 16_777_216
        || u64::from(width) * u64::from(height) > MAX_PAGE_PIXELS {
        return Err("System Tesseract requires a nonempty single page of at most 100 million pixels.".into());
    }
    Ok(())
}

/// Validate imported/exported evidence against its immutable original text.
/// Word boxes belong to source pixels, so only the page bounds apply here: a
/// later manual edit to the parent region must never rescale original boxes.
pub fn validate_word_evidence(envelope: &WordEvidenceEnvelope, text: &str, width: u32, height: u32) -> Result<(), String> {
    input_dimensions(width, height)?;
    if envelope.version != 1 || envelope.offset_unit != "utf16" || envelope.source.trim().is_empty()
        || envelope.source.encode_utf16().count() > 200 || envelope.source.contains('\0') {
        return Err("Unsupported OCR word evidence version, offset unit, or provider.".into());
    }
    if envelope.words.len() > MAX_WORDS || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
        return Err("OCR word evidence or its original text exceeds the supported limits.".into());
    }
    let valid_score = |score: Option<f32>| score.map_or(true, |v| v.is_finite() && (0.0..=1.0).contains(&v));
    let mut byte_cursor = 0;
    let mut utf16_cursor = 0;
    let mut previous_end = 0;
    let mut evidence_bytes = 0usize;
    for word in &envelope.words {
        if word.start < previous_end || word.end <= word.start || word.text.chars().all(char::is_whitespace) {
            return Err("OCR word evidence has an empty, unordered, or overlapping text span.".into());
        }
        let start = utf16_boundary(text, word.start, &mut byte_cursor, &mut utf16_cursor)?;
        let end = utf16_boundary(text, word.end, &mut byte_cursor, &mut utf16_cursor)?;
        if &text[start..end] != word.text {
            return Err("OCR word evidence does not match its original text exactly.".into());
        }
        previous_end = word.end;
        if ![word.x, word.y, word.w, word.h].iter().all(|v| v.is_finite())
            || word.x < 0.0 || word.y < 0.0 || word.w <= 0.0 || word.h <= 0.0
            || f64::from(word.x) + f64::from(word.w) > f64::from(width)
            || f64::from(word.y) + f64::from(word.h) > f64::from(height) {
            return Err("OCR word evidence has invalid geometry outside its source page.".into());
        }
        if !["recognition", "detection", "unknown"].contains(&word.confidence_basis.as_str()) || !valid_score(word.confidence) {
            return Err("OCR word confidence must have a known evidence basis and a finite score in 0..1 or unavailable.".into());
        }
        evidence_bytes = evidence_bytes.checked_add(word.text.len()).ok_or("OCR word evidence is too large.")?;
        if let Some(alternatives) = &word.alternatives {
            if alternatives.len() > MAX_ALTERNATIVES_PER_WORD {
                return Err("OCR word evidence contains too many alternatives.".into());
            }
            for alternative in alternatives {
                if alternative.text.chars().all(char::is_whitespace) || alternative.text.contains('\0')
                    || alternative.text.encode_utf16().count() > MAX_ALTERNATIVE_UTF16 || !valid_score(alternative.confidence) {
                    return Err("OCR word evidence contains an invalid alternative.".into());
                }
                evidence_bytes = evidence_bytes.checked_add(alternative.text.len()).ok_or("OCR word evidence is too large.")?;
            }
        }
        if evidence_bytes > MAX_TSV_BYTES { return Err("OCR word evidence exceeds the per-region size limit.".into()); }
    }
    Ok(())
}

/// Walk monotonically through UTF-8 once. An offset inside an astral character
/// is not a valid UTF-16 boundary even if JavaScript could slice its surrogate.
fn utf16_boundary(text: &str, target: usize, byte_cursor: &mut usize, utf16_cursor: &mut usize) -> Result<usize, String> {
    while *utf16_cursor < target {
        let character = text[*byte_cursor..].chars().next().ok_or("OCR word offset extends beyond its original text.")?;
        *byte_cursor += character.len_utf8();
        *utf16_cursor += character.len_utf16();
    }
    if *utf16_cursor != target {
        return Err("OCR word offset splits a UTF-16 surrogate pair or moves backwards.".into());
    }
    Ok(*byte_cursor)
}

fn integer(value: &str, row: usize, name: &str) -> Result<u32, String> {
    value.parse().map_err(|_| format!("Invalid Tesseract TSV {name} on row {row}."))
}

fn word_confidence(value: &str, row: usize) -> Result<Option<f32>, String> {
    let score: f32 = value.parse().map_err(|_| format!("Invalid Tesseract TSV confidence on row {row}."))?;
    if score == -1.0 { return Ok(None); }
    if !score.is_finite() || !(0.0..=100.0).contains(&score) {
        return Err(format!("Tesseract TSV word confidence is outside 0..100 on row {row}."));
    }
    Ok(Some(score / 100.0))
}

fn parse_words(tsv: &str, width: u32, height: u32) -> Result<Vec<TsvWord>, String> {
    let mut rows = tsv.lines();
    if rows.next().map(|line| line.trim_start_matches('\u{feff}')) != Some(TSV_HEADER) {
        return Err("Unsupported Tesseract TSV header; no word evidence was accepted.".into());
    }
    let mut saw_page = false;
    let mut ids = HashSet::new();
    let mut words = Vec::new();
    for (index, row) in rows.enumerate() {
        let row_number = index + 2;
        if row.is_empty() { continue; }
        // Text is the last column. Do not trim its punctuation or Unicode.
        let fields: Vec<_> = row.splitn(12, '\t').collect();
        if fields.len() != 12 {
            return Err(format!("Malformed Tesseract TSV row {row_number}; expected 12 columns."));
        }
        let level = integer(fields[0], row_number, "level")?;
        let page = integer(fields[1], row_number, "page number")?;
        let block = integer(fields[2], row_number, "block number")?;
        let paragraph = integer(fields[3], row_number, "paragraph number")?;
        let line = integer(fields[4], row_number, "line number")?;
        let word = integer(fields[5], row_number, "word number")?;
        if !(1..=5).contains(&level) || page != 1 {
            return Err(format!("Tesseract TSV must describe exactly one page (row {row_number})."));
        }
        let x = integer(fields[6], row_number, "left coordinate")?;
        let y = integer(fields[7], row_number, "top coordinate")?;
        let w = integer(fields[8], row_number, "width")?;
        let h = integer(fields[9], row_number, "height")?;
        if x.checked_add(w).filter(|right| *right <= width).is_none()
            || y.checked_add(h).filter(|bottom| *bottom <= height).is_none() {
            return Err(format!("Tesseract TSV geometry lies outside the supplied page on row {row_number}."));
        }
        if level == 1 {
            if saw_page || [block, paragraph, line, word, x, y] != [0; 6] || w != width || h != height {
                return Err("Tesseract TSV page dimensions or page identity do not match the supplied image.".into());
            }
            saw_page = true;
        } else if !saw_page {
            return Err("Tesseract TSV contains text before its page record.".into());
        }
        if level != 5 { continue; }
        if block == 0 || paragraph == 0 || line == 0 || word == 0 || w == 0 || h == 0 {
            return Err(format!("Tesseract TSV has an invalid word identity or empty box on row {row_number}."));
        }
        if !ids.insert((block, paragraph, line, word)) {
            return Err(format!("Tesseract TSV repeats a word identity on row {row_number}."));
        }
        let confidence = word_confidence(fields[10], row_number)?;
        if fields[11].chars().all(char::is_whitespace) { continue; }
        if words.len() >= MAX_WORDS { return Err("Tesseract returned too many words for one page.".into()); }
        words.push(TsvWord { paragraph: (block, paragraph), text: fields[11].into(), x, y, w, h, confidence });
    }
    if !saw_page { return Err("Tesseract TSV has no page record.".into()); }
    Ok(words)
}

/// Validate same-run TSV/TXT evidence without launching a process.
///
/// Paragraph text runs from its first word to its last word. Exact outer
/// separators remain in diagnostics; existing region exports choose their own
/// paragraph separators. A mismatch fails the entire result, never just a word.
pub fn parse_tesseract(tsv: &str, text: &str, width: u32, height: u32) -> Result<SystemTesseractResult, String> {
    input_dimensions(width, height)?;
    if tsv.len() > MAX_TSV_BYTES || text.len() > MAX_TEXT_BYTES {
        return Err("System Tesseract output exceeds the per-page evidence limit.".into());
    }
    if text.contains('\0') { return Err("Tesseract TXT contains an invalid NUL character.".into()); }
    let words = parse_words(tsv, width, height)?;
    let word_count = words.len();
    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut paragraph_ids = HashSet::new();
    let mut byte_cursor = 0;
    let mut utf16_cursor = 0;
    for word in words {
        for character in text[byte_cursor..].chars() {
            if !character.is_whitespace() { break; }
            byte_cursor += character.len_utf8();
            utf16_cursor += character.len_utf16();
        }
        if !text[byte_cursor..].starts_with(&word.text) {
            return Err("Tesseract TSV and TXT disagree; exact word alignment could not be verified. Existing reviewed text was not replaced.".into());
        }
        let byte_end = byte_cursor + word.text.len();
        let utf16_end = utf16_cursor + word.text.encode_utf16().count();
        if paragraphs.last().map(|p| p.key) != Some(word.paragraph) {
            if !paragraph_ids.insert(word.paragraph) {
                return Err("Tesseract TSV returned a noncontiguous paragraph; reading order could not be verified.".into());
            }
            paragraphs.push(Paragraph {
                key: word.paragraph, byte_start: byte_cursor, byte_end,
                utf16_start: utf16_cursor, utf16_end, words: Vec::new(),
            });
        }
        let paragraph = paragraphs.last_mut().expect("a paragraph was just selected");
        paragraph.byte_end = byte_end;
        paragraph.utf16_end = utf16_end;
        paragraph.words.push(NativeWordEvidence {
            text: word.text,
            start: utf16_cursor - paragraph.utf16_start,
            end: utf16_end - paragraph.utf16_start,
            x: word.x as f32, y: word.y as f32, w: word.w as f32, h: word.h as f32,
            confidence: word.confidence, confidence_basis: "recognition".into(), alternatives: None,
        });
        byte_cursor = byte_end;
        utf16_cursor = utf16_end;
    }
    if !text[byte_cursor..].chars().all(char::is_whitespace) {
        return Err("Tesseract TXT contains text missing from its TSV word evidence. Existing reviewed text was not replaced.".into());
    }
    let separators: Vec<_> = paragraphs.iter().enumerate().map(|(index, paragraph)| {
        let before = if index == 0 { 0 } else { paragraphs[index - 1].byte_end };
        let after = paragraphs.get(index + 1).map(|p| p.byte_start).unwrap_or(text.len());
        json!({"region_index":index, "block":paragraph.key.0, "paragraph":paragraph.key.1,
            "source_utf16_start":paragraph.utf16_start, "source_utf16_end":paragraph.utf16_end,
            "separator_before":&text[before..paragraph.byte_start], "separator_after":&text[paragraph.byte_end..after]})
    }).collect();
    let regions: Vec<_> = paragraphs.into_iter().map(|paragraph| {
        let x = paragraph.words.iter().map(|w| w.x).fold(f32::INFINITY, f32::min);
        let y = paragraph.words.iter().map(|w| w.y).fold(f32::INFINITY, f32::min);
        let right = paragraph.words.iter().map(|w| w.x + w.w).fold(0.0, f32::max);
        let bottom = paragraph.words.iter().map(|w| w.y + w.h).fold(0.0, f32::max);
        SystemTesseractRegion {
            text: text[paragraph.byte_start..paragraph.byte_end].into(), x, y, w: right - x, h: bottom - y,
            word_evidence: WordEvidenceEnvelope { version: 1, offset_unit: "utf16".into(), source: "tesseract_tsv".into(), words: paragraph.words },
        }
    }).collect();
    for region in &regions {
        validate_word_evidence(&region.word_evidence, &region.text, width, height)?;
    }
    Ok(SystemTesseractResult {
        width, height, full_text: text.into(), regions, warnings: Vec::new(),
        diagnostics: json!({
            "engine":"system_tesseract", "coordinate_space":"prepared_image_pixels",
            "confidence_source":"tesseract_tsv_word", "confidence_calibrated":false,
            "word_offset_unit":"utf16", "alignment_verified":true, "word_count":word_count,
            "full_text":text, "paragraph_separators":separators,
        }),
    })
}

fn language_code(code: &str) -> bool {
    let simple = |part: &str| !part.is_empty() && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
    if let Some(script) = code.strip_prefix("script/") { return simple(script); }
    simple(code) && !code.starts_with('-')
}

fn requested_language(language: Option<&str>, available: &[String]) -> Result<(String, bool), String> {
    let input = language.unwrap_or("auto").trim();
    let automatic = input.is_empty() || input.eq_ignore_ascii_case("auto");
    let input = if automatic { "eng" } else { input };
    if input.len() > 128 { return Err("The System Tesseract language list is too long.".into()); }
    let mut languages = Vec::new();
    for part in input.split('+') {
        let code = match part.to_ascii_lowercase().as_str() {
            "en" => "eng", "de" => "deu", "fr" => "fra", "es" => "spa", "it" => "ita",
            "pt" => "por", "nl" => "nld", "ru" => "rus", "ar" => "ara", "ja" => "jpn", "ko" => "kor",
            "zh" | "zh-cn" | "zh-hans" | "chi-sim" => "chi_sim",
            "zh-tw" | "zh-hant" | "chi-tra" => "chi_tra", _ => part,
        };
        if !language_code(code) || code == "osd" {
            return Err("Choose installed System Tesseract language codes such as eng or eng+deu.".into());
        }
        if !languages.iter().any(|existing| existing == code) { languages.push(code.to_owned()); }
    }
    let missing: Vec<_> = languages.iter().filter(|code| !available.contains(code)).cloned().collect();
    if !missing.is_empty() {
        return Err(format!("System Tesseract language pack(s) not installed: {}. Installed recognition languages: {}. Install the required packs and restart CrispSorter; the requested languages were not substituted.", missing.join(", "), available.join(", ")));
    }
    Ok((languages.join("+"), automatic))
}

fn check_loaded_languages(stderr: &str) -> Result<(), String> {
    // Tesseract can exit successfully after failing to load one of multiple
    // requested packs. --list-langs proves installation, not that a pack can
    // be decoded (or that it still exists when recognition begins).
    let diagnostic = stderr.to_ascii_lowercase();
    if diagnostic.contains("failed loading language") || diagnostic.contains("error opening data file") {
        return Err("System Tesseract could not load every required language pack. Repair/install the requested packs and retry; a partial-language result was not accepted.".into());
    }
    Ok(())
}

/// Recognize exactly this working image; never run embedded cleanup or fallback.
pub fn recognize(path: &Path, language: Option<&str>) -> Result<SystemTesseractResult, String> {
    #[cfg(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))))]
    { system::recognize(path, language) }
    #[cfg(not(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas")))))]
    {
        let _ = (path, language);
        Err(probe().reason.unwrap_or_else(|| "System Tesseract is unavailable in this build.".into()))
    }
}

#[cfg(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))))]
mod system {
    use super::*;
    use std::fs::{self, File};
    use std::io::{self, Read};
    use std::path::PathBuf;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::{Duration, Instant};

    const MAX_DIAGNOSTIC_BYTES: u64 = 1024 * 1024;
    const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
    const RECOGNITION_TIMEOUT: Duration = Duration::from_secs(120);

    struct RunningChild { child: Child, finished: bool }
    impl Drop for RunningChild {
        fn drop(&mut self) {
            if !self.finished {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    fn check_limits(paths: &[(&Path, u64)]) -> Result<(), String> {
        for (path, limit) in paths {
            match fs::metadata(path) {
                Ok(metadata) if metadata.len() > *limit => return Err("System Tesseract output exceeded its size limit; recognition was stopped.".into()),
                Ok(_) => {},
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(format!("Inspect System Tesseract output: {error}")),
            }
        }
        Ok(())
    }

    fn run(mut command: Command, stdout: &Path, stderr: &Path, outputs: &[(&Path, u64)], timeout: Duration) -> Result<ExitStatus, String> {
        let stdout_file = File::create(stdout).map_err(|e| format!("Create Tesseract output: {e}"))?;
        let stderr_file = File::create(stderr).map_err(|e| format!("Create Tesseract diagnostics: {e}"))?;
        command.stdin(Stdio::null()).stdout(Stdio::from(stdout_file)).stderr(Stdio::from(stderr_file));
        let child = command.spawn().map_err(|e| format!("Start System Tesseract: {e}. Install Tesseract 4 or newer on PATH and restart CrispSorter."))?;
        let mut running = RunningChild { child, finished: false };
        let started = Instant::now();
        let mut limits = vec![(stdout, MAX_DIAGNOSTIC_BYTES), (stderr, MAX_DIAGNOSTIC_BYTES)];
        limits.extend_from_slice(outputs);
        loop {
            check_limits(&limits)?;
            if let Some(status) = running.child.try_wait().map_err(|e| format!("Wait for System Tesseract: {e}"))? {
                running.finished = true;
                check_limits(&limits)?;
                return Ok(status);
            }
            if started.elapsed() >= timeout {
                return Err(format!("System Tesseract exceeded its {}-second time limit and was stopped. Try a smaller area or another engine.", timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn read_text(path: &Path, limit: usize) -> Result<String, String> {
        let file = File::open(path).map_err(|e| format!("Read Tesseract output {}: {e}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|e| format!("Read Tesseract output: {e}"))?;
        if bytes.len() > limit { return Err("System Tesseract output exceeded its size limit.".into()); }
        String::from_utf8(bytes).map_err(|_| "System Tesseract output is not valid UTF-8; no word evidence was accepted.".into())
    }

    fn diagnostics(path: &Path) -> String {
        read_text(path, MAX_DIAGNOSTIC_BYTES as usize).unwrap_or_default().chars().take(2000).collect::<String>().trim().to_owned()
    }

    fn probe_output(argument: &str) -> Result<(String, String), String> {
        let directory = tempfile::tempdir().map_err(|e| format!("Create Tesseract probe directory: {e}"))?;
        let stdout = directory.path().join("stdout");
        let stderr = directory.path().join("stderr");
        let mut command = Command::new("tesseract");
        command.arg(argument);
        let status = run(command, &stdout, &stderr, &[], PROBE_TIMEOUT)?;
        if !status.success() { return Err(format!("System Tesseract {argument} failed ({status}): {}", diagnostics(&stderr))); }
        Ok((read_text(&stdout, MAX_DIAGNOSTIC_BYTES as usize)?, read_text(&stderr, MAX_DIAGNOSTIC_BYTES as usize)?))
    }

    fn installed() -> Result<SystemTesseractCapabilities, String> {
        let (stdout, stderr) = probe_output("--version")?;
        let version = stdout.lines().chain(stderr.lines()).map(str::trim)
            .find(|line| line.starts_with("tesseract ")).ok_or("The executable on PATH did not identify itself as Tesseract.")?.to_owned();
        let major = version.split_whitespace().nth(1).and_then(|v| v.split('.').next()).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        if major < 4 { return Err("System Tesseract word evidence requires Tesseract 4 or newer.".into()); }
        let (stdout, stderr) = probe_output("--list-langs")?;
        let output = if stdout.contains("List of available languages") { &stdout } else { &stderr };
        let header = output.lines().position(|line| line.starts_with("List of available languages"))
            .ok_or("System Tesseract did not report its installed language packs.")?;
        let mut languages: Vec<_> = output.lines().skip(header + 1).map(str::trim)
            .filter(|code| language_code(code) && *code != "osd").map(str::to_owned).collect();
        languages.sort();
        languages.dedup();
        if languages.is_empty() { return Err("System Tesseract has no installed recognition language packs. Install a pack such as eng and restart CrispSorter.".into()); }
        Ok(SystemTesseractCapabilities { available: true, version: Some(version), languages, reason: None })
    }

    pub(super) fn probe() -> SystemTesseractCapabilities {
        installed().unwrap_or_else(SystemTesseractCapabilities::unavailable)
    }

    pub(super) fn recognize(path: &Path, language: Option<&str>) -> Result<SystemTesseractResult, String> {
        let installed = installed()?;
        let (language, automatic) = requested_language(language, &installed.languages)?;
        let path = path.canonicalize().map_err(|e| format!("Open System Tesseract page: {e}"))?;
        let (width, height) = image::image_dimensions(&path).map_err(|e| format!("Read System Tesseract page dimensions: {e}"))?;
        input_dimensions(width, height)?;
        let directory = tempfile::tempdir().map_err(|e| format!("Create Tesseract recognition directory: {e}"))?;
        let base = directory.path().join("recognition");
        let tsv: PathBuf = base.with_extension("tsv");
        let txt = base.with_extension("txt");
        let stdout = directory.path().join("stdout");
        let stderr = directory.path().join("stderr");
        let mut command = Command::new("tesseract");
        // Explicit parameters work with standalone traineddata directories as
        // well as system installations. Named config files are optional in
        // language-pack deployments and can be ignored with a zero exit code.
        command.arg(path).arg(&base).arg("-l").arg(&language).args([
            "--psm", "3", "-c", "tessedit_create_tsv=1", "-c", "tessedit_create_txt=1",
        ]);
        let started = Instant::now();
        let status = run(command, &stdout, &stderr, &[(&tsv, MAX_TSV_BYTES as u64), (&txt, MAX_TEXT_BYTES as u64)], RECOGNITION_TIMEOUT)?;
        if !status.success() { return Err(format!("System Tesseract recognition failed ({status}): {}", diagnostics(&stderr))); }
        let stderr_text = read_text(&stderr, MAX_DIAGNOSTIC_BYTES as usize)?;
        check_loaded_languages(&stderr_text)?;
        let tsv_text = read_text(&tsv, MAX_TSV_BYTES)?;
        let text = read_text(&txt, MAX_TEXT_BYTES)?;
        let mut result = parse_tesseract(&tsv_text, &text, width, height)?;
        result.diagnostics["engine_version"] = json!(installed.version);
        result.diagnostics["language"] = json!(language);
        result.diagnostics["page_segmentation_mode"] = json!(3);
        result.diagnostics["elapsed_ms"] = json!(started.elapsed().as_millis());
        result.diagnostics["stderr"] = json!(stderr_text.chars().take(2000).collect::<String>());
        result.diagnostics["internal_cleanup"] = json!(false);
        if automatic {
            result.warnings.push("System Tesseract used the installed English (eng) pack. It does not automatically detect the page language; select the installed language or language combination you need.".into());
        }
        Ok(result)
    }

    #[cfg(all(test, unix))]
    mod tests {
        use super::*;

        #[test]
        fn subprocess_timeout_returns_without_waiting_for_the_full_command() {
            let directory = tempfile::tempdir().unwrap();
            let stdout = directory.path().join("stdout");
            let stderr = directory.path().join("stderr");
            let mut command = Command::new("sh");
            // exec leaves one process for the same kill-and-reap boundary used
            // by Tesseract; there is no unowned shell child left running.
            command.args(["-c", "exec sleep 10"]);
            let started = Instant::now();
            let error = run(command, &stdout, &stderr, &[], Duration::from_millis(40)).unwrap_err();
            assert!(error.contains("time limit"));
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[test]
        fn completed_subprocess_cannot_bypass_an_output_limit() {
            let directory = tempfile::tempdir().unwrap();
            let stdout = directory.path().join("stdout");
            let stderr = directory.path().join("stderr");
            let mut command = Command::new("sh");
            command.args(["-c", "printf 0123456789"]);
            let error = run(command, &stdout, &stderr, &[(&stdout, 4)], PROBE_TIMEOUT).unwrap_err();
            assert!(error.contains("size limit"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rows: &str) -> String {
        format!("{TSV_HEADER}\n1\t1\t0\t0\t0\t0\t0\t0\t100\t100\t-1\t\n{rows}")
    }

    #[test]
    fn same_run_unicode_words_preserve_spacing_and_utf16_offsets() {
        let tsv = fixture(concat!(
            "5\t1\t1\t1\t1\t1\t5\t5\t20\t8\t96.5\tCafé\n",
            "5\t1\t1\t1\t1\t2\t30\t5\t8\t8\t0\t😀\n",
            "5\t1\t1\t1\t1\t3\t45\t5\t10\t8\t-1\té\n",
            "5\t1\t1\t1\t2\t1\t5\t20\t8\t8\t100\t中\n",
            "5\t1\t1\t1\t2\t2\t13\t20\t8\t8\t42\t文\n",
            "5\t1\t2\t1\t1\t1\t5\t40\t30\t8\t80\tAgain.\n",
        ));
        let text = "  Café\t😀 é\n中文\n\nAgain.\n";
        let result = parse_tesseract(&tsv, text, 100, 100).unwrap();
        assert_eq!(result.full_text, text);
        assert_eq!(result.regions.len(), 2);
        let first = &result.regions[0];
        assert_eq!(first.text, "Café\t😀 é\n中文");
        assert_eq!((first.x, first.y, first.w, first.h), (5.0, 5.0, 50.0, 23.0));
        let words = &first.word_evidence.words;
        assert_eq!(words.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(), [(0,4),(5,7),(8,10),(11,12),(12,13)]);
        assert_eq!(words[1].confidence, Some(0.0));
        assert_eq!(words[2].confidence, None);
        assert_eq!(words[3].confidence, Some(1.0));
        for word in words {
            let units: Vec<_> = first.text.encode_utf16().collect();
            assert_eq!(String::from_utf16(&units[word.start..word.end]).unwrap(), word.text);
            assert!(word.alternatives.is_none());
        }
        assert_eq!(result.diagnostics["paragraph_separators"][0]["separator_before"], "  ");
        assert_eq!(result.diagnostics["paragraph_separators"][0]["separator_after"], "\n\n");
        assert_eq!(result.diagnostics["paragraph_separators"][1]["separator_after"], "\n");
        let payload = serde_json::to_value(&first.word_evidence).unwrap();
        assert_eq!(payload["offset_unit"], "utf16");
        assert_eq!(payload["source"], "tesseract_tsv");
        assert_eq!(payload["words"][0]["confidence_basis"], "recognition");
        assert!(payload["words"][0].get("alternatives").is_none());
    }

    #[test]
    fn native_reading_order_does_not_sort_word_boxes_by_x() {
        let tsv = fixture("5\t1\t1\t1\t1\t1\t60\t5\t20\t8\t80\tשלום\n5\t1\t1\t1\t1\t2\t10\t5\t20\t8\t90\tעולם\n");
        let result = parse_tesseract(&tsv, "שלום עולם\n", 100, 100).unwrap();
        assert_eq!(result.regions[0].word_evidence.words[0].x, 60.0);
        assert_eq!(result.regions[0].word_evidence.words[1].text, "עולם");
    }

    #[test]
    fn disagreement_and_uncovered_text_fail_the_whole_result() {
        let tsv = fixture("5\t1\t1\t1\t1\t1\t5\t5\t20\t8\t90\tword\n");
        for text in ["ward", "prefix word", "word suffix", "word\u{200e}"] {
            assert!(parse_tesseract(&tsv, text, 100, 100).is_err(), "{text:?}");
        }
        assert!(parse_tesseract(&fixture(""), "unreported text", 100, 100).is_err());
    }

    #[test]
    fn blank_page_is_distinct_from_malformed_or_multi_page_output() {
        let result = parse_tesseract(&fixture(""), " \n", 100, 100).unwrap();
        assert!(result.regions.is_empty());
        assert_eq!(result.full_text, " \n");
        assert!(parse_tesseract(TSV_HEADER, "", 100, 100).is_err());
        assert!(parse_tesseract("not a TSV", "", 100, 100).is_err());
        assert!(parse_tesseract(&fixture("1\t2\t0\t0\t0\t0\t0\t0\t100\t100\t-1\t\n"), "", 100, 100).is_err());
        assert!(parse_tesseract(&fixture(""), "", 99, 100).is_err());
        assert!(parse_tesseract(&fixture(""), "\0", 100, 100).is_err());
    }

    #[test]
    fn malformed_confidence_and_geometry_never_become_trusted_evidence() {
        for confidence in ["NaN", "inf", "-2", "100.01", "n/a"] {
            let tsv = fixture(&format!("5\t1\t1\t1\t1\t1\t5\t5\t20\t8\t{confidence}\tword\n"));
            assert!(parse_tesseract(&tsv, "word", 100, 100).is_err(), "{confidence}");
        }
        for geometry in ["-1\t5\t20\t8", "95\t5\t20\t8", "5\t5\t0\t8", "5\t5\t20\t4294967295"] {
            let tsv = fixture(&format!("5\t1\t1\t1\t1\t1\t{geometry}\t90\tword\n"));
            assert!(parse_tesseract(&tsv, "word", 100, 100).is_err(), "{geometry}");
        }
    }

    #[test]
    fn duplicate_words_and_noncontiguous_paragraphs_are_rejected() {
        let row = "5\t1\t1\t1\t1\t1\t5\t5\t20\t8\t90\tword\n";
        assert!(parse_tesseract(&fixture(&format!("{row}{row}")), "word word", 100, 100).is_err());
        let tsv = fixture(&format!("{row}5\t1\t2\t1\t1\t1\t5\t30\t20\t8\t90\tother\n5\t1\t1\t1\t2\t1\t5\t50\t20\t8\t90\tword\n"));
        assert!(parse_tesseract(&tsv, "word\nother\nword", 100, 100).is_err());
    }

    #[test]
    fn multilingual_preflight_requires_every_requested_pack_and_preserves_order() {
        let installed = vec!["eng".into(), "deu".into(), "chi_sim".into()];
        assert_eq!(requested_language(Some("de+en"), &installed).unwrap(), ("deu+eng".into(), false));
        assert_eq!(requested_language(Some("zh-Hans"), &installed).unwrap().0, "chi_sim");
        assert_eq!(requested_language(None, &installed).unwrap(), ("eng".into(), true));
        assert!(requested_language(Some("eng+fra"), &installed).unwrap_err().contains("fra"));
        assert!(requested_language(Some("eng+../../model"), &installed).is_err());
        assert!(requested_language(Some("eng+"), &installed).is_err());
        assert!(requested_language(Some("osd"), &installed).is_err());
        assert!(requested_language(None, &["deu".into()]).is_err());
    }

    #[test]
    fn successful_partial_language_loading_is_not_accepted() {
        // A real Tesseract 5.5 `-l eng+zzzz_missing` run produced this message,
        // valid English TSV, and exit status 0. Success alone is insufficient.
        let stderr = "Error opening data file /tessdata/zzzz_missing.traineddata\nFailed loading language 'zzzz_missing'\nEstimating resolution as 503\n";
        assert!(check_loaded_languages(stderr).is_err());
        assert!(check_loaded_languages("Estimating resolution as 503\n").is_ok());
    }

    fn exported_evidence() -> WordEvidenceEnvelope {
        let tsv = fixture("5\t1\t1\t1\t1\t1\t5\t5\t10\t8\t90\t😀\n5\t1\t1\t1\t1\t2\t30\t5\t20\t8\t-1\tword\n");
        parse_tesseract(&tsv, "😀 word", 100, 100).unwrap().regions.remove(0).word_evidence
    }

    #[test]
    fn export_boundary_rejects_inexact_surrogate_or_overlapping_spans() {
        let original = exported_evidence();
        let round_trip: WordEvidenceEnvelope = serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert!(validate_word_evidence(&round_trip, "😀 word", 100, 100).is_ok());
        for (start, end) in [(1, 2), (0, 1), (0, 100)] {
            let mut evidence = original.clone();
            evidence.words[0].start = start;
            evidence.words[0].end = end;
            assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        }
        let mut evidence = original.clone();
        evidence.words.swap(0, 1);
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        assert!(validate_word_evidence(&original, "😀 Word", 100, 100).is_err());
    }

    #[test]
    fn export_boundary_preserves_source_boxes_but_rejects_invalid_evidence() {
        let mut evidence = exported_evidence();
        // A region may have been manually adjusted; source word boxes are not
        // rescaled or constrained to that new rectangle, only to this page.
        evidence.words[0].x = 90.0;
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_ok());
        evidence.words[0].x = 91.0;
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        for score in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            let mut evidence = exported_evidence();
            evidence.words[0].confidence = Some(score);
            assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        }
        let mut evidence = exported_evidence();
        evidence.words[0].confidence_basis = "detection".into();
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_ok());
        evidence.words[0].confidence_basis = "unknown".into();
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_ok());
        evidence.words[0].confidence_basis = "made-up".into();
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        evidence = exported_evidence();
        evidence.words[0].w = f32::NAN;
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
    }

    #[test]
    fn export_boundary_bounds_provider_metadata_and_alternatives() {
        let mut evidence = exported_evidence();
        evidence.source = "😀".repeat(100);
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_ok());
        evidence.source.push('x');
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        for source in ["", " \n", "provider\0"] {
            evidence.source = source.into();
            assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        }
        evidence = exported_evidence();
        evidence.version = 2;
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        evidence = exported_evidence();
        evidence.offset_unit = "unicode_codepoint".into();
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        evidence = exported_evidence();
        let alternative = NativeWordAlternative { text: "other".into(), confidence: None };
        evidence.words[0].alternatives = Some(vec![alternative.clone(); 50]);
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_ok());
        evidence.words[0].alternatives.as_mut().unwrap().push(alternative);
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        for text in [" ".into(), "bad\0text".into(), "😀".repeat(5001)] {
            evidence.words[0].alternatives = Some(vec![NativeWordAlternative { text, confidence: None }]);
            assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
        }
        evidence.words[0].alternatives = Some(vec![NativeWordAlternative { text: "other".into(), confidence: Some(f32::NAN) }]);
        assert!(validate_word_evidence(&evidence, "😀 word", 100, 100).is_err());
    }

    #[cfg(not(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas")))))]
    #[test]
    fn sandboxed_build_reports_unavailable_instead_of_spawning() {
        assert!(!SYSTEM_TESSERACT_AVAILABLE);
        assert!(!capabilities().available);
        assert!(recognize(Path::new("unused.png"), Some("eng")).unwrap_err().contains("sidecars"));
    }

    #[cfg(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))))]
    #[test]
    #[ignore = "requires installed Tesseract 4+ and the eng language pack; run explicitly as a native smoke gate"]
    fn system_tesseract_live_fixture_has_real_word_evidence() {
        let path = std::env::var_os("CRISPSORTER_OCR_FIXTURE").map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/quality/ocr-task.png"));
        let result = recognize(&path, Some("eng")).unwrap();
        assert!(result.full_text.contains("Cobalt"));
        assert!(result.full_text.contains("warranty"));
        assert!(result.full_text.contains("2031"));
        let words: Vec<_> = result.regions.iter().flat_map(|r| &r.word_evidence.words).collect();
        assert!(words.len() >= 5);
        assert!(words.iter().all(|w| w.w > 0.0 && w.h > 0.0));
        assert!(words.iter().any(|w| w.confidence.is_some_and(|c| c > 0.0 && c <= 1.0)));
        assert_eq!(result.diagnostics["alignment_verified"], true);
    }
}
