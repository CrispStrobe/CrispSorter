# Retained iOS application navigation probe

The initial simulator startup screenshot showed the Inbox but did not show the application's bottom navigation. Startup alone does not establish that a user can reach the OCR workspace. This probe investigates that specific gap before changing shell CSS.

The independent workflow uses the unchanged simulator application from CrispSorter `646dbc486438d0e8955af193b30e6382632cdf45`, run `42619149388`, artifact `13680296839`. `tools/ios-shell-probe/artifact.json` pins the outer artifact, inner application ZIP, source revision, bundle identity, and iPhone 16 Pro / iOS 18.5 simulator. Staging rejects a different digest, duplicate or escaping archive member, symlink, oversized bundle, mismatched receipt, or different application byte identity. It verifies the installed bundle before and after the test.

The test target builds only a small Swift UI-test host. It launches the separately installed CrispSorter application by its bundle identifier using [`XCUIApplication.init(bundleIdentifier:)`](https://developer.apple.com/documentation/xcuiautomation/xcuiapplication/init(bundleidentifier:)). [XcodeGen's project specification](https://yonaskolb.github.io/XcodeGen/Docs/ProjectSpec.html) supplies the separate UI-test target and runner host. The retained application is not rebuilt, rewritten, or repackaged.

The probe waits for startup, saves accessibility hierarchies, button frames, hittability, and screenshots, then attempts **Tools → OCR**. A passing result requires the OCR project-name field and reachable **Add documents** control. It fails explicitly if Tools is absent, outside the application frame, or not hittable, or if the menu/workspace cannot be reached. Evidence is saved before a failure interrupts the test, and the Xcode result bundle is retained.

This is simulator navigation evidence for the pinned historical app. It does not qualify subsequent shell changes, OCR accuracy, camera capture, document providers, or physical devices. New application builds need their own source and bundle identity and a repeated navigation test. No layout cause or fix should be inferred from the startup screenshot alone.

## First hosted attempt

Run `42635676372`, probe source `2c47e3060eb90ab4c88636839cbfb80224be7ea4`, validated the retained artifact and all 13 application files on Xcode 16.4 / iOS SDK 18.5. The first `simctl list runtimes --json` call then timed out after 60 seconds. The application was not installed and UI tests did not start. Its [original receipt and qualification record](../tests/results/ocr-ios-shell/hosted-2c47e306/qualification.json) are retained with verified artifact digest `ca8b81fc049fbdc4c996b51643b002de557b78689b0db511744df2686b9d0f76`.

Simulator discovery now has two bounded attempts, with 90 and 180 second deadlines, recorded individually. This accommodates a cold CoreSimulator service while preserving the same device/runtime pins and UI assertions. A discovery timeout is an infrastructure failure, not a navigation result.

## Retained application geometry result

Run `42637514755` at probe source `61d1585f49d872502e7ca189815ecfb62a143c89` completed simulator discovery, installation, and the independent UI-test build. The installed historical application matched its pinned bundle hash before launch. Its [raw hierarchy, screenshot, logs, and qualification record](../tests/results/ocr-ios-shell/hosted-61d1585/qualification.json) are preserved with verified artifact SHA-256 `23ddb79185e5d2f85f5d75a3fb290e7b3afef5d61c92e4007fc15cf27d7a5d64`.

The actual 402 × 874 point application contained Tools at `(268, 892, 67, 41)` and its navigation bar at `(0, 884, 402, 52)`. Both were below the screen. The document began at y = 62; its visible safe content was 778 points high, while the shell retained its 874-point `100vh` height. This diagnoses an offscreen geometry defect. The three shell source files were byte-identical between this application source and `b876fb43eb19cc7de44760400e50735aa27c2c78`; that comparison does not equate their complete binaries or later workspace features.

The test did **not** pass navigation: its diagnostic enumeration asked XCTest to determine the hittability of an unrelated offscreen column-resize button, causing one test failure before the Tools assertion. The Inbox hierarchy and screenshot survived; `navigation.json` and the after-test bundle hash were not produced. Follow-up runners restrict hit-testing to relevant controls and preserve partial reports. The actual corrected iOS application still needs its own successful navigation result.
