//! LanceDB schema migrations driven by the framework in
//! [`crate::migrations`].
//!
//! Index-specific migrations live here; framework-side mechanics
//! (ledger, ordering, gap/duplicate detection, idempotent reruns) is
//! in the parent module.  Convention: one struct per migration, all
//! exported via [`all()`] for the runner-registration site in
//! `index::tauri_commands::init_index`.
//!
//! ## Coexistence with the legacy ad-hoc migrations
//!
//! Two pre-existing column-add migrations live in
//! [`crate::index::local_index`]: `migrate_add_parent_dir_column`
//! and `migrate_add_volume_id_column`.  They predate the framework
//! and run inside `LocalIndex::open_or_create`; both are idempotent
//! (presence-check via `schema.field_with_name(...).is_ok()`) so
//! they're safe to leave as-is.  This module's migrations run AFTER
//! `LocalIndex::open_or_create` returns — sequencing is:
//!
//!   1. `LocalIndex::open_or_create` runs the legacy two.
//!   2. `MigrationRunner::run` (this module's registration set) runs
//!      every framework-tracked migration in version order.
//!
//! The legacy two would land at version 1 + 2 if they were
//! retrofitted into the framework; for clarity, this module starts
//! at version 100 to leave room for the legacy ones to be migrated
//! into the framework later without renumbering.

use anyhow::{anyhow, Context, Result};
use arrow_array::{Array as _, StringArray};
use async_trait::async_trait;
use std::sync::Arc;

use crate::migrations::{Migration, MigrationContext};

/// All migrations registered with the runner at startup.  Adding a
/// new migration means: write the struct here, add it to this
/// `Vec`, and pick the next free version number.
pub fn all() -> Vec<Box<dyn Migration>> {
    vec![
        Box::new(AddTextTranslatedColumns),
        Box::new(AddAudioMetadataColumns),
        Box::new(AddImageMetadataColumns),
        Box::new(RebuildFtsForBodyTranslated),
    ]
}

/// **v100** — Add the `text_translated` + `text_translated_lang`
/// Utf8 columns to the documents table.  Backfills existing rows
/// with nulls.  Idempotent: re-running on a schema that already has
/// both columns is a no-op.
///
/// First real consumer of the migration framework — exercises the
/// `lance.add_columns(NewColumnTransform::AllNulls(...), None)` path
/// the legacy ad-hoc migrations use, just plumbed through the
/// framework's version-ledger so we can grow more migrations
/// without bolting more ad-hoc functions onto `open_or_create`.
pub struct AddTextTranslatedColumns;

#[async_trait]
impl Migration for AddTextTranslatedColumns {
    fn version(&self) -> u32 {
        100
    }
    fn name(&self) -> &str {
        "add text_translated + text_translated_lang columns"
    }
    async fn apply(&self, ctx: &MigrationContext) -> Result<()> {
        let lance = ctx
            .lance
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(
                "v100 (add_text_translated_columns) needs the LanceDB \
                 handle in MigrationContext — caller didn't supply one"
            ))?;
        let table = lance.table_ref();
        let schema = table
            .schema()
            .await
            .context("reading LanceDB table schema for v100 migration")?;

        // Idempotency: if both columns are already present, exit early.
        let has_text_translated = schema.field_with_name("text_translated").is_ok();
        let has_lang = schema.field_with_name("text_translated_lang").is_ok();
        if has_text_translated && has_lang {
            eprintln!(
                "[index] v100 migration skipped — columns already present"
            );
            return Ok(());
        }

        // Build the field list to add.  Both columns are nullable
        // because the vast majority of rows won't have a translation
        // (translation is opt-in via `ExtractOptions::translate_to`).
        let mut fields_to_add = Vec::new();
        if !has_text_translated {
            fields_to_add.push(arrow_schema::Field::new(
                "text_translated",
                arrow_schema::DataType::Utf8,
                true,
            ));
        }
        if !has_lang {
            fields_to_add.push(arrow_schema::Field::new(
                "text_translated_lang",
                arrow_schema::DataType::Utf8,
                true,
            ));
        }
        let col_schema = Arc::new(arrow_schema::Schema::new(fields_to_add));
        table
            .add_columns(
                lancedb::table::NewColumnTransform::AllNulls(col_schema),
                None,
            )
            .await
            .context("adding text_translated columns (v100)")?;
        eprintln!(
            "[index] v100 migration applied — added text_translated + text_translated_lang"
        );
        Ok(())
    }
}

