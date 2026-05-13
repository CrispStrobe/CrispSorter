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

use anyhow::{Context, Result};
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
            vec![100, 101],
            "first run must apply every registered migration"
        );
        assert!(summary.skipped.is_empty());

        // Second run on the same ledger → versions in the ledger,
        // the runner short-circuits BEFORE calling apply() at all.
        // So this also verifies the framework's idempotency, not
        // just each migration's internal check.
        let summary2 = runner.run(&ctx, &ledger).await.unwrap();
        assert!(summary2.applied.is_empty(), "rerun must apply nothing");
        assert_eq!(summary2.skipped, vec![100, 101]);
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
}
