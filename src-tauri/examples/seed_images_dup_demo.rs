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
    eprintln!("seeding LanceDB under {}", data_dir.display());

    let local = Arc::new(LocalIndex::open_or_create(&data_dir, 1024).await?);

    let l1 = |doc: &str, name: &str, ext: &str, hash: &str, ts: i64| DocumentChunk {
        id:                chunk_row_id(doc, -1),
        doc_id:            doc.to_owned(),
        location_uri:      format!("crisp+local://demo//demo/{name}"),
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
        parent_dir:        Some("/demo".to_owned()),
        volume_id:         None,
    };

    let chunks = vec![
        // Cluster A — three byte-identical JPEGs.
        l1("img-1", "sunset.jpg",       "jpg", "shared-hash-A", 1_700_000_001_000),
        l1("img-2", "sunset_copy.jpg",  "jpg", "shared-hash-A", 1_700_000_002_000),
        l1("img-3", "sunset_again.jpg", "jpg", "shared-hash-A", 1_700_000_003_000),
        // Cluster B — two byte-identical JPEGs.
        l1("img-4", "lake.jpeg",        "jpeg","shared-hash-B", 1_700_000_004_000),
        l1("img-5", "lake_copy.jpeg",   "jpeg","shared-hash-B", 1_700_000_005_000),
        // Two unique singletons (must NOT appear in the dup view).
        l1("img-6", "alone.png",        "png", "uniq-1",        1_700_000_006_000),
        l1("img-7", "lonely.bmp",       "bmp", "uniq-2",        1_700_000_007_000),
        // Non-image row sharing hash-A — proves the IMAGE_EXTS filter
        // narrows BEFORE grouping (this PDF must NOT bump cluster A
        // to 4 entries).
        l1("doc-1", "sunset_print.pdf", "pdf", "shared-hash-A", 1_700_000_008_000),
    ];

    local.ingest_batch(&chunks).await?;
    eprintln!("seeded {} rows ({} image, 1 non-image)", chunks.len(), chunks.len() - 1);
    eprintln!(
        "now run: crispsorter --data-dir {} images duplicates -f text",
        data_dir.display()
    );

    Ok(())
}
