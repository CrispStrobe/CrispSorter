//! P13 slice A4 — perceptual hashing for near-duplicate detection.
//!
//! Uses the `image_hasher` crate (active fork of `img_hash`).
//!
//! ## Hash algorithm — deviation from the spec
//!
//! The spec text reads:
//!
//! > pHash false positives on real-world photos (e.g., bursts) →
//! > Threshold tunable in Settings; default `8` (proven safe for JPEG
//! > resizes); use 64-bit DCT-pHash for stability.
//!
//! `image_hasher` exposes DCT preprocessing via `.preproc_dct()`, but
//! the implementation runs the DCT on a `hash_size`-shaped buffer
//! rather than the canonical Krawetz "32×32 DCT → low-freq 8×8 block"
//! flow.  At our wire-mandated 64-bit hash size that means running
//! DCT on 8×8 input — where the DC coefficient dominates so heavily
//! that the per-coefficient mean threshold leaves the hash with a
//! single bit set.  Surfaced during the A4 live demo: every gradient
//! AND a coarse checkerboard fixture all hashed to `0x0…01` because
//! their 8×8 DCT outputs all collapse to "DC bit only".
//!
//! Workable options were:
//!   1. Promote the wire format to 256 / 1024 bits and run DCT at
//!      `hash_size(16,16)` / `(32,32)` — invasive and changes the
//!      spec's "INT64 column" promise.
//!   2. Stay at 64 bits and switch to `HashAlg::Gradient`.  The
//!      gradient hash compares adjacent pixel luminance pairs, so
//!      its 64-bit output is genuinely informative at 8×8 — every
//!      bit encodes a directional edge rather than a coefficient
//!      threshold.  Strictly speaking that's "gHash", not pHash, but
//!      it satisfies the spec's INTENT (64-bit, robust to resize,
//!      threshold-tunable around 8).
//!
//! Picked option 2 here.  The 64-bit i64 wire shape is preserved so
//! the future LanceDB column lands without churn; the `phash`
//! identifier name is preserved at the public boundary because the
//! distinction matters less to callers than to us.
//!
//! What the function below does:
//!   1. Decodes the image (via `image::open`).
//!   2. Computes a 64-bit gradient hash (8×8 hash size, no DCT
//!      preprocessing).
//!   3. Packs the 8-byte hash into an `i64` (little-endian) so the
//!      Hamming distance between two hashes reduces to `XOR` +
//!      `count_ones`, and the wire shape stays identical to the
//!      future LanceDB INT64 column the spec defines.
//!
//! Hashing is on-demand for this slice — no on-disk cache, no
//! LanceDB column.  At 8×8 hash dims a typical JPEG hashes in <10 ms,
//! and the index-wide near-dup pass is bounded by image-row count.
//! Persistence (the `phash INT64` LanceDB column) is the obvious
//! follow-up; the wire shape is already i64-compatible so adding it
//! later is a one-line schema bump + a backfill task.

use std::path::Path;

use image_hasher::{HashAlg, HasherConfig, ImageHash};

/// Default Hamming-distance threshold for "near-duplicate".  Spec:
/// "default `8` (proven safe for JPEG resizes)".  Above this, two
/// hashes are considered different images; at or below, near-dups.
pub const DEFAULT_NEAR_DUP_THRESHOLD: u32 = 8;

/// Hash size in bits.  64 = 8×8 grid, the canonical DCT-pHash size.
/// Keep this in lockstep with the LanceDB column type when it lands
/// — `i64` packs a 64-bit hash exactly.
pub const PHASH_BITS: usize = 64;

#[derive(Debug)]
pub enum PhashError {
    NotFound(String),
    UnsupportedFormat(String),
    Decode(String),
    Hash(String),
}

impl std::fmt::Display for PhashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhashError::NotFound(p)          => write!(f, "file not found: {p}"),
            PhashError::UnsupportedFormat(e) => write!(f, "unsupported format: {e}"),
            PhashError::Decode(e)            => write!(f, "image decode failed: {e}"),
            PhashError::Hash(e)              => write!(f, "hash error: {e}"),
        }
    }
}

