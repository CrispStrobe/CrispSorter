# OCR device qualification

This protocol separates build identity, executed application commands, and
observed system camera/provider interactions. A report must name its actual
scope. An emulator result does not qualify physical camera capture, sustained
phone performance, or low-memory behavior.

## Verify the artifact first

The mobile workflow writes `build-receipt.json` after a successful build. It
contains the clean application commit/tree, paired CrispEmbed/ggml commits,
native build receipt hash, target and artifact identity. Android validation
compares the APK's allocated native code/data with the source-built runtime;
removing nonallocated debug sections is permitted, substituting an older
library is not. The app command probe also hashes the installed `base.apk`
and requires it to equal the receipt and supplied APK. Simulator startup checks
compare the supplied and installed iOS bundle manifests. These are reproducible
build records, not independently signed supply-chain attestations.

The same record identifies the pinned ONNX Runtime used by existing indexing
features and verifies its packaged license/notices. Android compares packaged
allocated native code/data with the qualified API-23 library. iOS records the
qualified static framework, forced Cargo path and defined C API symbol in the
app. A separate tiny Add-model target probe must execute against those inputs;
see the [runtime qualification](../tools/mobile-ort-probe/README.md). This is
separate from OCR model inference and does not establish embedding quality.

Keep the APK or app bundle, build receipt, workflow URL, native runtime receipt,
model input lock, and resulting evidence together. Record any manual rebuild
as a new artifact. An unchanged filename or version label does not establish
that the same binary was installed.

## Execute native commands on a dedicated Android device

The same command probe can target an explicitly selected physical Android test
device. It does not install an app, erase data, select a device automatically,
grant provider permissions, or open the system camera. Install and launch the
matching **debug** APK using the normal platform tools first. Use a dedicated
test installation with no existing OCR projects or installed baseline models;
the probe refuses those states instead of deleting them. It leaves its synthetic
project and downloaded models available for inspection.

Prepare inputs with the checked-in model hashes and an installed font:

```sh
python3 tools/mobile-ocr-probe/prepare.py \
  --output prepared-inputs --font /path/to/DejaVuSans.ttf
```

Select the device's exact authorized `adb` serial, the receipt's source commits,
and a new evidence directory. The default memory gate is 1024 MiB of process
high-water RSS; choose and record the intended device budget explicitly.

```sh
node scripts/mobile_ocr_webview_smoke.mjs \
  --device-kind physical --serial DEVICE_SERIAL \
  --apk /path/to/CrispSorter-ocr.apk \
  --build-receipt /path/to/build-receipt.json \
  --runtime-receipt /path/to/runtime-build.json \
  --inputs prepared-inputs --output new-device-evidence \
  --source-commit FULL_APPLICATION_COMMIT --embed-commit FULL_NATIVE_COMMIT \
  --repeat-pages 8 --max-rss-mib 1024
```

Set `PLAYWRIGHT_MODULE_PATH` to the pinned Playwright 1.62.1 package. The driver
attaches only to the selected application's debug WebView socket. Every adb
operation includes the selected serial. Physical selection rejects emulator
serials and the emulator kernel property; a report must still identify the
actual hardware used by the operator.

The probe retains raw recognition results, private model hashes, restored
project contents, reviewed export bytes, application logs, screenshot, and
failure receipts. It measures wall-clock recognition time, native-reported
elapsed time, current RSS, kernel high-water RSS, and sampled PSS. Raw process
status and `dumpsys meminfo` accompany those measurements. Missing observations
are `null`, never zero. Samples taken every five seconds can miss short PSS
peaks; `VmHWM` reports the process RSS high-water mark. The bounded repeat case
alternates two synthetic rasters and durably saves each recognition result. Its
page count is not a diverse-corpus or large-document capacity claim.

The defined native quality cases add German diacritics and an English page with
two columns under a spanning heading. Their source spec, rendered bytes, font,
installed model and application/native commits are recorded. The German model
adds 513,952 pinned bytes; the column case uses native connected components
without another layout model. A 0.25 Unicode CER ceiling is supplemented by
required accented words and observed column geometry, so a tolerant text score
cannot hide lost diacritics or interleaved columns. Exact native results are
saved and reloaded without test-side text repair. These checks remain defined
until an actual application execution receipt passes; even then they cover
only the named synthetic cases.

