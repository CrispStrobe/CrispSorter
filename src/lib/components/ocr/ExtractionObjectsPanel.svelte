<script lang="ts">
  import type { OcrPage, OcrProject } from "$lib/ocr/model";
  import {
    getExtractionObjects, extractionObjectFreshness, updateExtractionObject,
    editExtractionField, editExtractionCell, editExtractionFormula,
    type ExtractionObject,
  } from "$lib/ocr/extractionObjects";
  import FormulaPreview from "./FormulaPreview.svelte";
  import TableEditor from "./TableEditor.svelte";
  import TableGridPreview from "./TableGridPreview.svelte";

  let { project, page, kind, selectedId, disabled = false, imageUrl, onselect, onchange, onendedit, oncopy, onsave, onrepair, onjoin }: {
    project: OcrProject;
    page: OcrPage;
    kind: ExtractionObject["kind"];
    selectedId?: string;
    disabled?: boolean;
    imageUrl: (path: string) => string;
    onselect: (id: string) => void;
    onchange: (next: OcrProject, groupKey?: string) => void;
    onendedit: () => void;
    oncopy: (object: ExtractionObject) => void;
    onsave: (object: ExtractionObject) => void;
    onrepair: (object: ExtractionObject, cellId: string | null) => void;
    onjoin: (object: ExtractionObject) => void;
  } = $props();

  let localError = $state("");
  let selectedCellId = $state("");
  let joinedSnapshotId = $state("");
  let sourceId = $state("");
  let originalOpen = $state(false);
  let sourceTextOpen = $state(false);
  let legacyOpen = $state(false);
  let sourceImageFailed = $state(false);
  let lastObjectId = "";
  let lastCaptureId = "";
  const saved = $derived(getExtractionObjects(page));
  const objects = $derived(saved.collection.objects.filter((object) => object.kind === kind));
  const selected = $derived(objects.find((object) => object.id === selectedId) ?? objects[objects.length - 1]);
  const selectedCell = $derived(selected?.kind === "table" ? selected.currentData.cells.find((cell) => cell.id === selectedCellId)
    ?? selected.currentData.cells.slice().sort((a, b) => a.row - b.row || a.column - b.column)[0] : undefined);
  const joinedSnapshots = $derived(selected?.kind === "table" && selected.originalOutputKind === "table-join" ? selected.tableSources?.snapshots ?? [] : []);
  const joinedSnapshot = $derived(joinedSnapshots.find((snapshot) => snapshot.id === joinedSnapshotId) ?? joinedSnapshots[0]);
  const freshness = $derived(selected ? extractionObjectFreshness(project, selected) : null);
  const capture = $derived(selected?.captures.find((item) => item.id === sourceId) ?? selected?.captures[0]);
  const sourcePage = $derived(capture ? project.pages.find((item) => item.id === capture.pageId) : undefined);
  const sourceViewBox = $derived.by(() => {
    if (!capture) return "0 0 1 1";
    if (!capture.bounds) return "0 0 " + capture.width + " " + capture.height;
    const x = Math.max(0, capture.bounds.x - 10);
    const y = Math.max(0, capture.bounds.y - 10);
    const right = Math.min(capture.width, capture.bounds.x + capture.bounds.w + 10);
    const bottom = Math.min(capture.height, capture.bounds.y + capture.bounds.h + 10);
    return x + " " + y + " " + Math.max(1, right - x) + " " + Math.max(1, bottom - y);
  });

  $effect(() => {
    const id = selected?.id ?? "";
    if (id !== lastObjectId) {
      lastObjectId = id;
      sourceId = "";
      selectedCellId = "";
      joinedSnapshotId = "";
      originalOpen = false;
      sourceTextOpen = false;
      localError = "";
    }
  });
  $effect(() => {
    const id = capture?.id ?? "";
    if (id !== lastCaptureId) { lastCaptureId = id; sourceImageFailed = false; }
  });

  function mutate(action: () => OcrProject, groupKey?: string) {
    if (disabled) return;
    try { localError = ""; onchange(action(), groupKey); }
    catch (failure) { localError = "This edit has not been saved: " + (failure instanceof Error ? failure.message : String(failure)); }
  }
  function outputText(value: unknown): string {
    return typeof value === "string" ? value : JSON.stringify(value, null, 2) ?? "";
  }
  function fieldBudget(object: Extract<ExtractionObject, { kind: "fields" }>, fieldId: string): number {
    const data = object.currentData;
    const used = data.labels.reduce((sum, label) => sum + label.length, 0)
      + data.fields.reduce((sum, field) => sum + field.label.length + (field.id === fieldId ? 0 : field.value.length), 0);
    return Math.max(0, 1_000_000 - used);
  }