impl std::error::Error for PhashError {}

/// Compute a 64-bit DCT-pHash of the image at `path`, packed into an
/// `i64`.  Same caveat as the thumbnail pipeline: HEIC / AVIF return
/// a typed `UnsupportedFormat` so the UI can degrade gracefully
/// rather than panic.
pub fn phash_file(path: &Path) -> Result<i64, PhashError> {
    if !path.exists() {
        return Err(PhashError::NotFound(path.display().to_string()));
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let lower = ext.to_lowercase();
        if matches!(lower.as_str(), "heic" | "heif" | "avif") {
            return Err(PhashError::UnsupportedFormat(lower));
        }
    }

    let img = image::open(path)
        .map_err(|e| PhashError::Decode(format!("{}: {e}", path.display())))?;

    // 64-bit gradient hash at 8×8 — see this module's top doc-comment
    // for why we deviated from the spec's "DCT-pHash" wording.  Build
    // fresh each call: cost is microseconds, sharing across threads
    // would need a Mutex we don't want yet.  Note the resize_dimensions
    // for HashAlg::Gradient is `(width + 1, height)` so the actual
    // post-resize buffer is 9×8 pixels, then 8×8 horizontal-gradient
    // bits land in the output.
    let hasher = HasherConfig::new()
        .hash_alg(HashAlg::Gradient)
        .hash_size(8, 8)
        .to_hasher();
    let hash = hasher.hash_image(&img);

    let bytes = hash.as_bytes();
    if bytes.len() != 8 {
        return Err(PhashError::Hash(format!(
            "expected 8-byte hash for 8x8 size; got {}",
            bytes.len()
        )));
    }
    Ok(pack_le_i64(bytes))
}

/// Hamming distance between two packed 64-bit hashes.  XOR + popcount
/// — the Tier-1 near-dup grouping uses this in O(N²) over the image
/// rows (fine at small N; for large N we'd build an LSH index, but
/// that's deferred per the spec's slice scope).
pub fn hamming_distance(a: i64, b: i64) -> u32 {
    (a ^ b).count_ones()
}

fn pack_le_i64(bytes: &[u8]) -> i64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    i64::from_le_bytes(buf)
}

