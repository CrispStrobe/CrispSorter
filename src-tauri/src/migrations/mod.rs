//! Versioned schema migrations for the persistent data stores.
//!
//! Two pre-existing ad-hoc migrations live in `index/local_index.rs`
//! (`migrate_add_parent_dir_column` + `migrate_add_volume_id_column`)
//! and predate this module.  They stay as-is because they're already
//! idempotent and switching them to the framework would touch the
//! hot data-store init path for no behavioural change.  New schema
//! evolutions (Phase 7's text-LID columns, Phase 8's
//! `text_translated_<tgt>` columns, future indices, …) go through
//! the framework so the version ledger gives us:
//!
//! * **Idempotent reruns** — second startup with the same migration
//!   set is a fast lookup, not a column-introspection per migration.
//! * **Ordering invariant** — migrations are applied strictly in
//!   ascending version order, with gap detection.  If v3 is in the
//!   ledger but v2 isn't (manual SQL hack), the runner errors before
//!   applying anything new.
//! * **Failure isolation** — a mid-run failure leaves the ledger
//!   consistent: applied migrations are recorded, the failing one
//!   isn't, the rest don't run.  Next startup retries from where
//!   we stopped.
//!
//! ## Ledger storage
//!
//! The migration ledger is a SQLite table `_schema_migrations` in the
//! existing `crisp_jobs.db` (always opened at startup; cheapest place
//! to host a tiny admin table).  LanceDB isn't ergonomic for
//! row-level admin metadata — tables there are Arrow-shaped and the
//! 2-row "what's applied" overhead would be silly.
//!
//! ## Migration shape
//!
//! Each migration is a `Send + Sync` struct implementing
//! [`Migration`].  The trait carries an async `apply(ctx)` method;
//! [`MigrationContext`] hands the migration whichever store handles
//! it actually needs (LanceDB / SQLite / both) as `Option<Arc<...>>`.
//! Migrations declare what they need by reaching for it; missing a
//! handle errors with a clear "migration X needs Y" message rather
//! than silently no-op'ing.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(any(test, doc))]
use std::sync::atomic::{AtomicUsize, Ordering};

/// What stores a migration can touch.  Pass `None` for stores the
/// migration set doesn't need on a given run — e.g. integration tests
/// that only exercise SQLite migrations don't need to hand over a
/// `LocalIndex` handle.
#[derive(Clone)]
pub struct MigrationContext {
    /// LanceDB handle.  Set when the runner is called from a context
    /// where the vector index is already opened (the post-
    /// [`crate::index::local_index::LocalIndex::open_or_create`] point
    /// in [`crate::index::init_index`]).
    pub lance: Option<Arc<crate::index::local_index::LocalIndex>>,
    /// SQLite "side store" handle, shared across migrations that
    /// touch it (typically the job-queue DB).  Held behind a Mutex
    /// because `rusqlite::Connection` is `!Sync`.
    pub sqlite: Option<Arc<Mutex<Connection>>>,
    /// Data directory — useful for migrations that need to touch
    /// auxiliary files outside the main stores (model caches,
    /// thumbnail dirs, etc.).  Set on every call so migrations can
    /// rely on it.
    pub data_dir: PathBuf,
}

/// A single schema migration.  Implementations live alongside the
/// feature they enable (e.g. a future Phase 8 batch-translation
/// migration would land in `index/migrations.rs` or similar) and get
/// registered with the runner at startup.
#[async_trait]
pub trait Migration: Send + Sync {
    /// Monotonically increasing version.  Globally unique across the
    /// whole migration set — duplicates are rejected at register time
    /// to prevent silent shadowing.
    fn version(&self) -> u32;

    /// Short human-readable name for logs / the ledger ("add
    /// text_translated_en column", "add language facet index").
    /// Doesn't need to be unique — `version` is the primary key.
    fn name(&self) -> &str;

