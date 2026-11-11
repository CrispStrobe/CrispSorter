# Remote integration audit — 11 November 2026

## Scope and result

The initial read-only audit fetched all 74 live GitHub branch heads without pruning or checking out another branch. It compared them with OCR integration baseline `0be20e8a5c004b04bc5c449f16977252d02ba27b` and main `2a9a74d509c6803e50c15e5284d5c95401f8646d`. Work continued to arrive during integration. The final source inventory at **11 November 2026, 10:37 UTC** contains **76 heads**: the PDF editor and OCR document-view branches, plus the advanced SMB head, are all included below. This timestamp and the exact source SHAs define the integration snapshot.

- 71 heads, including main itself, were already ancestors of main. Their work is present by commit ancestry; there is no reason to reapply their commits.
- `feat/shared-ocr-policy-20261111` was the current integration baseline and contained the three OCR integration commits beyond main.
- `feature/smb-storage-optimization-2026-11-11` initially had five unique commits, integrated by merge `07548c56dcf3fb0bb67e884a27eb51a022bdd22c`. The app-provider commit `5c8e84b3` entered through `bde9dbf2`; its qualification follow-up at final source head `eba2ca6c6974374aa6d34954a03baee457628d54` entered through `29d7d696`.
- `feat/pdf-editor-consolidation` initially entered at `1bdf4dbe` through merge `fe274b06`. Final source head `afcb4a25fc9af83b250807e1eac23cd5c38b2722`, including view controls and interaction tests, entered through `ad854a2b`.
- `feat/ocr-document-view` at `dbec5870e8f4593f52e120676f471a7cad085eb9` adds the paragraph document editor and native grouping. It is included with the evidence, layout and editing fixes described below.
- `mas-sandbox-build` contains one graph-unique July prototype commit. It is not patch-equivalent to current main, but its remaining changes are temporary build workarounds for an old product variant, not a missing current feature. The specific disposition is explained below.

The inventory is a snapshot of the live remote refs at the audit. Remote branches can move after this snapshot. Local stale tracking refs were not used to invent additional candidates.

## Pull-request coverage

