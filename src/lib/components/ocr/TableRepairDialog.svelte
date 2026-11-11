<script lang="ts">
  import { tick, untrack } from "svelte";
  import type { OcrProject } from "$lib/ocr/model";
  import { getExtractionObjects, extractionObjectFreshness } from "$lib/ocr/extractionObjects";
  import { prepareTableOperation, type TableOperationRequest, type TableOperationPreview, type TableExtractionObject } from "$lib/ocr/tableOperations";
  import TableGridPreview from "./TableGridPreview.svelte";

  let { project, projectId, pageId, initialObject, initialCellId, disabled = false, imageUrl, onapply, onclose }: {
    project: OcrProject;
    projectId: string;
    pageId: string;
    initialObject: TableExtractionObject;
    initialCellId: string | null;
    disabled?: boolean;
    imageUrl: (path: string) => string;
    onapply: (preview: TableOperationPreview) => string | null;
    onclose: () => void;
  } = $props();

  const initialCell = untrack(() => initialObject.currentData.cells.find((cell) => cell.id === initialCellId)
    ?? initialObject.currentData.cells.slice().sort((a, b) => a.row - b.row || a.column - b.column)[0]);
  const initialRow = initialCell ? initialCell.row + 1 : 1;
  const initialColumn = initialCell ? initialCell.column + 1 : 1;
  let dialog = $state<HTMLDialogElement>();
  let operationSelect = $state<HTMLSelectElement>();
  let previewHeading = $state<HTMLHeadingElement>();
  let operation = $state<TableOperationRequest["kind"]>("insert-row");
  let rowPosition = $state<number | undefined>(initialRow);
  let columnPosition = $state<number | undefined>(initialColumn);
  let rowToDelete = $state<number | undefined>(initialRow);
  let columnToDelete = $state<number | undefined>(initialColumn);
  let firstRow = $state<number | undefined>(initialRow);
  let lastRow = $state<number | undefined>(initialCell ? initialCell.row + initialCell.rowSpan : 1);
  let firstColumn = $state<number | undefined>(initialColumn);
  let lastColumn = $state<number | undefined>(initialCell ? initialCell.column + initialCell.columnSpan : 1);
  let cellRow = $state<number | undefined>(initialRow);
  let cellColumn = $state<number | undefined>(initialColumn);
  let separator = $state<"newline" | "blank-line" | "space" | "none">("newline");
  let markHeader = $state(true);
  let preview = $state<TableOperationPreview | null>(null);
  let previewRejected = $state(false);
  let preparing = $state(false);
  let applying = $state(false);
  let composing = $state(false);
  let actionError = $state("");
  let localNotice = $state("");
  let sourceId = $state("");
  let sourceImageFailed = $state(false);
  let lastCaptureId = "";

  const currentObject = $derived.by(() => {
    if (project.id !== projectId) return null;
    const page = project.pages.find((item) => item.id === pageId);
    if (!page) return null;
    const object = getExtractionObjects(page).collection.objects.find((item) => item.id === initialObject.id);
    return object?.kind === "table" ? object : null;
  });
  const table = $derived(currentObject?.currentData ?? initialObject.currentData);
  const sourceObject = $derived(preview?.sourceObject ?? currentObject ?? initialObject);
  const freshness = $derived(project.id === projectId ? extractionObjectFreshness(project, sourceObject)
    : { current: false, reasons: ["The source project is no longer active."] });
  const capture = $derived(sourceObject.captures.find((item) => item.id === sourceId) ?? sourceObject.captures[0]);
  const sourceViewBox = $derived.by(() => {
    if (!capture) return "0 0 1 1";
    if (!capture.bounds) return "0 0 " + capture.width + " " + capture.height;
    const x = Math.max(0, capture.bounds.x - 10), y = Math.max(0, capture.bounds.y - 10);
    const right = Math.min(capture.width, capture.bounds.x + capture.bounds.w + 10);
    const bottom = Math.min(capture.height, capture.bounds.y + capture.bounds.h + 10);
    return x + " " + y + " " + Math.max(1, right - x) + " " + Math.max(1, bottom - y);
  });
  const removedText = $derived(preview?.deletedCells.filter((cell) => cell.text.length > 0) ?? []);
  const busy = $derived(disabled || preparing || applying);
  const operationHelp = $derived({
    "insert-row": "Insert a blank row at the chosen position. A merged cell crossing that position expands to cover it.",
    "delete-row": "Delete one row. A merged cell that still covers surviving rows keeps all of its text.",
    "insert-column": "Insert a blank column at the chosen position. A merged cell crossing that position expands to cover it.",
    "delete-column": "Delete one column. A merged cell that still covers surviving columns keeps all of its text.",
    merge: "Select a full rectangle containing at least two whole cells. Their exact text is joined in row order; empty cells also contribute the chosen separator.",
    split: "Split the merged cell covering the chosen position. All of its text stays in the top-left cell; the other resulting cells start empty.",
    header: "Mark or clear the header setting for whole cells inside the chosen rectangle.",
  }[operation]);

  $effect(() => { if (dialog && !dialog.open) dialog.showModal(); });
  $effect(() => {
    const id = capture?.id ?? "";
    if (id !== lastCaptureId) { lastCaptureId = id; sourceImageFailed = false; }
  });

  function coordinate(value: number | undefined, maximum: number, label: string): number {
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 1 || value > maximum) {
      throw new Error(label + " must be a whole number from 1 to " + maximum + ".");
    }
    return value - 1;
  }
  function selectedCellIds(): string[] {
    if (!table.rows || !table.columns) throw new Error("Insert a row or column to begin an empty table.");
    const top = coordinate(firstRow, table.rows, "First row");
    const bottom = coordinate(lastRow, table.rows, "Last row");
    const left = coordinate(firstColumn, table.columns, "First column");
    const right = coordinate(lastColumn, table.columns, "Last column");
    if (bottom < top || right < left) throw new Error("The last row and column must follow or equal the first.");
    const cells = table.cells.filter((cell) => cell.row <= bottom && cell.row + cell.rowSpan > top &&
      cell.column <= right && cell.column + cell.columnSpan > left);
    if (!cells.length) throw new Error("The selected rectangle contains no extracted cells.");
    if (cells.some((cell) => cell.row < top || cell.column < left ||
      cell.row + cell.rowSpan - 1 > bottom || cell.column + cell.columnSpan - 1 > right)) {
      throw new Error("The rectangle cuts through a merged cell. Expand the coordinates to include that whole cell.");
    }
    return cells.slice().sort((a, b) => a.row - b.row || a.column - b.column).map((cell) => cell.id);
  }
  function request(): TableOperationRequest {
    if (operation === "insert-row") return { kind: operation, index: coordinate(rowPosition, table.rows + 1, "Row position") };
    if (operation === "insert-column") return { kind: operation, index: coordinate(columnPosition, table.columns + 1, "Column position") };
    if (operation === "delete-row") {
      if (!table.rows) throw new Error("There are no rows to delete.");
      return { kind: operation, index: coordinate(rowToDelete, table.rows, "Row to delete") };
    }
    if (operation === "delete-column") {
      if (!table.columns) throw new Error("There are no columns to delete.");
      return { kind: operation, index: coordinate(columnToDelete, table.columns, "Column to delete") };
    }
    if (operation === "merge") {
      const separators = { newline: "\n", "blank-line": "\n\n", space: " ", none: "" } as const;
      return { kind: "merge", cellIds: selectedCellIds(), separator: separators[separator] };
    }
    if (operation === "header") return { kind: "header", cellIds: selectedCellIds(), header: markHeader };
    if (!table.rows || !table.columns) throw new Error("There are no cells to split.");
    const row = coordinate(cellRow, table.rows, "Cell row"), column = coordinate(cellColumn, table.columns, "Cell column");
    const cell = table.cells.find((item) => item.row <= row && item.row + item.rowSpan > row && item.column <= column && item.column + item.columnSpan > column);
    if (!cell) throw new Error("There is no extracted cell at the chosen position.");
    return { kind: "split", cellId: cell.id };
  }
  async function review() {
    if (busy || composing) return;
    preparing = true;
    actionError = "";
    localNotice = "";
    try {
      if (project.id !== projectId) throw new Error("The active project changed. Your repair choices remain here; return to the source project before reviewing.");
      if (!currentObject) throw new Error("The source table is no longer available. Your repair choices have been kept.");
      const result = prepareTableOperation(project, pageId, initialObject.id, request());
      if (!result) {
        preview = null;
        previewRejected = false;
        localNotice = "The selected cells already have that header setting.";
        await tick();
        operationSelect?.focus();
        return;
      }
      preview = result;
      previewRejected = false;
      await tick();
      previewHeading?.focus();
    } catch (failure) {
      actionError = failure instanceof Error ? failure.message : String(failure);
      if (preview) previewRejected = true;
    } finally { preparing = false; }
  }
  async function backToOptions() {
    if (busy || composing) return;
    preview = null;
    previewRejected = false;
    actionError = "";
    localNotice = "";
    await tick();
    operationSelect?.focus();
  }
  function apply() {
    if (!preview || previewRejected || busy || composing) return;
    applying = true;
    actionError = "";
    localNotice = "";
    try {
      const error = onapply(preview);
      if (error) { actionError = error; previewRejected = true; }
      else dialog?.close();
    } catch (failure) {
      actionError = failure instanceof Error ? failure.message : String(failure);
      previewRejected = true;
    } finally { applying = false; }
  }
  function cancel() {
    if (preparing || applying || composing) return;
    dialog?.close();
  }
  function keydown(event: KeyboardEvent) {
    event.stopPropagation();
    if (!event.isComposing && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      localNotice = "These table changes are unapplied. Review and apply them to save the result in the project.";
    }
  }