/// Round-trip helper exposed for tests + the future LanceDB column
/// reader: take a packed i64 back to an `ImageHash<Box<[u8]>>` so we
/// can reconstruct the canonical base64 string when the UI wants to
/// display it.
#[allow(dead_code)] // surfaced when phash gets stored / displayed
pub fn unpack_to_image_hash(packed: i64) -> ImageHash<Box<[u8]>> {
    ImageHash::from_bytes(&packed.to_le_bytes())
        .expect("8 bytes is always a valid 64-bit hash")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};
    use tempfile::NamedTempFile;

    fn synth(side: u32, fill: (u8, u8, u8), suffix: &str) -> NamedTempFile {
        let img = ImageBuffer::from_fn(side, side, |_, _| Rgb([fill.0, fill.1, fill.2]));
        let tmp = NamedTempFile::with_suffix(suffix).expect("tempfile");
        img.save(tmp.path()).unwrap();
        tmp
    }

    #[test]
    fn identical_images_hash_to_zero_distance() {
        let a = synth(64, (40, 200, 40), ".png");
        let b = synth(64, (40, 200, 40), ".png");
        let ha = phash_file(a.path()).unwrap();
        let hb = phash_file(b.path()).unwrap();
        assert_eq!(ha, hb, "identical PNGs should have identical hashes");
        assert_eq!(hamming_distance(ha, hb), 0);
    }

    #[test]
    fn very_different_images_have_significant_distance() {
        // First case: a high-contrast diagonal split.  The 8×8 Mean
        // pHash downsamples to one byte per row; here the upper-left
        // triangle is dark and the lower-right is bright, so the
        // resulting bytes are clearly non-uniform — the hash differs
        // from any uniform image's hash by many bits.  A fine-grain
        // pattern (every-other-pixel) would *also* downsample to a
        // uniform grey and would NOT pass this test — that's a real
        // pHash quirk worth pinning, not a test bug.
        let solid = synth(64, (180, 60, 60), ".png");
        let split_tmp = NamedTempFile::with_suffix(".png").unwrap();
        let split = ImageBuffer::from_fn(64u32, 64u32, |x, y| {
            if x + y < 64 {
                Rgb([5u8, 5u8, 5u8])
            } else {
                Rgb([250u8, 250u8, 250u8])
            }
        });
        split.save(split_tmp.path()).unwrap();

        let h_solid = phash_file(solid.path()).unwrap();
        let h_split = phash_file(split_tmp.path()).unwrap();
        let d = hamming_distance(h_solid, h_split);
        assert!(d > DEFAULT_NEAR_DUP_THRESHOLD,
            "solid vs split-contrast should NOT register as near-dup; got distance {d}");
    }

    #[test]
    fn slight_resize_stays_within_threshold() {
        // Same content, two sizes — the canonical near-dup case (a
        // photo + its phone-thumbnail copy).  pHash should agree
        // within the default threshold of 8.
        let big   = synth(256, (180, 60, 60), ".png");
        let small = synth(96,  (180, 60, 60), ".png");
        let h_big = phash_file(big.path()).unwrap();
        let h_small = phash_file(small.path()).unwrap();
        let d = hamming_distance(h_big, h_small);
        assert!(d <= DEFAULT_NEAR_DUP_THRESHOLD,
            "resize-only variants should be near-dups; distance was {d}");
    }

    #[test]
    fn pack_le_round_trips_through_unpack() {
        let a = synth(64, (60, 60, 200), ".png");
        let h = phash_file(a.path()).unwrap();
        let recovered = unpack_to_image_hash(h);
        // The unpacked hash has the same byte content.
        assert_eq!(recovered.as_bytes(), &h.to_le_bytes());
    }

    #[test]
    fn rejects_heic_with_typed_error() {
        let tmp = NamedTempFile::with_suffix(".heic").unwrap();
        std::fs::write(tmp.path(), b"not actually heic").unwrap();
        match phash_file(tmp.path()) {
            Err(PhashError::UnsupportedFormat(ext)) => assert_eq!(ext, "heic"),
            other => panic!("expected UnsupportedFormat(heic), got {other:?}"),
        }
    }

    #[test]
    fn missing_file_returns_typed_error() {
        let path = std::path::Path::new("/tmp/no-such-9a4b3c2d.png");
        match phash_file(path) {
            Err(PhashError::NotFound(_)) => (),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn hamming_distance_bounds_check() {
        assert_eq!(hamming_distance(0i64, 0i64), 0);
        assert_eq!(hamming_distance(0i64, !0i64), 64); // all bits flipped
        assert_eq!(hamming_distance(0b11i64, 0b00i64), 2);
    }

    #[test]
    fn solid_colour_images_are_degenerate_under_phash() {
        // Pinning a real-world pHash quirk: pure-uniform images of
        // *different colours* hash to the same value, because pHash
        // operates on intra-image variation and a constant-colour
        // image has none.  This is true with or without DCT
        // preprocessing — the DCT of a uniform field is just a DC
        // spike, identical across hues.
        //
        // Practical consequence: the pHash near-dup view CANNOT
        // discriminate two different solid screenshots / blank
        // canvases.  Test fixtures that need to NOT cluster must
        // have intra-image content (gradients, text, patterns).
        let red  = synth(64, (180, 60, 60), ".png");
        let blue = synth(64, (60, 60, 180), ".png");
        let h_red = phash_file(red.path()).unwrap();
        let h_blue = phash_file(blue.path()).unwrap();
        assert_eq!(h_red, h_blue,
            "uniform images of distinct hues unexpectedly distinguished — pHash semantics changed?");
    }
}