No connected Android tools/device or Apple toolchain/device was available in
the recovery workspace when this protocol was added. The protocol itself is
not an execution receipt. On iOS, use a normally provisioned device build and
the observed cases below; the automated lane currently covers a simulator app
and standalone native inference, not physical-device application commands.

The emulator harness separately defines a real DocumentsUI cancellation and
local PDF selection case. It records fresh system UI trees and screenshots,
selects only its uniquely named pinned fixture, then verifies the actual
provider grant, original archive and durable native page import. Unexpected
packages, permission prompts or picker layouts fail the case. This automation
never runs against a physical device and does not establish cloud-provider or
post-restart grant behavior. Inspect its execution receipt before claiming the
case passed; the existence of a script is not execution evidence.

## Observe the actual mobile workflows

Use disposable fixtures and a new project. Record each case as **passed**,
**failed**, or **not run**, with device/OS, artifact hash, input hashes, expected
result, actual result, and evidence paths. Do not mark a case passed from a
mock, raw IPC command, simulator screenshot, or another platform's result.

| Case | Operation and expected evidence |
| --- | --- |
| Camera capture | Open **Capture photo**, grant or deny the real permission prompt, cancel once, then capture a printed fixture. Preserve the original photo and its hash. Confirm orientation, complete page bounds, and usable controls after portrait/landscape changes. |
| Perspective and tilt | Capture the same printed page at clockwise/counterclockwise angles and oblique perspective. Record corners and requested/applied correction, compare the rectified page with the original, and verify undo restores the exact previous raster and review state. |
| Lighting and blur | Test uneven lighting, shadow, glare, and an intentionally blurred capture. Preserve recognition before/after preparation. Check that guidance appears without silently applying corrections or claiming a measured quality score guarantees legibility. |
| Native photo formats | Select real camera-produced HEIC/HEIF/AVIF where the platform advertises them, including orientation metadata. Confirm valid raster dimensions and unchanged archived original bytes. Unsupported and malformed files must produce an actionable error. |
| System document picker | Select an image and multipage PDF from local storage and a real cloud provider. Include Unicode names, picker cancellation, encrypted/invalid PDF, and the declared byte/page/pixel limits. Record actual source URI, imported page count, page geometry, and archived source hash. |
| Provider lifetime | Close and reopen the app, verify the retained grant, then revoke provider access externally. Reopening must either succeed through the retained grant or ask the user to select it again; it must not replace source identity with an inaccessible or unrelated file. |
| Model lifecycle | Start offline, start a download, interrupt connectivity, retry, and relaunch. Only verified complete artifacts may become available. A failed update must preserve the previously complete version. Record selected language, model IDs and hashes. |
| Recognition and review | Recognize a known fixture, select low-confidence regions, edit punctuation/digits/non-ASCII characters, reorder regions, exclude a region, undo/redo, and approve the page. Verify geometry uses the displayed raster and approval is revoked by an actual content change. |
| Multilingual documents | Use independently transcribed pages for each claimed language/script and the appropriate installed model. Include diacritics, mixed LTR/RTL text, digits, and CJK where supported. Retain UTF-8 ground truth and actual text; report character errors without discarding non-ASCII characters. |
| Complex layout | Use columns, a ruled table, a caption beside an image, a header/footer, and rotated text. Check reading order, cell content, exclusion, and selected-region recognition against the page. Preserve the edited export and its text extraction. |
| Recovery | Interrupt a selected-page batch, relaunch, and resume. Confirm the original page selection/settings, completed pages, failed-page retry, saved corrections, and project revision survive. Do not infer this from a successful new batch. |
| System export | Save through the actual native picker, cancel, retry after provider failure, and reopen the chosen file in another application. Compare resulting bytes with the private export, check page count/size and corrected search/copy text, and verify temporary staging is removed after completion/cancel. |
| Large projects | Run distinct 10/100/500-page fixtures as separate declared workloads where hardware permits. Record input megapixels, model, processing profile, peak process RSS/PSS, elapsed time, responsiveness, persistence/recovery, and failures. A reduced page count is a separate result, not a pass for a larger workload. |
| Sustained use | Repeat the declared workload while monitoring temperature/thermal throttling, battery state, background/foreground transitions, OS memory pressure and app restarts. Preserve failures and identify the first failed page/operation. |