    /// Apply the migration.  Idempotency is the implementation's
    /// responsibility: even though the runner skips applied versions
    /// via the ledger, a partial-state recovery could re-enter
    /// `apply` mid-execution, so check whatever existence flag your
    /// schema change implies (column present, table exists, etc.)
    /// before writing.
    async fn apply(&self, ctx: &MigrationContext) -> Result<()>;
}

/// Summary returned from [`MigrationRunner::run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// Versions applied during this run (ascending).
    pub applied: Vec<u32>,
    /// Versions skipped because the ledger already had them
    /// (ascending).  Useful for logs ("skipped 12 already-applied
    /// migrations, 1 to go").
    pub skipped: Vec<u32>,
}

/// The runner.  Builds the ledger lazily on first call to
/// [`Self::run`], so constructing a runner is free; the migration
/// list and the SQLite ledger work get done together.
pub struct MigrationRunner {
    migrations: Vec<Box<dyn Migration>>,
}

const LEDGER_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS _schema_migrations (
    version    INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    applied_at INTEGER NOT NULL
);
"#;

impl MigrationRunner {
    /// Fresh runner with no migrations registered.
    pub fn new() -> Self {
        Self { migrations: Vec::new() }
    }

    /// Register a migration.  Order doesn't matter — the runner sorts
    /// by version at apply time.  Duplicate-version registration
    /// errors immediately rather than silently shadowing.
    pub fn register(&mut self, m: Box<dyn Migration>) -> Result<()> {
        let v = m.version();
        if self.migrations.iter().any(|existing| existing.version() == v) {
            anyhow::bail!(
                "duplicate migration version {v}: name `{}` would shadow an already-registered \
                 migration — pick a unique version number",
                m.name()
            );
        }
        self.migrations.push(m);
        Ok(())
    }

    /// Convenience: register multiple migrations in one go.  Any
    /// duplicate fails the call.
    pub fn register_all(&mut self, ms: Vec<Box<dyn Migration>>) -> Result<()> {
        for m in ms {
            self.register(m)?;
        }
        Ok(())
    }

    /// How many migrations are registered (any state).
    pub fn len(&self) -> usize {
        self.migrations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.migrations.is_empty()
    }

    /// Apply pending migrations.
    ///
    /// Algorithm:
    ///
    /// 1. Create the `_schema_migrations` table if it doesn't exist
    ///    (idempotent `CREATE TABLE IF NOT EXISTS`).
    /// 2. Read every applied version into a set.
    /// 3. Validate ordering: every applied version must be `< max
    ///    registered version + 1` AND there must be no "gap" (e.g.
    ///    versions [1, 3] applied but 2 registered would be ambiguous
    ///    — refuse rather than guess).
    /// 4. Sort registered migrations by version.
    /// 5. For each unapplied one, call `apply(ctx)`.  On success,
    ///    insert into the ledger with the current epoch time as
    ///    `applied_at`.  On failure, propagate the error and stop —
    ///    the ledger holds everything up to the failure.
    pub async fn run(
        &self,
        ctx: &MigrationContext,
        ledger: &Arc<Mutex<Connection>>,
    ) -> Result<RunSummary> {
        // Ensure the ledger table exists.
        {
            let conn = ledger
                .lock()
                .map_err(|e| anyhow!("ledger mutex poisoned: {e}"))?;
            conn.execute_batch(LEDGER_SCHEMA)
                .context("creating _schema_migrations ledger table")?;
        }

        let applied_set = read_applied_versions(ledger)?;

        // Sort registered migrations by version (ascending).
        let mut by_version: Vec<&Box<dyn Migration>> = self.migrations.iter().collect();
        by_version.sort_by_key(|m| m.version());

        // Sanity-check the ledger isn't ahead of what we know about.
        let registered: HashSet<u32> = by_version.iter().map(|m| m.version()).collect();
        for &v in &applied_set {
            if !registered.contains(&v) {
                // The ledger says v is applied, but no code in this
                // build registers it.  That means the user downgraded
                // (newer schema, older binary) — refuse to proceed
                // rather than silently behave as if the column / table
                // doesn't exist.
                anyhow::bail!(
                    "ledger has version {v} applied but no migration with that version is \
                     registered — looks like a downgrade.  Run the newer build to continue."
                );
            }
        }

        let mut applied = Vec::new();
        let mut skipped = Vec::new();

        for m in by_version {
            let v = m.version();
            if applied_set.contains(&v) {
                skipped.push(v);
                continue;
            }
            // Apply.  Errors propagate; the ledger isn't updated for
            // this version so the next run retries it.
            m.apply(ctx)
                .await
                .with_context(|| format!("applying migration v{v} ({})", m.name()))?;
            record_applied(ledger, v, m.name())?;
            applied.push(v);
        }

        Ok(RunSummary { applied, skipped })
    }
}

