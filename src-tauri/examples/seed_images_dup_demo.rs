//! P13/A3 live-demo seeder.  Writes a fresh LanceDB at
//! `--data-dir` populated with image-row L1 chunks where some files
//! deliberately share a `source_hash`, so `crispsorter images
//! duplicates` against the same data dir surfaces a non-trivial group
//! list.
//!
//! Not part of the production build — invoke via:
//!
//! ```
//! cargo run -p tauri-app --example seed_images_dup_demo -- /tmp/p13_a3_demo
//! crispsorter --data-dir /tmp/p13_a3_demo images duplicates -f text
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use image::{ImageBuffer, Rgb};
use tauri_app_lib::index::ingest::chunk_row_id;
use tauri_app_lib::index::local_index::LocalIndex;
use tauri_app_lib::index::schema::DocumentChunk;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/p13_a3_demo"));
    std::fs::create_dir_all(&data_dir)?;

    // P13/A4 — write real image files alongside the LanceDB rows so
    // `crispsorter images near-duplicates` can decode + hash them.
    // Files live in <data_dir>/_demo_images/; the L1 chunks below
    // point at those absolute paths so the URI→path resolver finds
    // them without any drive plumbing.
    let img_dir = data_dir.join("_demo_images");
    std::fs::create_dir_all(&img_dir)?;

    // Content-rich gradient: x-direction red ramp + y-direction
    // green ramp, blue constant.  Has real intra-image variation so
    // pHash has signal to work with.  Resizes preserve the gradient
    // shape → pHash-similar.
    let mut write_gradient = |name: &str, side: u32| -> anyhow::Result<PathBuf> {
        let p = img_dir.join(name);
        let img = ImageBuffer::from_fn(side, side, |x, y| {
            let r = (x as f32 / side as f32 * 255.0) as u8;
            let g = (y as f32 / side as f32 * 255.0) as u8;
            Rgb([r, g, 64u8])
        });
        img.save(&p)?;
        Ok(p)
    };
    // High-contrast diagonal split — orthogonal pHash to the gradient.
    let mut write_split = |name: &str| -> anyhow::Result<PathBuf> {
        let p = img_dir.join(name);
        let img = ImageBuffer::from_fn(128u32, 128u32, |x, y| {
            if x + y < 128 { Rgb([5u8, 5u8, 5u8]) } else { Rgb([250u8, 250u8, 250u8]) }
        });
        img.save(&p)?;
        Ok(p)
    };
    // Coarse 16-pixel checkerboard — strong horizontal AND vertical
    // frequency components that DCT-pHash picks up clearly.  Truly
    // distinct from any gradient (gradients have only DC + low
    // frequency; checkerboard concentrates energy in higher bins).
    let mut write_checker = |name: &str| -> anyhow::Result<PathBuf> {
        let p = img_dir.join(name);
        let img = ImageBuffer::from_fn(128u32, 128u32, |x, y| {
            if ((x / 16) + (y / 16)) % 2 == 0 {
                Rgb([20u8, 20u8, 20u8])
            } else {
                Rgb([240u8, 240u8, 240u8])
            }
        });
        img.save(&p)?;
        Ok(p)
    };

    // Three RESIZES of the same gradient — different pixel counts +
    // different SHA-256 bytes, but pHash should cluster them.
    let p_grad_a = write_gradient("gradient_a.jpg", 256)?;
    let p_grad_b = write_gradient("gradient_b.jpg", 128)?;
    let p_grad_c = write_gradient("gradient_c.jpg",  96)?;
    // Two distinct visual signatures — they must NOT join the cluster.
    let p_split   = write_split("split.png")?;
    let p_checker = write_checker("checker.png")?;
    eprintln!("wrote 5 demo image files under {}", img_dir.display());

    eprintln!("seeding LanceDB under {}", data_dir.display());
    let local = Arc::new(LocalIndex::open_or_create(&data_dir, 1024).await?);

    let l1 = |doc: &str, name: &str, ext: &str, hash: &str, ts: i64, abs_path: Option<&std::path::Path>| {
        let location_uri = match abs_path {
            Some(p) => p.to_string_lossy().into_owned(),
            None    => format!("crisp+local://demo//demo/{name}"),
        };
        DocumentChunk {
            id:                chunk_row_id(doc, -1),
            doc_id:            doc.to_owned(),
            location_uri,
            owner_id:          "demo".to_owned(),
            filename:          Some(name.to_owned()),
            title:             None,
            author:            None,
            year:              None,
            ext:               Some(ext.to_owned()),
            language:          None,
            page_count:        None,
            headings_text:     None,
            full_text:         None,
            full_text_md:      None,
            embedding:         None,
            embedding_sparse:  None,
            embedding_model:   None,
            chunk_index:       -1,
            chunk_total:       0,
            chunk_start_char:  None,
            chunk_end_char:    None,
            indexed_at:        ts,
            source_hash:       hash.to_owned(),
            tags:              vec![],
            metadata_json:     Some(format!(
                r#"{{"level":1,"fs_size":12345,"parent_dir":"/demo"}}"#
            )),
            parent_dir:             Some("/demo".to_owned()),
            volume_id:              None,
            text_translated:        None,
            text_translated_lang:   None,
            audio_duration_seconds: None,
            audio_codec:            None,
            audio_sample_rate_hz:   None,
            audio_channels:         None,
            audio_bitrate_kbps:     None,
            image_camera_make:      None,
            image_camera_model:     None,
            image_lens_model:       None,
            image_taken_at_unix:    None,
            image_iso:              None,
            multivec_packed:        None,
            multivec_n_tokens:      None,
        }
    };

    let chunks = vec![
        // SHA cluster A — three byte-identical JPEGs (location_uri
        // pointing at /tmp/sunset.jpg etc.; files don't exist on disk
        // — A3's `images duplicates` doesn't decode them, just groups
        // by source_hash).
        l1("img-1", "sunset.jpg",       "jpg", "shared-hash-A", 1_700_000_001_000, None),
        l1("img-2", "sunset_copy.jpg",  "jpg", "shared-hash-A", 1_700_000_002_000, None),
        l1("img-3", "sunset_again.jpg", "jpg", "shared-hash-A", 1_700_000_003_000, None),
        // SHA cluster B — two byte-identical JPEGs.
        l1("img-4", "lake.jpeg",        "jpeg","shared-hash-B", 1_700_000_004_000, None),
        l1("img-5", "lake_copy.jpeg",   "jpeg","shared-hash-B", 1_700_000_005_000, None),
        // Two unique singletons (must NOT appear in any dup view).
        l1("img-6", "alone.png",        "png", "uniq-1",        1_700_000_006_000, None),
        l1("img-7", "lonely.bmp",       "bmp", "uniq-2",        1_700_000_007_000, None),
        // Non-image row sharing hash-A — proves the IMAGE_EXTS filter
        // narrows BEFORE grouping (this PDF must NOT bump cluster A
        // to 4 entries in `images duplicates`).
        l1("doc-1", "sunset_print.pdf", "pdf", "shared-hash-A", 1_700_000_008_000, None),
        // P13/A4 — pHash near-dup fixtures.  Each row points at a
        // real file on disk; SHA hashes all distinct (so they're
        // singletons in `images duplicates`) but the three gradient
        // resizes should cluster in `images near-duplicates`.
        l1("ndup-grad-a",  "gradient_a.jpg", "jpg", "phash-uniq-grad-a",   1_700_000_010_000, Some(&p_grad_a)),
        l1("ndup-grad-b",  "gradient_b.jpg", "jpg", "phash-uniq-grad-b",   1_700_000_011_000, Some(&p_grad_b)),
        l1("ndup-grad-c",  "gradient_c.jpg", "jpg", "phash-uniq-grad-c",   1_700_000_012_000, Some(&p_grad_c)),
        l1("ndup-split",   "split.png",      "png", "phash-uniq-split",    1_700_000_013_000, Some(&p_split)),
        l1("ndup-checker", "checker.png",    "png", "phash-uniq-checker",  1_700_000_014_000, Some(&p_checker)),
    ];

    let n_image = chunks.iter().filter(|c| c.ext.as_deref() != Some("pdf")).count();
    local.ingest_batch(&chunks).await?;
    eprintln!("seeded {} rows ({} image, 1 non-image)", chunks.len(), n_image);
    eprintln!();
    eprintln!("try:");
    eprintln!("  crispsorter images --data-dir {} duplicates -f text", data_dir.display());
    eprintln!("  crispsorter images --data-dir {} near-duplicates -f text", data_dir.display());

    Ok(())
}
