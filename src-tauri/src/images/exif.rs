//! P13 slice A2 — EXIF metadata reader.
//!
//! Surfaces the small set of tags the Bilder preview pane shows
//! (camera, date, GPS, dimensions, exposure).  Every field is
//! `Option<_>` so the wire payload deserialises cleanly even on
//! photos that strip half the EXIF (re-saves through a phone gallery,
//! Telegram, etc., commonly drop tags).
//!
//! GPS surfacing is intentional but flagged as PII: per the spec's
//! risk register, `gps_lat` / `gps_lon` must be stripped before any
//! `SyncManager` push.  The stripping happens at the *sync boundary*,
//! not here — this reader's job is "give the user what's in the file".
//!
//! No write path here — A2 is read-only.  Editing EXIF is delegated
//! to CrispLens (Tier 2) per the spec's "out of scope" list.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Curated EXIF subset returned to the frontend.  More fields can be
/// added as the preview pane gains rows; the wire shape is
/// `#[serde(default)]` everywhere so callers tolerate growth.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExifSummary {
    /// `EXIF.Make` — camera manufacturer (e.g. "Apple", "Canon").
    pub camera_make: Option<String>,
    /// `EXIF.Model` — body model (e.g. "iPhone 15 Pro", "EOS R6").
    pub camera_model: Option<String>,
    /// `EXIF.LensModel` if present.
    pub lens_model: Option<String>,
    /// `EXIF.DateTimeOriginal` reformatted as ISO-8601 `YYYY-MM-DDTHH:MM:SS`.
    /// EXIF stores it as `YYYY:MM:DD HH:MM:SS`; we swap colons for
    /// the JS / SQLite / Lance-friendly hyphens + `T`.
    pub taken_at: Option<String>,
    /// Same instant as a Unix-seconds integer.  Convenience for sort
    /// + numeric comparisons.  Naive parser (no timezone offsetting)
    /// because EXIF DateTimeOriginal has no zone semantics by spec —
    /// it's local time at the camera.  Frontend can render this in
    /// local-time without further conversion.
    pub taken_at_unix: Option<i64>,
    /// Pixel dimensions from the EXIF `PixelXDimension`/`YDimension`
    /// tags.  Falls back to `ImageWidth`/`ImageLength` (some old
    /// scanners only set those).
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `FNumber` (e.g. 2.8 for f/2.8).
    pub f_number: Option<f64>,
    /// `ExposureTime` as a "1/N" string when N >= 1, else decimal.
    /// Storing as a string sidesteps the rational-fraction round-trip
    /// — the UI only needs to display it.
    pub exposure_time: Option<String>,
    /// `PhotographicSensitivity` (ISO).
    pub iso: Option<u32>,
    /// `FocalLength` in mm (e.g. 35.0).
    pub focal_length_mm: Option<f64>,
    /// GPS latitude in signed decimal degrees (positive = N).
    /// Resolved from `GPSLatitude` (rational triplet d/m/s) +
    /// `GPSLatitudeRef`.  PII: strip before sync.
    pub gps_lat: Option<f64>,
    /// GPS longitude in signed decimal degrees (positive = E).
    /// PII: strip before sync.
    pub gps_lon: Option<f64>,
    /// `Orientation` raw EXIF value (1..=8).  The UI uses this to
    /// rotate the preview for tags 3/6/8 — phones almost always
    /// store landscape sensor data with an orientation flag rather
    /// than rotated pixels.
    pub orientation: Option<u32>,
}

impl ExifSummary {
    /// `true` when every meaningful field is `None` — UI uses this to
    /// decide whether to render "no EXIF available" copy vs. the table.
    pub fn is_empty(&self) -> bool {
        self.camera_make.is_none()
            && self.camera_model.is_none()
            && self.lens_model.is_none()
            && self.taken_at.is_none()
            && self.width.is_none()
            && self.height.is_none()
            && self.f_number.is_none()
            && self.exposure_time.is_none()
            && self.iso.is_none()
            && self.focal_length_mm.is_none()
            && self.gps_lat.is_none()
            && self.gps_lon.is_none()
            && self.orientation.is_none()
    }
}

