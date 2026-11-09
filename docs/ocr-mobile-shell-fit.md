# Mobile application navigation and safe areas

## Observed failure

The retained iOS application built from `646dbc486438d0e8955af193b30e6382632cdf45`
was installed with its original bundle hash in [probe run 42637514755](https://github.com/CrispStrobe/CrispSorter/actions/runs/42637514755).
On the iPhone 16 Pro simulator running iOS 18.5, the accessibility tree measured
an application frame of 402 × 874 points. WebKit placed the document at y = 62,
with 778 points inside the safe area. The shell still occupied 874 points because
its height was `100vh`. Application navigation therefore lay at y = 884–936;
Tools occupied x = 268, y = 892, width = 67, height = 41. The control was present
in the accessibility tree but below the visible screen.

The probe retained the Inbox screenshot and full accessibility tree before its
diagnostic enumeration asked XCTest about an unrelated offscreen column-resize
button. That hittability query failed, so the run did not reach Tools or OCR and
did not produce a successful navigation report. This observed geometry is not a
claim that the corrected application has passed native navigation.

The evidence ZIP is artifact `13686169363`, 59,681,970 bytes, SHA-256
`23ddb79185e5d2f85f5d75a3fb290e7b3afef5d61c92e4007fc15cf27d7a5d64`.
The retained bundle hash is
`a86a9f6fb3d8e39383716be1d3318bc17bcdf96f0b18902699df85bf2d66b7c3`.

## Layout contract

The viewport now uses `viewport-fit=cover`. The shell has a bounded `100dvh`
height, with `100vh` as a fallback, and applies all four safe-area insets as
border-box padding. The bottom navigation no longer applies the bottom inset a
second time. This pairs WebKit's full-screen viewport opt-in with explicit safe
content placement, following [WebKit's viewport and safe-area guidance](https://webkit.org/blog/7929/designing-websites-for-iphone-x/).
Replacing `100vh` with a percentage alone would not establish that contract in
the observed automatically inset WebView.

The Tools popup stays above the navigation and home indicator, has a bounded
scrollable height, and respects landscape side insets. Phone navigation and
popup actions have a minimum height of 44 CSS pixels. Desktop safe-area insets
normally resolve to zero and its existing sidebar breakpoints remain in place.

## Qualification scope

The source change and the original native failure are saved separately. A
source-matched WebKit geometry control is being prepared. Actual corrected-app
Tools → OCR → Add documents navigation remains pending until the next compiled
iOS application is installed and the strict targeted UI probe succeeds.
Simulator results do not qualify physical camera capture or external document
providers.