/// **v101** — Add the 5 audio L2 metadata columns:
/// `audio_duration_seconds` (Float64), `audio_codec` (Utf8),
/// `audio_sample_rate_hz` (Int32), `audio_channels` (Int32),
/// `audio_bitrate_kbps` (Int32).  All nullable — non-audio rows
/// leave them NULL.
///
/// Populated at ingest time from `ExtractedDocument.audio` (the
/// symphonia probe added in P13.6 Step 3a).  Backfills existing
/// rows with nulls.  Idempotent: re-running on a schema that
/// already has all five columns is a no-op.
pub struct AddAudioMetadataColumns;

#[async_trait]
impl Migration for AddAudioMetadataColumns {
    fn version(&self) -> u32 {
        101
    }
    fn name(&self) -> &str {
        "add audio_* L2 metadata columns"
    }
    async fn apply(&self, ctx: &MigrationContext) -> Result<()> {
        let lance = ctx
            .lance
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(
                "v101 (add_audio_metadata_columns) needs the LanceDB \
                 handle in MigrationContext — caller didn't supply one"
            ))?;
        let table = lance.table_ref();
        let schema = table
            .schema()
            .await
            .context("reading LanceDB table schema for v101 migration")?;

        // 5 columns; check each independently so partial-applied
        // migrations (interrupted by app crash mid-run) finish
        // cleanly on next start.
        type Pending = (&'static str, arrow_schema::DataType);
        let candidates: [Pending; 5] = [
            ("audio_duration_seconds", arrow_schema::DataType::Float64),
            ("audio_codec",             arrow_schema::DataType::Utf8),
            ("audio_sample_rate_hz",    arrow_schema::DataType::Int32),
            ("audio_channels",          arrow_schema::DataType::Int32),
            ("audio_bitrate_kbps",      arrow_schema::DataType::Int32),
        ];
        let fields_to_add: Vec<arrow_schema::Field> = candidates
            .into_iter()
            .filter(|(name, _)| schema.field_with_name(name).is_err())
            .map(|(name, ty)| arrow_schema::Field::new(name, ty, true))
            .collect();

        if fields_to_add.is_empty() {
            eprintln!(
                "[index] v101 migration skipped — all 5 audio_* columns already present"
            );
            return Ok(());
        }
        let added_names: Vec<&str> =
            fields_to_add.iter().map(|f| f.name().as_str()).collect();
        let col_schema = Arc::new(arrow_schema::Schema::new(fields_to_add.clone()));
        table
            .add_columns(
                lancedb::table::NewColumnTransform::AllNulls(col_schema),
                None,
            )
            .await
            .context("adding audio_* metadata columns (v101)")?;
        eprintln!(
            "[index] v101 migration applied — added columns: {:?}",
            added_names
        );
        Ok(())
    }
}

/// **v102** — Add the 5 image L2 (EXIF) metadata columns:
/// `image_camera_make` (Utf8), `image_camera_model` (Utf8),
/// `image_lens_model` (Utf8), `image_taken_at_unix` (Int64),
/// `image_iso` (Int32).  All nullable — non-image rows leave them
/// NULL.
///
/// Populated at ingest time from `ExtractedDocument.image_exif`
/// (curated subset of kamadak-exif tags).  Backfills existing
/// rows with nulls.  Idempotent: re-running on a schema that
/// already has all five columns is a no-op.  Same shape as the
/// v101 audio migration.
pub struct AddImageMetadataColumns;

#[async_trait]
impl Migration for AddImageMetadataColumns {
    fn version(&self) -> u32 {
        102
    }
    fn name(&self) -> &str {
        "add image_* L2 metadata columns"
    }
    async fn apply(&self, ctx: &MigrationContext) -> Result<()> {
        let lance = ctx
            .lance
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(
                "v102 (add_image_metadata_columns) needs the LanceDB \
                 handle in MigrationContext — caller didn't supply one"
            ))?;
        let table = lance.table_ref();
        let schema = table
            .schema()
            .await
            .context("reading LanceDB table schema for v102 migration")?;

