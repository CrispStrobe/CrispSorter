<script lang="ts">
  import { Camera, Crop, RotateCcw, ScanLine, WandSparkles, X, AlertTriangle } from "lucide-svelte";
  import type { PhotoAssessment } from "$lib/ocr/client";
  import { CORNER_NAMES, photoQuadError, type PhotoQuad } from "$lib/ocr/photo";

  let { width, height, assessment, assessing = false, disabled = false,
    perspective = false, quad, angle = 0, onassess, onperspective, onquad,
    onangle, onapplyperspective, onapplyangle, onwhiten, canWhiten = false, onclose,
  }: {
    width: number; height: number; assessment: PhotoAssessment | null;
    assessing?: boolean; disabled?: boolean; perspective?: boolean; quad: PhotoQuad;
    angle?: number; canWhiten?: boolean; onassess: () => void; onperspective: (enabled: boolean) => void;
    onquad: (quad: PhotoQuad) => void; onangle: (angle: number) => void;
    onapplyperspective: () => void; onapplyangle: () => void; onwhiten: () => void; onclose: () => void;
  } = $props();
  let mobileStep = $state<"check" | "corners" | "lighting">("check");
  const cornerError = $derived(photoQuadError(quad, width, height));
  const angleValid = $derived(Number.isFinite(angle) && Math.abs(angle) <= 20 && Math.abs(angle) >= 0.05);
  function chooseStep(step: typeof mobileStep) {
    mobileStep = step;
    onperspective(step === "corners");
  }
  function coordinate(index: number, axis: "x" | "y", value: number) {
    onquad(quad.map((point, i) => i === index ? { ...point, [axis]: value } : { ...point }) as PhotoQuad);
  }
</script>