/// Read the EXIF block from `path` and pull the curated fields.
///
/// Returns `Ok(ExifSummary::default())` (every field `None`) when the
/// file has no EXIF block — that's the common case for PNGs and
/// re-encoded JPEGs.  Hard errors only fire when the file can't be
/// opened at all.
pub fn read_exif(path: &Path) -> anyhow::Result<ExifSummary> {
    use exif::{In, Reader, Tag, Value};

    if !path.exists() {
        anyhow::bail!("file not found: {}", path.display());
    }

    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))?;
    let mut buf = std::io::BufReader::new(&file);

    // `continue_on_error(true)` is essential for real-world EXIF
    // tolerance.  Strict mode rejects the IFD chain shape some EXIF
    // writers (notably piexif and a handful of phone vendors) emit:
    // an `Unexpected next IFD` error fires the moment a thumbnail
    // chain or GPS pointer references an offset that doesn't fit the
    // strict TIFF spec, even though every field upstream of the
    // problem parsed cleanly.  Permissive mode returns the parsed
    // fields anyway, wrapped in an `Err(PartialResult(...))` we
    // unwrap below — the strict failure becomes a successful read of
    // whatever was salvageable.
    let mut reader_owner = Reader::new();
    let reader = reader_owner.continue_on_error(true);

    let exif = match reader.read_from_container(&mut buf) {
        Ok(e) => e,
        Err(exif::Error::PartialResult(partial)) => {
            // The crate has wrapped the parsed fields + the per-tag
            // errors in a PartialResult.  We don't surface the errors
            // to the caller — A2 is "give the user what's in the
            // file"; partial data is exactly that.
            let (e, _errors) = partial.into_inner();
            e
        }
        // Genuinely no EXIF (PNG, image with the APP1 stripped,
        // truly unparseable container).  Empty summary, not an error.
        Err(_) => return Ok(ExifSummary::default()),
    };

    let mut out = ExifSummary::default();

    // String fields
    out.camera_make  = ascii_field(&exif, Tag::Make);
    out.camera_model = ascii_field(&exif, Tag::Model);
    out.lens_model   = ascii_field(&exif, Tag::LensModel);

    // Date — reformat YYYY:MM:DD HH:MM:SS → YYYY-MM-DDTHH:MM:SS, plus
    // a unix-seconds integer for the UI to sort on.
    if let Some(raw) = ascii_field(&exif, Tag::DateTimeOriginal) {
        let iso = exif_datetime_to_iso(&raw);
        out.taken_at_unix = iso.as_deref().and_then(parse_iso_to_unix_seconds);
        out.taken_at = iso;
    }

    // Pixel dimensions — try EXIF tags first, fall back to the older
    // "ImageWidth/Length" tags some scanners use.
    out.width = u32_field(&exif, Tag::PixelXDimension)
        .or_else(|| u32_field(&exif, Tag::ImageWidth));
    out.height = u32_field(&exif, Tag::PixelYDimension)
        .or_else(|| u32_field(&exif, Tag::ImageLength));

    out.iso = u32_field(&exif, Tag::PhotographicSensitivity)
        .or_else(|| u32_field(&exif, Tag::ISOSpeed));
    out.orientation = u32_field(&exif, Tag::Orientation);

    out.f_number        = rational_to_f64(&exif, Tag::FNumber);
    out.focal_length_mm = rational_to_f64(&exif, Tag::FocalLength);

    // ExposureTime is a single rational; format as "1/N" when N >= 1.
    if let Some(field) = exif.get_field(Tag::ExposureTime, In::PRIMARY) {
        if let Value::Rational(ref vs) = field.value {
            if let Some(r) = vs.first() {
                out.exposure_time = Some(format_exposure(r.num, r.denom));
            }
        }
    }

    out.gps_lat = gps_decimal_degrees(&exif, Tag::GPSLatitude,  Tag::GPSLatitudeRef,  'S');
    out.gps_lon = gps_decimal_degrees(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, 'W');

    Ok(out)
}

// ── helpers ──────────────────────────────────────────────────────────────