        type Pending = (&'static str, arrow_schema::DataType);
        let candidates: [Pending; 5] = [
            ("image_camera_make",  arrow_schema::DataType::Utf8),
            ("image_camera_model", arrow_schema::DataType::Utf8),
            ("image_lens_model",   arrow_schema::DataType::Utf8),
            ("image_taken_at_unix", arrow_schema::DataType::Int64),
            ("image_iso",          arrow_schema::DataType::Int32),
        ];
        let fields_to_add: Vec<arrow_schema::Field> = candidates
            .into_iter()
            .filter(|(name, _)| schema.field_with_name(name).is_err())
            .map(|(name, ty)| arrow_schema::Field::new(name, ty, true))
            .collect();

        if fields_to_add.is_empty() {
            eprintln!(
                "[index] v102 migration skipped — all 5 image_* columns already present"
            );
            return Ok(());
        }
        let added_names: Vec<&str> =
            fields_to_add.iter().map(|f| f.name().as_str()).collect();
        let col_schema = Arc::new(arrow_schema::Schema::new(fields_to_add.clone()));
        table
            .add_columns(
                lancedb::table::NewColumnTransform::AllNulls(col_schema),
                None,
            )
            .await
            .context("adding image_* metadata columns (v102)")?;
        eprintln!(
            "[index] v102 migration applied — added columns: {:?}",
            added_names
        );
        Ok(())
    }
}

/// **v103** — Rebuild the Tantivy FTS index from LanceDB so that the
/// `body_translated` field is present in the on-disk schema.
///
/// Tantivy's schema is write-once: a field omitted at index creation can
/// never be added retroactively without deleting and recreating the index.
/// Fresh installations already get the full schema (including
/// `body_translated`) because `fts_index::build_schema()` has included it
/// since the `be73321` commit.  Existing users whose index predates that
/// commit get FTS-over-translated-body only after this migration rebuilds
/// their Tantivy index from the rows stored in LanceDB.
///
/// Safety model:
/// * The migration runs BEFORE `FtsIndex::open_or_create` is called by the
///   init path (the caller is responsible for this ordering — see Stage Y
///   reorder in `tauri_commands::init_index`).
/// * Idempotency: a `.v103_done` marker file is written as the last step
///   of a successful rebuild.  If the process is killed between "delete
///   old dir" and "commit writer", the next startup re-enters `apply`
///   (no ledger entry yet), finds no marker, and rebuilds from scratch.
///   The empty or partial FTS dir that may remain from the interrupted run
///   is handled: Tantivy recreates a fresh schema on an empty dir, and if
///   body_translated is already in that schema the migration writes the
///   marker and exits without touching LanceDB.
/// * LanceDB is unchanged — only the Tantivy `fts/` subtree is modified.
pub struct RebuildFtsForBodyTranslated;

#[async_trait]
impl Migration for RebuildFtsForBodyTranslated {
    fn version(&self) -> u32 { 103 }
    fn name(&self) -> &str { "rebuild fts for body_translated field" }

