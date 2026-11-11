<script lang="ts">
  import type { OcrProject } from "$lib/ocr/model";
  import {
    capturePassageSplit, capturePassageMerge, capturePassageRepair,
    passageEligibility, previewRepairText,
    type PassagePreview, type RepairPreview, type PassageRepairKind,
    type PassageSeparator, type SplitCoverage,
  } from "$lib/ocr/passages";

  let { project, pageId, regionId, mode, offset = 0, imageUrl, onapply, onrepair, onclose }: {
    project: OcrProject;
    pageId: string;
    regionId: string;
    mode: "split" | "merge" | "repair";
    offset?: number;
    imageUrl: (path: string) => string;
    onapply: (preview: PassagePreview) => string | null;
    onrepair: (preview: RepairPreview, selectedIds: string[]) => string | null;
    onclose: () => void;
  } = $props();

  let dialog = $state<HTMLDialogElement>();
  let coverageMode = $state<"shared" | "divide" | null>(null);
  let axis = $state<"x" | "y">("y");
  let position = $state<number | undefined>();
  let firstSide = $state<"low" | "high">("low");
  let separator = $state<PassageSeparator>("\n");
  let repairKind = $state<PassageRepairKind>("normalize-line-endings");
  let selectedIds = $state<string[]>([]);
  let visibleStart = $state(0);
  let actionError = $state("");
  const pageSize = 100;
  const page = $derived(project.pages.find((item) => item.id === pageId));
  const source = $derived(page?.regions.find((item) => item.id === regionId));
  const eligibility = $derived(page ? passageEligibility(page, regionId) : null);
  const title = $derived(mode === "split" ? "Split passage at caret" : mode === "merge" ? "Merge adjacent passages" : "Repair passage text");
  const previewResult = $derived.by((): { structural: PassagePreview | null; repair: RepairPreview | null; error: string } => {
    try {
      if (mode === "repair") return { structural: null, repair: capturePassageRepair(project, pageId, [regionId], repairKind), error: "" };
      if (mode === "merge") {
        if (!eligibility?.nextMergeId) return { structural: null, repair: null, error: "Choose a passage with an immediately following included passage of the same kind." };
        return { structural: capturePassageMerge(project, pageId, [regionId, eligibility.nextMergeId], separator), repair: null, error: "" };
      }
      if (!coverageMode) return { structural: null, repair: null, error: "" };
      const coverage: SplitCoverage = coverageMode === "shared" ? { mode: "shared" }
        : { mode: "divide", axis, position: position ?? Number.NaN, firstSide };
      return { structural: capturePassageSplit(project, pageId, regionId, offset, coverage), repair: null, error: "" };
    } catch (error) {
      return { structural: null, repair: null, error: error instanceof Error ? error.message : String(error) };
    }
  });
  const selected = $derived(new Set(selectedIds));
  const visibleChanges = $derived(previewResult.repair?.changes.slice(visibleStart, visibleStart + pageSize) ?? []);
  const proposedRepair = $derived.by(() => {
    if (!previewResult.repair) return "";
    // A different repair kind may render before the selection-reset effect.
    const knownIds = new Set(previewResult.repair.changes.map((change) => change.id));
    return previewRepairText(previewResult.repair, selectedIds.filter((id) => knownIds.has(id)));
  });
  const sourceRegions = $derived(page?.regions.filter((item) => item.id === regionId || (mode === "merge" && item.id === eligibility?.nextMergeId)) ?? []);
  const imageViewBox = $derived.by(() => {
    if (!page || !sourceRegions.length) return "0 0 1 1";
    const x = Math.max(0, Math.min(...sourceRegions.map((item) => item.x)) - 16);
    const y = Math.max(0, Math.min(...sourceRegions.map((item) => item.y)) - 16);
    const right = Math.min(page.width, Math.max(...sourceRegions.map((item) => item.x + item.w)) + 16);
    const bottom = Math.min(page.height, Math.max(...sourceRegions.map((item) => item.y + item.h)) + 16);
    return `${x} ${y} ${right - x} ${bottom - y}`;
  });
  const warnings = $derived(previewResult.structural?.warnings ?? previewResult.repair?.warnings ?? []);

  $effect(() => { if (dialog && !dialog.open) dialog.showModal(); });
  $effect(() => {
    const repair = previewResult.repair;
    selectedIds = repair?.changes.filter((change) => !change.risky).map((change) => change.id) ?? [];
    visibleStart = 0;
    actionError = "";
  });
  function chooseDivision(nextAxis: "x" | "y" = axis) {
    axis = nextAxis;
    coverageMode = "divide";
    if (source) position = source[nextAxis] + (nextAxis === "x" ? source.w : source.h) / 2;
  }
  function toggleChange(id: string, checked: boolean) {
    selectedIds = checked ? [...new Set([...selectedIds, id])] : selectedIds.filter((item) => item !== id);
    actionError = "";
  }
  function whitespace(text: string): string {
    return text ? text.replace(/\r/g, "␍").replace(/\n/g, "↵\n").replace(/\t/g, "⇥").replace(/ /g, "·") : "(remove)";
  }
  function apply() {
    actionError = "";
    if (mode === "repair" && previewResult.repair && selectedIds.length) actionError = onrepair(previewResult.repair, selectedIds) ?? "";
    else if (previewResult.structural) actionError = onapply(previewResult.structural) ?? "";
  }
