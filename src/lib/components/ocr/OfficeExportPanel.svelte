<script lang="ts">
  import type { OcrProject } from "../../ocr/model";
  import { getExtractionObjects, extractionObjectFreshness } from "../../ocr/extractionObjects";
  import { MAX_OFFICE_OBJECTS, officeObjectKey, officeScopedPages, prepareOfficeExport, type OfficeExportOptions, type OfficeObjectChoice } from "../../ocr/officeExport";

  let { project, pageIds, value, disabled = false, onchange }: {
    project: OcrProject;
    pageIds?: string[];
    value: OfficeExportOptions;
    disabled?: boolean;
    onchange: (next: OfficeExportOptions) => void;
  } = $props();

  const pages = $derived(officeScopedPages(project, pageIds));
  const catalog = $derived(pages.map(page => ({ page, objects: getExtractionObjects(page).collection.objects.filter(object => object.kind === "table" || object.kind === "formula") })));
  const check = $derived(prepareOfficeExport(project, pageIds, value));
  const selectedKeys = $derived(new Set(value.objects.map(choice => officeObjectKey(choice.pageId, choice.objectId))));
  const count = $derived(catalog.reduce((sum, entry) => sum + entry.objects.length, 0));
  const describe = (name: string, pageName: string) => `${name} from ${pageName}`;

  function toggle(pageId: string, objectId: string, selected: boolean) {
    const key = officeObjectKey(pageId, objectId);
    if (selected && value.objects.length < MAX_OFFICE_OBJECTS && !selectedKeys.has(key)) {
      onchange({ version: 1, objects: [...value.objects, { pageId, objectId, placement: "append" }] });
    } else if (!selected) {
      onchange({ version: 1, objects: value.objects.filter(choice => officeObjectKey(choice.pageId, choice.objectId) !== key) });
    }
  }
  function placement(index: number, next: OfficeObjectChoice["placement"]) {
    onchange({ version: 1, objects: value.objects.map((choice, i) => i === index ? { pageId: choice.pageId, objectId: choice.objectId, placement: next } : choice) });
  }
  function target(index: number, regionId: string) {
    onchange({ version: 1, objects: value.objects.map((choice, i) => i === index ? { pageId: choice.pageId, objectId: choice.objectId, placement: "replace-region", ...(regionId ? { regionId } : {}) } : choice) });
  }
  function neighbor(index: number, direction: -1 | 1): number {
    const current = value.objects[index];
    for (let i = index + direction; i >= 0 && i < value.objects.length; i += direction) {
      if (value.objects[i].pageId === current.pageId && value.objects[i].placement === "append") return i;
    }
    return -1;
  }
  function move(index: number, direction: -1 | 1) {
    const other = neighbor(index, direction);
    if (other < 0) return;
    const choices = [...value.objects];
    [choices[index], choices[other]] = [choices[other], choices[index]];
    onchange({ version: 1, objects: choices });
  }
  function remove(index: number) {
    onchange({ version: 1, objects: value.objects.filter((_, i) => i !== index) });
  }
</script>

