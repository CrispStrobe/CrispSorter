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

No connected Android tools/device or Apple toolchain/device was available in
the recovery workspace when this protocol was added. The protocol itself is
not an execution receipt. On iOS, use a normally provisioned device build and
the observed cases below; the automated lane currently covers a simulator app
and standalone native inference, not physical-device application commands.

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