<section class="photo-preparation" aria-label="Photo preparation">
  <header><div><Camera size={16} /><strong>Prepare photo</strong><span>Original preserved · Undo available</span></div><button class="close" onclick={onclose} aria-label="Close photo preparation"><X size={17} /></button></header>
  <nav class="photo-step-nav" aria-label="Photo tools">
    <button aria-pressed={mobileStep === "check"} onclick={() => chooseStep("check")} disabled={disabled}><ScanLine size={14} />Check</button>
    <button aria-pressed={mobileStep === "corners"} onclick={() => chooseStep("corners")} disabled={disabled}><Crop size={14} />Corners</button>
    <button aria-pressed={mobileStep === "lighting"} onclick={() => chooseStep("lighting")} disabled={disabled}><WandSparkles size={14} />Lighting</button>
  </nav>
  <div class="photo-controls">
    <div class="photo-step quality" class:mobile-active={mobileStep === "check"}>
      <h3><span>1</span> Check the capture</h3>
      <p>Keep the complete page in frame. Use even light and check small text at full size.</p>
      <button onclick={onassess} disabled={disabled || assessing}><ScanLine size={14} />{assessing ? "Checking photo…" : "Check photo quality"}</button>
      {#if assessment}
        <div class="quality-result" aria-live="polite">
          <strong>{assessment.correction_degrees === null ? "No reliable tilt estimate" : `Suggested correction: ${assessment.correction_degrees.toFixed(1)}°`}</strong>
          <span>Image checks are guidance; inspect the page before recognition.</span>
          {#if assessment.warnings.length}<ul>{#each assessment.warnings as warning}<li><AlertTriangle size={12} />{warning}</li>{/each}</ul>{/if}
        </div>
      {/if}
    </div>
    <div class="photo-step" class:mobile-active={mobileStep === "corners"}>
      <h3><span>2</span> Square the page</h3>
      <p>Drag the four corner handles to the paper edges, then apply. Arrow keys move a focused corner; Shift moves 10 pixels.</p>
      <button class:active={perspective} aria-pressed={perspective} onclick={() => onperspective(!perspective)} disabled={disabled}><Crop size={14} />{perspective ? "Finish choosing corners" : "Correct perspective"}</button>
      {#if perspective}
        <button class="primary" onclick={onapplyperspective} disabled={disabled || !!cornerError}>Apply perspective</button>
        {#if cornerError}<p class="warning" role="status">{cornerError}</p>{/if}
        <details><summary>Exact corner positions</summary><div class="coordinate-grid">
          {#each quad as point, index}<label>{CORNER_NAMES[index]}<div><input type="number" min="0" max={width - 1} step="1" value={point.x} aria-label={`${CORNER_NAMES[index]} corner X`} oninput={(event) => coordinate(index, "x", event.currentTarget.valueAsNumber)} disabled={disabled} /><input type="number" min="0" max={height - 1} step="1" value={point.y} aria-label={`${CORNER_NAMES[index]} corner Y`} oninput={(event) => coordinate(index, "y", event.currentTarget.valueAsNumber)} disabled={disabled} /></div></label>{/each}
        </div></details>
      {/if}
    </div>
    <div class="photo-step" class:mobile-active={mobileStep === "lighting"}>
      <h3><span>3</span> Level & brighten</h3>
      <label class="angle-label">Tilt correction (degrees)<div class="angle-control"><input type="number" min="-20" max="20" step="0.1" value={angle} oninput={(event) => onangle(event.currentTarget.valueAsNumber)} disabled={disabled} /><span>+ clockwise</span></div></label>
      <button onclick={onapplyangle} disabled={disabled || !angleValid}><RotateCcw size={14} />Apply tilt correction</button>
      <button onclick={onwhiten} disabled={disabled || !canWhiten}><WandSparkles size={14} />Flatten shadows</button>
      <p>For uneven lighting, flatten shadows after correcting tilt. Use Original to compare before recognizing.</p>
    </div>
  </div>
</section>

<style>
  .photo-preparation { background: #172435; border-bottom: 1px solid #35516e; color: #dce8f5; flex: 0 1 auto; min-height: 96px; max-height: 43%; overflow: auto; font-size: 11px; }
  header { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 8px 12px; border-bottom: 1px solid #30445b; }
  header > div { display: flex; align-items: center; gap: 7px; flex-wrap: wrap; }
  header span { color: #a6b7cc; font-size: 10px; }
  .photo-step-nav { display: none; }
  .photo-controls { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .photo-step { padding: 10px 12px; min-width: 0; border-right: 1px solid #30445b; }
  .photo-step:last-child { border: 0; }
  h3 { margin: 0 0 7px; display: flex; align-items: center; gap: 7px; font-size: 11px; }
  h3 span { display: grid; place-items: center; border-radius: 50%; width: 20px; height: 20px; background: #2c4766; color: #bfdcff; }
  p { color: #a8bad0; line-height: 1.5; margin: 6px 0 8px; }
  button { display: inline-flex; align-items: center; justify-content: center; gap: 6px; min-height: 32px; border: 1px solid #44607c; border-radius: 6px; background: #20364e; color: #dcecff; padding: 6px 9px; margin: 3px 4px 3px 0; font: inherit; }
  button:hover:not(:disabled), button.active { background: #304f71; border-color: #79a5d7; }
  button.primary { background: #286bbd; }
  button.close { min-width: 32px; flex: none; margin: 0; background: transparent; border-color: transparent; }
  .angle-label { display: block; color: #cbd8e9; font-size: 10px; margin-top: 5px; }
  .angle-control { display: flex; gap: 8px; align-items: center; margin-top: 5px; }
  .angle-control input { width: 95px; }
  .angle-control span { color: #9eafc5; }
  .quality-result { display: flex; flex-direction: column; gap: 6px; padding: 8px 0; }
  .quality-result > span { color: #9eafc5; line-height: 1.4; }
  ul { list-style: none; padding: 0; margin: 0; }
  li { display: flex; gap: 6px; align-items: flex-start; color: #efc58b; line-height: 1.5; padding: 3px 0; }
  li :global(svg) { flex: none; margin-top: 2px; }
  .warning { color: #efc58b; }
  details { margin-top: 7px; }
  summary { cursor: pointer; color: #accfff; padding: 5px 0; }
  .coordinate-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 7px; padding-top: 7px; }
  .coordinate-grid label { min-width: 0; font-size: 10px; }
  .coordinate-grid label > div { display: flex; gap: 3px; margin-top: 3px; }
  .coordinate-grid input { width: 50%; padding: 5px !important; }
  @media (max-width: 1200px) { .photo-controls { grid-template-columns: 1fr; } .photo-step { border-right: 0; border-bottom: 1px solid #30445b; } }
  @media (max-width: 780px) {
    button { min-height: 44px; }
    button.close { min-width: 44px; }
    .photo-preparation { max-height: 50%; }
    header span { display: none; }
    .photo-step-nav { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 4px; padding: 5px 8px; border-bottom: 1px solid #30445b; }
    .photo-step-nav button { margin: 0; min-width: 0; padding: 6px 3px; }
    .photo-step-nav button[aria-pressed="true"] { background: #304f71; border-color: #79a5d7; color: #fff; }
    .photo-controls { grid-template-columns: 1fr; }
    .photo-step { display: none; border: 0; }
    .photo-step.mobile-active { display: block; }
    .angle-control input { font-size: 16px !important; min-height: 44px; }
  }
</style>
