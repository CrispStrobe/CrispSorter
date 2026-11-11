<script lang="ts">
  import { tick, untrack } from "svelte";
  import type { OcrPage, OcrProject } from "$lib/ocr/model";
  import { getExtractionObjects, extractionObjectFreshness } from "$lib/ocr/extractionObjects";
  import type { TableExtractionObject } from "$lib/ocr/tableOperations";
  import { prepareContinuationJoin, type ContinuationJoinRequest, type ContinuationJoinPreview } from "$lib/ocr/continuationJoin";
  import TableGridPreview from "./TableGridPreview.svelte";
  import TableSourcePreview from "./TableSourcePreview.svelte";

  type Choice = {
    key: string; pageId: string; pageName: string; objectId: string; snapshot: TableExtractionObject;
    dropHeaderRows: number | undefined; columnMap: (number | undefined)[]; mappingOpen: boolean; sourceOpen: boolean;
  };
  type Candidate = { key: string; page: OcrPage; object: TableExtractionObject };
  let { project, projectId, initialPageId, initialObject, disabled = false, imageUrl, onapply, onclose }: {
    project: OcrProject;
    projectId: string;
    initialPageId: string;
    initialObject: TableExtractionObject;
    disabled?: boolean;
    imageUrl: (path: string) => string;
    onapply: (preview: ContinuationJoinPreview) => string | null;
    onclose: () => void;
  } = $props();

  function keyFor(pageId: string, objectId: string) { return JSON.stringify([pageId, objectId]); }
  function choiceFor(pageId: string, pageName: string, object: TableExtractionObject): Choice {
    return { key: keyFor(pageId, object.id), pageId, pageName, objectId: object.id, snapshot: object,
      dropHeaderRows: 0, columnMap: Array.from({ length: object.currentData.columns }, () => undefined),
      mappingOpen: object.currentData.columns <= 8, sourceOpen: false };
  }
  let choices = $state<Choice[]>(untrack(() => [choiceFor(initialPageId, project.pages.find((page) => page.id === initialPageId)?.name ?? initialPageId, initialObject)]));
  let joinedName = $state(untrack(() => initialObject.name.slice(0, 990) + " joined"));
  let candidateKey = $state("");
  let customMapping = $state(false);
  let targetColumns = $state<number | undefined>(untrack(() => initialObject.currentData.columns));
  let dialog = $state<HTMLDialogElement>();
  let nameInput = $state<HTMLInputElement>();
  let previewHeading = $state<HTMLHeadingElement>();
  let preview = $state<ContinuationJoinPreview | null>(null);
  let previewRejected = $state(false);
  let previewSourcesOpen = $state<Record<string, boolean>>({});
  let preparing = $state(false);
  let applying = $state(false);
  let composing = $state(false);
  let actionError = $state("");
  let localNotice = $state("");

  const allTables = $derived.by(() => {
    const candidates: Candidate[] = [];
    if (project.id !== projectId) return candidates;
    for (const page of project.pages) {
      for (const object of getExtractionObjects(page).collection.objects) {
        if (object.kind === "table") candidates.push({ key: keyFor(page.id, object.id), page, object });
      }
    }
    return candidates;
  });
  const available = $derived(allTables.filter((candidate) => candidate.object.currentData.rows > 0 && candidate.object.currentData.columns > 0 &&
    !choices.some((choice) => choice.key === candidate.key)));
  const resolved = $derived(choices.map((choice) => {
    const current = allTables.find((candidate) => candidate.key === choice.key);
    return { choice, current, object: current?.object ?? choice.snapshot };
  }));
  const columnCounts = $derived(resolved.map((item) => item.object.currentData.columns));
  const differingColumns = $derived(new Set(columnCounts).size > 1);
  const mappingNeeded = $derived(customMapping || differingColumns);
  const destinationName = $derived(choices[0] ? project.pages.find((page) => page.id === choices[0].pageId)?.name ?? choices[0].pageName : "Choose a first table");
  const busy = $derived(disabled || preparing || applying);

  $effect(() => { if (dialog && !dialog.open) dialog.showModal(); });

  function describe(candidate: Candidate): string {
    const object = candidate.object;
    const current = extractionObjectFreshness(project, object).current;
    return candidate.page.name + " · " + object.name + " (" + object.currentData.rows + " × " + object.currentData.columns + ")" +
      (object.excluded || candidate.page.excluded ? " · excluded" : "") + (current ? "" : " · historical source");
  }
  function columnHint(object: TableExtractionObject, column: number): string {
    const cell = object.currentData.cells.find((item) => item.row === 0 && item.column <= column && item.column + item.columnSpan > column);
    if (!cell) return "No first-row cell";
    if (!cell.text.length) return "First-row cell is empty";
    return "First row: " + (cell.text.length > 90 ? cell.text.slice(0, 90) + "…" : cell.text);
  }
  function updateChoice(index: number, patch: Partial<Choice>) {
    choices = choices.map((choice, position) => position === index ? { ...choice, ...patch } : choice);
  }
  function addTable() {
    if (busy || composing) return;
    actionError = "";
    localNotice = "";
    if (choices.length >= 16) { actionError = "A join can contain at most 16 tables."; return; }
    const candidate = available.find((item) => item.key === (candidateKey || available[0]?.key));
    if (!candidate) { actionError = "Choose an available nonempty table to add."; return; }
    const added = choiceFor(candidate.page.id, candidate.page.name, candidate.object);
    const next = [...choices, added];
    const counts = next.map((choice) => allTables.find((item) => item.key === choice.key)?.object.currentData.columns ?? choice.snapshot.currentData.columns);
    choices = next;
    candidateKey = "";
    if (new Set(counts).size > 1) {
      customMapping = true;
      targetColumns = Math.max(typeof targetColumns === "number" && Number.isFinite(targetColumns) ? targetColumns : 0, ...counts);
      localNotice = "The tables have different column counts. Choose a destination for every source column before reviewing.";
    }
  }
  function normalizeFirst(next: Choice[]): { choices: Choice[]; reset: boolean } {
    if (next[0] && next[0].dropHeaderRows !== 0) return { choices: [{ ...next[0], dropHeaderRows: 0 }, ...next.slice(1)], reset: true };
    return { choices: next, reset: false };
  }
  function moveTable(index: number, delta: number) {
    if (busy || composing || index + delta < 0 || index + delta >= choices.length) return;
    const next = [...choices];
    [next[index], next[index + delta]] = [next[index + delta], next[index]];
    const normalized = normalizeFirst(next);
    choices = normalized.choices;
    actionError = "";
    localNotice = normalized.reset ? "The new first table keeps all its rows. Its repeated-header count was reset to 0." : "Table order updated. The first table determines the destination page.";
  }
  function removeTable(index: number) {
    if (busy || composing) return;
    const normalized = normalizeFirst(choices.filter((_, position) => position !== index));
    choices = normalized.choices;
    actionError = "";
    localNotice = normalized.reset ? "The new first table keeps all its rows. Its repeated-header count was reset to 0." : "";
  }
  function setMappingEnabled(enabled: boolean) {
    if (differingColumns) return;
    customMapping = enabled;
    if (enabled) targetColumns = Math.max(typeof targetColumns === "number" && Number.isFinite(targetColumns) ? targetColumns : 0, ...columnCounts, 1);
    actionError = "";
  }
  function useColumnOrder(index: number) {
    if (busy || composing) return;
    const item = resolved[index];
    if (!item) return;
    if (typeof targetColumns !== "number" || !Number.isSafeInteger(targetColumns) || targetColumns < item.object.currentData.columns || targetColumns > 1000) {
      actionError = "Set a joined column count of at least " + item.object.currentData.columns + " before mapping this table in column order.";
      return;
    }
    updateChoice(index, { columnMap: Array.from({ length: item.object.currentData.columns }, (_, column) => column + 1) });
    actionError = "";
    localNotice = "Column order selected for table " + (index + 1) + ". Review the destination columns against its header text.";
  }
  function setColumn(index: number, column: number, value: number) {
    const entry = choices[index];
    if (!entry) return;
    const mapping = [...entry.columnMap];
    mapping[column] = Number.isNaN(value) ? undefined : value;
    updateChoice(index, { columnMap: mapping });
  }
  function wholeNumber(value: number | undefined, minimum: number, maximum: number, label: string): number {
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < minimum || value > maximum) {
      throw new Error(label + " must be a whole number from " + minimum + " to " + maximum + ".");
    }
    return value;
  }
  function buildRequest(): ContinuationJoinRequest {
    if (choices.length < 2 || choices.length > 16) throw new Error("Choose between 2 and 16 tables in the order they should appear.");
    const name = joinedName.trim();
    if (!name) throw new Error("Enter a name for the joined table.");
    const columns = mappingNeeded ? wholeNumber(targetColumns, 1, 1000, "Joined column count") : undefined;
    const inputs = choices.map((choice, index) => {
      const current = allTables.find((item) => item.key === choice.key);
      if (!current) throw new Error("Table " + (index + 1) + " is no longer available. Remove it or choose its replacement; your other choices have been kept.");
      const data = current.object.currentData;
      if (!data.rows || !data.columns) throw new Error("Table " + (index + 1) + " is empty. Choose a nonempty table.");
      const dropHeaderRows = wholeNumber(choice.dropHeaderRows, 0, index === 0 ? 0 : data.rows, "Repeated header rows for table " + (index + 1));
      if (!mappingNeeded) return { pageId: choice.pageId, objectId: choice.objectId, dropHeaderRows };
      const columnMap = Array.from({ length: data.columns }, (_, column) =>
        wholeNumber(choice.columnMap[column], 1, columns!, "Joined column for table " + (index + 1) + " source column " + (column + 1)) - 1);
      if (new Set(columnMap).size !== columnMap.length) throw new Error("Each source column in table " + (index + 1) + " needs a different joined column. No source column can be dropped.");
      return { pageId: choice.pageId, objectId: choice.objectId, dropHeaderRows, columnMap };
    });
    return { inputs, name, ...(mappingNeeded ? { targetColumns: columns } : {}) };
  }
  async function review() {
    if (busy || composing || choices.length < 2) return;
    preparing = true;
    actionError = "";
    localNotice = "";
    try {
      if (project.id !== projectId) throw new Error("The active project changed. Your join choices remain here; return to the source project before reviewing.");
      const result = prepareContinuationJoin(project, buildRequest());
      preview = result;
      previewRejected = false;
      const small = result.inputs.reduce((sum, item) => sum + item.object.currentData.cells.length, 0) <= 200;
      const opened = { ...previewSourcesOpen };
      for (const item of result.inputs) {
        const key = keyFor(item.pageId, item.object.id);
        if (!(key in opened)) opened[key] = small;
      }
      previewSourcesOpen = opened;
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
    nameInput?.focus();
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
      localNotice = "This join is unapplied. Review and create the joined table to save it in the project.";
    }
  }