fn ascii_field(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let f = exif.get_field(tag, exif::In::PRIMARY)?;
    if let exif::Value::Ascii(ref vs) = f.value {
        let bytes: Vec<u8> = vs.iter().flatten().copied().collect();
        let s = String::from_utf8_lossy(&bytes).trim().to_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        None
    }
}

fn u32_field(exif: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    let f = exif.get_field(tag, exif::In::PRIMARY)?;
    match f.value {
        exif::Value::Short(ref vs) => vs.first().copied().map(|v| v as u32),
        exif::Value::Long(ref vs)  => vs.first().copied(),
        _ => None,
    }
}

fn rational_to_f64(exif: &exif::Exif, tag: exif::Tag) -> Option<f64> {
    let f = exif.get_field(tag, exif::In::PRIMARY)?;
    if let exif::Value::Rational(ref vs) = f.value {
        let r = vs.first()?;
        if r.denom == 0 {
            None
        } else {
            Some(r.num as f64 / r.denom as f64)
        }
    } else {
        None
    }
}

/// EXIF stores `DateTimeOriginal` as `YYYY:MM:DD HH:MM:SS` — reformat
/// to ISO-8601 (`YYYY-MM-DDTHH:MM:SS`).  Returns `None` if the input
/// doesn't fit the canonical 19-char shape — a few cameras emit junk.
pub fn exif_datetime_to_iso(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.len() != 19 {
        return None;
    }
    // Bytes 0..10 are date "YYYY:MM:DD"; byte 10 is the space separator
    // mandated by the EXIF spec; 11..19 is time "HH:MM:SS".
    let date = &s[..10];
    let time = &s[11..19];
    // Strict check: positions 4/7 are date colons, position 10 is the
    // canonical separator (we reject T/whatever to keep callers honest
    // about the input — round-tripping ambiguous input would mask bugs
    // upstream of this function).
    let bytes = s.as_bytes();
    if bytes[4] != b':' || bytes[7] != b':' || bytes[10] != b' ' {
        return None;
    }
    let mut iso = String::with_capacity(19);
    iso.push_str(&date[..4]);
    iso.push('-');
    iso.push_str(&date[5..7]);
    iso.push('-');
    iso.push_str(&date[8..10]);
    iso.push('T');
    iso.push_str(time);
    Some(iso)
}