</script>

<section class="extraction-objects" data-extraction-objects aria-label="Saved extraction objects">
  <div class="object-heading"><h3>Saved {kind === "fields" ? "field sets" : kind === "table" ? "tables" : "formulas"}</h3><span>{objects.length}</span></div>
  <p class="object-help">Each extraction is saved separately. Choose a result to inspect or correct it.</p>
  {#if saved.diagnostics.length}<details class="extraction-notes">
    <summary>Saved extraction notes ({saved.diagnostics.length})</summary>
    <ul>{#each saved.diagnostics as diagnostic}<li>{diagnostic}</li>{/each}</ul>
  </details>{/if}
  {#if objects.length}<label class="object-field">Saved extraction<select aria-label="Saved extraction" value={selected?.id ?? ""} disabled={disabled}
    onchange={(event) => { onendedit(); onselect(event.currentTarget.value); }}>
    {#each objects as object (object.id)}<option value={object.id}>{object.name}{object.excluded ? " · excluded" : ""}</option>{/each}
  </select></label>{:else}<p class="empty-objects">No saved {kind === "fields" ? "field sets" : kind === "table" ? "tables" : "formulas"} for this page yet.</p>{/if}

  {#if selected}
    {@const object = selected}
    <div class="object-editor" data-extraction-object-id={object.id}>
      <label class="object-field">Object name<input aria-label="Object name" value={object.name} maxlength="1000" disabled={disabled}
        onchange={(event) => mutate(() => updateExtractionObject(project, page.id, object.id, { name: event.currentTarget.value }))} /></label>
      <label class="exclude-choice"><input type="checkbox" aria-label="Exclude object" checked={object.excluded} disabled={disabled}
        onchange={(event) => mutate(() => updateExtractionObject(project, page.id, object.id, { excluded: event.currentTarget.checked }))} /><span>Exclude object</span></label>
      {#if object.excluded}<p class="object-help">Marked excluded. Its data remains in the project and can still be copied or saved explicitly.</p>{/if}
      <div class="object-source-state" class:historical={!freshness?.current} data-extraction-source-state={freshness?.current ? "current" : "historical"}>
        {freshness?.current ? "Current source" : "Historical source"}
      </div>
      {#if freshness && !freshness.current}<div class="source-warning">
        <p>This result is retained from an earlier source. Compare its captured source before using the edited result.</p>
        <ul>{#each freshness.reasons as reason}<li>{reason}</li>{/each}</ul>
      </div>{/if}
      {#if capture}
        {#if object.captures.length > 1}<label class="object-field">Captured source<select aria-label="Captured source" value={capture.id} onchange={(event) => (sourceId = event.currentTarget.value)}>
          {#each object.captures as item, index (item.id)}<option value={item.id}>Source {index + 1} · {item.regionId ? "region" : "page"}</option>{/each}
        </select></label>{/if}
        <figure class="source-preview">
          <svg viewBox={sourceViewBox} role="img" aria-label="Captured extraction source">
            <image href={imageUrl(capture.imagePath)} width={capture.width} height={capture.height} onerror={() => (sourceImageFailed = true)} />
            {#if capture.bounds}<rect x={capture.bounds.x} y={capture.bounds.y} width={capture.bounds.w} height={capture.bounds.h} vector-effect="non-scaling-stroke"><title>Captured region area</title></rect>{/if}
          </svg>
          <figcaption>{sourcePage?.name ?? "Earlier page"} · {capture.regionId ? "Captured region" : "Captured page"} · {capture.method === "text" ? "Reviewed text input" : "Image input"}</figcaption>
        </figure>
        {#if sourceImageFailed}<p class="source-warning" role="status">The captured source image could not be loaded. The saved result and original output remain available below.</p>{/if}
        {#if capture.method === "text"}<details class="source-text" bind:open={sourceTextOpen}><summary>Captured source text</summary>
          {#if sourceTextOpen}<textarea readonly aria-label="Captured source text" value={capture.text ?? ""} rows="6" dir="auto"></textarea>{/if}
        </details>{/if}
      {:else}<p class="source-warning">This earlier record has no validated source snapshot. Its saved data is retained without assigning it current page coordinates.</p>{/if}

      {#if object.kind === "fields"}
        <div class="result-heading"><strong>{object.currentData.fields.length} extracted fields</strong><button type="button" onclick={() => oncopy(object)} disabled={disabled} title="Copy selected field set as JSON">JSON</button></div>
        {#each object.currentData.fields as field (field.id)}
          {@const original = object.originalData.fields.find((item) => item.id === field.id)}
          {@const edited = !original || field.value !== original.value || field.label !== original.label}
          <div class="extracted-field" data-extraction-field-id={field.id}>
            <label>{field.label}<input value={field.value} maxlength={fieldBudget(object, field.id)} disabled={disabled} data-extraction-editor
              oninput={(event) => mutate(() => editExtractionField(project, page.id, object.id, field.id, { value: event.currentTarget.value }), "extraction:" + page.id + ":" + object.id + ":field:" + field.id)}
              onblur={onendedit} dir="auto" /></label>
            <span title="Original model extraction score; manual corrections do not produce a new model score">{edited ? "Edited · " : ""}{typeof original?.score === "number" ? "Original score " + Math.round(original.score * 100) + "%" : "Unscored"}</span>
          </div>
        {/each}
        <p class="object-help">Check each value against the captured source. Scores describe the original model result.</p>
      {:else if object.kind === "table"}
        <div class="result-heading"><strong>Editable table</strong><button type="button" onclick={() => oncopy(object)} disabled={disabled} title="Copy selected table as CSV">CSV</button></div>
        <TableEditor rows={object.currentData.rows} columns={object.currentData.columns} cells={object.currentData.cells} {disabled}
          selectedCellId={selectedCell?.id} onselect={(cellId) => (selectedCellId = cellId)}
          onedit={(cellId, text) => mutate(() => editExtractionCell(project, page.id, object.id, cellId, text), "extraction:" + page.id + ":" + object.id + ":cell:" + cellId)} {onendedit} />
        <div class="table-repair-tools">
          <p class="object-help">{selectedCell ? "Selected cell: row " + (selectedCell.row + 1) + ", column " + (selectedCell.column + 1) + ". Focus a cell to choose a starting position." : "This table is empty. Inserting a row or column starts a one-cell table."}</p>
          <button type="button" onclick={() => { onendedit(); onrepair(object, selectedCell?.id ?? null); }} disabled={disabled || !!localError}>Repair table structure</button>
          <button type="button" onclick={() => { onendedit(); onjoin(object); }} disabled={disabled || !!localError || !object.currentData.rows || !object.currentData.columns}>Join continuation tables</button>
        </div>
      {:else}
        <FormulaPreview latex={object.currentData.latex} />
        <label class="object-field">LaTeX<textarea class="latex-editor" aria-label="LaTeX" value={object.currentData.latex} maxlength="1000000" disabled={disabled}
          data-extraction-editor oninput={(event) => mutate(() => editExtractionFormula(project, page.id, object.id, event.currentTarget.value), "extraction:" + page.id + ":" + object.id + ":latex")}
          onblur={onendedit} rows="8" spellcheck="false"></textarea></label>
        <button type="button" onclick={() => oncopy(object)} disabled={disabled}>Copy LaTeX</button>
      {/if}

      <details class="original-output" bind:open={originalOpen}>
        <summary>{object.originalOutputKind === "table-join" ? "Source tables at joining" : object.originalOutputKind === "model" ? "Original model output" : "Original saved record"}</summary>
        {#if originalOpen}
          {#if object.kind === "table" && object.originalOutputKind === "table-join"}
            <p class="object-help">This table has a manual joined baseline. Its source snapshots preserve the input tables, their corrections at joining, and their earlier output.</p>
            <div class="initial-joined-grid"><TableGridPreview table={object.originalData} label="Initial joined table" /></div>
            <label class="object-field">Initial joined structure<textarea readonly aria-label="Initial joined structure" value={outputText(object.originalData)} rows="6" spellcheck="false"></textarea></label>
            {#if joinedSnapshot}
              <label class="object-field">Joined source snapshot<select aria-label="Joined source snapshot" value={joinedSnapshot.id} onchange={(event) => (joinedSnapshotId = event.currentTarget.value)}>
                {#each joinedSnapshots as snapshot (snapshot.id)}<option value={snapshot.id}>{snapshot.name} · {project.pages.find((item) => item.id === snapshot.ownerPageId)?.name ?? snapshot.ownerPageId}</option>{/each}
              </select></label>
              <section class="joined-snapshot" data-joined-source-snapshot-id={joinedSnapshot.id}>
                <p class="object-help"><strong>{joinedSnapshot.name}</strong><br />Source page: {project.pages.find((item) => item.id === joinedSnapshot.ownerPageId)?.name ?? joinedSnapshot.ownerPageId}<br />Snapshot captured: {joinedSnapshot.capturedAt}</p>
                <TableGridPreview table={joinedSnapshot.currentData} label="Source table at joining" />
                <div class="source-original-grid"><TableGridPreview table={joinedSnapshot.originalData}
                  label={joinedSnapshot.originalOutputKind === "table-join" ? "Initial source join structure" : joinedSnapshot.originalOutputKind === "model" ? "Original recognized table" : "Earlier saved source structure"} /></div>
                <label class="object-field">{joinedSnapshot.originalOutputKind === "table-join" ? "Source join receipt" : joinedSnapshot.originalOutputKind === "model" ? "Original source output" : "Earlier source record"}<textarea readonly
                  aria-label={joinedSnapshot.originalOutputKind === "table-join" ? "Source join receipt" : joinedSnapshot.originalOutputKind === "model" ? "Original source output" : "Earlier source record"}
                  value={outputText(joinedSnapshot.originalOutput)} rows="7" spellcheck="false"></textarea></label>
              </section>
            {/if}
            <label class="object-field">Join receipt<textarea readonly aria-label="Join receipt" value={outputText(object.originalOutput)} rows="6" spellcheck="false"></textarea></label>
            <label class="object-field">Source table snapshots<textarea readonly data-joined-source-snapshots aria-label="Source table snapshots" value={outputText(object.tableSources)} rows="8" spellcheck="false"></textarea></label>
          {:else}
            {#if object.originalOutputKind === "legacy-record"}<p class="object-help">This record preserves previously saved data. It does not recover an original model response that was not saved.</p>{/if}
            <label class="object-field">Original extraction output<textarea readonly aria-label="Original extraction output" value={outputText(object.originalOutput)} rows="8" spellcheck="false"></textarea></label>
            <label class="object-field">Original structured data<textarea readonly aria-label="Original structured data" value={outputText(object.originalData)} rows="6" spellcheck="false"></textarea></label>
            <p class="object-help">Original output stays separate from your corrections. Markup is shown as text.</p>
          {/if}
        {/if}
      </details>
      <button type="button" class="save-object" onclick={() => onsave(object)} disabled={disabled}>Save {object.kind === "fields" ? "fields as JSON" : object.kind === "table" ? "table as CSV" : "formula as LaTeX"}</button>
    </div>
  {/if}

  {#if localError}<p class="edit-error" role="alert">{localError}</p>{/if}
  {#if page.provenance.extraction !== undefined}<details class="legacy-record" bind:open={legacyOpen}><summary>Earlier extraction record</summary>
    {#if legacyOpen}<p class="object-help">This is the unchanged earlier saved record. Any unverified source dimensions or raw output stay here for inspection.</p><textarea readonly aria-label="Earlier extraction record JSON" value={outputText(page.provenance.extraction)} rows="9" spellcheck="false"></textarea>{/if}
  </details>{/if}
</section>

<style>
  .extraction-objects { margin-top: 22px; padding-top: 15px; border-top: 1px solid #52525b; }
  .object-heading, .result-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  h3 { margin: 0; font-size: 13px; color: #bfdbfe; }
  .object-heading > span { min-width: 22px; text-align: center; padding: 2px 5px; border-radius: 4px; background: #3f3f46; color: #bfdbfe; font-size: 11px; }
  .object-help, .empty-objects { color: #93c5fd; font-size: 11px; line-height: 1.7; margin: 9px 0 12px; overflow-wrap: anywhere; }
  .object-field { display: flex; flex-direction: column; gap: 6px; margin: 13px 0; color: #bfdbfe; font-size: 11px; }
  input:not([type="checkbox"]), select, textarea { box-sizing: border-box; width: 100%; min-width: 0; padding: 9px; border: 1px solid #52525b; border-radius: 5px; background: #18181b; color: #dbeafe; font: inherit; font-size: 12px; }
  textarea { line-height: 1.65; resize: vertical; }
  textarea[readonly] { color: #d4d4d8; }
  .latex-editor, .original-output textarea, .legacy-record textarea { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .exclude-choice { display: flex; align-items: center; gap: 8px; color: #d4d4d8; font-size: 11px; margin: 12px 0; }
  .object-source-state { display: inline-block; margin: 3px 0 10px; padding: 4px 7px; border: 1px solid #71717a; border-radius: 4px; background: #14532d; color: #86efac; font-size: 10px; }
  .object-source-state.historical { border-color: #71717a; background: #27272a; color: #fcd34d; }
  .source-warning, .edit-error { padding: 10px; border: 1px solid #71717a; border-radius: 5px; color: #fcd34d; background: #27272a; font-size: 11px; line-height: 1.7; overflow-wrap: anywhere; }
  .source-warning p { margin: 0; } .source-warning ul { margin: 6px 0 0; padding-left: 19px; }
  .edit-error { border-color: #71717a; background: #27272a; color: #fecaca; }
  .source-preview { margin: 10px 0 13px; }
  .source-preview svg { display: block; width: 100%; height: 160px; background: #fff; border: 1px solid #71717a; border-radius: 4px; }
  .source-preview rect { fill: none; stroke: #2563eb; stroke-width: 2px; }
  figcaption { color: #a1a1aa; font-size: 10px; line-height: 1.7; margin-top: 5px; overflow-wrap: anywhere; }
  .result-heading { margin: 19px 0 12px; } .result-heading strong { color: #bfdbfe; font-size: 12px; }
  .extracted-field { display: flex; align-items: center; gap: 8px; margin: 12px 0; }
  .extracted-field label { display: flex; flex: 1; min-width: 0; flex-direction: column; gap: 5px; color: #d4d4d8; font-size: 11px; overflow-wrap: anywhere; }
  .extracted-field > span { max-width: 95px; padding-top: 17px; color: #eab308; font-size: 10px; line-height: 1.6; }
  details { margin: 14px 0; padding: 10px; border: 1px solid #52525b; border-radius: 5px; }
  summary { cursor: pointer; color: #bfdbfe; font-size: 11px; line-height: 1.6; }
  .extraction-notes ul { padding-left: 18px; color: #fbbf24; font-size: 11px; line-height: 1.7; }
  .source-text textarea, .legacy-record textarea { margin-top: 10px; }
  button { padding: 7px 10px; border: 1px solid #71717a; border-radius: 5px; background: #3f3f46; color: #dbeafe; font: inherit; font-size: 11px; cursor: pointer; }
  .save-object { margin-top: 10px; width: 100%; }
  .table-repair-tools { margin-top: 12px; }
  .table-repair-tools > button { width: 100%; margin-top: 8px; }
  .initial-joined-grid, .source-original-grid { margin: 14px 0; }
  .joined-snapshot { padding: 10px; border: 1px solid #52525b; border-radius: 5px; background: #18181b; }
  button:disabled, input:disabled, textarea:disabled, select:disabled { opacity: .5; cursor: default; }
  button:focus-visible, input:focus-visible, textarea:focus-visible, select:focus-visible, summary:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
</style>
