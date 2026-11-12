# Windows full-text recovery evidence

The deterministic fixture denies replacement of Tantivy's `meta.json` after a
successful baseline commit. It requires successful writer acquisition, an error
inside commit, and raw Windows error 5. This tests durable recovery; it does not
identify the filesystem cause of the original intermittent access denial.

## Preserved runs

| Run | Exact source | Observed result |
| --- | --- | --- |
| [Original reproduction, attempt 2](https://github.com/CrispStrobe/CrispSorter/actions/runs/43235808370/attempts/2) | `a113b4d1896bb1e20b4d9bd86ead6cf2eb1dabbe` | Commit failed with access denied, error 5; the failing filesystem operation was not identified. |
| [Initial instrumented probe](https://github.com/CrispStrobe/CrispSorter/actions/runs/43244959365) | `637f9dd21c216c16d04f64902e666beb25f4aaaf` | Passed, but instrumentation added a second buffer. This is nonreproduction, not a repair. |
| [Paired unchanged control and minimal trace](https://github.com/CrispStrobe/CrispSorter/actions/runs/43249240218) | `3f3bd7018d3ed2ede56b472b5b9d8b5ee783e23a` | Both arms passed 24 independent indexes each. Nonreproduction. |
| [Bounded single-buffer stress](https://github.com/CrispStrobe/CrispSorter/actions/runs/43253065385) | `01b27b0e2a3183e915ac52fb0bc17f3ab17a82d1` | Both arms passed 120 independent indexes each and buffer/error equivalence checks. Nonreproduction. |
| [Deterministic publication denial before repair](https://github.com/CrispStrobe/CrispSorter/actions/runs/43262281694) | `03cc4004ff9cc20f5c16a0992a0ed626931bc7bd` | Both arms verified the real commit-stage error 5, then failed replay because a managed `<segment>.6.del` file already existed. |
| [Managed-GC candidate](https://github.com/CrispStrobe/CrispSorter/actions/runs/43265083082) | `6afcc38eefa2e60b0f6dff44634d4246f9b96fca` | Compiled; both arms failed fixture assumptions. Read-only attributes did not prevent removal of orphan files, so incomplete-cleanup injection failed and a later read found a collected file missing. The candidate was not qualified. |
| [Explicit sharing-denial fixtures](https://github.com/CrispStrobe/CrispSorter/actions/runs/43267701623) | `6caf8804845dd7f8381d8cbed2c3eec062667041` | Each arm passed the uncommitted-intent/first-pass exclusion test. Recovery passed the injected errors, writer exclusion, incomplete-cleanup checks, replay and old snapshot query, then failed an assertion reopening obsolete segment paths. Mappings remained readable after those paths were unlinked. The candidate was not qualified. |
| [Qualified recovery control and trace](https://github.com/CrispStrobe/CrispSorter/actions/runs/43269603178) | `86233f78fc025c2c46c13a153142a83adef1a7a9` | Both arms passed both recovery tests, with 316 unrelated tests filtered out: control 1.37 s, trace 1.35 s. All strict injected-failure, ownership, cleanup, preservation and replay checks passed. |

The latest fixture makes path retention explicit by keeping read handles that
deny delete sharing for the committed snapshot files. It retains the strict
commit-denial, journal, generation, reader, byte-preservation, untracked-file and
replay-idempotency assertions. Synthetic failures are labeled in the logs.

## Repair scope and remaining qualification

Tantivy 0.22.1 writes a deletion file before atomically publishing `meta.json`.
If publication fails, its post-commit garbage collection does not run. A newly
opened writer restarts from the persisted opstamp and can reuse that deletion
filename during journal replay.

The candidate invokes Tantivy's managed garbage collection only for a journal
intent proven committed in the vector store, under the existing application
write leases and an exclusive Tantivy writer. It checks both GC errors and
incomplete deletions before replay. Normal first-pass writes and uncommitted
intents do not use this strict cleanup path. It performs no hand deletion or
blanket retry.

An unrelated obsolete file held by a reader that denies deletion can delay
recovery. The journal and last committed postings remain intact until that
reader releases the file. This is an availability tradeoff, not permission to
discard the intent or committed data.

The successful recovery-only run of `.github/workflows/windows-index-repro.yml`
qualifies this bounded recovery behavior. It does not qualify the application's
full suite or release artifacts. The original access-denial cause remains
unresolved.

The review candidate preserves the exact production, deterministic-test and
harness blobs from the successful run. Its workflow is manual only and defaults
missing scope inputs to recovery as well as an explicit `recovery` selection;
stress requires an explicit `stress` selection. There is no pull-request trigger
to repeat the already completed qualification automatically.