<section class="office-options" data-office-export-options aria-label="Word extracted content">
  <div class="office-heading"><h3>Word extracted content</h3><span>{value.objects.length} selected</span></div>
  <p class="office-help">Choose saved tables and formulas explicitly. Each selected object is added at the end of its own page unless you choose a passage to replace. These choices do not edit the project.</p>
  {#if count}
    <div class="object-catalog">
      {#each catalog as entry (entry.page.id)}
        {#if entry.objects.length}
          <fieldset disabled={disabled}>
            <legend>{entry.page.name}</legend>
            {#each entry.objects as object (object.id)}
              {@const selected = selectedKeys.has(officeObjectKey(entry.page.id, object.id))}
              {@const freshness = extractionObjectFreshness(project, object)}
              <div class="object-candidate" data-office-candidate data-office-page-id={entry.page.id} data-office-object-id={object.id}>
                <label class="include-object">
                  <input type="checkbox" checked={selected} disabled={object.excluded || (!selected && value.objects.length >= MAX_OFFICE_OBJECTS)}
                    aria-label={`Include ${describe(object.name, entry.page.name)} in Word`}
                    onchange={(event) => toggle(entry.page.id, object.id, event.currentTarget.checked)} />
                  <span><strong>{object.name}</strong><small>{object.kind === "table" ? `${object.currentData.rows} rows × ${object.currentData.columns} columns` : "Formula · editable LaTeX"}{object.excluded ? " · excluded" : ""}</small></span>
                </label>
                <span class="source-state" class:historical={!freshness.current}>{freshness.current ? "Current source" : "Historical source"}</span>
                {#if !freshness.current}<p class="historical-reason">{freshness.reasons.join(" ")}</p>{/if}
                {#if object.originalOutputKind === "table-join"}<p class="office-help joined-hint">Joined table. Its source tables stay separate and are not selected automatically.</p>{/if}
              </div>
            {/each}
          </fieldset>
        {/if}
      {/each}
    </div>
  {:else}
    <p class="office-help">No saved tables or formulas on the included pages in this scope. Use Extract to create them. Existing OCR passages and figures are still exported.</p>
  {/if}

  {#if value.objects.length}
    <div class="selection-heading"><h4>Placement choices</h4><button type="button" disabled={disabled} onclick={() => onchange({ version: 1, objects: [] })}>Clear Word choices</button></div>
    <div class="placement-choices">
      {#each check.selections as selection, index (officeObjectKey(selection.choice.pageId, selection.choice.objectId) + ":" + index)}
        {@const label = describe(selection.label, selection.page?.name ?? "missing page")}
        {@const candidates = selection.page?.regions.filter(region => !region.excluded && region.kind === selection.object?.kind) ?? []}
        {@const replaced = selection.choice.placement === "replace-region" ? candidates.find(region => region.id === selection.choice.regionId) : undefined}
        <section class="placement-card" data-office-selection data-office-page-id={selection.choice.pageId} data-office-object-id={selection.choice.objectId}>
          <h4>{selection.label}</h4><p class="page-label">{selection.page?.name ?? "Source page no longer exists"}</p>
          <label class="office-field">Placement
            <select aria-label={`Placement for ${label}`} value={selection.choice.placement} disabled={disabled}
              onchange={(event) => placement(index, event.currentTarget.value as OfficeObjectChoice["placement"])}>
              <option value="append">Append at end of this page</option>
              <option value="replace-region">Replace one OCR passage</option>
            </select>
          </label>
          {#if selection.choice.placement === "replace-region"}
            <label class="office-field">Passage to replace
              <select aria-label={`Replace passage for ${label}`} value={selection.choice.regionId ?? ""} disabled={disabled}
                onchange={(event) => target(index, event.currentTarget.value)}>
                <option value="">Choose an included {selection.object?.kind ?? "matching"} passage</option>
                {#if selection.choice.regionId && !replaced}<option value={selection.choice.regionId}>Earlier choice is unavailable</option>{/if}
                {#each candidates as region (region.id)}<option value={region.id}>Passage {(selection.page?.regions.indexOf(region) ?? 0) + 1} · {region.text.replace(/\s+/g, " ").slice(0, 90) || "(empty text)"}</option>{/each}
              </select>
            </label>
            {#if replaced}
              <label class="office-field">OCR text omitted by this replacement<textarea readonly rows="4" aria-label={`OCR text replaced by ${label}`} value={replaced.text}></textarea></label>
              <p class="office-help">Only this passage is replaced. Other source passages remain included, including passages used by a joined table.</p>
            {:else}<p class="office-help">Choose a same-kind passage explicitly. Matching capture boxes never select a replacement for you.</p>{/if}
          {:else}
            <p class="office-help">Append order on this page: {value.objects.slice(0, index + 1).filter(choice => choice.pageId === selection.choice.pageId && choice.placement === "append").length}. Existing OCR passages remain included.</p>
            <div class="choice-actions">
              <button type="button" aria-label={`Move ${label} earlier on page`} disabled={disabled || neighbor(index, -1) < 0} onclick={() => move(index, -1)}>Move earlier</button>
              <button type="button" aria-label={`Move ${label} later on page`} disabled={disabled || neighbor(index, 1) < 0} onclick={() => move(index, 1)}>Move later</button>
            </div>
          {/if}
          {#if selection.errors.length}<ul class="choice-errors">{#each selection.errors as problem}<li>{problem}</li>{/each}</ul>{/if}
          <button type="button" class="remove-choice" aria-label={`Remove ${label} from Word`} disabled={disabled} onclick={() => remove(index)}>Remove choice</button>
        </section>
      {/each}
    </div>
  {/if}
  <details class="word-limits"><summary>What Word preserves</summary>
    <p>Headings, inline styles, lists, alignment, page breaks, reading columns and figures use the reviewed document structure. Tables preserve literal cell text and spans.</p>
    <p>Word list numbering keeps any literal bullets or numbers already in the text. Remove an existing marker in the text editor if it would produce two visible markers.</p>
    <p>Formulas remain editable LaTeX text. Headers, footers and notes stay in the body. Figure alt text, decorative flags and repeating table-header rows are not written. Word lays out the document; the original scan's fonts and exact positions are not reproduced.</p>
  </details>
</section>

<style>
  .office-options { margin: 20px 0; padding: 15px 0; border-top: 1px solid #3c516f; border-bottom: 1px solid #3c516f; min-width: 0; }
  .office-heading, .selection-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; flex-wrap: wrap; }
  h3, h4 { margin: 0; color: #cbdcf1; font-size: 12px; overflow-wrap: anywhere; }
  .office-heading > span { color: #a9c4e3; font-size: 11px; }
  .office-help, .historical-reason, .word-limits, .page-label { color: #a6bdd8; font-size: 11px; line-height: 1.7; overflow-wrap: anywhere; }
  .office-help { margin: 9px 0 12px; }
  .object-catalog { max-height: 340px; overflow: auto; padding-right: 3px; }
  fieldset { margin: 12px 0; padding: 10px; border: 1px solid #3b5473; border-radius: 5px; min-width: 0; }
  legend { padding: 0 5px; color: #bcd1eb; font-size: 11px; overflow-wrap: anywhere; max-width: 100%; }
  .object-candidate + .object-candidate { padding-top: 12px; margin-top: 12px; border-top: 1px solid #344a64; }
  .include-object { display: flex; align-items: flex-start; gap: 8px; color: #d0e1f5; font-size: 12px; }
  .include-object input { margin-top: 3px; flex-shrink: 0; }
  .include-object span { min-width: 0; overflow-wrap: anywhere; }
  .include-object small { display: block; color: #a0b8d6; line-height: 1.7; margin-top: 3px; }
  .source-state { display: inline-block; margin: 7px 0 0 23px; padding: 2px 5px; border: 1px solid #417565; border-radius: 3px; color: #b9e1d2; font-size: 10px; }
  .source-state.historical { color: #e9c996; border-color: #896c40; }
  .historical-reason { margin: 5px 0 0 23px; color: #e3c99d; }
  .joined-hint { margin-bottom: 0; }
  .selection-heading { margin: 16px 0 10px; }
  .placement-card { padding: 12px; margin: 10px 0; border: 1px solid #3d5572; border-radius: 5px; background: #14273d; min-width: 0; }
  .page-label { margin: 4px 0 12px; }
  .office-field { display: flex; flex-direction: column; gap: 6px; margin: 10px 0; font-size: 11px; color: #bbd0eb; }
  select, textarea { box-sizing: border-box; width: 100%; min-width: 0; padding: 8px; border: 1px solid #455f80; border-radius: 4px; color: #d4e5f8; background: #0f2035; font: inherit; font-size: 12px; }
  textarea { resize: vertical; line-height: 1.65; }
  .choice-actions { display: flex; gap: 7px; flex-wrap: wrap; }
  button { border: 1px solid #4d698a; border-radius: 4px; background: #203b59; color: #c9def4; font-size: 11px; padding: 7px 9px; cursor: pointer; max-width: 100%; }
  button:disabled { opacity: .45; cursor: default; }
  button:focus-visible, select:focus-visible, textarea:focus-visible, input:focus-visible, summary:focus-visible { outline: 2px solid #85b8ff; outline-offset: 3px; }
  .remove-choice { margin-top: 11px; }
  .choice-errors { margin: 12px 0 0; padding-left: 17px; color: #ffd1db; font-size: 11px; line-height: 1.7; overflow-wrap: anywhere; }
  .word-limits { margin-top: 16px; }
  summary { cursor: pointer; color: #bfd6f2; }
  .word-limits p { margin: 9px 0; }
</style>
