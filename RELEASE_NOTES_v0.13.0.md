# CrispSorter v0.13.0

CrispSorter v0.13.0 improves the path from imported documents through review to cited answers and sorting. All eight improvement areas and the five-platform artifact set are complete. The release remains an unpublished draft for review.

Changes:

- Retrieval reranks full matching passages, preserves the candidate window and fallback ordering, and keys caches by the requested result limit.
- Chat retrieves evidence from selected documents or the library, budgets the complete prompt and output, and opens cited sources with honest anchors.
- Native desktop builds pin verified CrispEmbed 0.17.12 sources and archive hashes. Unsupported development engines remain unavailable through the released ABI.
- OCR offers explicit task presets, weak-page and region retries, preserved corrections, and undo. The frozen real-page evaluation retains every timeout and recognition error.
- MoEdern and SystemOne connect through an independent optional service. Measured quality gates keep both integrations opt-in; suggestions require review.
- Extracted fields retain quotations, normalized values, source anchors, model/review status and revisions. Approved saved values support exact filtering, JSON/CSV export and SHA-verified sorting.
- Inbox / Review / Library, document-context tools, mobile navigation and processing presets make incomplete work visible while preserving advanced controls.
- Final desktop packages execute startup diagnostics and image-only PDF OCR with development libraries hidden; all platform archives have source and checksum receipts.

Downloads:

| Platform | Primary artifact |
| --- | --- |
| Linux x86_64 | `CrispSorter_0.13.0_amd64.deb` |
| Windows x86_64 | `CrispSorter_0.13.0_x64-windows-portable.zip` |
| macOS arm64 | `CrispSorter_0.13.0_aarch64.dmg` (matching `.app.tar.gz` also available) |
| Android arm64 | `CrispSorter_0.13.0_android-aarch64-dev-signed.apk` |
| iOS arm64 | `CrispSorter_0.13.0_ios-arm64.ipa` |

The Android development APK is the installable testing artifact; the original unsigned APK is retained as build provenance. The iOS IPA has an App Store distribution signature and provisioning profile; no App Store or TestFlight upload was performed. macOS uses an ad-hoc signature and was not notarized; Windows has no Authenticode certificate. Mobile archive/signature checks do not establish physical-device execution. The mobile builds do not bundle the native CrispEmbed runtime; native presets follow capability checks.

Validation: 75 frontend unit tests, zero Svelte errors, production build and ten browser scenarios; 1,490 Linux / 1,484 Windows / 1,495 macOS native tests; all three extracted desktop packages passed version/doctor and scanned-PDF OCR. Browser scenarios use explicit IPC/provider fixtures. Native inference and package checks are separate. Initial build/evaluation failures remain in the evidence folder.

The original real-scan OCR comparison executed all 15 cases, including four declared timeouts. The separate CPU pixel-budget profile completed all nine Qwen cases (one exploratory English case and eight confirmation cases on five further pages): 82–185 seconds and 5,260–5,266 MiB peak RSS. It reduces resource use but loses fine-text accuracy, especially on degraded print, and remains an explicit desktop choice. No automatic default changed. Full measurements and task limits are documented in `docs/library-model-evaluation.md`; usage is in `docs/library-workflows.md`. Newer MoEdern 27M matched the dense baseline on the small frozen panel; other sizes missed the gate. SystemOne scored 75%, below its 90% gate. Neither is a new automatic default.

The combined manifest records actual binary sources: `0eb1a377899347b1740c0a245f0e73326a4f9398` for the original platform jobs and `d3cb9da9d4d89f6b0a0a1cfaf61f26161ae25b75` for Windows recovery. Later application-source differences are confined to optional-provider test assumptions; workflow/tool/documentation fixes are retained on `feat/library-workflows-20261028`. The original all-platform run keeps its Windows failure; the targeted successful Windows job supplies the final portable package.

`CrispSorter_0.13.0_release-manifest.json` and `CrispSorter_0.13.0_SHA256SUMS` accompany the six binary archives. The manifest identifies the exact successful platform jobs, actual source revisions, verification scopes and measured limits.