    async fn apply(&self, ctx: &MigrationContext) -> Result<()> {
        let fts_dir = ctx.data_dir.join("fts");
        let done_marker = fts_dir.join(".v103_done");

        // Fast path 1: marker from a previous (possibly un-ledgered) run.
        if done_marker.exists() {
            eprintln!("[index] v103 migration skipped — .v103_done marker present");
            return Ok(());
        }

        // Fast path 2: no FTS dir at all (fresh install, first ingest hasn't run yet).
        // The dir will be created with the new schema by open_or_create.
        if !fts_dir.exists() {
            eprintln!("[index] v103 migration skipped — no fts/ dir yet (fresh install)");
            return Ok(());
        }

        // Check whether the on-disk schema already has body_translated.
        // If yes, just write the marker and exit.  This handles the case
        // where the dir exists but the process was killed after building
        // the fresh dir but before writing the marker.
        {
            let fts_check = super::fts_index::FtsIndex::open_or_create(&fts_dir)
                .context("v103: opening fts dir for schema check")?;
            if fts_check.fields.body_translated.is_some() {
                eprintln!("[index] v103 migration skipped — body_translated already in FTS schema");
                let _ = std::fs::write(&done_marker, b"");
                return Ok(());
            }
        }

        let lance = ctx
            .lance
            .as_ref()
            .ok_or_else(|| anyhow!("v103 (rebuild_fts_for_body_translated) needs the LanceDB handle"))?;

        eprintln!("[index] v103 migration: rebuilding FTS index for body_translated …");

        // ── Stream FTS-relevant columns from LanceDB ──────────────────────
        let batches = lance
            .scan_for_fts_rebuild()
            .await
            .context("v103: scanning LanceDB for FTS rebuild")?;

        // ── Delete old Tantivy dir, create fresh ──────────────────────────
        std::fs::remove_dir_all(&fts_dir)
            .context("v103: removing old fts dir")?;

        let fts = super::fts_index::FtsIndex::open_or_create(&fts_dir)
            .context("v103: creating fresh fts dir")?;
        let mut writer = fts.writer().context("v103: opening fts writer")?;
        let mut count = 0usize;

        for batch in &batches {
            // Option<&StringArray> — Copy, so plain .map() works without .as_ref().
            let doc_id_col: Option<&StringArray> = col_str(batch, "doc_id");
            let owner_col:  Option<&StringArray> = col_str(batch, "owner_id");
            let lang_col:   Option<&StringArray> = col_str(batch, "language");
            let title_col:  Option<&StringArray> = col_str(batch, "title");
            let heads_col:  Option<&StringArray> = col_str(batch, "headings_text");
            let text_col:   Option<&StringArray> = col_str(batch, "full_text");
            let trans_col:  Option<&StringArray> = col_str(batch, "text_translated");
            let cidx_col: Option<&arrow_array::Int32Array> = batch
                .schema()
                .index_of("chunk_index")
                .ok()
                .and_then(|idx| batch.column(idx).as_any().downcast_ref());

            let (Some(doc_id_col), Some(owner_col)) = (doc_id_col, owner_col) else {
                continue;
            };

            for i in 0..batch.num_rows() {
                // Index only the first chunk per document.
                if let Some(c) = cidx_col {
                    if !c.is_null(i) && c.value(i) != 0 {
                        continue;
                    }
                }
                if doc_id_col.is_null(i) {
                    continue;
                }

                let doc_id = doc_id_col.value(i).to_owned();
                let owner  = str_val_or(owner_col, i, "");
                let lang   = lang_col.map(|c| str_val_or(c, i, "")).unwrap_or_default();
                let title  = title_col.map(|c| str_val_or(c, i, "")).unwrap_or_default();
                let heads  = heads_col.map(|c| str_val_or(c, i, "")).unwrap_or_default();
                let body   = text_col.map(|c| str_val_or(c, i, "")).unwrap_or_default();
                let body_translated = trans_col
                    .and_then(|c| if c.is_null(i) { None } else { Some(c.value(i)) });

                fts.add_document(
                    &mut writer,
                    super::fts_index::TantivyInput {
                        doc_id: &doc_id,
                        owner_id: &owner,
                        language: &lang,
                        title: &title,
                        headings: &heads,
                        body: &body,
                        body_translated,
                    },
                )
                .context("v103: writing fts doc")?;
                count += 1;
            }
        }

        writer.commit().context("v103: committing fts writer")?;

        // Write done marker — last thing so interruption mid-rebuild
        // leaves the marker absent and causes a retry on next startup.
        std::fs::write(&done_marker, b"").context("v103: writing done marker")?;

        eprintln!(
            "[index] v103 migration applied — rebuilt FTS with {count} docs (body_translated schema)"
        );
        Ok(())
    }
}

fn col_str<'a>(
    batch: &'a arrow_array::RecordBatch,
    name: &str,
) -> Option<&'a StringArray> {
    batch
        .schema()
        .index_of(name)
        .ok()
        .and_then(|i| batch.column(i).as_any().downcast_ref::<StringArray>())
}