</script>

<dialog bind:this={dialog} class="passage-dialog" aria-labelledby="passage-dialog-title" onclose={onclose}>
  <h2 id="passage-dialog-title">{title}</h2>
  <p class="dialog-context">{page?.name} · Passage {(page?.regions.findIndex((item) => item.id === regionId) ?? -1) + 1}</p>
  {#if mode === "split"}
    <p class="help">The caret separates the text exactly as shown below. Choose how the two passages relate to the source image; text alone cannot determine their image areas.</p>
    <fieldset>
      <legend>Source area for the new passages</legend>
      <label class="radio"><input type="radio" name="passage-coverage" checked={coverageMode === "shared"} onchange={() => (coverageMode = "shared")} />Shared source bounds</label>
      <p class="choice-help">Both passages retain the same area. Plain text and JSON keep their exact text order. Spatial exports require separate coverage or merging the passages again.</p>
      <label class="radio"><input type="radio" name="passage-coverage" checked={coverageMode === "divide"} onchange={() => chooseDivision()} />Divide source area</label>
      {#if coverageMode === "divide" && source}
        <div class="division-fields">
          <label>Division direction<select aria-label="Division direction" value={axis} onchange={(event) => chooseDivision(event.currentTarget.value as "x" | "y")}>
            <option value="y">Top and bottom</option><option value="x">Left and right</option>
          </select></label>
          <label>Division position (pixels)<input type="number" aria-label="Division position (pixels)" bind:value={position} step="any" min={source[axis]} max={source[axis] + (axis === "x" ? source.w : source.h)} /></label>
          <label>First passage area<select aria-label="First passage area" bind:value={firstSide}>
            <option value="low">{axis === "x" ? "Left side" : "Top side"}</option><option value="high">{axis === "x" ? "Right side" : "Bottom side"}</option>
          </select></label>
        </div>
        <p class="choice-help">Position is measured from the page's {axis === "x" ? "left" : "top"} edge. The cut must be inside {source[axis]}–{source[axis] + (axis === "x" ? source.w : source.h)} pixels. Verify the proposed areas against the image.</p>
      {/if}
    </fieldset>
    <div class="text-comparison">
      <label>First passage<textarea readonly aria-label="First passage preview" dir="auto" value={source?.text.slice(0, offset) ?? ""}></textarea></label>
      <label>Second passage<textarea readonly aria-label="Second passage preview" dir="auto" value={source?.text.slice(offset) ?? ""}></textarea></label>
    </div>
  {:else if mode === "merge"}
    <p class="help">Merge this passage with the immediately following passage of the same kind. Choose the exact text inserted between them; existing spaces and line breaks remain.</p>
    <label>Passage separator<select aria-label="Passage separator" bind:value={separator}>
      <option value="">No separator</option><option value=" ">Space</option><option value={"\n"}>Line break</option><option value={"\n\n"}>Blank line</option>
    </select></label>
    {#if previewResult.structural}<div class="text-comparison">
      <label>Current passages<textarea readonly aria-label="Current passages" dir="auto" value={previewResult.structural.beforeText}></textarea></label>
      <label>Merged passage preview<textarea readonly aria-label="Merged passage preview" dir="auto" value={previewResult.structural.afterText}></textarea></label>
    </div>{/if}
    <p class="help">The merged source area encloses both passages. The original components and their evidence remain available individually.</p>
  {:else}
    <p class="help">Preview one repair operation in this passage. Only checked changes will be applied. Joins, dehyphenation, and spacing changes need explicit review.</p>
    <label>Repair operation<select aria-label="Repair operation" bind:value={repairKind}>
      <option value="normalize-line-endings">Normalize line endings</option>
      <option value="trim-trailing-spaces">Trim trailing spaces</option>
      <option value="collapse-inline-spaces">Collapse inline spaces</option>
      <option value="join-lines">Join continued lines</option>
      <option value="dehyphenate">Join words hyphenated across lines</option>
    </select></label>
    {#if previewResult.repair}
      <div class="repair-summary" data-text-repair-preview>{previewResult.repair.changes.length} proposed change{previewResult.repair.changes.length === 1 ? "" : "s"} · {selectedIds.length} selected</div>
      {#if previewResult.repair.changes.length}
        <label class="select-all"><input type="checkbox" checked={previewResult.repair.changes.every((change) => selected.has(change.id))}
          onchange={(event) => (selectedIds = event.currentTarget.checked ? previewResult.repair!.changes.map((change) => change.id) : [])} />Select all text repairs</label>
        <div class="repair-list">
          {#each visibleChanges as change, index (change.id)}
            <label class="repair-change" data-text-repair-change={change.id}>
              <input type="checkbox" aria-label={`Apply text repair ${visibleStart + index + 1}`} checked={selected.has(change.id)} onchange={(event) => toggleChange(change.id, event.currentTarget.checked)} />
              <span><span class="context" dir="auto">{change.context}</span><span class="replacement"><del>{whitespace(change.before)}</del><span aria-hidden="true"> → </span><ins>{whitespace(change.after)}</ins></span>
              {#if change.risky}<small>Review needed</small>{/if}</span>
            </label>
          {/each}
        </div>
        {#if previewResult.repair.changes.length > pageSize}<div class="pagination">
          <button type="button" disabled={visibleStart === 0} onclick={() => (visibleStart = Math.max(0, visibleStart - pageSize))}>Previous text repairs</button>
          <span>{visibleStart + 1}–{Math.min(visibleStart + pageSize, previewResult.repair.changes.length)} of {previewResult.repair.changes.length}</span>
          <button type="button" disabled={visibleStart + pageSize >= previewResult.repair.changes.length} onclick={() => (visibleStart += pageSize)}>Next text repairs</button>
        </div>{/if}
        <p class="choice-help">In change details, · marks a space, ⇥ a tab, and ↵ a line break.</p>
      {:else}<p class="help">No matching repairs were found. Choose another operation or keep the current text.</p>{/if}
      <div class="text-comparison">
        <label>Current passage<textarea readonly aria-label="Current passage" dir="auto" value={previewResult.repair.beforeText}></textarea></label>
        <label>Proposed passage<textarea readonly aria-label="Proposed passage" dir="auto" value={proposedRepair}></textarea></label>
      </div>
    {/if}
  {/if}
  {#if mode !== "repair" && page && sourceRegions.length}
    <div class="source-preview"><span>Source coverage preview</span>
      <svg viewBox={imageViewBox} role="img" aria-label="Passage source coverage preview">
        <image href={imageUrl(page.imagePath)} width={page.width} height={page.height} />
        {#each previewResult.structural?.regions ?? sourceRegions as candidate, index (candidate.id)}
          <rect data-passage-preview-area={index + 1} class:second={index === 1} x={candidate.x} y={candidate.y} width={candidate.w} height={candidate.h} vector-effect="non-scaling-stroke"><title>{mode === "split" ? `Passage ${index + 1}` : "Merged passage"} source area</title></rect>
        {/each}
      </svg>
      {#if mode === "split" && coverageMode === "divide"}<p class="choice-help">Blue: first passage. Amber: second passage.</p>{/if}
    </div>
  {/if}
  {#if warnings.length}<ul class="warnings">{#each warnings as warning}<li>{warning}</li>{/each}</ul>{/if}
  {#if previewResult.error || actionError}<p class="error" role="alert">{actionError || previewResult.error}</p>{/if}
  <p class="help">{mode === "repair" ? "Original OCR evidence remains available." : "New passages are manual text; original OCR scores remain attached only to their preserved source components."} Affected approvals reopen. Undo restores the previous state.</p>
  <div class="dialog-actions"><button type="button" onclick={onclose}>Cancel</button><button type="button" class="primary" disabled={mode === "repair" ? !previewResult.repair || !selectedIds.length : !previewResult.structural} onclick={apply}>{mode === "split" ? "Apply split" : mode === "merge" ? "Apply merge" : "Apply text repair"}</button></div>
</dialog>

<style>
  .passage-dialog { box-sizing: border-box; width: min(850px, calc(100vw - 32px)); max-height: calc(100vh - 40px); overflow: auto; padding: 24px; border: 1px solid #71717a; border-radius: 10px; background: #27272a; color: #dbeafe; box-shadow: 0 18px 80px #0009; font: inherit; font-size: 13px; }
  .passage-dialog::backdrop { background: #07101dc9; }
  h2 { margin: 0 0 6px; font-size: 20px; }
  .dialog-context, .help, .choice-help { color: #93c5fd; line-height: 1.65; margin: 10px 0; overflow-wrap: anywhere; }
  .dialog-context { color: #93c5fd; font-size: 12px; }
  .choice-help { font-size: 11px; }
  fieldset { margin: 15px 0; padding: 12px; border: 1px solid #52525b; border-radius: 6px; min-width: 0; }
  legend { padding: 0 5px; color: #bfdbfe; }
  label { display: flex; flex-direction: column; gap: 6px; margin: 12px 0; min-width: 0; color: #bfdbfe; }
  .radio, .select-all { flex-direction: row; align-items: center; gap: 8px; }
  select, input[type="number"], textarea { box-sizing: border-box; min-width: 0; padding: 8px; border: 1px solid #52525b; border-radius: 4px; background: #18181b; color: #dbeafe; font: inherit; }
  select { width: 100%; }
  textarea { width: 100%; min-height: 118px; line-height: 1.7; resize: vertical; }
  .division-fields, .text-comparison { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
  .division-fields { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .source-preview { margin: 12px 0; color: #bfdbfe; font-size: 12px; }
  .source-preview svg { display: block; width: 100%; height: 160px; background: #fff; margin-top: 8px; border-radius: 4px; }
  rect { stroke: #2563eb; stroke-width: 3px; fill: #4687e21c; }
  rect.second { stroke: #d97706; stroke-dasharray: 7 3; fill: #ffbf2722; }
  .warnings { color: #fcd34d; line-height: 1.6; padding-left: 22px; }
  .error { padding: 10px; border: 1px solid #71717a; border-radius: 4px; color: #fecaca; background: #27272a; line-height: 1.6; }
  .repair-summary { color: #d4d4d8; padding-top: 10px; }
  .repair-list { max-height: 250px; overflow-y: auto; border: 1px solid #52525b; border-radius: 5px; }
  .repair-change { flex-direction: row; align-items: flex-start; gap: 10px; margin: 0; padding: 11px; border-bottom: 1px solid #3f3f46; }
  .repair-change:last-child { border-bottom: 0; }
  .repair-change input { flex: none; margin-top: 4px; }
  .repair-change > span { min-width: 0; }
  .context, .replacement { display: block; white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.55; }
  .context { color: #93c5fd; margin-bottom: 5px; }
  .replacement { color: #e4e4e7; }
  del { color: #fecaca; background: #27272a; }
  ins { color: #bbf7d0; background: #14532d; text-decoration: none; }
  small { display: block; color: #fbbf24; margin-top: 4px; }
  .pagination, .dialog-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 9px; margin-top: 15px; }
  .dialog-actions { justify-content: flex-end; }
  button { padding: 8px 12px; border: 1px solid #71717a; border-radius: 5px; background: #3f3f46; color: #dbeafe; font: inherit; cursor: pointer; }
  button.primary { background: #2563eb; border-color: #60a5fa; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
  @media (max-width: 600px) { .passage-dialog { padding: 17px; } .division-fields, .text-comparison { grid-template-columns: minmax(0, 1fr); gap: 0; } }
</style>
