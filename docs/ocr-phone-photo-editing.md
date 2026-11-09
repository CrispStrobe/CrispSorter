# Phone photo preparation

On phone layouts, **Prepare photo** has three visible steps: **Check**, **Corners**, and **Lighting**. Only the selected step's controls are shown. Desktop layouts retain all preparation panels.

Choosing **Corners** fits the page and opens a focused editor. The project header, command bar, page navigation, image tools, and preparation panel give their space to the image. **Back to photo tools**, **Apply perspective**, zoom, and Fit page remain available. Each corner handle and the focused editor's action controls have a minimum 44 CSS pixel touch target. Zoom can be changed while placing corners; Fit page brings all four handles back into view.

Dragging a corner changes only the pending selection. Back or Escape restores the tool chrome without changing image pixels. Apply sends the selected quadrilateral to the existing perspective transform. The preserved original remains available, and Undo restores the previous image and review evidence.

## Verification scope

The browser harness exercises the production Svelte components at 390×844. Its phone corner case measures a fitted portrait page of at least 300×420 CSS pixels, a canvas at least 540 CSS pixels high, and four complete touch targets inside both the canvas and the screen. It sends native browser touch input through Chromium's input protocol and checks that the trusted touch pointer updates the expected image coordinates. It also checks Back, Escape, Apply, preservation of the original, Undo, and the absence of recognition calls.

The browser report retains measured geometry and touch evidence in `phoneCornerEvidence`, and captures `ocr-phone-corner-focus.png`. This case uses a synthetic document and transform client. It qualifies the user interface and coordinate handling; native perspective quality and physical phone camera/provider behavior require their separate native and device checks. A passing hosted run and its source revision must be recorded before claiming the new browser case has passed.