An overall qualification report should link the underlying case evidence and
list unrun cases. Keep synthetic UI, native inference, provider helper tests,
full application commands, and observed camera/picker flows individually
identifiable so a later change can rerun the affected level.

## Current Android import follow-up

The Android WebView uses Tauri postMessage, which delivers JSON rather than a
raw byte body. The application now sends bounded base64 for Android and an
unknown platform, while retaining raw bytes on known desktop and iOS targets.
The native import and generic image OCR commands validate metadata first and
accept either representation. Legacy JSON byte arrays are limited to 1 MiB;
the normal base64 and raw paths retain the 32 MiB input limit. The maximum
size is a validation limit, not a measured device-memory guarantee.

The rebuilt Android x86_64 emulator run verified installed model hashes and
imported an image through the fixed IPC, then failed a sandbox-path assertion.
The subsequent retained flow failed earlier because the device's `realpath`
preserved different spellings for `/data/data/<package>` and
`/data/user/0/<package>`. That receipt establishes the spelling difference but
does not establish that the directories share an identity. The harness now
requires matching device/inode/owner/directory identities for both exact roots
and their resolved paths, checked through the dedicated package's `run-as`
namespace. It records the observations and rejects unexpected roots or identity
mismatches before accepting either alias. Per-file resolution still rejects
redirects to a different relative path or outside those verified roots.

The new identity gate has focused simulated tests; matching directory identities
have not yet been observed on the retained emulator. Run the retained full WebView
flow using the newly pinned APK; do not count either failed attempt as a pass. The mobile native
workflow supports a single-target manual run (Android x86_64 by default),
while push runs retain the complete target matrix. Record its source revision,
artifact hash, actual import, recognition, export and recovery results before
claiming success. Physical Android and iPhone devices are unavailable; camera,
real provider grants, low-memory behavior and device performance remain unrun.

The retained emulator rerun verified sandbox directory identity, installed
and hashed models, imported an image, exercised native recognition and document
formats, and passed the German quality case. It then failed the English
column-order case (CER 0.503 versus the unchanged 0.25 threshold): native
text detection had merged adjacent columns into page-wide lines. The current
source adds bounded experimental CC-layout recognition crops for supported
layouts, with a whole-page fallback for empty crops. This path defaults off;
an explicit request/profile `experimental_cc_column_crops: true` enables it
only alongside the existing supported CC/Tesseract profile. Ordinary CC layout
continues to annotate whole-page recognition. Geometry and source-bound
contract tests pass; the retained APK predates this change. Rebuild the Android
x86_64 application and rerun the full flow before reporting the fix or later
export and recovery steps as passed.

CC box geometry does not prove complete page-text coverage. Text missed by
thresholding, morphology or component filtering outside the padded crops is
never presented to the recognizer; nonempty crop results do not trigger the
whole-page fallback. The experimental path therefore has an unresolved
missing-text risk. Its 64-crop and source-pixel-area bounds also do not bound
resized detector work or elapsed time. Retain full-page baselines and measure
complete text, raw regions, CER/WER and latency on identical frozen inputs
before any default promotion. Keep this app gate off when qualifying a native
gutter change so the native engine still receives the unchanged whole page;
an app-crop arm changes recognition inputs and must be reported separately.

The subsequent rebuilt emulator run used the earlier experimental-crop
revision. It passed the named English two-column threshold and reached repeated
recognition, then failed a PDF export path comparison: the saved path and
requested path used different device-verified aliases of the same sandbox.
The harness now compares their verified canonical identities and still checks
the exported file itself. Rerun the retained experimental APK to continue
export and recovery qualification. This is evidence for that opt-in candidate,
not proof of complete text coverage or of current default-off behavior.