/// Parse an ISO-8601 naive datetime (`YYYY-MM-DDTHH:MM:SS`) to Unix
/// seconds.  Treats it as UTC because EXIF `DateTimeOriginal` has no
/// timezone — see the spec note in the struct doc above.
pub fn parse_iso_to_unix_seconds(iso: &str) -> Option<i64> {
    if iso.len() != 19 {
        return None;
    }
    let year: i64  = iso.get(0..4)?.parse().ok()?;
    let month: u32 = iso.get(5..7)?.parse().ok()?;
    let day: u32   = iso.get(8..10)?.parse().ok()?;
    let hour: u32  = iso.get(11..13)?.parse().ok()?;
    let min:  u32  = iso.get(14..16)?.parse().ok()?;
    let sec:  u32  = iso.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Days from 1970-01-01 to (year, month, day) using Howard Hinnant's
    // civil_from_days inverse — copied because we don't pull `chrono`
    // for one date conversion.  Returns `i64` so pre-1970 dates work
    // (some scanned slides have 1960s timestamps).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as i64;
    let m = month as i64;
    let d = day as i64;
    let doy = (153 * if m > 2 { m - 3 } else { m + 9 } + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe - 719468;

    let secs = days_since_epoch * 86_400
        + hour as i64 * 3_600
        + min  as i64 * 60
        + sec  as i64;
    Some(secs)
}

fn format_exposure(num: u32, denom: u32) -> String {
    if denom == 0 {
        return "?".to_owned();
    }
    let v = num as f64 / denom as f64;
    if v >= 1.0 {
        // "0.5"-ish slow shutters — show as decimal seconds.
        format!("{v:.2}s")
    } else {
        // "1/N" form — divide both sides by num so 5/1000 → 1/200.
        if num == 0 {
            "0".to_owned()
        } else {
            let n = (denom as f64 / num as f64).round() as u32;
            format!("1/{n}")
        }
    }
}

/// Resolve a GPS lat or lon from the EXIF `(triplet, ref)` pair.
/// `negative_ref_letter` is `'S'` for latitude and `'W'` for longitude
/// — the EXIF ref tag is a single ASCII char ("N"/"S" or "E"/"W").
fn gps_decimal_degrees(
    exif: &exif::Exif,
    coord_tag: exif::Tag,
    ref_tag: exif::Tag,
    negative_ref_letter: char,
) -> Option<f64> {
    let coord_field = exif.get_field(coord_tag, exif::In::PRIMARY)?;
    let triplet = if let exif::Value::Rational(ref vs) = coord_field.value {
        vs
    } else {
        return None;
    };
    if triplet.len() < 3 {
        return None;
    }
    let to_f64 = |r: &exif::Rational| if r.denom == 0 { None } else { Some(r.num as f64 / r.denom as f64) };
    let deg = to_f64(&triplet[0])?;
    let min = to_f64(&triplet[1])?;
    let sec = to_f64(&triplet[2])?;
    let mut decimal = deg + min / 60.0 + sec / 3600.0;

    // Sign from ref tag.
    let ref_letter = ascii_field(exif, ref_tag)?;
    if ref_letter
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase() == negative_ref_letter.to_ascii_uppercase())
        .unwrap_or(false)
    {
        decimal = -decimal;
    }
    Some(decimal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_datetime_to_iso_reformats_canonical_input() {
        assert_eq!(
            exif_datetime_to_iso("2024:03:15 14:22:09").as_deref(),
            Some("2024-03-15T14:22:09")
        );
    }

    #[test]
    fn exif_datetime_to_iso_rejects_malformed_inputs() {
        assert!(exif_datetime_to_iso("garbage").is_none());
        assert!(exif_datetime_to_iso("2024-03-15 14:22:09").is_none()); // wrong date sep
        assert!(exif_datetime_to_iso("").is_none());
        assert!(exif_datetime_to_iso("2024:03:15T14:22:09").is_none());  // missing space
    }

    #[test]
    fn parse_iso_to_unix_seconds_known_epochs() {
        // 1970-01-01T00:00:00 → 0
        assert_eq!(parse_iso_to_unix_seconds("1970-01-01T00:00:00"), Some(0));
        // 2020-01-01T00:00:00 → 1577836800
        assert_eq!(parse_iso_to_unix_seconds("2020-01-01T00:00:00"), Some(1_577_836_800));
        // Round-trip with the EXIF reformatter.
        let iso = exif_datetime_to_iso("2024:03:15 14:22:09").unwrap();
        let unix = parse_iso_to_unix_seconds(&iso).unwrap();
        // Sanity: 2024 → between 2020 (1.58e9) and 2030 (1.89e9) seconds.
        assert!(unix > 1_577_836_800);
        assert!(unix < 1_893_456_000);
    }

    #[test]
    fn parse_iso_handles_pre_1970_dates() {
        // Scanned-slide timestamp from the 1960s should round-trip
        // negative without panicking (legacy archives).
        let v = parse_iso_to_unix_seconds("1960-06-15T12:00:00").unwrap();
        assert!(v < 0, "pre-epoch should be negative, got {v}");
    }

    #[test]
    fn parse_iso_rejects_obvious_garbage() {
        assert!(parse_iso_to_unix_seconds("").is_none());
        assert!(parse_iso_to_unix_seconds("2024-13-15T00:00:00").is_none()); // month 13
        assert!(parse_iso_to_unix_seconds("2024-02-99T00:00:00").is_none()); // day 99
    }

    #[test]
    fn format_exposure_renders_phone_typical_shutter_speeds() {
        assert_eq!(format_exposure(1,    200), "1/200");
        assert_eq!(format_exposure(5,   1000), "1/200");   // 5/1000 = 1/200
        assert_eq!(format_exposure(1,    100), "1/100");
        assert_eq!(format_exposure(1,      1), "1.00s");    // hand-held cap
        assert_eq!(format_exposure(2,      1), "2.00s");    // long exposure
        assert_eq!(format_exposure(0,    100), "0");
        assert_eq!(format_exposure(1,      0), "?");        // div-by-zero guard
    }

    #[test]
    fn empty_summary_reports_empty() {
        assert!(ExifSummary::default().is_empty());
        let with_make = ExifSummary {
            camera_make: Some("Apple".to_owned()),
            ..Default::default()
        };
        assert!(!with_make.is_empty());
    }

    #[test]
    fn read_exif_returns_empty_for_files_without_exif_block() {
        // A bare PNG buffer has no EXIF.  read_exif must NOT error;
        // it returns ExifSummary::default().
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(8u32, 8u32, |_, _| Rgb([0u8, 0u8, 0u8]));
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        img.save(tmp.path()).unwrap();
        let s = read_exif(tmp.path()).unwrap();
        assert!(s.is_empty(), "bare PNG should yield empty EXIF, got {s:?}");
    }

    /// Fixture round-trip: load a JPEG synthesised by the piexif
    /// Python library and assert that *despite* kamadak-exif's strict
    /// reader rejecting the IFD chain shape, the permissive mode
    /// (`continue_on_error(true)`) still gives us every curated tag.
    ///
    /// This is a regression test for the bug found during P13 slice
    /// A2 live demo: piexif's `dump()` emits a `Next IFD` pointer
    /// that strict kamadak rejects with `InvalidFormat("Unexpected
    /// next IFD")`, returning `Err`.  The whole curated-summary
    /// returned `Default::default()` before the fix.  Many phone
    /// vendors emit similarly-shaped EXIF chains; the permissive
    /// branch is the production path, not just a niche tolerance.
    #[test]
    fn read_exif_recovers_full_summary_from_piexif_written_jpeg() {
        // Fixture bytes embedded at compile time; the file lives at
        // src-tauri/src/images/fixtures/exif_piexif_strict_failure.jpg
        // and was synthesised once via PIL + piexif (a 800×600 RGB
        // JPEG with Make=AcmePhone, Model=AcmePhone Pro Max,
        // DateTimeOriginal=2024-03-15 14:22:09, GPS 51°30'26" N,
        // 7°28'15" E, f/2.8, 1/200, ISO 400, 26mm).
        const FIXTURE: &[u8] =
            include_bytes!("fixtures/exif_piexif_strict_failure.jpg");

        let tmp = tempfile::NamedTempFile::with_suffix(".jpg").unwrap();
        std::fs::write(tmp.path(), FIXTURE).unwrap();

        let s = read_exif(tmp.path()).unwrap();
        assert_eq!(s.camera_make.as_deref(),  Some("AcmePhone"));
        assert_eq!(s.camera_model.as_deref(), Some("AcmePhone Pro Max"));
        assert_eq!(s.lens_model.as_deref(),   Some("Acme 26mm f/1.8"));
        assert_eq!(s.taken_at.as_deref(),     Some("2024-03-15T14:22:09"));
        assert_eq!(s.width,                   Some(800));
        assert_eq!(s.height,                  Some(600));
        assert_eq!(s.iso,                     Some(400));
        assert_eq!(s.orientation,             Some(1));
        // f/2.8 was written as 28/10.
        let fnum = s.f_number.expect("f_number missing");
        assert!((fnum - 2.8).abs() < 1e-6, "fNumber {fnum}");
        assert_eq!(s.exposure_time.as_deref(), Some("1/200"));
        let focal = s.focal_length_mm.expect("focal missing");
        assert!((focal - 26.0).abs() < 1e-6, "focal {focal}");
        // GPS: 51°30'26" N → 51.50722 (≈) ; 7°28'15" E → 7.47083 (≈).
        let lat = s.gps_lat.expect("gps_lat missing");
        let lon = s.gps_lon.expect("gps_lon missing");
        assert!((lat - 51.5_072_222).abs() < 1e-4, "lat {lat}");
        assert!((lon -  7.470_833_3).abs() < 1e-4, "lon {lon}");
        // Sanity on the unix-seconds derivation.
        let ts = s.taken_at_unix.expect("taken_at_unix");
        // 2024-03-15 14:22:09 UTC → 1710512529
        assert_eq!(ts, 1_710_512_529, "taken_at_unix {ts}");
    }
}
