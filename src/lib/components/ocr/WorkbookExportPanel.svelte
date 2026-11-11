<script lang="ts">
  import type { OcrProject } from "../../ocr/model";
  import { extractionObjectFreshness, getExtractionObjects } from "../../ocr/extractionObjects";
  import { MAX_WORKBOOK_SHEETS, emptyWorkbookOptions, nextWorksheetName, prepareWorkbookExport, workbookObjectKey, workbookScopedPages, worksheetNameError, worksheetNameKey, type WorkbookOptions } from "../../ocr/workbookExport";
  import TableGridPreview from "./TableGridPreview.svelte";

  let { project, pageIds, value, disabled = false, onchange }: {
    project: OcrProject;
    pageIds?: string[];
    value: WorkbookOptions;
    disabled?: boolean;
    onchange: (next: WorkbookOptions) => void;
  } = $props();
  let previewOpen = $state<Record<string, boolean>>({});
  const pages = $derived(workbookScopedPages(project, pageIds));
  const catalog = $derived(pages.map(page => {
    const saved = getExtractionObjects(page);
    return { page, objects: saved.collection.objects.filter(object => object.kind === "table"), diagnostics: saved.diagnostics };
  }));
  const count = $derived(catalog.reduce((sum, entry) => sum + entry.objects.length, 0));
  const check = $derived(prepareWorkbookExport(project, pageIds, value));
  const selectedKeys = $derived(new Set(value.sheets.map(sheet => workbookObjectKey(sheet.pageId, sheet.objectId))));
  const describe = (name: string, pageName: string) => `${name} from ${pageName}`;

  function toggle(pageId: string, objectId: string, selected: boolean) {
    const key = workbookObjectKey(pageId, objectId);
    if (selected && !selectedKeys.has(key) && value.sheets.length < MAX_WORKBOOK_SHEETS) {
      onchange({ version: 1, sheets: [...value.sheets, { pageId, objectId, sheetName: nextWorksheetName(value) }] });
    } else if (!selected) {
      onchange({ version: 1, sheets: value.sheets.filter(sheet => workbookObjectKey(sheet.pageId, sheet.objectId) !== key) });
    }
  }
  function rename(index: number, sheetName: string) {
    onchange({ version: 1, sheets: value.sheets.map((sheet, i) => i === index ? { ...sheet, sheetName } : sheet) });
  }
  function move(index: number, direction: -1 | 1) {
    const other = index + direction;
    if (other < 0 || other >= value.sheets.length) return;
    const sheets = [...value.sheets];
    [sheets[index], sheets[other]] = [sheets[other], sheets[index]];
    onchange({ version: 1, sheets });
  }
  function remove(index: number) {
    onchange({ version: 1, sheets: value.sheets.filter((_, i) => i !== index) });
  }
  function selectionKey(index: number): string {
    const sheet = value.sheets[index], key = workbookObjectKey(sheet.pageId, sheet.objectId);
    // Preserve keyboard focus when a sheet moves. The suffix also makes an
    // invalid repeated choice inspectable instead of breaking keyed rendering.
    return key + ":" + value.sheets.slice(0, index).filter(prior => workbookObjectKey(prior.pageId, prior.objectId) === key).length;
  }
  function invalidName(index: number): boolean {
    const name = value.sheets[index].sheetName;
    return !!worksheetNameError(name) || value.sheets.some((sheet, i) => i !== index && worksheetNameKey(sheet.sheetName) === worksheetNameKey(name));
  }
</script>

