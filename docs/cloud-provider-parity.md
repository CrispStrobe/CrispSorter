# Cloud provider parity and integration notes

This document records the cloud work completed in the Rust/Tauri application,
the lessons learned while comparing implementations, and the boundaries that
remain intentionally incomplete. It is a design and handover document, not a
credential store. Never put tokens, passwords, TOTP codes, client secrets, or
contents of a local `.env` file here.

## Authority and comparison method

The authoritative behavioral blueprint is the MIT-licensed Internxt Go source
and its gateway/API behavior. The Python and Dart clients are valuable
cross-checks for user-facing behavior, especially login/2FA, copy/update,
cache invalidation, wildcard search, resumable transfers, and interoperability.
They do not override the Go provider contract when the clients disagree.

The Rust native crates are deliberately kept close to the standalone
`crisp-internxt` and `crisp-filen` APIs so Rust consumers can use the same
reader/writer, transfer, mutation, and error concepts. The application facade
then maps those native APIs into the synchronous, object-safe `CloudDrive`
trait used by CrispSorter.

## Current provider surface

| Area | Rust/application status | Important boundary |
| --- | --- | --- |
| Listing | Cache-aware native listings with invalidation after mutations; bounded recursive filename search | Remote full-text search is not assumed; fallback content search is bounded to readable files of at most 256 KiB |
| Transfers | Shared bounded `TransferQueue`, retry/backoff, cancellation, progress, reader/writer APIs, and native resume checkpoints | The legacy synchronous trait uses a bounded blocking adapter; FUSE remains synchronous by design |
| Mutations | Create, rename, move, copy, replace/update, timestamps where the gateway safely permits them, delete/trash | Capabilities are checked before IPC; unsupported operations fail explicitly |
| Reversible trash | Native Internxt/Filen and Google Drive restore paths; duplicate UI and CLI/Tauri restore guards | LocalDrive and generic WebDAV do not claim recoverability; OneDrive recycle-bin restore is gated pending opaque-ID/account semantics |
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
  built-in pin sets and rotation checks fail closed on malformed policy, but
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
mutations persist bounded audit records; cloud moves can be undone, while
trash restore is offered only for providers advertising a real restore API.

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