</script>

<dialog bind:this={dialog} class="table-repair-dialog" aria-labelledby="table-repair-title" data-table-repair-dialog
  onkeydown={keydown} oncompositionstart={() => (composing = true)} oncompositionend={() => (composing = false)}
  oncancel={(event) => { event.preventDefault(); cancel(); }} onclose={onclose}>
  <h2 id="table-repair-title">Repair table structure</h2>
  <p class="table-name">{sourceObject.name}</p>
  <div class="source-header"><span class="source-badge" class:historical={!freshness.current} data-table-repair-source-state={freshness.current ? "current" : "historical"}>{freshness.current ? "Current source" : "Historical source"}</span><span>Current table: {table.rows} × {table.columns}</span></div>
  {#if !freshness.current}<p class="warning">{freshness.reasons.join(" ")}</p>{/if}
  {#if capture}
    {#if sourceObject.captures.length > 1}<label class="field-label">Captured source<select aria-label="Captured source" value={capture.id} onchange={(event) => (sourceId = event.currentTarget.value)}>
      {#each sourceObject.captures as item, index (item.id)}<option value={item.id}>Source {index + 1} · {item.regionId ? "region" : "page"}</option>{/each}
    </select></label>{/if}
    <svg class="source-preview" viewBox={sourceViewBox} role="img" aria-label="Captured table source">
      <image href={imageUrl(capture.imagePath)} width={capture.width} height={capture.height} onerror={() => (sourceImageFailed = true)} />
      {#if capture.bounds}<rect x={capture.bounds.x} y={capture.bounds.y} width={capture.bounds.w} height={capture.bounds.h} vector-effect="non-scaling-stroke"><title>Captured source region</title></rect>{/if}
    </svg>
    {#if sourceImageFailed}<p class="warning">The captured image could not be loaded. The saved table and exact cell text remain available for review.</p>{/if}
  {:else}<p class="warning">This saved table has no validated source image snapshot. Review its recorded cells before changing the structure.</p>{/if}

  <div class="repair-options" hidden={!!preview}>
    <label class="field-label">Table operation<select bind:this={operationSelect} aria-label="Table operation" bind:value={operation} disabled={busy}>
      <option value="insert-row">Insert row</option><option value="delete-row">Delete row</option>
      <option value="insert-column">Insert column</option><option value="delete-column">Delete column</option>
      <option value="merge">Merge cells</option><option value="split">Split merged cell</option><option value="header">Header cells</option>
    </select></label>
    <p class="help">{operationHelp}</p>
    {#if operation === "insert-row"}
      <label class="field-label">Row position<input type="number" aria-label="Row position" min="1" max={table.rows + 1} step="1" bind:value={rowPosition} disabled={busy} /></label>
      <p class="help">Position 1 inserts before the first row; position {table.rows + 1} appends a row.</p>
    {:else if operation === "insert-column"}
      <label class="field-label">Column position<input type="number" aria-label="Column position" min="1" max={table.columns + 1} step="1" bind:value={columnPosition} disabled={busy} /></label>
      <p class="help">Position 1 inserts before the first column; position {table.columns + 1} appends a column.</p>
    {:else if operation === "delete-row"}
      <label class="field-label">Row to delete<input type="number" aria-label="Row to delete" min="1" max={Math.max(1, table.rows)} step="1" bind:value={rowToDelete} disabled={busy} /></label>
    {:else if operation === "delete-column"}
      <label class="field-label">Column to delete<input type="number" aria-label="Column to delete" min="1" max={Math.max(1, table.columns)} step="1" bind:value={columnToDelete} disabled={busy} /></label>
    {:else if operation === "split"}
      <div class="coordinate-grid">
        <label class="field-label">Cell row<input type="number" aria-label="Cell row" min="1" max={Math.max(1, table.rows)} step="1" bind:value={cellRow} disabled={busy} /></label>
        <label class="field-label">Cell column<input type="number" aria-label="Cell column" min="1" max={Math.max(1, table.columns)} step="1" bind:value={cellColumn} disabled={busy} /></label>
      </div>
    {:else}
      <div class="coordinate-grid">
        <label class="field-label">First row<input type="number" aria-label="First row" min="1" max={Math.max(1, table.rows)} step="1" bind:value={firstRow} disabled={busy} /></label>
        <label class="field-label">Last row<input type="number" aria-label="Last row" min="1" max={Math.max(1, table.rows)} step="1" bind:value={lastRow} disabled={busy} /></label>
        <label class="field-label">First column<input type="number" aria-label="First column" min="1" max={Math.max(1, table.columns)} step="1" bind:value={firstColumn} disabled={busy} /></label>
        <label class="field-label">Last column<input type="number" aria-label="Last column" min="1" max={Math.max(1, table.columns)} step="1" bind:value={lastColumn} disabled={busy} /></label>
      </div>
      <p class="help">Coordinates start at 1 and include both ends of the rectangle. Include every part of any merged cell.</p>
      {#if operation === "merge"}<label class="field-label">Merge separator<select aria-label="Merge separator" bind:value={separator} disabled={busy}>
        <option value="newline">New line</option><option value="blank-line">Blank line</option><option value="space">Space</option><option value="none">No separator</option>
      </select></label>{:else}<label class="header-choice"><input type="checkbox" aria-label="Mark selected cells as headers" bind:checked={markHeader} disabled={busy} />Mark selected cells as headers</label>{/if}
    {/if}
    <div class="current-table"><TableGridPreview {table} label="Current table structure" /></div>
  </div>

  {#if preview}
    <section class="repair-preview" data-table-repair-preview>
      <h3 bind:this={previewHeading} tabindex="-1">Review table changes</h3>
      <p class="help">Apply records one undoable change. Shaded cells are headers; span labels show merged cells.</p>
      {#if removedText.length}<section class="removed-values" data-table-deleted-cells aria-label="Cell text to remove">
        <h4>{removedText.length} cell{removedText.length === 1 ? "" : "s"} with text will be removed</h4>
        {#each removedText as cell (cell.id)}<label class="field-label">Removed row {cell.row + 1}, column {cell.column + 1}<textarea readonly
          aria-label={"Removed row " + (cell.row + 1) + ", column " + (cell.column + 1)} value={cell.text} rows="3" dir="auto"></textarea></label>{/each}
      </section>{/if}
      {#if preview.request.kind === "merge"}<label class="field-label merge-result">Merged cell text<textarea readonly aria-label="Merged cell text" value={preview.mergeText ?? ""} rows="5" dir="auto"></textarea></label>{/if}
      <div class="table-comparison">
        <div data-table-before><TableGridPreview table={preview.before} label="Current table structure" /></div>
        <div data-table-after><TableGridPreview table={preview.after} label="Proposed table structure" /></div>
      </div>
      {#if preview.warnings.length}<ul class="repair-warnings" aria-label="Table repair notes">{#each preview.warnings as warning}<li>{warning}</li>{/each}</ul>{/if}
    </section>
  {/if}
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
  {#if previewRejected}<p class="warning">This preview cannot be applied. Your operation and coordinates have been kept. Review again against the current table, or go back to adjust them.</p>{/if}
  {#if localNotice}<p class="local-notice" role="status">{localNotice}</p>{/if}
  {#if composing}<p class="help">Finish the current text input before reviewing or applying.</p>{/if}
  <footer class="dialog-actions">
    <button type="button" onclick={cancel} disabled={preparing || applying || composing}>Cancel</button>
    {#if preview}
      <button type="button" onclick={backToOptions} disabled={busy || composing}>Back to options</button>
      <button type="button" onclick={review} disabled={busy || composing}>Review again</button>
      <button type="button" class="primary" onclick={apply} disabled={busy || composing || previewRejected}>Apply table changes</button>
    {:else}<button type="button" class="primary" onclick={review} disabled={busy || composing}>Review table changes</button>{/if}
  </footer>
</dialog>

<style>
  .table-repair-dialog { box-sizing: border-box; width: min(1120px, calc(100vw - 28px)); max-width: calc(100vw - 28px); max-height: calc(100vh - 30px); overflow: auto; padding: 22px; border: 1px solid #71717a; border-radius: 10px; background: #27272a; color: #dbeafe; box-shadow: 0 18px 80px #0009; font: inherit; font-size: 13px; }
  .table-repair-dialog::backdrop { background: #07101dd4; }
  h2 { margin: 0 0 6px; font-size: 20px; } h3 { margin: 15px 0 7px; font-size: 16px; } h4 { margin: 0; font-size: 13px; }
  .table-name { margin: 0 0 12px; color: #93c5fd; overflow-wrap: anywhere; }
  .source-header { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 15px; margin-bottom: 10px; color: #93c5fd; font-size: 11px; }
  .source-badge { padding: 4px 7px; border: 1px solid #71717a; border-radius: 4px; background: #14532d; color: #86efac; }
  .source-badge.historical { border-color: #71717a; background: #27272a; color: #fcd34d; }
  .source-preview { box-sizing: border-box; display: block; width: 100%; height: 140px; border: 1px solid #71717a; border-radius: 5px; background: #fff; }
  .source-preview rect { fill: none; stroke: #2563eb; stroke-width: 2px; }
  .field-label { display: flex; flex-direction: column; gap: 6px; min-width: 0; margin: 13px 0; color: #bfdbfe; font-size: 12px; }
  input[type="number"], select, textarea { box-sizing: border-box; width: 100%; min-width: 0; padding: 9px 10px; border: 1px solid #52525b; border-radius: 5px; background: #172554; color: #dbeafe; font: inherit; font-size: 13px; line-height: 1.6; }
  textarea { resize: vertical; }
  input[type="number"] { max-width: 220px; }
  .coordinate-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 0 14px; }
  .coordinate-grid input { max-width: none; }
  .help { margin: 9px 0 12px; color: #93c5fd; font-size: 12px; line-height: 1.7; overflow-wrap: anywhere; }
  .repair-options[hidden] { display: none; }
  .header-choice { display: flex; align-items: center; gap: 9px; margin: 15px 0; color: #bfdbfe; font-size: 12px; }
  .current-table { margin: 17px 0 10px; }
  .table-comparison { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; }
  .table-comparison > div { min-width: 0; }
  .removed-values { margin: 13px 0; padding: 12px; border: 1px solid #d97706; border-radius: 5px; background: #27272a; }
  .removed-values h4 { color: #fcd34d; line-height: 1.6; }
  .warning, .error { margin: 12px 0; padding: 11px; border: 1px solid #71717a; border-radius: 5px; background: #27272a; color: #fcd34d; font-size: 12px; line-height: 1.7; overflow-wrap: anywhere; }
  .error { border-color: #71717a; background: #27272a; color: #fecaca; }
  .repair-warnings { padding-left: 21px; color: #fcd34d; font-size: 12px; line-height: 1.7; }
  .local-notice { color: #bfdbfe; font-size: 12px; line-height: 1.7; }
  .dialog-actions { position: sticky; bottom: -22px; display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 9px; margin: 18px -1px -1px; padding: 14px 0 2px; border-top: 1px solid #52525b; background: #27272a; }
  button { padding: 9px 12px; border: 1px solid #71717a; border-radius: 5px; background: #3f3f46; color: #dbeafe; font: inherit; font-size: 12px; cursor: pointer; }
  button.primary { background: #2563eb; border-color: #93c5fd; }
  button:disabled, input:disabled, select:disabled { opacity: .5; cursor: default; }
  button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible { outline: 2px solid #bfdbfe; outline-offset: 2px; }
  h3:focus { outline: none; }
  @media (max-width: 720px) { .table-repair-dialog { padding: 16px; } .table-comparison { grid-template-columns: minmax(0, 1fr); } .dialog-actions { bottom: -16px; } .dialog-actions button { flex: 1 1 auto; } }
</style>