<section class="workbook-options" data-workbook-export-options aria-label="Workbook worksheets">
  <div class="workbook-heading"><h3>Workbook worksheets</h3><span>{value.sheets.length} selected</span></div>
  <p class="workbook-help">Each chosen saved table becomes one worksheet, in the order below. Values are stored as text to preserve leading zeros, currency strings and formula-looking text. Cell spans become merged cells.</p>
  {#if count}
    <div class="table-catalog">
      {#each catalog as entry (entry.page.id)}
        {#if entry.objects.length || entry.diagnostics.length}
          <fieldset disabled={disabled}>
            <legend>{entry.page.name}</legend>
            {#each entry.objects as object (object.id)}
              {@const selected = selectedKeys.has(workbookObjectKey(entry.page.id, object.id))}
              {@const freshness = extractionObjectFreshness(project, object)}
              <div class="table-candidate" data-workbook-candidate data-workbook-page-id={entry.page.id} data-workbook-object-id={object.id}>
                <label class="include-table">
                  <input type="checkbox" checked={selected} disabled={object.excluded || (!selected && value.sheets.length >= MAX_WORKBOOK_SHEETS)}
                    aria-label={`Include ${describe(object.name, entry.page.name)} in workbook`}
                    onchange={(event) => toggle(entry.page.id, object.id, event.currentTarget.checked)} />
                  <span><strong>{object.name}</strong><small>{object.currentData.rows} rows × {object.currentData.columns} columns{object.excluded ? " · excluded" : ""}{!object.currentData.rows ? " · empty worksheet" : ""}</small></span>
                </label>
                <span class="source-state" class:historical={!freshness.current}>{freshness.current ? "Current source" : "Historical source"}</span>
                {#if !freshness.current}<p class="historical-reason">{freshness.reasons.join(" ")} Saved values remain available.</p>{/if}
                {#if object.originalOutputKind === "table-join"}<p class="workbook-help joined-hint">Joined table. Choose its source tables separately only if you also want their worksheets.</p>{/if}
              </div>
            {/each}
            {#if entry.diagnostics.length}<ul class="source-notes">{#each entry.diagnostics as diagnostic}<li>{diagnostic}</li>{/each}</ul>{/if}
          </fieldset>
        {/if}
      {/each}
    </div>
  {:else}
    <p class="workbook-help">No saved tables on the included pages in this scope. Use Extract → Table to create one, or change Pages.</p>
    {#each catalog.filter(entry => entry.diagnostics.length) as entry (entry.page.id)}
      <p class="source-notes">{entry.page.name}: {entry.diagnostics.join(" ")}</p>
    {/each}
  {/if}
  {#if value.sheets.length >= MAX_WORKBOOK_SHEETS}<p class="source-notes">This workbook has reached the limit of 512 worksheets. Remove a choice before adding another.</p>{/if}

  {#if value.sheets.length}
    <div class="selection-heading"><h4>Worksheet order</h4><button type="button" disabled={disabled} onclick={() => onchange(emptyWorkbookOptions())}>Clear workbook choices</button></div>
    {#each check.selections as selection, index (selectionKey(index))}
      {@const label = describe(selection.label, selection.page?.name ?? "missing page")}
      {@const key = workbookObjectKey(selection.choice.pageId, selection.choice.objectId)}
      <section class="sheet-card" data-workbook-sheet data-workbook-page-id={selection.choice.pageId} data-workbook-object-id={selection.choice.objectId}>
        <h4>Worksheet {index + 1} · {selection.label}</h4><p class="page-label">{selection.page?.name ?? "Source page no longer exists"}</p>
        <label class="workbook-field">Worksheet name
          <input type="text" aria-label={`Worksheet name for ${label}`} aria-invalid={invalidName(index)} value={selection.choice.sheetName}
            disabled={disabled} spellcheck="false" oninput={(event) => rename(index, event.currentTarget.value)} />
        </label>
        <div class="sheet-actions">
          <button type="button" aria-label={`Move worksheet ${selection.choice.sheetName} earlier`} disabled={disabled || index === 0} onclick={() => move(index, -1)}>Move earlier</button>
          <button type="button" aria-label={`Move worksheet ${selection.choice.sheetName} later`} disabled={disabled || index + 1 === value.sheets.length} onclick={() => move(index, 1)}>Move later</button>
        </div>
        {#if selection.errors.length}<ul class="sheet-errors" role="alert">{#each selection.errors as problem}<li>{problem}</li>{/each}</ul>{/if}
        {#if selection.warnings.length}<ul class="source-notes">{#each selection.warnings as warning}<li>{warning}</li>{/each}</ul>{/if}
        {#if selection.object}
          <details class="table-preview" ontoggle={(event) => (previewOpen = { ...previewOpen, [key]: event.currentTarget.open })}>
            <summary>Preview saved table</summary>
            {#if previewOpen[key]}<TableGridPreview table={selection.object.currentData} label={selection.label + " worksheet values"} />{/if}
          </details>
        {/if}
        <button type="button" class="remove-sheet" aria-label={`Remove ${label} from workbook`} disabled={disabled} onclick={() => remove(index)}>Remove worksheet</button>
      </section>
    {/each}
  {:else}<p class="workbook-help">Select at least one saved table for this workbook.</p>{/if}
  <details class="workbook-limits"><summary>Worksheet names and limits</summary>
    <p>Names must be unique, contain 1–31 characters, and exclude control characters and / \ ? * : [ ]. They cannot begin or end with an apostrophe or use the reserved name History. Some Unicode characters, including many emoji, count as two.</p>
    <p>A workbook can contain up to 512 selected worksheets, 1,000,000 grid positions and 64 MiB of cell text. Each cell can contain up to 32,767 characters and 253 line feeds. Some Unicode characters count as two. Values are not shortened or converted into formulas.</p>
  </details>
</section>

<style>
  .workbook-options { margin: 20px 0; padding: 15px 0; border-top: 1px solid #52525b; border-bottom: 1px solid #52525b; min-width: 0; }
  .workbook-heading, .selection-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; flex-wrap: wrap; }
  h3, h4 { margin: 0; color: #bfdbfe; font-size: 12px; overflow-wrap: anywhere; }
  .workbook-heading > span { color: #93c5fd; font-size: 11px; }
  .workbook-help, .historical-reason, .workbook-limits, .page-label, .source-notes { color: #93c5fd; font-size: 11px; line-height: 1.7; overflow-wrap: anywhere; }
  .workbook-help { margin: 9px 0 12px; }
  .table-catalog { max-height: 340px; overflow: auto; padding-right: 3px; }
  fieldset { margin: 12px 0; padding: 10px; border: 1px solid #52525b; border-radius: 5px; min-width: 0; }
  legend { padding: 0 5px; color: #d4d4d8; font-size: 11px; overflow-wrap: anywhere; max-width: 100%; }
  .table-candidate + .table-candidate { padding-top: 12px; margin-top: 12px; border-top: 1px solid #3f3f46; }
  .include-table { display: flex; align-items: flex-start; gap: 8px; color: #bfdbfe; font-size: 12px; }
  .include-table input { margin-top: 3px; flex-shrink: 0; }
  .include-table span { min-width: 0; overflow-wrap: anywhere; }
  .include-table small { display: block; color: #93c5fd; line-height: 1.7; margin-top: 3px; }
  .source-state { display: inline-block; margin: 7px 0 0 23px; padding: 2px 5px; border: 1px solid #71717a; border-radius: 3px; color: #86efac; font-size: 10px; }
  .source-state.historical { color: #fcd34d; border-color: #71717a; }
  .historical-reason { margin: 5px 0 0 23px; color: #fcd34d; }
  .joined-hint { margin-bottom: 0; }
  .selection-heading { margin: 16px 0 10px; }
  .sheet-card { padding: 12px; margin: 10px 0; border: 1px solid #52525b; border-radius: 5px; background: #27272a; min-width: 0; }
  .page-label { margin: 4px 0 12px; }
  .workbook-field { display: flex; flex-direction: column; gap: 6px; margin: 10px 0; font-size: 11px; color: #d4d4d8; }
  input[type="text"] { box-sizing: border-box; width: 100%; min-width: 0; padding: 8px; border: 1px solid #52525b; border-radius: 4px; color: #dbeafe; background: #172554; font: inherit; font-size: 12px; }
  input[aria-invalid="true"] { border-color: #f87171; }
  .sheet-actions { display: flex; gap: 7px; flex-wrap: wrap; }
  button { border: 1px solid #71717a; border-radius: 4px; background: #3f3f46; color: #bfdbfe; font-size: 11px; padding: 7px 9px; cursor: pointer; max-width: 100%; }
  button:disabled { opacity: .45; cursor: default; }
  button:focus-visible, input:focus-visible, summary:focus-visible { outline: 2px solid #93c5fd; outline-offset: 3px; }
  .remove-sheet { margin-top: 11px; }
  .sheet-errors, ul.source-notes { margin: 12px 0 0; padding-left: 17px; font-size: 11px; line-height: 1.7; overflow-wrap: anywhere; }
  .sheet-errors { color: #fecaca; } .source-notes { color: #fcd34d; }
  .table-preview { margin-top: 12px; font-size: 11px; }
  .table-preview summary { margin-bottom: 10px; }
  .workbook-limits { margin-top: 16px; }
  summary { cursor: pointer; color: #bfdbfe; }
  .workbook-limits p { margin: 9px 0; }
</style>