The initial GitHub inventory reported [PR #44](https://github.com/CrispStrobe/CrispSorter/pull/44) as the only open pull request. A subsequent open-PR check at 10:47 UTC also found draft [PR #45](https://github.com/CrispStrobe/CrispSorter/pull/45) for OCR document view and draft [PR #46](https://github.com/CrispStrobe/CrispSorter/pull/46) for PDF consolidation. Their heads exactly match the final source inventory and are incorporated into PR #44. The SMB feature branch has no open PR, so it was found through the branch audit.

[PR #42](https://github.com/CrispStrobe/CrispSorter/pull/42) is already merged at `2a9a74d5`. Its merge tree is identical to the prior search-workspace feature head `48bab31f`. The older OCR, mobile, search, library, provider, and catalog branches listed below are already integrated by ancestry.

In particular, `feature/catalog-scan-improvements-2026-11-07` at `19ec5a25329c16a49eff05aa16d021f5d511403e` is not an outstanding integration candidate. It entered the expansions integration at `a07464c2`, which reached main through [PR #23](https://github.com/CrispStrobe/CrispSorter/pull/23), merge `3a3e6c64`.

## Integrated SMB work

Initial source head: `e737a132fdade74fed7c6c87566348621319d722`. Final integrated source head: `eba2ca6c6974374aa6d34954a03baee457628d54`.

The branch starts directly from main `2a9a74d5`. These five patch-unique commits were brought in together:

| Commit | Purpose |
| --- | --- |
| `1e298744f081d105814cd9d73919ff35f0e727a9` | Record mounted-storage implementation and optimization priorities. |
| `adb2826944e82d1531f8de73342a687a95923e06` | Add bounded mounted-file reads through `LocalDrive::read_file_to_writer`. |
| `b17d08384a4482fa18e932c8715927adf818ba32` | Add isolated mounted-reader tests and a targeted workflow. |
| `5345d24a679a5c197ad57a9b3bffcefd27dc9695` | Add the independent direct-Rust SMB qualification crate. |
| `e737a132fdade74fed7c6c87566348621319d722` | Automate the shared SMB fixture and mounted-read qualification. |

The 17 changed files had no overlap with the 44 OCR integration files changed since main. A read-only three-way merge simulation reported no conflict markers. The completed integration merge is `07548c56dcf3fb0bb67e884a27eb51a022bdd22c`.

At the initial source head, direct SMB was a standalone experimental qualification boundary. The later `5c8e84b3` commit registers the SMB app drive and shares its settings between the IndexIngest GUI and `drives smb` CLI. It supports an OS-mounted transport and an explicit experimental Rust transport. The independent crate stays excluded from workspace membership but is now an app path dependency. Both locks and the fixture use immutable CrispCloud core revision `4b44dc4d44ef05a18d2da2579d2e3b866b245643` (later advanced to `13ca681f0c706cf24e65c7906f8a98e20421c590`). Existing local/OS-mounted folders continue to work through LocalDrive.

The second merge preserves the new `filesystem.rs` / `registry.rs` split while carrying forward the staged LocalDrive writes, streaming receipts and bounded reads. The production-source contract harnesses follow the relocated code. Mounted SMB uploads use the same verified staging boundary. Direct Rust uploads retain the upstream create/truncate semantics; failed writes that may have committed are excluded from immediate and durable automatic replay. No atomic direct-SMB upload guarantee is made.

The `eba2ca6c` follow-up adds creation of missing registry directories, the GUI transport label, settings IPC browser coverage, failed-stream/session-reuse cases and a full-app typecheck gate. Its failed-write cleanup is reconciled with verified staging: mounted failures must leave no new destination, while direct transport cleanup handles any partial remote file. A queued mounted upload whose settings later switch to Rust is rejected before replay begins, and its diagnostic record is retained as terminal.

The incoming branch added four isolated production-reader tests, one LocalDrive dispatch test, and one explicitly invoked authenticated SMB fixture. These are source-level inventory counts, not a claim that this audit executed the full Tauri app or real NAS qualification.

A follow-up consumer audit found that the new streaming facade initially had no production caller outside provider implementations: GUI/CLI/sync download consumers still used the whole-buffer queue API, and index promotion downloaded to a vector before writing a temporary file. The integration follow-up now routes actual disk-bound consumers through that boundary:

| Surface | Integrated behavior |
| --- | --- |
| GUI and CLI backup restore | Shared queue streams into a private file beside the destination, verifies the source size, then publishes the completed file. |
| GUI and CLI sync pull | Uses the same file-target queue and updates successful-file counts only after publication. |
| Drive archive promotion into the index | Streams into a private per-request directory whose lifetime covers extraction and indexing. |
| FUSE indexing of mounted files | Reads only the requested byte range through seek/read and the shared queue. |
| FUSE indexing of other providers | Materializes to a disk-backed LRU and allocates only each requested slice; active readers retain ownership through eviction. |
| Byte-array GUI preview | Enforces a 64 MiB payload limit; larger files require a file download. |
| GUI and CLI provider content search | Enforces the existing 256 KiB content limit against actual bytes as well as metadata. |
| Local/mounted writes | Validates the complete declared stream before replacing an existing destination. |

The file-target queue retains concurrency limits and retries, stops cooperative writers on cancellation, cleans partial stages, and serializes cancellation with final publication. Structured operation receipts cover queue lifecycle, retries, materialization and local stream reads/writes without file paths, provider error text or credentials.

The legacy byte-returning API remains for consumers that actually require a byte payload, including the cloud-backup shard-import transport. Providers whose trait implementation internally falls back to a whole-file API still have that limitation; this change does not claim every cloud SDK or application format is fully streaming. Direct SMB is an explicitly selected experimental app transport; real NAS qualification remains separate from in-memory and loopback contract tests.

The new `tests/storage-contract` gate compiles production queue/storage/logger code and covers bounded dispatch, verified publication, partial failure/retry cleanup, cancellation, backpressure, disk-cache ownership and safe receipts. The creation workspace has no Rust toolchain. Hosted macOS execution at PDF merge `fe274b06` passed all 53 contract tests; its generated lock was retrieved from run `43002813054`, artifact `13846748660`, verified against the artifact SHA-256 and committed for subsequent locked runs on Linux/macOS/Windows. That pass predates the newer SMB provider integration and does not validate its later changes. The available Python fixture-runner tests passed locally: three tests for invalid readiness, startup timeout and process cleanup.

The same checkpoint's authenticated SMB fixture exposed a real missing-file bug: the pinned core deliberately returns JSON null for absent metadata, and the adapter had treated that as success. The integration normalizes absent metadata to a not-found error at both asynchronous and blocking app boundaries. The live regression remains in place alongside model-free metadata tests.

## PDF editor consolidation

Source head `1bdf4dbe21475b0d3449995baf6225214acfb726` replaces the four overlapping PDF tabs with one editor and a document-tools drawer. It adds drawing/erasing real PDF annotations, shared document actions, and text-layer exports. The source commit merged cleanly, but the integration audit found behavioral conflicts that a textual merge cannot detect.

The later `afcb4a25` follow-up enables font rendering in both document loaders and adds floating view controls and interaction tests. The integration retains all ownership guards and both browser suites: six lifecycle cases plus nine interaction cases. Logging assertions follow the safe event contract; detailed failures remain visible in the UI.

The follow-up protects session ownership across open/edit/save/close, serializes actions, bounds page-range parsing, and avoids form-loading retry loops. DOCX/conversion handoffs reject stale UI results. Native saves and exports stage output before publication; validation failure preserves an existing destination. Save As retargets the session, branch edits after undo remain dirty, and private snapshot names cannot escape their session directory. Shared operation receipts replace path-bearing diagnostics.

CLI parity uses the same native code: `pdf export` provides text, Markdown and feature-gated Word output; `pdf apply` executes the editor's serializable operations, including annotations and erasing, from a bounded JSON recipe. See [PDF editor and CLI](pdf-editor-cli.md). Text-layer export is explicitly separate from recognition; scanned documents continue through the shared OCR policy.

## OCR document view

Source head `dbec5870e8f4593f52e120676f471a7cad085eb9` replaces per-line controls with editable paragraph regions in a continuous page view, compacts the workspace and updates browser/mobile smoke selectors. Its engine catalog and policy API are unchanged; the integration preserves the shared dispatch, capability validation, saves, exports and content-free logging introduced earlier.

Native grouping classifies layout while line boxes are still distinct, then combines compatible prose. Non-text regions and native word evidence remain barriers. Character confidence is retained only when units, provenance, lengths and finite score ranges satisfy the same conservative contract as line grouping. Token scores and unknown/mixed bases are not relabeled as character evidence.

Paragraphs retain line breaks and their enclosing region box. The pinned CrispEmbed renderer supports multiline region text and estimates each line's placement within that box; grouping does not create verified line or word coordinates. Native word-evidence regions are not grouped. Workspace export regressions cover the grouped text and spatial-export contract, while the pinned renderer's own artifact test checks multiline text extraction. A local [reproducible renderer probe](../tests/quality/measurements/paragraph-render-20261111/rebuild.py) also passed exact TXT/hOCR/ALTO text checks and PDF extraction through Poppler and pypdf at source `e14136d3`. Its [receipt](../tests/quality/measurements/paragraph-render-20261111/receipt.json) explicitly excludes image decoding and the application's Rust binding.

The document editor handles plaintext-only contenteditable elements explicitly. Model changes reconcile the DOM while preserving the selection where possible; locking an active editor prevents unrecorded edits. Project undo and subsequent typing are covered by browser regressions, alongside native tests for layout barriers and score provenance. These tests are separate from actual OCR model qualification.

## Historical MAS branch disposition

`mas-sandbox-build` points to `45a1b4e9efd8a7a427bf08a22c2ec46feb64d58f`, authored on 17 July 2026. Relative to the OCR baseline it has one unique commit and lacks 992 current commits. Its merge base is `9b4091ac172653e204858c127044f93e2334f5cf`.

The commit describes a cloud-only Mac App Store sandbox prototype and explicitly records a local sibling-version workaround. Current code supersedes or conflicts with its three changes:

| Historical change | Current disposition |
| --- | --- |
| Remove `shell:default` and `process:default` from default capabilities. | Already true in the current default capability file; no additional patch is needed. |
| Add unconditional `reqwest/native-tls`. | Current desktop feature selects native TLS while the app has separate mobile/runtime configuration. Transplanting the old unconditional feature would change current build behavior. The old reqwest line also lacks current `query` and `form` features and conflicts during a three-way merge. |
| Comment out `crisp-docx-align` and `translate-align` because of a local version mismatch. | Current code retains the optional dependency and supported feature. The July workaround would remove functioning product capability and is not a current fix. |

Do not mark this commit as merged or patch-equivalent. Retain the historical branch, but exclude these temporary workarounds from the current integration. A future dedicated Mac App Store variant would need a new implementation against the current feature and entitlement model.

## Full live-head inventory

All 76 heads from the final source inventory are listed below. “Already included in main” means `git merge-base --is-ancestor <head> 2a9a74d5` succeeded. The integration branch row records the head at inventory time, before the subsequent merges and fixes.

| Remote branch | Audited head | Disposition |
| --- | --- | --- |
| `backup/main-2026-07-17` | `d1c8bbea998e5d1c8cd3d9ef5e75ee5cf29a2ac9` | Already included in main |
| `backup/main-2026-07-17-b` | `9b4091ac172653e204858c127044f93e2334f5cf` | Already included in main |
| `backup/native-cloud-pre-main-2026-07-31` | `3b821945c5cdbefd63fccd756b19b63386008faf` | Already included in main |
| `chore/crispembed-0.17.15` | `81e1a911bcdfd4744b71cde0a97c2227ce267412` | Already included in main |
| `ci/retry-network-downloads` | `05204068cdcb8f11e201184a9da669bf71b79778` | Already included in main |
| `crispembed-0.17.12-integration` | `0af4c342f0cef8e4fda1b629d5b9fdfc82ed15b4` | Already included in main |
| `crispembed-ocr-ids-13-23` | `f66b18efd1a39026edf9276182e5365f16505b41` | Already included in main |
| `deps/crispembed-0.17.16` | `33785b62641458258d101b074c388adad92a0245` | Already included in main |
| `deps/crispembed-0.17.18` | `992317c82d56ae28f5432e5ea3f37576540bf7cd` | Already included in main |
| `feat/crispembed-sync-qwen3vl-lora-lid` | `0cd67370caff5c3cd8e136b5a12c41b678346f0a` | Already included in main |
| `feat/document-conversion` | `10de47d45ee8414f9653ce7fdf8d6c1eba3d1215` | Already included in main |
| `feat/library-workflows-20261028` | `bf165fc511e6d9728fc9f3b6a194458c913474af` | Already included in main |
| `feat/ocr-android-launch-diagnostics` | `15d98cfd274aad5560347bddd3705075d9ff1f4c` | Already included in main |
| `feat/ocr-archival-option-receipt` | `e39529f3f0887eb231a19060c7c5cba049fafa6a` | Already included in main |
| `feat/ocr-archival-option-semantics` | `76bf5d8d9f388f93b7109558b8bcaf834d8e0c14` | Already included in main |
| `feat/ocr-document-studio` | `b876fb43eb19cc7de44760400e50735aa27c2c78` | Already included in main |
| `feat/ocr-document-view` | `dbec5870e8f4593f52e120676f471a7cad085eb9` | Included with paragraph/evidence/editor corrections; PR #45 |
| `feat/ocr-export-cancel-races` | `33c2d5aef9a689c8e273b2d82f81a3769bd3b40d` | Already included in main |
| `feat/ocr-export-controls` | `d0f9baa9d424a4aaa3d439ae08965191de715cc8` | Already included in main |
| `feat/ocr-export-error-identity` | `8bba64f677b5ea8951915116e0f7b75607efdb98` | Already included in main |
| `feat/ocr-final-native-qualification` | `26bcee97a2c32363915078d670f868a1cbc8caaa` | Already included in main |
| `feat/ocr-final-native-triage` | `17bd2c7f04efd16dc54ac99f840f48881054f8d1` | Already included in main |
| `feat/ocr-import-error-identity` | `b876fb43eb19cc7de44760400e50735aa27c2c78` | Already included in main |
| `feat/ocr-ios-shell-geometry` | `656fb951dfeb774a9f928d50b85ab596790a5ed7` | Already included in main |
| `feat/ocr-ios-shell-probe` | `7f268bc15cac3a16cec3ab6e42530319749c0f97` | Already included in main |
| `feat/ocr-ios-shell-runner` | `d9ab4240b5ef3ad3d87f70b226f0e24ad1c8104d` | Already included in main |
| `feat/ocr-ios-workspace-qualification` | `20c64c385d9f697a5d2fe1f864f4cd42c1e6add7` | Already included in main |
| `feat/ocr-mobile-artifact-retention` | `f82ea23de0f9f466d130efb9d5d2990a11481322` | Already included in main |
| `feat/ocr-mobile-asset-integrity` | `fd8ace709f1deb142b7ab9ce7d142b31d88fcddc` | Already included in main |
| `feat/ocr-mobile-build` | `c1f8dd2c99cdadb144e128b30c15e3eb6af1108c` | Already included in main |
| `feat/ocr-mobile-documents` | `76de6dad49f9474aa1034e3030f95cd3f4a6d144` | Already included in main |
| `feat/ocr-mobile-heic-fixture` | `76de6dad49f9474aa1034e3030f95cd3f4a6d144` | Already included in main |
| `feat/ocr-mobile-provider-receipts` | `fca0a04b8669d085f6a977b62e3003ba1f885826` | Already included in main |
| `feat/ocr-mobile-receipt-fix` | `1297098103de81cfd51ed7274635370ea011be29` | Already included in main |
| `feat/ocr-mobile-shell-fit` | `4cd99bb4b1ea863516b8a89dc85d6cec90a0de41` | Already included in main |
| `feat/ocr-mobile-source-validation` | `28b0033750f08a6ab5c58a70b382bf0abc72c1e7` | Already included in main |
| `feat/ocr-mobile-word-probe` | `8490ae4a3a9e00c2cce61a7971b2ded1cfa66cb0` | Already included in main |
| `feat/ocr-native-cache-fixture` | `1b6e5f705b6b1b1473838997c6ed17d70235b0a7` | Already included in main |
| `feat/ocr-native-lock-fix` | `1c8c4d713c7b42a101f864f1d278f335235c9681` | Already included in main |
| `feat/ocr-native-target-fixes` | `ddf3fc19372418656c2eed3f727f18fad43b75ef` | Already included in main |
| `feat/ocr-native-validation` | `0b347002456f9706086139225543e5fbbb518690` | Already included in main |
| `feat/ocr-pdf-density-advisory` | `6f9c2aab85f344f6713f18aa792051628aaa9fdd` | Already included in main |
| `feat/ocr-pdf-text-alternatives` | `5382ccc5ca70c983c7e8c963e1de7ab00e5501cb` | Already included in main |
| `feat/ocr-phone-corner-focus` | `8db598bb415cbbb79c69f5ca8d14b7c121b1de3c` | Already included in main |
| `feat/ocr-reliability-qualification` | `f3cd96af7aa6ab0e1bb0b19598e1e31a9bc376ea` | Already included in main |
| `feat/ocr-reviewed-pdf-consumer` | `a892ff450e8e7cc667cd1f6a5d78a5eb2dff9c0f` | Already included in main |
| `feat/ocr-safe-overlays` | `2c5b4c1f3a911b95b866421414b270e268e52fc4` | Already included in main |
| `feat/ocr-streaming-export` | `685a7ebdeaa7a7f6f884b6f94dfa8002e0e54b5b` | Already included in main |
| `feat/ocr-studio-integration` | `ad672046f77e0c873162573a5c2253bdf7b3bf53` | Already included in main |
| `feat/ocr-word-review` | `37c76dfb6a325561f53b1e5a062ef342106f1a2c` | Already included in main |
| `feat/ocr-workspace-engines-cpu-budget` | `11515f1580fba95eede4cfd0b4d99da5c1784626` | Already included in main |
| `feat/pdf-editor-consolidation` | `afcb4a25fc9af83b250807e1eac23cd5c38b2722` | Initial consolidation at `fe274b06`; follow-up at `ad854a2b`; PR #46 |
| `feat/search-pipeline-20261110` | `8ceb25ff78710d91c39209473c83b2dea0d671f7` | Already included in main |
| `feat/search-workspace-completion-20261110` | `48bab31f67da2bf09c514c9cdfd7324ab4ef6a3e` | Already included in main |
| `feat/shared-ocr-policy-20261111` | `fe274b06ddd7522eabe3eabf5eebfd21f5aa33b7` | Integration branch at final source inventory |
| `feat/ux-workflow-essentials` | `d9118c45bef5a7bf8429ed8180fd7d8a8a29d1f4` | Already included in main |
| `feature/catalog-scan-improvements-2026-11-07` | `19ec5a25329c16a49eff05aa16d021f5d511403e` | Already included in main |
| `feature/filen-full-parity-backup` | `7bd6d8fd1769b86217a564fb23ac489b87777477` | Already included in main |
| `feature/ocr-production-workspace` | `3e5e95e5b72cdc55942744dda55a7450a59aee52` | Already included in main |
| `feature/single-part-default` | `a34e8bb90201aa651f727cf87567fd88d260631d` | Already included in main |
| `feature/smb-storage-optimization-2026-11-11` | `eba2ca6c6974374aa6d34954a03baee457628d54` | Initial five commits at `07548c56`; app provider at `bde9dbf2`; qualification at `29d7d696` |
| `fix-windows-msvc-toolchain` | `149a3c72fc987b0ec6492d61e3ca3ef1a497958d` | Already included in main |
| `fix/ios-release-unsigned-archive` | `f54137de2a0cf60bf96c41c272df3e829a9ad1e1` | Already included in main |
| `fix/ocr-review-ux` | `73c6070f1d3688557ccf728a491fe9280e5bd7bc` | Already included in main |
| `fix/per-model-license-consent` | `668645cc272b47daeedbf051530fea719f0b2891` | Already included in main |
| `fix/secret-store-startup-fallback` | `dbacc0d2e78039e3538cc6dfded6eec239d1545f` | Already included in main |
| `fix/secret-store-ux` | `112aa92e3c172b7b6dbed43a118c19da8d0d0566` | Already included in main |
| `graft/capability-registry` | `4e47bd7090c297a0575db326b44aa69888faaa2a` | Already included in main |
| `integrate/ocr-studio-licence` | `b559a82a7565693710440711ee8b0560c414e39e` | Already included in main |
| `integrate/ocr-word-review` | `f28cee0176d9733e5408ba732a10e00e8b542205` | Already included in main |
| `legacy-no-indexing` | `657b2bb7cb97fca1dd8246f5c0e738fa2aec03e2` | Already included in main |
| `main` | `2a9a74d509c6803e50c15e5284d5c95401f8646d` | Already included in main |
| `mas-sandbox-build` | `45a1b4e9efd8a7a427bf08a22c2ec46feb64d58f` | Historical variant; do not transplant temporary workarounds |
| `mobile-app-smoke-20261029` | `2b0f2cff09175c0b1c6fe64e051ba5eb0bab059e` | Already included in main |
| `mobile-app-smoke-20261029-ios2` | `fad6eff2f973fb10082d7796944a310f78338566` | Already included in main |
| `mobile-provider-check-20261029` | `01586562f1198ef3321261f00455a340e0b74d62` | Already included in main |

## Verification method

The audit used these graph operations against explicit commits:

```sh
git fetch --no-prune --no-tags origin '+refs/heads/*:refs/remotes/origin/*'
git ls-remote --heads origin
git merge-base --is-ancestor <remote-head> <main-or-integration-baseline>
git rev-list --count <remote-head> ^<integration-baseline>
git cherry -v <integration-baseline> <unique-branch>
git diff --name-only <merge-base> <candidate>
git merge-tree <merge-base> <integration-baseline> <candidate>
```

All six commits on the two initially graph-unique branches were also patch-unique according to `git cherry`. The second inventory added one PDF commit and one SMB commit. The final inventory added OCR document view plus the PDF/SMB follow-ups; no other source branch moved or appeared. There were no additional non-ancestor, patch-equivalent branches to classify. Two pairs of historical branch names share a head (`feat/ocr-document-studio` / `feat/ocr-import-error-identity`, and `feat/ocr-mobile-documents` / `feat/ocr-mobile-heic-fixture`); they do not represent additional work.