The [retained experimental-crop emulator run](https://github.com/CrispStrobe/CrispSorter/actions/runs/43293853454)
subsequently passed all 33 recorded steps, including native document and image
imports, the named quality fixtures, repeated recognition, PDF and Word export,
staging cleanup, and the system document picker. Its receipt explicitly records
synthetic inputs, no physical camera/device qualification, no system save
picker, and no representative corpus quality qualification. The APK predates
the current default-off gate; this result does not establish current-default
behavior or justify promoting the experimental option.


## Current gated-source Android delta

[Run 43298111978](https://github.com/CrispStrobe/CrispSorter/actions/runs/43298111978)
completed successfully with all 11 checks at app candidate
`0dbb1e69797f01dab819dfe59487564b5634c9db`, using the immutable v0.17.19 runtime.
The [source-bound receipt](../tests/results/ocr-mobile-ui/android-cc-crop-gate-20261112.json)
seals the exact APK, build/runtime receipts, fixture and raw-response hashes.
The same-run build passed clean application-source, installed-APK and native
mapped-section checks; its sole generated tracked exception is the regenerated
license inventory. The APK remains in the workflow artifact.

The omitted-gate single-column control retains its three expected lines. With
otherwise identical settings, omitted-gate two-column recognition remains at
CER **0.502857** with five regions; explicit `experimental_cc_column_crops: true`
uses nine real crops and reaches **0.022857**, passing the unchanged 0.25 ceiling,
required tokens and four-left/four-right column order. The opt-in still returns
`cond floor ready.` for `Second floor ready.`. A passing aggregate CER therefore
does not prove complete text capture. Native elapsed time is 20,734 ms for the
default column arm and 34,296 ms for opt-in; this is not a speed improvement.

Save/reload preserves opt-in result provenance without changing project defaults;
both original managed assets and the application process remain intact. The
observed kernel high-water RSS is 921,276 KiB; discrete samples and this short
run do not qualify sustained memory use. Export, repeated-page, picker and
recovery checks were not repeated: retain their earlier 33-step pre-gate source
attribution above. No physical camera/device or representative corpus was tested.

The receipt maps tested production OCR blobs to merged main `87cf0b44`, while
identifying different dedicated harness/workflow and unrelated main application
changes. It is evidence for the tested candidate and those identical OCR blobs,
not a device qualification of a full main APK. Keep the app crop gate off by
default, retain the immutable runtime pin and unchanged quality thresholds, and
keep native gutter qualification separate.

## Signed mobile release profile and integration handoff

The current `release.yml` Android and iOS Tauri commands omit
`--features ocr-mobile`; Cargo defaults are empty. They do not run the paired
native/ORT preparation used by the qualification lane. These are ordinary
**standard** mobile builds. Source-bound receipts now explicitly record
`mobile_release_profile`, including that this recipe does not include native
OCR and that OCR qualification is not established. This statement describes the
release recipe; package identity/signature inspection does not establish device
behavior or recognition quality.

A manual release may explicitly request `mobile_profile: ocr`. That request
currently fails closed before the all-platform build or retained-IPA download.
No input silently enables an unintegrated feature. Standard releases, desktop
retries and standard TestFlight remain available, including the existing exact
IPA/build/cutoff verification and intentional resume refusal. No new human
approval or admission file is required. This guard does not promote the
experimental crop option or an experimental native runtime.

The remaining integration is concrete:

1. Preserve the immutable production runtime lock. The current release pin is
   CrispEmbed v0.17.19 at the matching locked source/ggml commits, and published
   archives already exist; an unchanged baseline does not require releasing
   CrispEmbed main. A new candidate requires its own qualification and immutable
   archives before moving release/CI pins and hash locks together. Keep source
   overrides for unpublished experiments separate.
2. Reuse `build_mobile_crispembed.py`, `prepare_mobile_ort.py` and
   `prepare_ocr_cargo_env.py` with the exact intended lock; initialize native
   submodules recursively. Stage native receipt and license notices. Android
   needs `bundle_android_native_libs.sh --require-crispembed`, the exact ORT
   library, and `ocr-mobile` in the Tauri build. iOS needs the mobile config,
   `prepare_ocr_ios_project.py` with the device ORT framework and `ocr-mobile`
   before the existing manual signing/export route. Simulator linkage is a
   reference recipe, not a passed signed device build.
3. Use the clean sibling checkout layout described below and require lock
   agreement before copying qualification steps. Keep the existing source
   identity guard; do not reset files to conceal build inputs.
4. Bind the final signed APK and exported iOS app to the native and ORT code/data
   with the existing build/provenance validators. Record the signed IPA archive
   hash separately from its app manifest. Then replace the unsupported-profile
   refusal with objective final-artifact provenance checks; a requested OCR
   profile must still fail if native code, receipt, target, source or ORT does
   not match. A filename, feature flag or successful build alone is insufficient.
5. Execute the named application cases on the exact artifacts before claiming
   their behavior or quality. Current default-off Android behavior, the failing
   two-column case, signed iOS OCR-device integration and broader device/camera
   limits retain their existing evidence gates. Prior experimental-crop and
   simulator results cannot qualify another artifact/profile.

For bounded unpublished preparation use the existing target-selectable
`ocr-mobile-native.yml` lane, one target at a time. It already prepares runtime,
ORT, forced Cargo paths, notices and build identity. The separate signed OCR
draft selectors described below prepare native staging and final-artifact
provenance without enabling public OCR release or claiming device qualification.

For ordinary current-source artifacts, select a version/draft target bound to
that source before building: the retained v0.15.0 packages and receipts bind an
older commit and cannot be reused as current-source proof. Existing Android and
App Store signing secrets are available by name; their presence is not a fresh
signing test. Direct-download Windows production signing and macOS Developer ID
secrets remain absent, so those platforms can produce explicitly unsigned drafts
but public desktop publication remains blocked by its existing signing gate.

## Clean-source preparation for signed mobile builds

Android and iOS release jobs now check out the application and its dependencies
as sibling repositories below `repos/`: `CrispSorter`, `CrispEmbed`, `CrispASR`
and `crisp-docx`. This matches the committed Cargo path dependencies. Their
shell working directory is the application checkout; Rust cache workspace paths
are updated to match. Build, signing and upload commands retain their existing
relative paths. No tracked Cargo manifest rewrite is needed. Other release jobs
retain their existing checkout layout and recipe.

`check_mobile_release_layout.py` validates the clean application source with the
existing source-identity guard, exact release runtime/ggml checkout, clean pinned
ASR/docx checkouts and all seven committed sibling Cargo paths. It prints the
source/tree/manifest/dependency identity without changing them. The native
checkout initializes submodules recursively. Standard builds do not require an
independent experimental mobile lock to match the production lock. Future OCR
preparation must additionally pass `--require-ocr-lock` before using the existing
OCR build/provenance recipe. Final release receipts discover these external
sibling commits, while preserving nested sibling discovery for desktop jobs.

This resolves the tracked Cargo-path rewrite conflict. It does not enable OCR,
change native/runtime pins, execute a platform build or weaken the source receipt
guard. The fixture test creates real Git sibling repositories and uses offline,
locked Cargo metadata to check all seven dependencies resolve while the
application manifest/lock/source remain clean; this does not compile code.

The signed OCR draft implementation follows this sequence:

1. Reuse this clean checkout layout and require production/mobile lock agreement.
   Stage native build, pinned ORT, forced Cargo environment, notices and the
   `ocr-mobile` profile using the existing preparation helpers. Keep experimental
   source overrides out of this production-pin recipe.
2. Apply Android native bundling and device iOS framework/project preparation
   before the existing signing/archive/export steps. Keep the first OCR-capable
   draft path unpublished and separate from automatic TestFlight upload; retain
   exact failures and generated build records.
3. Run the existing final-artifact native/ORT provenance validator before placing
   additional untracked files in the application root. Android signed output can
   stay in `RUNNER_TEMP`; iOS export already lives below ignored `build/`.
   Keep receipts outside the source tree until the clean-source check completes.
   Do not broaden Git ignores or reset manifests/lockfiles to conceal changes.
4. Preserve the original committed Cargo.lock. If dependency resolution changes
   it, stop and reconcile/commit that change before rebuilding; the source guard
   must continue rejecting a changed dependency lock. The generated license
   inventory remains the existing explicitly recorded exception.
5. Bind the signed APK/exported app and signed IPA hash to the build records, then
   execute the applicable application/quality cases on those exact artifacts.
   Only replace an unsupported OCR-profile refusal with objective checks after
   those build/provenance paths exist. A passed preparation test is neither a
   signed platform build nor an application/quality qualification result.

## Production-pinned signed OCR drafts

The Release workflow now has two explicit manual selectors:
`android_ocr_draft` and `ios_ocr_draft`. Each runs only its named mobile job,
requires `publish=false` and `mac_appstore=false`, and retains a workflow artifact
named `signed-ocr-draft-TARGET-unqualified`. Public `mobile_profile: ocr` remains
unsupported. The selectors do not create/alter a GitHub release, upload to App
Store Connect, assign TestFlight groups or invoke the desktop matrix. Invalid
publish/App Store combinations fail the draft preflight; they cannot divert the
request into a separate Mac App Store build. Existing standard builds and
TestFlight retain their own behavior.

`prepare_signed_mobile_ocr_draft.py` requires the committed production and
mobile runtime locks to select the same source/ggml identity and validates the
clean sibling layout with `--require-ocr-lock`. There is no candidate source,
lock or artifact override. It builds the existing production-source native
runtime with two jobs, stages the pinned official ORT through the existing
archive/header/library/notice checks, forces runtime/protoc/ORT Cargo paths,
stages native/ORT notices and records preparation identity outside application
source. Native license metadata is collected from the locked exact target/feature
graph. Android adds the qualification recipe's 16 KiB native linker settings and
native/ORT libraries; iOS adds the mobile configuration and exact device ORT
framework before the existing archive-unsigned/manual-sign-at-export procedure.
The new route requires the retained signing configuration; it cannot silently
fall back to a development Android key or unsigned iOS app.

`record_signed_mobile_ocr_draft.py` independently rechecks the Android APK
signature/production signing record or the provisioned iOS app signature, then
runs the existing final-app native/ORT code/data and notice provenance recorder.
It requires the same prepared application/runtime identity and binds the final
signed APK/IPA bytes to the app-build receipt. Only after every validation and
metadata copy succeeds does it write `signed-draft.json` with
`validation_status: passed`. This is a **provenance** result, never device or OCR
quality qualification. Binary names and artifact names remain explicitly
unqualified even on success. A failure retains the binary with that label and
`failure.json`, raises the error, and omits the success marker; partial output
must not be interpreted as verified. The artifact retention path is an explicit
allowlist of signed package and build records, never keychains, P12/P8 material
or provisioning secret directories.

The implementation has focused mocked preparation/provenance and selector
safety tests. The [iOS-only signed draft run](https://github.com/CrispStrobe/CrispSorter/actions/runs/43320049351)
was queued at this update; inspect its result before claiming a platform build
or signing result. No new upload or device execution is established. In particular, linking/stripping behavior of the
signed iOS OCR-device app is not yet verified; required native symbol/provenance
checks fail rather than being bypassed if export strips needed evidence.

Next, inspect the iOS-only run and execute the Android selector on the exact
committed source when the existing Android candidate run has finished. Inspect the retained build/provenance result and original
failure logs before running application cases. The iOS artifact is not in
TestFlight; uploading an OCR-capable IPA requires a source/hash-bound route after its
actual build and application evidence exist.
Current-default Android recognition, the two-column case, representative corpus,
physical camera/provider behavior and signed iOS application execution retain
the separate qualification requirements above. This draft implementation does
not move runtime pins, turn on experimental crop behavior or remove the public
OCR profile refusal.