fn str_val_or<'a>(col: &'a StringArray, i: usize, default: &'a str) -> &'a str {
    if col.is_null(i) { default } else { col.value(i) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::MigrationRunner;
    use crate::index::local_index::LocalIndex;
    use rusqlite::Connection;
    use std::sync::Mutex;

    /// Real end-to-end test: open a fresh in-tempdir LanceDB table,
    /// confirm the migration adds both columns, rerun, confirm it's
    /// a no-op.  This is the framework's first real consumer test —
    /// exercises both the runner mechanics AND the actual
    /// `add_columns` plumbing.
    #[tokio::test]
    async fn v100_adds_columns_and_is_idempotent() {
        // Note: the schema build in build_schema() ALREADY includes
        // the new columns (we added them in this commit), so a fresh
        // table comes up with the columns already present.  The
        // migration's idempotency check fires immediately — same
        // outcome as if the migration had run on an old table and
        // then been re-invoked.  That's exactly what we want to
        // verify: the migration is safe to re-run regardless of the
        // table's starting state.
        let tmp = tempfile::TempDir::new().unwrap();
        let local = Arc::new(
            LocalIndex::open_or_create(tmp.path(), 384)
                .await
                .expect("open LanceDB"),
        );

        let ledger = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let mut runner = MigrationRunner::new();
        runner.register_all(all()).unwrap();
        let ctx = MigrationContext {
            lance: Some(local.clone()),
            sqlite: None,
            data_dir: tmp.path().to_path_buf(),
        };
        let summary = runner.run(&ctx, &ledger).await.unwrap();
        // First run applies every registered migration in `all()` —
        // v100 (text_translated) + v101 (audio_*).  Pinning the full
        // list catches accidental migration drift (new version added
        // without updating tests) AND the in-order property the
        // framework guarantees.
        assert_eq!(
            summary.applied,
            vec![100, 101, 102, 103],
            "first run must apply every registered migration"
        );
        assert!(summary.skipped.is_empty());

        // Second run on the same ledger → versions in the ledger,
        // the runner short-circuits BEFORE calling apply() at all.
        // So this also verifies the framework's idempotency, not
        // just each migration's internal check.
        let summary2 = runner.run(&ctx, &ledger).await.unwrap();
        assert!(summary2.applied.is_empty(), "rerun must apply nothing");
        assert_eq!(summary2.skipped, vec![100, 101, 102, 103]);
    }

    #[tokio::test]
    async fn v100_errors_without_lance_handle() {
        // The migration declares "I need lance" via the
        // ctx.lance.ok_or_else(...) call.  A caller that constructs
        // a ctx without a Lance handle hits a clear error rather
        // than panicking.
        let mig = AddTextTranslatedColumns;
        let ctx = MigrationContext {
            lance: None,
            sqlite: None,
            data_dir: std::env::temp_dir(),
        };
        let err = mig.apply(&ctx).await.expect_err("must error");
        let msg = err.to_string();
        assert!(msg.contains("LanceDB handle"), "{msg}");
        assert!(msg.contains("v100"), "{msg}");
    }

    /// Pin v101's missing-handle error path the same way v100 has —
    /// catches a future refactor that silently swallows the
    /// "needs lance" guard.
    #[tokio::test]
    async fn v101_errors_without_lance_handle() {
        let mig = AddAudioMetadataColumns;
        let ctx = MigrationContext {
            lance: None,
            sqlite: None,
            data_dir: std::env::temp_dir(),
        };
        let err = mig.apply(&ctx).await.expect_err("must error");
        let msg = err.to_string();
        assert!(msg.contains("LanceDB handle"), "{msg}");
        assert!(msg.contains("v101"), "{msg}");
    }

    /// Sanity-pin the version + name surface so a future drift
    /// (e.g. someone reassigning version() to 102) trips immediately.
    #[test]
    fn v101_version_and_name_are_stable() {
        let mig = AddAudioMetadataColumns;
        assert_eq!(mig.version(), 101);
        assert!(mig.name().contains("audio"), "name = {:?}", mig.name());
    }

    /// Mirror the v101 guards for v102.
    #[tokio::test]
    async fn v102_errors_without_lance_handle() {
        let mig = AddImageMetadataColumns;
        let ctx = MigrationContext {
            lance: None,
            sqlite: None,
            data_dir: std::env::temp_dir(),
        };
        let err = mig.apply(&ctx).await.expect_err("must error");
        let msg = err.to_string();
        assert!(msg.contains("LanceDB handle"), "{msg}");
        assert!(msg.contains("v102"), "{msg}");
    }

    #[test]
    fn v102_version_and_name_are_stable() {
        let mig = AddImageMetadataColumns;
        assert_eq!(mig.version(), 102);
        assert!(mig.name().contains("image"), "name = {:?}", mig.name());
    }

    // ── v103 tests ────────────────────────────────────────────────────────

    #[test]
    fn v103_version_and_name_are_stable() {
        let mig = RebuildFtsForBodyTranslated;
        assert_eq!(mig.version(), 103);
        assert!(mig.name().contains("body_translated"), "name = {:?}", mig.name());
    }

    /// v103 skips when no fts/ dir exists (fresh install, first ingest
    /// not yet run).  The migration's "no fts dir" fast-path must be
    /// a no-op — NOT an error — so startup proceeds and Tantivy gets
    /// created with the full schema by open_or_create.
    #[tokio::test]
    async fn v103_skips_when_no_fts_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No fts/ dir created — simulates a fresh install.
        let mig = RebuildFtsForBodyTranslated;
        let ctx = MigrationContext {
            lance: None, // not needed for this fast path
            sqlite: None,
            data_dir: tmp.path().to_path_buf(),
        };
        mig.apply(&ctx).await.expect("must succeed (fast path)");
        // fts dir still doesn't exist.
        assert!(!tmp.path().join("fts").exists());
    }

    /// v103 skips when the fts/ dir already has the fresh schema
    /// (body_translated field present).  Simulates a new install where
    /// the first `open_or_create` already produced the full schema.
    #[tokio::test]
    async fn v103_skips_when_fresh_schema() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fts_dir = tmp.path().join("fts");

        // Create a fresh FtsIndex — build_schema() includes body_translated.
        let fts = super::super::fts_index::FtsIndex::open_or_create(&fts_dir).unwrap();
        assert!(fts.fields.body_translated.is_some(), "sanity: fresh schema must have body_translated");
        drop(fts);

        let mig = RebuildFtsForBodyTranslated;
        let ctx = MigrationContext {
            lance: None, // not needed — fast path fires first
            sqlite: None,
            data_dir: tmp.path().to_path_buf(),
        };
        mig.apply(&ctx).await.expect("must succeed (skip)");

        // Done marker written.
        assert!(tmp.path().join("fts/.v103_done").exists());
    }

    /// v103 with a fresh-schema FTS dir and a real LanceDB handle: the
    /// migration detects body_translated is already present, writes the
    /// marker, and returns Ok.  Covers the path where the lance handle IS
    /// provided (proving we don't panic on a Some(lance) context).
    #[tokio::test]
    async fn v103_skips_fresh_schema_with_lance_handle() {
        use crate::index::fts_index::{FtsIndex, TantivyInput};

        let tmp = tempfile::TempDir::new().unwrap();
        let fts_dir = tmp.path().join("fts");

        // Open a real LanceDB (empty — no rows).
        let local = Arc::new(
            LocalIndex::open_or_create(tmp.path(), 384)
                .await
                .expect("open LanceDB"),
        );

        // Create a fresh FTS dir with the new schema (body_translated present).
        let fts = FtsIndex::open_or_create(&fts_dir).unwrap();
        let mut writer = fts.writer().unwrap();
        fts.add_document(&mut writer, TantivyInput {
            doc_id: "doc001", owner_id: "owner", language: "en",
            title: "Hello", headings: "", body: "hello world", body_translated: None,
        }).unwrap();
        writer.commit().unwrap();
        assert!(fts.fields.body_translated.is_some(), "sanity: fresh schema has body_translated");
        drop(fts);

        // Run v103 — should detect fresh schema, write marker, skip rebuild.
        let mig = RebuildFtsForBodyTranslated;
        let ctx = MigrationContext {
            lance: Some(local.clone()),
            sqlite: None,
            data_dir: tmp.path().to_path_buf(),
        };
        mig.apply(&ctx).await.expect("v103 must succeed");

        // Marker must be present (idempotency signal).
        assert!(fts_dir.join(".v103_done").exists(), ".v103_done marker missing");

        // FTS still readable and schema intact.
        let fts_after = FtsIndex::open_or_create(&fts_dir).unwrap();
        assert!(fts_after.fields.body_translated.is_some(), "body_translated must remain in schema");
    }

    /// v103 done-marker acts as a permanent skip — re-running apply() when
    /// the marker exists returns Ok immediately without touching the FTS dir.
    #[tokio::test]
    async fn v103_done_marker_skips_on_rerun() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fts_dir = tmp.path().join("fts");
        std::fs::create_dir_all(&fts_dir).unwrap();
        // Write the marker without any FTS content.
        std::fs::write(fts_dir.join(".v103_done"), b"").unwrap();

        let mig = RebuildFtsForBodyTranslated;
        let ctx = MigrationContext {
            lance: None, // marker fast-path must not reach lance
            sqlite: None,
            data_dir: tmp.path().to_path_buf(),
        };
        mig.apply(&ctx).await.expect("marker fast-path must succeed without lance handle");
    }

}
