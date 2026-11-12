# Cloud provider parity and integration notes

This document records the cloud work completed in the Rust/Tauri application,
the lessons learned while comparing implementations, and the boundaries that
remain intentionally incomplete. It is a design and handover document, not a
credential store. Never put tokens, passwords, TOTP codes, client secrets, or
contents of a local `.env` file here.

## Cross-app engine boundary

Sorter already calls the native Rust Filen/Internxt libraries. Cloud and Player
currently use Dart clients; sharing these Rust libraries through their Dart
bridge is proposed work. The `desktop` feature includes both native provider
features; there is no Python subprocess fallback. Engine selection, host-owned
queues/secrets, browser limits, source/license evidence and cross-engine test
requirements are in
[STORAGE_ENGINE_ARCHITECTURE.md](STORAGE_ENGINE_ARCHITECTURE.md). Existing queue
retry/replay behavior must not be repurposed to switch engines after a mutation.

## Authority and comparison method

The authoritative behavioral blueprint is the MIT-licensed Internxt Go source
and its gateway/API behavior. The Python and Dart clients are valuable
cross-checks for user-facing behavior, especially login/2FA, copy/update,
cache invalidation, wildcard search, resumable transfers, and interoperability.
They do not override the Go provider contract when the clients disagree.

The Rust native crates are the standalone `crisp-internxt` and `crisp-filen`
crates from [crisp-cloud-rs](https://github.com/CrispStrobe/crisp-cloud-rs),
used as git dependencies (no app-local copies), so Rust consumers share the same
reader/writer, transfer, mutation, and error concepts. The application facade
then maps those native APIs into the synchronous, object-safe `CloudDrive`
trait used by CrispSorter.

## Current provider surface

| Area | Rust/application status | Important boundary |
| --- | --- | --- |
| Listing | Cache-aware native listings with invalidation after mutations; bounded recursive filename search | Remote full-text search is not assumed; fallback content search is bounded to readable files of at most 256 KiB |
| Transfers | Shared bounded `TransferQueue`, retry/backoff, cancellation, progress, reader/writer APIs, and native resume checkpoints | The legacy synchronous trait uses a bounded blocking adapter; FUSE remains synchronous by design |
| Mutations | Create, rename, move, copy, replace/update, timestamps where the gateway safely permits them, delete/trash | Capabilities are checked before IPC; unsupported operations fail explicitly |
| Reversible trash | Google Drive explicit path restore; Internxt exact-object duplicate receipts; CLI/Tauri guards | Native Internxt/Filen path-only restore remains disabled; LocalDrive and generic WebDAV do not claim recoverability; OneDrive recycle-bin restore is gated pending opaque-ID/account semantics |
| Versions | Google Drive and OneDrive list/restore contracts, UI capability gating, ignored live tests | OAuth tokens are required explicitly for live runs |
| Search | Recursive bounded filename/path search in GUI and CLI; optional bounded content scan | Do not turn this into an unrestricted provider walk |
| Proxy | HTTP(S), SOCKS5/SOCKS5H, persisted non-secret settings, keychain password, shared construction paths | Proxy passwords never serialize into settings or drive metadata |
| OAuth | Desktop loopback PKCE, state/code validation, refresh, Google revoke, Microsoft logout/clear | Mobile deep-link callback support remains open |
| Plugin boundary | Validated remote-provider manifest types and security checks | Generic authenticated request transport is not implemented yet |

## Security lessons

* Authentication is a product boundary. Drive metadata, settings JSON, logs,
  URLs, crash reports, and IPC responses must contain presence booleans or
  redacted sentinels, never secrets.
* Tests must not probe the OS keychain. Unit and hermetic HTTP tests use mock
  keyring backends or explicit fixtures. Live tests are ignored by default and
  require named environment variables or manually configured CI secrets.
* A generic provider request command is dangerous. The manifest validator in
  `src-tauri/src/plugins.rs` rejects non-HTTPS URLs, URL credentials,
  query/fragment-bearing base URLs, localhost, `.local`, loopback, private,
  link-local, multicast, and unspecified IP hosts. The eventual transport must
  use the validated install-time host and explicit user consent; it must not
  accept a per-request arbitrary URL.
* Certificate pin policy validation is not certificate pin enforcement. The
  built-in pin sets and rotation checks fail closed on malformed policy.
  Host selection compares whole DNS labels, including the regional S3
  patterns; each wildcard consumes one nonempty label. URL authorities and
  malformed names cannot select a policy. The source-bound
  `tests/cert-pin-contract` validates this selection and pin data, but
  the actual verifier/client-construction work remains a separate security
  item in `PLAN.md`.
* A provider capability is a promise. If a backend cannot safely implement an
  operation, its capability is false and the trait returns an explicit error.
  This is preferable to discovering a late HTTP failure after the user has
  started a destructive workflow.

## Transfer and failure behavior

The application-wide queue owns concurrency, retry/backoff, cancellation,
progress snapshots, and terminal job history. Provider clients retain auth,
encryption, endpoint, and chunk semantics. Failed GUI/CLI writes stage bytes
and enqueue replay descriptors; startup/reconnect maintenance retries them
with bounded backoff. Queue adapters exist because `CloudDrive` is synchronous
and object-safe; changing the trait to capture borrowed provider state in an
async `'static` worker would be unsafe and would break FUSE/legacy adapters.

Native resumable state is provider/session/key/chunk-specific. A resume state
must validate source/destination identity and metadata before continuing; an
incompatible checkpoint is rejected or discarded rather than applied to a
replacement object. This behavior is covered by native unit/hermetic tests
and opt-in large-file live tests.

## Restore and conflict behavior

`CloudDrive::restore_deleted` and `DriveCapabilities::reversible_trash` form
the common restore boundary. The Tauri command and `crispsorter drives
restore-deleted` CLI both check the capability before invoking it. Duplicate
mutations persist bounded audit records and cloud moves can be undone.
Path-only historical restore controls remain disabled. New duplicate trash
uses only the qualified object-receipt adapter described below.

Native Filen and Internxt path-only restores remain disabled: a basename,
including a unique current basename, does not prove historical object identity.
Their legacy `reversible_trash` capability remains false. Explicit existing
delete APIs are unchanged. Google Drive explicit path-based restore still uses
unique live ancestors and a trashed leaf; legacy duplicate path-only history
never enables restore. Use the provider app to restore existing native trash.

### Newest-wins metadata safety

Pair comparison marks an existing file for manual review when the remote
modification time is unknown, or when timestamps match but sizes differ. A
newest-wins push rejects the entire plan before transfers if any such conflict
exists. Returning a partial plan would let a later upload advance the watermark
past an unresolved file. Zero timestamps also require review: the local scanner
uses zero for unavailable or pre-epoch modification times, so a genuine Unix-
epoch timestamp cannot be distinguished from missing metadata. Unknown-time files
remain in incremental plans even after a newer watermark. Epoch-zero files
need an explicit policy; representable negative timestamps can still be ordered.
Explicit local/remote conflict decisions retain their existing behavior. The CLI pair comparison shares these decisions; CLI push still
requires its explicit local-wins policy.

For file-manager move/copy against an existing destination, newest-wins permits
replacement only when both timestamps are known and the source is strictly
newer. Unknown, zero or equal timestamps preserve the destination. These checks do
not add provider conditional writes: source or destination changes after a
comparison, failed destination lookups, retry/offline replay, and provider cache
freshness remain separate transaction/CAS work. Equal size and timestamp still
mean metadata-unchanged, not verified content equality; timestamp precision and
clock skew can also hide differences. Keep-both and durable per-file manual
resolution remain open.

### Sync pull path boundary

GUI and CLI sync inventory validate provider names before stat or recursive
descent. Empty names, dot components, slashes, backslashes, NULs and exact
duplicate names are rejected rather than mapped to a different file. Unicode,
spaces and literal percent sequences are preserved. Windows additionally
rejects device names, alternate-data-stream/drive spellings, invalid characters
and trailing-dot/space aliases. POSIX filenames such as `file:notes` remain
supported on POSIX; names that cannot be represented safely on the local
platform fail explicitly. Filesystem-specific case/Unicode-normalization
collisions are not resolved by this validation.

Before each queued pull, the configured local root must already exist and is
canonicalized as the authority; an explicitly configured root symlink may name
that authority. Descendant directory and leaf symlinks, dangling links and
nonregular leaf targets are rejected. Missing ordinary subdirectories may be
created beneath the canonical root. Both GUI and CLI use the same destination
check. Tests exercise the real inventory/path functions with a provider double,
temporary local directories and Unix symlinks.

This preflight does not use descriptor-relative filesystem operations. Another
process can replace a checked directory after validation, including while a
download is queued or retried. Race-safe traversal/publication requires anchored
directory handles and platform-specific APIs; this change does not claim that
TOCTOU protection, atomic multi-file pull or provider CAS.

### Durable duplicate trash: Internxt receipt slice

Only Internxt advertises `trash_receipts` and exposes the exact-object adapter.
Both app SDK dependencies and `Cargo.lock` pin merged upstream
`9b57beee7ac2e6ae9aaa15a1332a92bcfb5e1460`. This includes the filename correction
in [PR #5](https://github.com/CrispStrobe/crisp-cloud-rs/pull/5) and the receipt-specific
one-shot transport in [PR #6](https://github.com/CrispStrobe/crisp-cloud-rs/pull/6).
Legacy path-only restore stays disabled for native providers; Filen, Google and
other providers do not enable the duplicate receipt workflow. Existing explicit
delete APIs remain unchanged.

Receipt deletion calls `trash_once`; restoration calls
`restore_from_trash_once`. They bypass the legacy gateway retry helper. Their
separate SDK client shares the ordinary proxy, TLS defaults, HTTP version,
user-agent and timeout builder, then disables protocol retry and redirect
following. There is no automatic auth-refresh/retry wrapper in the app receipt
adapter. Legacy SDK mutation methods retain their existing retry behavior and
must not be substituted into this contract. Listing caches and recent-write
overlays for the affected UUID are cleared after every attempted one-shot
mutation; local overlays do not establish remote completion.

The advance from `33bdc33f` also includes Filen zero-byte download handling and
Internxt empty-file handling, safe registration retries, recent-write listing
reconciliation, in-place update changes and additional storage/search APIs.
Public APIs and session identity fields used by the app adapters remain
compatible. Internxt still rejects unsupported zero-byte uploads before remote
registration; Filen chunkless empty files no longer request a nonexistent chunk.
Receipt commands instantiate their own provider/client, so identity observation
does not reuse a client containing another command's recent-write overlays.

Qualification at the merged tree: all 11 targeted real HTTP one-shot tests and
four existing HTTP-configuration regressions passed. These cover lost responses,
502/503/504, redirect rejection, 401 without refresh, timeout, configured proxy,
folder operations, escaped identifiers and cache/overlay invalidation. The app's
complete native adapter modules, real CloudDrive/filesystem boundary, client
cache, logger and journal passed a narrow compile check against both real
pinned SDKs (host-only keychain/log-sink/proxy-configuration shims). Full locked
metadata with both native features passed with isolated sibling versions
matching the app lock: CrispASR 0.8.32, CrispEmbed 0.17.19 and crisp-docx 0.1.0.
No unrelated sibling versions or registry lock entries were changed. Focused
receipt and frontend checks qualify journal/identity/IPC behavior, not a full
Tauri build or live account execution. Prior API-only compilation was not
sufficient to establish no replay: the real SDK HTTP controls are a prerequisite.

Before deletion, the application writes a private, fsynced journal containing
registered drive/group, a fingerprint of provider endpoint + stable account +
root IDs, object UUID/type/name, original path and original folder UUID. Every
ancestor and source is resolved freshly with ambiguity rejection. The provider
is then asked to trash that exact UUID. A success marker confirms deletion;
the journal is not dependent on the browser's bounded audit/localStorage.
Credentials and session tokens are never included or logged. Unix journal
files and directories use modes 0600 and 0700 respectively.

Restore accepts only an opaque journal receipt ID. It verifies the registered
drive, account fingerprint, exact fresh trash UUID/type/name and original folder
UUID, and refuses a replacement occupying the original name. A durable
cross-process single-use claim precedes the provider mutation. Internxt moves
the exact UUID/type into the recorded folder UUID; there is no caller-selected
replacement identity or destination. A receipt is never automatically replayed.

After interruption, **Recheck without retry** observes the recorded UUID only.
An uncertain deletion found in trash becomes restorable. An object still in its
original folder remains `delete_uncertain`: that snapshot cannot prove that an
in-flight request will not subsequently commit. Legacy `not-deleted` journal
markers are ignored; a later confirmed deletion cannot be hidden by them.
An uncertain restoration found at its
original folder becomes `restored`. Unknown state, missing/changed identity,
account changes or a still-trashed claimed restore remain blocked; use the
provider app when observation cannot establish completion. This avoids issuing
another mutation when a remote request may still have completed. Directory
fsync includes the journal's parent entry on first creation on Unix; oversized
serialized receipts are rejected before remote deletion. Power-loss and physical-
crash behavior have not been tested on any platform.
The journal and receipt-specific Internxt capability are disabled on non-Unix
targets: directory-entry persistence is not implemented there, so a receipt
cannot authorize remote deletion or restoration. Unix behavior is unchanged;
mobile Unix targets execute the fsync protocol but remain unqualified for
device crash durability. Windows/mobile durability,
full Tauri execution and live provider accounts remain unqualified. Provider APIs offer no cross-client
transaction/CAS around the last validation and mutation, so concurrent remote
changes can still produce a refused/uncertain outcome requiring recheck.

`tests/trash-receipt-contract` executes the production journal and selected
production native adapter methods against an in-memory SDK fixture. It covers
restart, account/endpoint/root/drive mismatch, UUID/type/name/parent changes,
duplicate names, destination replacements, concurrent claims and failures before
or after mutation. Frontend tests verify capability gating and opaque-ID-only
restore/recheck payloads. Full Tauri/application compilation and live provider
qualification are separate gates. The source-bound gate also reproduces
incomplete provider filename metadata
and verifies that restore fails closed; it verifies that the enabled receipt
capability remains separate from unsupported legacy path restore. No vendored
SDK divergence is introduced.

Sync-pair comparison is metadata-only until the user explicitly applies a
policy. Confirmed one-click `local_wins` and `remote_wins` use the existing
guarded pair push/pull commands. `newest_wins`, `keep_both`, and `manual` still
need per-file transactional resolution; they must not be silently reduced to
an entire-pair overwrite.

## Test matrix

The ordinary suite is hermetic. It covers native gateway parsing, cache
invalidation, retries, concurrency, wildcard matching, range/resume logic,
copy/update/trash/restore, timestamps, crypto vectors, capability contracts,
proxy validation, OAuth state/PKCE/token handling, redaction, and plugin
manifest validation. Frontend changes should pass `npm test -- --run` and
`npm run check`.

Live tests are `#[ignore]` and must be run serially against accounts dedicated
to destructive testing. They create unique temporary names, verify cleanup,
and never discover credentials automatically. The manual `cloud-live` GitHub
job skips a provider when its explicitly configured repository secrets are
absent. The current live coverage includes Internxt, Filen, Google Drive,
OneDrive, and WebDAV, plus Rust↔Python/Dart interoperability where the
companion clients are explicitly opted in.

Typical commands and required variable names are documented in the README;
only short-lived tokens or test-account credentials should be supplied at
runtime. Do not paste their values into issue reports, CI logs, or Markdown.

## Remaining work

The authoritative checklist is `PLAN.md`. The most relevant open items are:

1. Wire an actual certificate verifier/pinned client through every pinnable
   constructor and document root-CA rotation diagnostics.
2. Add mobile deep-link OAuth registration and callback lifecycle handling.
3. Finish per-file conflict transactions for newest/keep-both/manual and
   complete the remaining provider-specific restore semantics.
4. Extend the generic plugin surface from validated manifests to explicit
   install consent, keychain namespacing, capability probing, and a fixed-host
   authenticated transport; then move the optional image service out of the
   application binary.
5. Add further providers only when a real workflow and hermetic/live test
   environment exist. Nextcloud/ownCloud delta-sync work must follow the
   existing CrispCloud/crispcloud-delta-sync and upstream client semantics,
   rather than being treated as generic WebDAV feature flags.

When continuing this work, fetch and rebase from `origin/main` first. Parallel
agents land directly on main, so preserve newer commits and keep one focused
change per handover step.
