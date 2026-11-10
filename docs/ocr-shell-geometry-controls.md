# Source-matched application shell geometry controls

This lightweight native WebKit fixture isolates the shell's viewport and CSS
contract. It extracts the exact viewport tag, global CSS and Svelte shell style
block from the current commit and historical source
`646dbc486438d0e8955af193b30e6382632cdf45`. It records each source and generated
HTML hash. The simplified shell markup contains navigation and a Tools popup;
it does not mount the OCR application or call its commands.

The native host fills an iPhone 16 Pro / iOS 18.5 window with a normal WKWebView.
The historical negative control must reproduce automatic native content
insetting and offscreen Tools placement. If that condition is absent, the
suite fails: a different embedding cannot silently qualify the fix.

Current-source controls require WebKit's automatic content insets to be zero
and each explicit shell padding to equal UIKit's measured safe-area inset.
They measure portrait navigation and popup bounds, rotate into the existing
landscape tablet sidebar breakpoint, and repeat the portrait popup with
[`WKPreferences.minimumFontSize`](https://developer.apple.com/documentation/webkit/wkpreferences/minimumfontsize)
set to 28 points. This is a native WebKit font preference, not a claim about a
physical device's accessibility settings. A popup hidden with the mobile
navigation in landscape is checked as hidden; it is not reported as visible.

A separate pinned Chromium control measures desktop and phone layouts, including
a 740 × 360 landscape viewport with asymmetric 62/34 point side insets and a
21 point bottom inset supplied through CDP's real CSS safe-area override. It
asserts that computed padding matches those values and that every popup action
can be scrolled into its bounded menu and focused. A 32px text-style stress case
also requires the popup to remain above the navigation's actual enlarged
height. This browser case does not stand in for native WebKit inset behavior.

Receipts, source HTML, raw metrics, accessibility trees, screenshots, Xcode logs
and result bundles are retained on both success and failure. Building this
small Swift fixture does not rebuild Rust or CrispSorter. The complete
corrected application still requires its independent Tools → OCR → Add
documents probe with exact application and installed-bundle identities.