</script>

<dialog bind:this={dialog} class="table-join-dialog" aria-labelledby="table-join-title" data-table-join-dialog
  onkeydown={keydown} oncompositionstart={() => (composing = true)} oncompositionend={() => (composing = false)}
  oncancel={(event) => { event.preventDefault(); cancel(); }} onclose={onclose}>
  <h2 id="table-join-title">Join continuation tables</h2>
  <p class="help">Choose the tables in reading order. Joining creates a new table on the first table's page and preserves every source table.</p>
  <div class="join-options" hidden={!!preview}>
    <label class="field-label">Joined table name<input bind:this={nameInput} aria-label="Joined table name" bind:value={joinedName} maxlength="1000" disabled={busy} /></label>
    <div class="add-table-row">
      <label class="field-label">Continuation table<select aria-label="Continuation table" value={candidateKey || available[0]?.key || ""} disabled={busy || !available.length || choices.length >= 16} onchange={(event) => (candidateKey = event.currentTarget.value)}>
        {#if !available.length}<option value="">No other nonempty tables available</option>{/if}
        {#each available as candidate (candidate.key)}<option value={candidate.key}>{describe(candidate)}</option>{/each}
      </select></label>
      <button type="button" onclick={addTable} disabled={busy || composing || !available.length || choices.length >= 16}>Add table to join</button>
    </div>
    <p class="destination" data-join-destination>New table destination: <strong>{destinationName}</strong></p>
    <label class="mapping-choice"><input type="checkbox" aria-label="Set column mapping" checked={mappingNeeded} disabled={busy || differingColumns} onchange={(event) => setMappingEnabled(event.currentTarget.checked)} /><span>Set column mapping</span></label>
    {#if mappingNeeded}
      <label class="field-label column-count">Joined column count<input type="number" aria-label="Joined column count" min="1" max="1000" step="1" bind:value={targetColumns} disabled={busy} /></label>
      <p class="help">{differingColumns ? "Different column counts require a destination for every column in every input table. " : ""}Destinations start at 1. Each source column needs a unique destination within its table; extra destination positions become blank cells.</p>
    {:else}<p class="help">These tables have the same column count. Columns will keep their order; enable mapping to change their destinations.</p>{/if}

    <ol class="join-inputs" aria-label="Tables in join order">
      {#each resolved as item, index (item.choice.key)}
        {@const choice = item.choice}
        {@const object = item.object}
        {@const freshness = extractionObjectFreshness(project, object)}
        <li class="join-input" data-join-input-index={index} data-join-page-id={choice.pageId} data-join-object-id={choice.objectId}>
          <div class="input-title"><h3>{index + 1}. {item.current?.page.name ?? choice.pageName} · {object.name}</h3><span>{object.currentData.rows} × {object.currentData.columns}</span></div>
          <div class="input-badges"><span class:historical={!freshness.current}>{freshness.current ? "Current source" : "Historical source"}</span>
            {#if object.excluded}<span>Source table excluded</span>{/if}{#if item.current?.page.excluded}<span>Source page excluded</span>{/if}
          </div>
          {#if !item.current}<p class="warning">This source table is no longer available. Its saved selection is kept so you can remove or replace it.</p>{/if}
          <div class="input-actions">
            <button type="button" aria-label={"Move table " + (index + 1) + " up"} onclick={() => moveTable(index, -1)} disabled={busy || composing || index === 0}>Move up</button>
            <button type="button" aria-label={"Move table " + (index + 1) + " down"} onclick={() => moveTable(index, 1)} disabled={busy || composing || index === choices.length - 1}>Move down</button>
            <button type="button" aria-label={"Remove table " + (index + 1)} onclick={() => removeTable(index)} disabled={busy || composing}>Remove</button>
          </div>
          <label class="field-label header-count">Repeated header rows for table {index + 1}<input type="number" aria-label={"Repeated header rows for table " + (index + 1)}
            min="0" max={index === 0 ? 0 : object.currentData.rows} step="1" value={choice.dropHeaderRows ?? ""} disabled={busy || index === 0}
            oninput={(event) => updateChoice(index, { dropHeaderRows: Number.isNaN(event.currentTarget.valueAsNumber) ? undefined : event.currentTarget.valueAsNumber })} /></label>
          <p class="help">{index === 0 ? "The first table keeps all its rows." : "Choose how many top rows to omit from the joined result. The preview shows every omitted value, including rows that do not match the first table."}</p>
          {#if mappingNeeded}<details class="mapping-details" open={choice.mappingOpen} ontoggle={(event) => { if (choice.mappingOpen !== event.currentTarget.open) updateChoice(index, { mappingOpen: event.currentTarget.open }); }}>
            <summary>Column mapping for table {index + 1}</summary>
            {#if choice.mappingOpen}
              <button type="button" class="column-order" onclick={() => useColumnOrder(index)} disabled={busy || composing}>Use column order for table {index + 1}</button>
              <div class="column-map-grid">
                {#each Array.from({ length: object.currentData.columns }, (_, column) => column) as column (column)}
                  <label class="field-label">Source column {column + 1} → joined column<input type="number"
                    aria-label={"Joined column for table " + (index + 1) + " source column " + (column + 1)}
                    min="1" max={typeof targetColumns === "number" && Number.isFinite(targetColumns) ? Math.max(1, targetColumns) : 1000}
                    step="1" value={choice.columnMap[column] ?? ""} disabled={busy}
                    oninput={(event) => setColumn(index, column, event.currentTarget.valueAsNumber)} /><small class="column-hint">{columnHint(object, column)}</small></label>
                {/each}
              </div>
            {/if}
          </details>{/if}
          <details class="input-source" open={choice.sourceOpen} ontoggle={(event) => { if (choice.sourceOpen !== event.currentTarget.open) updateChoice(index, { sourceOpen: event.currentTarget.open }); }}>
            <summary>Table {index + 1} source and cells</summary>
            {#if choice.sourceOpen}<TableSourcePreview {project} {object} label={"Table " + (index + 1)} {imageUrl} />{/if}
          </details>
        </li>
      {/each}
    </ol>
    {#if choices.length < 2}<p class="help">Add at least two nonempty tables to review a join.</p>{/if}
  </div>

  {#if preview}
    <section class="join-preview" data-table-join-preview>
      <h3 bind:this={previewHeading} tabindex="-1">Review joined table</h3>
      <p class="help"><strong>{preview.result.name}</strong> will be created on <strong>{project.pages.find((page) => page.id === preview!.request.inputs[0].pageId)?.name ?? destinationName}</strong>. Create records one undoable project change.</p>
      {#if preview.removedHeaderCells.length}<section class="omitted-headers" data-join-removed-headers aria-label="Repeated header values omitted from joined table">
        <h4>Values omitted from the joined result</h4>
        <p class="help">These values remain in their source tables and saved snapshots.</p>
        {#each preview.removedHeaderCells as removed, index (removed.inputIndex + ":" + removed.cell.id + ":" + index)}
          <label class="field-label">Omitted header from table {removed.inputIndex + 1}, row {removed.cell.row + 1}, column {removed.cell.column + 1}<textarea readonly
            aria-label={"Omitted header from table " + (removed.inputIndex + 1) + ", row " + (removed.cell.row + 1) + ", column " + (removed.cell.column + 1)}
            value={removed.cell.text} rows="2" placeholder="Empty value" dir="auto"></textarea></label>
        {/each}
      </section>{/if}
      {#if preview.warnings.length}<ul class="join-warnings" aria-label="Table join notes">{#each preview.warnings as warning}<li>{warning}</li>{/each}</ul>{/if}
      <div data-join-proposed-table><TableGridPreview table={preview.result.currentData} label="Proposed joined table" /></div>
      <div class="input-previews">
        {#each preview.inputs as item, index (keyFor(item.pageId, item.object.id))}
          {@const key = keyFor(item.pageId, item.object.id)}
          <details data-join-input-preview-index={index} open={previewSourcesOpen[key]} ontoggle={(event) => { if (previewSourcesOpen[key] !== event.currentTarget.open) previewSourcesOpen = { ...previewSourcesOpen, [key]: event.currentTarget.open }; }}>
            <summary>Input table {index + 1}: {project.pages.find((page) => page.id === item.pageId)?.name ?? item.pageId} · {item.object.name}</summary>
            {#if previewSourcesOpen[key]}<TableSourcePreview {project} object={item.object} label={"Input table " + (index + 1)} {imageUrl} />{/if}
          </details>
        {/each}
      </div>
    </section>
  {/if}
  <p class="inclusion-note">Source tables keep their current inclusion settings. Including source tables alongside the joined table repeats their rows; review which tables to include before exporting.</p>
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
  {#if previewRejected}<p class="warning">This preview cannot be applied. Table order, header counts, and mappings have been kept. Review again against the current sources, or go back to adjust the choices.</p>{/if}
  {#if localNotice}<p class="local-notice" role="status">{localNotice}</p>{/if}
  {#if composing}<p class="help">Finish the current text input before reviewing or creating the joined table.</p>{/if}
  <footer class="dialog-actions">
    <button type="button" onclick={cancel} disabled={preparing || applying || composing}>Cancel</button>
    {#if preview}
      <button type="button" onclick={backToOptions} disabled={busy || composing}>Back to join options</button>
      <button type="button" onclick={review} disabled={busy || composing}>Review again</button>
      <button type="button" class="primary" onclick={apply} disabled={busy || composing || previewRejected}>Create joined table</button>
    {:else}<button type="button" class="primary" onclick={review} disabled={busy || composing || choices.length < 2}>Review joined table</button>{/if}
  </footer>
</dialog>

<style>
  .table-join-dialog { box-sizing: border-box; width: min(1120px, calc(100vw - 28px)); max-width: calc(100vw - 28px); max-height: calc(100vh - 30px); overflow: auto; padding: 22px; border: 1px solid #71717a; border-radius: 10px; background: #27272a; color: #dbeafe; box-shadow: 0 18px 80px #0009; font: inherit; font-size: 13px; }
  .table-join-dialog::backdrop { background: #07101dd4; }
  h2 { margin: 0 0 9px; font-size: 20px; } h3 { margin: 0; font-size: 15px; line-height: 1.6; overflow-wrap: anywhere; } h4 { margin: 0; font-size: 13px; }
  .help { margin: 9px 0 12px; color: #93c5fd; font-size: 12px; line-height: 1.7; overflow-wrap: anywhere; }
  .field-label { display: flex; flex-direction: column; gap: 6px; min-width: 0; margin: 12px 0; color: #bfdbfe; font-size: 12px; }
  input:not([type="checkbox"]), select, textarea { box-sizing: border-box; width: 100%; min-width: 0; padding: 9px 10px; border: 1px solid #52525b; border-radius: 5px; background: #172554; color: #dbeafe; font: inherit; font-size: 13px; line-height: 1.6; }
  textarea { resize: vertical; }
  .join-options[hidden] { display: none; }
  .add-table-row { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: end; gap: 12px; margin: 15px 0; }
  .add-table-row .field-label { margin: 0; }
  .destination { color: #bfdbfe; font-size: 12px; line-height: 1.7; overflow-wrap: anywhere; }
  .mapping-choice { display: flex; align-items: center; gap: 9px; margin: 14px 0 4px; color: #bfdbfe; font-size: 12px; }
  .column-count, .header-count { max-width: 280px; }
  .join-inputs { list-style: none; padding: 0; margin: 18px 0 0; }
  .join-input { margin: 14px 0; padding: 14px; border: 1px solid #52525b; border-radius: 6px; background: #18181b; }
  .input-title { display: flex; justify-content: space-between; gap: 12px; align-items: start; }
  .input-title > span { flex: none; padding-top: 4px; color: #93c5fd; font-size: 11px; }
  .input-badges { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0 10px; }
  .input-badges > span { padding: 3px 6px; border: 1px solid #71717a; border-radius: 4px; background: #14532d; color: #86efac; font-size: 10px; }
  .input-badges > span.historical { border-color: #71717a; background: #27272a; color: #fcd34d; }
  .input-actions { display: flex; flex-wrap: wrap; gap: 8px; margin: 10px 0; }
  details { margin: 12px 0; padding: 10px; border: 1px solid #52525b; border-radius: 5px; }
  summary { color: #bfdbfe; font-size: 12px; line-height: 1.7; cursor: pointer; overflow-wrap: anywhere; }
  .column-order { margin: 12px 0 0; }
  .column-map-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 0 12px; margin-top: 3px; }
  .column-map-grid .field-label { font-size: 11px; }
  .column-hint { color: #93c5fd; font-size: 10px; line-height: 1.6; overflow-wrap: anywhere; }
  .join-preview > h3 { margin: 15px 0 7px; }
  .omitted-headers { margin: 15px 0; padding: 13px; border: 1px solid #71717a; border-radius: 5px; background: #27272a; }
  .omitted-headers h4 { color: #fcd34d; line-height: 1.6; }
  .join-warnings { padding-left: 21px; color: #fcd34d; font-size: 12px; line-height: 1.7; }
  .input-previews { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 0 14px; margin-top: 17px; }
  .input-previews > details { min-width: 0; }
  .inclusion-note { margin: 16px 0 12px; padding: 10px; border-left: 3px solid #71717a; color: #bfdbfe; background: #27272a; font-size: 12px; line-height: 1.7; }
  .error, .warning { padding: 11px; border: 1px solid #71717a; border-radius: 5px; background: #27272a; color: #fcd34d; font-size: 12px; line-height: 1.7; overflow-wrap: anywhere; }
  .error { border-color: #71717a; background: #27272a; color: #fecaca; }
  .local-notice { color: #bfdbfe; font-size: 12px; line-height: 1.7; }
  .dialog-actions { position: sticky; bottom: -22px; display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 9px; margin: 18px -1px -1px; padding: 14px 0 2px; border-top: 1px solid #52525b; background: #27272a; }
  button { padding: 9px 12px; border: 1px solid #71717a; border-radius: 5px; background: #3f3f46; color: #dbeafe; font: inherit; font-size: 12px; cursor: pointer; }
  button.primary { border-color: #93c5fd; background: #2563eb; }
  button:disabled, input:disabled, select:disabled { opacity: .5; cursor: default; }
  button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible, summary:focus-visible { outline: 2px solid #bfdbfe; outline-offset: 2px; }
  h3:focus { outline: none; }
  @media (max-width: 720px) { .table-join-dialog { padding: 16px; } .add-table-row { grid-template-columns: minmax(0, 1fr); } .column-map-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } .input-previews { grid-template-columns: minmax(0, 1fr); } .dialog-actions { bottom: -16px; } .dialog-actions button { flex: 1 1 auto; } }
  @media (max-width: 430px) { .column-map-grid { grid-template-columns: minmax(0, 1fr); } .join-input { padding: 11px; } .input-title { flex-direction: column; gap: 2px; } }
</style>
