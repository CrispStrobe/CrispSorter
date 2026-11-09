//! Source checks that must run before tolerant image decoders or OCR fallbacks.
//!
//! `image` 0.25.10 deliberately disables strict JPEG decoding. A decoder may
//! return usable-looking pixels without an end marker, so decoding alone does
//! not establish that a transferred photo is complete. Parse marker boundaries
//! without rewriting the source or buffering its entropy data. Pixel validity
//! remains the image decoder's responsibility.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

fn invalid(detail: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!(
        "The JPEG source is incomplete or malformed ({detail}). Select a complete photo or export a new JPEG/PNG copy."
    ))
}

fn exact(reader: &mut impl Read, bytes: &mut [u8]) -> io::Result<()> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof { invalid("unexpected end of source") }
        else { error }
    })
}

fn byte(reader: &mut impl Read) -> io::Result<u8> {
    let mut value = [0];
    exact(reader, &mut value)?;
    Ok(value[0])
}

fn segment(reader: &mut impl Read) -> io::Result<()> {
    let mut length = [0; 2];
    exact(reader, &mut length)?;
    let length = usize::from(u16::from_be_bytes(length));
    if length < 2 { return Err(invalid("invalid marker length")); }
    // Consume the declared segment, including APP metadata, rather than looking
    // for an apparent end marker inside it. Never seek past an undetected EOF.
    let mut remaining = length - 2;
    let mut buffer = [0; 4096];
    while remaining != 0 {
        let count = remaining.min(buffer.len());
        exact(reader, &mut buffer[..count])?;
        remaining -= count;
    }
    Ok(())
}

fn marker(reader: &mut impl Read) -> io::Result<u8> {
    if byte(reader)? != 0xff { return Err(invalid("expected a JPEG marker")); }
    let mut value = byte(reader)?;
    while value == 0xff { value = byte(reader)?; }
    if value == 0 { return Err(invalid("stuffed byte outside an image scan")); }
    Ok(value)
}

fn scan_marker(reader: &mut impl BufRead) -> io::Result<u8> {
    loop {
        // Scan the current read buffer in place. Memory remains bounded even
        // for a large source containing no marker before its final bytes.
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() { return Err(invalid("missing JPEG end marker")); }
        let position = buffer.iter().position(|value| *value == 0xff);
        let consumed = position.map_or(buffer.len(), |offset| offset + 1);
        reader.consume(consumed);
        if position.is_none() { continue; }
        let mut value = byte(reader)?;
        while value == 0xff { value = byte(reader)?; }
        match value {
            0 | 0x01 | 0xd0..=0xd7 => continue, // Stuffing, TEM, and restart markers.
            0xdc => segment(reader)?, // DNL can interrupt and then resume a scan.
            _ => return Ok(value),
        }
    }
}

/// Validate JPEG structure by signature. Other formats retain their own decoder
/// validation. A complete primary JPEG may legally contain trailing bytes.
pub(crate) fn validate(mut reader: impl BufRead) -> io::Result<()> {
    let mut signature = [0; 2];
    match reader.read_exact(&mut signature) {
        Ok(()) if signature == [0xff, 0xd8] => {},
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(error) => return Err(error),
    }
    let mut current = marker(&mut reader)?;
    let mut saw_scan = false;
    loop {
        match current {
            0xd9 if saw_scan => return Ok(()),
            0xd9 => return Err(invalid("end marker before an image scan")),
            0 | 0xd8 | 0xd0..=0xd7 => return Err(invalid("unexpected JPEG marker")),
            0x01 => {},
            _ => {
                segment(&mut reader)?;
                if current == 0xda {
                    saw_scan = true;
                    current = scan_marker(&mut reader)?;
                    continue;
                }
            },
        }
        current = marker(&mut reader)?;
    }
}

pub(crate) fn validate_file(path: &Path) -> io::Result<()> {
    validate(BufReader::with_capacity(64 * 1024, File::open(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // A marker-grammar control, not a decodable image. Real JPEG pixel and
    // import controls live in the image-input contract and workspace tests.
    fn marker_stream() -> Vec<u8> {
        vec![0xff, 0xd8, 0xff, 0xef, 0, 4, 0xff, 0xd9,
             0xff, 0xda, 0, 2, 11, 0xff, 0, 0xd9, 0xff, 0xd0, 22,
             0xff, 0x01, 33, 0xff, 0xdc, 0, 4, 0, 1, 44,
             0xff, 0xc4, 0, 2, 0xff, 0xda, 0, 2, 55, 0xff, 0xff, 0xd9]
    }

    #[test]
    fn accepts_scan_stuffing_restarts_dnl_multiple_scans_and_trailing_bytes() {
        let mut bytes = marker_stream();
        validate(BufReader::with_capacity(1, Cursor::new(&bytes))).unwrap();
        bytes.extend_from_slice(b"trailing metadata");
        validate(Cursor::new(&bytes)).unwrap();
    }

    #[test]
    fn every_truncated_marker_stream_is_rejected_despite_metadata_end_marker() {
        let bytes = marker_stream();
        for length in 2..bytes.len() {
            assert!(validate(Cursor::new(&bytes[..length])).is_err(), "accepted prefix {length}");
        }
    }

    #[test]
    fn rejects_invalid_segment_lengths_and_end_markers_without_scans() {
        for bytes in [vec![0xff, 0xd8, 0xff, 0xef, 0, 1],
                      vec![0xff, 0xd8, 0xff, 0xef, 0xff, 0xff, 0xff, 0xd9],
                      vec![0xff, 0xd8, 0xff, 0xd9]] {
            assert!(validate(Cursor::new(bytes)).is_err());
        }
    }
}