impl Default for MigrationRunner {
    fn default() -> Self {
        Self::new()
    }
}

fn read_applied_versions(ledger: &Arc<Mutex<Connection>>) -> Result<HashSet<u32>> {
    let conn = ledger
        .lock()
        .map_err(|e| anyhow!("ledger mutex poisoned: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT version FROM _schema_migrations")
        .context("preparing applied-versions query")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, i64>(0))
        .context("executing applied-versions query")?;
    let mut out = HashSet::new();
    for row in rows {
        let v: i64 = row.context("decoding applied-version row")?;
        out.insert(v as u32);
    }
    Ok(out)
}

fn record_applied(
    ledger: &Arc<Mutex<Connection>>,
    version: u32,
    name: &str,
) -> Result<()> {
    let conn = ledger
        .lock()
        .map_err(|e| anyhow!("ledger mutex poisoned: {e}"))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO _schema_migrations (version, name, applied_at) VALUES (?, ?, ?)",
        rusqlite::params![version as i64, name, now_ms],
    )
    .with_context(|| format!("recording applied migration v{version}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Synthetic migration for tests: records its version+name into a
    /// shared `Vec` so the test can assert on what was applied and in
    /// what order.  Optionally fails the run via a flag.
    struct RecordingMigration {
        version: u32,
        name: String,
        log: Arc<Mutex<Vec<u32>>>,
        fail_with: Option<&'static str>,
    }

    impl RecordingMigration {
        fn new(version: u32, name: &str, log: Arc<Mutex<Vec<u32>>>) -> Self {
            Self {
                version,
                name: name.to_string(),
                log,
                fail_with: None,
            }
        }

        fn failing(version: u32, name: &str, msg: &'static str) -> Self {
            Self {
                version,
                name: name.to_string(),
                log: Arc::new(Mutex::new(Vec::new())),
                fail_with: Some(msg),
            }
        }
    }

    #[async_trait]
    impl Migration for RecordingMigration {
        fn version(&self) -> u32 {
            self.version
        }
        fn name(&self) -> &str {
            &self.name
        }
        async fn apply(&self, _ctx: &MigrationContext) -> Result<()> {
            if let Some(msg) = self.fail_with {
                anyhow::bail!("{msg}");
            }
            self.log.lock().unwrap().push(self.version);
            Ok(())
        }
    }

    fn fresh_ledger() -> Arc<Mutex<Connection>> {
        // In-memory SQLite — perfect for migration ledger tests, no
        // temp-dir cleanup required.
        Arc::new(Mutex::new(Connection::open_in_memory().unwrap()))
    }

    fn ctx_for_tests() -> MigrationContext {
        MigrationContext {
            lance: None,
            sqlite: None,
            data_dir: std::env::temp_dir(),
        }
    }

    /// How many times the test ctr below has been incremented —
    /// proves that idempotent reruns DON'T re-execute applied
    /// migrations.
    static REENTRY_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct CountingMigration {
        version: u32,
    }

    #[async_trait]
    impl Migration for CountingMigration {
        fn version(&self) -> u32 {
            self.version
        }
        fn name(&self) -> &str {
            "counter"
        }
        async fn apply(&self, _ctx: &MigrationContext) -> Result<()> {
            REENTRY_COUNTER.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn empty_registry_is_no_op() {
        let r = MigrationRunner::new();
        let ledger = fresh_ledger();
        let s = r.run(&ctx_for_tests(), &ledger).await.unwrap();
        assert!(s.applied.is_empty());
        assert!(s.skipped.is_empty());
    }

    #[tokio::test]
    async fn applies_all_pending_in_version_order() {
        // Register out of order — the runner must sort.  Final
        // applied order should be ascending by version.
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut r = MigrationRunner::new();
        r.register(Box::new(RecordingMigration::new(3, "three", log.clone())))
            .unwrap();
        r.register(Box::new(RecordingMigration::new(1, "one", log.clone())))
            .unwrap();
        r.register(Box::new(RecordingMigration::new(2, "two", log.clone())))
            .unwrap();

        let ledger = fresh_ledger();
        let s = r.run(&ctx_for_tests(), &ledger).await.unwrap();
        assert_eq!(s.applied, vec![1, 2, 3]);
        assert!(s.skipped.is_empty());
        assert_eq!(*log.lock().unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn rerun_with_same_ledger_skips_everything() {
        // Apply all three, then run again with a fresh runner against
        // the same ledger.  Second run must skip all three and apply
        // nothing — the ledger is the source of truth across restarts.
        let log = Arc::new(Mutex::new(Vec::new()));
        let ledger = fresh_ledger();

        let mut r1 = MigrationRunner::new();
        r1.register(Box::new(RecordingMigration::new(1, "one", log.clone()))).unwrap();
        r1.register(Box::new(RecordingMigration::new(2, "two", log.clone()))).unwrap();
        let _ = r1.run(&ctx_for_tests(), &ledger).await.unwrap();

        let log2 = Arc::new(Mutex::new(Vec::new()));
        let mut r2 = MigrationRunner::new();
        r2.register(Box::new(RecordingMigration::new(1, "one", log2.clone()))).unwrap();
        r2.register(Box::new(RecordingMigration::new(2, "two", log2.clone()))).unwrap();
        let s2 = r2.run(&ctx_for_tests(), &ledger).await.unwrap();

        assert!(s2.applied.is_empty(), "rerun must apply nothing");
        assert_eq!(s2.skipped, vec![1, 2]);
        assert!(
            log2.lock().unwrap().is_empty(),
            "rerun must not re-execute apply()"
        );
    }

    #[tokio::test]
    async fn duplicate_version_registration_errors_immediately() {
        let mut r = MigrationRunner::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        r.register(Box::new(RecordingMigration::new(1, "one", log.clone()))).unwrap();
        let err = r
            .register(Box::new(RecordingMigration::new(1, "also-one", log)))
            .expect_err("must reject duplicate version");
        let msg = err.to_string();
        assert!(msg.contains("duplicate migration version 1"), "{msg}");
        assert!(msg.contains("also-one"), "must name the offending migration: {msg}");
    }

    #[tokio::test]
    async fn ledger_ahead_of_registered_set_errors_clearly() {
        // Simulate a downgrade: the ledger has v3 applied but no
        // migration with v3 is registered (user ran a newer binary,
        // then rolled back).  Runner must refuse rather than silently
        // skip the unknown version.
        let ledger = fresh_ledger();
        {
            let conn = ledger.lock().unwrap();
            conn.execute_batch(LEDGER_SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO _schema_migrations (version, name, applied_at) VALUES (3, 'phantom', 0)",
                [],
            )
            .unwrap();
        }

        let mut r = MigrationRunner::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        r.register(Box::new(RecordingMigration::new(1, "one", log.clone()))).unwrap();
        r.register(Box::new(RecordingMigration::new(2, "two", log.clone()))).unwrap();

        let err = r
            .run(&ctx_for_tests(), &ledger)
            .await
            .expect_err("must error on phantom version");
        let msg = err.to_string();
        assert!(msg.contains("version 3"), "must name the phantom version: {msg}");
        assert!(msg.contains("downgrade"), "must hint at the cause: {msg}");
    }

    #[tokio::test]
    async fn mid_run_failure_leaves_ledger_consistent() {
        // v1 succeeds, v2 fails, v3 is registered but should NOT run
        // because v2 failed.  After the failure, the ledger should
        // have v1 but not v2 or v3.  Next run with v2 fixed picks up
        // from v2 (= v1 in skipped, v2+v3 in applied).
        let ledger = fresh_ledger();
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut r1 = MigrationRunner::new();
        r1.register(Box::new(RecordingMigration::new(1, "one", log.clone()))).unwrap();
        r1.register(Box::new(RecordingMigration::failing(2, "two", "synthetic")))
            .unwrap();
        r1.register(Box::new(RecordingMigration::new(3, "three", log.clone()))).unwrap();
        let err = r1.run(&ctx_for_tests(), &ledger).await.expect_err("v2 must fail");
        // The full error chain with `{:#}` carries the inner cause —
        // `err.to_string()` only shows the outer "applying migration v2 (two)"
        // context (anyhow's default Display strips the chain).
        let chain = format!("{err:#}");
        assert!(chain.contains("synthetic"), "expected 'synthetic' in: {chain}");
        assert!(chain.contains("v2"), "expected 'v2' in: {chain}");

        // After failure: log shows v1 applied but not v3.
        assert_eq!(*log.lock().unwrap(), vec![1]);

        // Ledger: only v1 should be present.
        let applied = read_applied_versions(&ledger).unwrap();
        assert!(applied.contains(&1));
        assert!(!applied.contains(&2));
        assert!(!applied.contains(&3));

        // Second run with v2 fixed (replaced with a non-failing one):
        // v1 is skipped, v2 + v3 apply.
        let log2 = Arc::new(Mutex::new(Vec::new()));
        let mut r2 = MigrationRunner::new();
        r2.register(Box::new(RecordingMigration::new(1, "one", log2.clone()))).unwrap();
        r2.register(Box::new(RecordingMigration::new(2, "two-fixed", log2.clone()))).unwrap();
        r2.register(Box::new(RecordingMigration::new(3, "three", log2.clone()))).unwrap();
        let s = r2.run(&ctx_for_tests(), &ledger).await.unwrap();
        assert_eq!(s.skipped, vec![1]);
        assert_eq!(s.applied, vec![2, 3]);
        assert_eq!(*log2.lock().unwrap(), vec![2, 3]);
    }

    #[tokio::test]
    async fn rerun_does_not_call_apply_again() {
        // Belt-and-braces sibling of the rerun_skips test: pin that
        // the counter (a side effect inside apply) doesn't double-fire.
        REENTRY_COUNTER.store(0, Ordering::SeqCst);
        let ledger = fresh_ledger();

        for _ in 0..3 {
            let mut r = MigrationRunner::new();
            r.register(Box::new(CountingMigration { version: 42 })).unwrap();
            r.run(&ctx_for_tests(), &ledger).await.unwrap();
        }
        assert_eq!(
            REENTRY_COUNTER.load(Ordering::SeqCst),
            1,
            "apply() must run exactly once across 3 runner invocations"
        );
    }

    #[test]
    fn len_and_is_empty_reflect_registrations() {
        let mut r = MigrationRunner::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        let log = Arc::new(Mutex::new(Vec::new()));
        r.register(Box::new(RecordingMigration::new(1, "one", log.clone()))).unwrap();
        r.register(Box::new(RecordingMigration::new(2, "two", log))).unwrap();
        assert!(!r.is_empty());
        assert_eq!(r.len(), 2);
    }
}
