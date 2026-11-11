<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import type { SaveStatus } from "$lib/ocr/autosave";
  import { projectPageText, type PageTextDraft, type PageTextEditPreview } from "$lib/ocr/pageTextDraft";

  let { draft, saveStatus, disabled = false, imageUrl, onupdate, onreview, onapply, ondiscard, onclose, onsave, onlocalunsaved }: {
    draft: PageTextDraft;
    saveStatus: SaveStatus;
    disabled?: boolean;
    imageUrl: (path: string) => string;
    onupdate: (text: string) => string | null;
    onreview: () => PageTextEditPreview | null;
    onapply: (preview: PageTextEditPreview, acceptCombinedCoverage: boolean) => string | null;
    ondiscard: () => string | null;
    onclose: () => void;
    onsave: () => Promise<void>;
    onlocalunsaved: (unsaved: boolean) => void;
  } = $props();

  let dialog = $state<HTMLDialogElement>();
  let editor = $state<HTMLTextAreaElement>();
  let reviewHeading = $state<HTMLHeadingElement>();
  let editingText = $state(untrack(() => draft.text));
  let preview = $state<PageTextEditPreview | null>(null);
  let acceptCombinedCoverage = $state(false);
  let composing = $state(false);
  let discardPending = $state(false);
  let actionError = $state("");
  let inputError = $state("");
  let localNotice = $state("");
  let retrying = $state(false);
  let lastDraftId = untrack(() => draft.id);
  let lastRevision = untrack(() => draft.revision);
  const sourceText = $derived(projectPageText(draft.page).replace(/\r\n?/gu, "\n"));
  const changed = $derived(editingText !== sourceText);
  const localUnsaved = $derived(editingText !== draft.text);
  const statusText = $derived(localUnsaved ? "This input has not been saved"
    : saveStatus.state === "saved" ? "Draft saved · not applied"
    : saveStatus.state === "saving" ? "Saving draft · not applied"
    : saveStatus.state === "pending" ? "Draft waiting to save · not applied"
    : "Draft save failed · not applied");
  const affected = $derived(preview ? preview.page.regions.filter((region) => preview!.regionIds.includes(region.id)) : []);
  const coverageViewBox = $derived.by(() => {
    if (!preview || !affected.length) return "0 0 1 1";
    const boxes = [...affected, ...preview.regions];
    const x = Math.max(0, Math.min(...boxes.map((region) => region.x)) - 12);
    const y = Math.max(0, Math.min(...boxes.map((region) => region.y)) - 12);
    const right = Math.min(preview.page.width, Math.max(...boxes.map((region) => region.x + region.w)) + 12);
    const bottom = Math.min(preview.page.height, Math.max(...boxes.map((region) => region.y + region.h)) + 12);
    return `${x} ${y} ${Math.max(1, right - x)} ${Math.max(1, bottom - y)}`;
  });

  $effect(() => { if (dialog && !dialog.open) dialog.showModal(); });
  $effect(() => {
    if (draft.id !== lastDraftId || draft.revision !== lastRevision) {
      lastDraftId = draft.id;
      lastRevision = draft.revision;
      if (!inputError && editingText !== draft.text) editingText = draft.text;
      if (preview && (preview.draftId !== draft.id || preview.draftRevision !== draft.revision)) {
        preview = null;
        acceptCombinedCoverage = false;
        localNotice = "The draft changed. Review the latest text before applying it.";
      }
    }
  });
  $effect(() => { onlocalunsaved(localUnsaved); });
  onDestroy(() => onlocalunsaved(false));

  function update(text: string) {
    editingText = text;
    actionError = "";
    localNotice = "";
    try { inputError = onupdate(text) ?? ""; }
    catch (error) { inputError = error instanceof Error ? error.message : String(error); }
  }
  function endComposition(event: CompositionEvent & { currentTarget: EventTarget & HTMLTextAreaElement }) {
    composing = false;
    update(event.currentTarget.value);
  }
  async function review() {
    if (disabled || composing || discardPending) return;
    actionError = "";
    localNotice = "";
    if (localUnsaved || inputError) { actionError = "Store the current draft text before reviewing. Reduce any unsupported input, or copy it before discarding this draft."; return; }
    try {
      const result = onreview();
      if (!result) { localNotice = "The draft has no page-text changes to apply."; return; }
      preview = result;
      acceptCombinedCoverage = false;
      await tick();
      reviewHeading?.focus();
    } catch (error) { actionError = error instanceof Error ? error.message : String(error); }
  }
  async function backToEditing() {
    preview = null;
    acceptCombinedCoverage = false;
    actionError = "";
    discardPending = false;
    await tick();
    editor?.focus();
  }
  function apply() {
    if (!preview || disabled || composing || discardPending || (preview.requiresCombinedCoverage && !acceptCombinedCoverage)) return;
    actionError = "";
    try {
      const error = onapply(preview, acceptCombinedCoverage);
      if (error) actionError = error;
      else dialog?.close();
    } catch (error) { actionError = error instanceof Error ? error.message : String(error); }
  }
  function closeAndKeep() {
    if (composing) { actionError = "Finish the current text composition before closing the draft."; return; }
    if (localUnsaved || inputError) { actionError = "This input has not been stored. Reduce the unsupported input or copy it before explicitly discarding the draft."; return; }
    dialog?.close();
  }
  function requestDiscard() {
    if (disabled || composing) return;
    if (changed || localUnsaved) { discardPending = true; actionError = ""; return; }
    discard();
  }
  function discard() {
    if (disabled || composing) return;
    actionError = "";
    try {
      const error = ondiscard();
      if (error) actionError = error;
      else dialog?.close();
    } catch (error) { actionError = error instanceof Error ? error.message : String(error); }
  }
  async function save() {
    if (retrying || composing) return;
    retrying = true;
    actionError = "";
    try {
      inputError = onupdate(editingText) ?? "";
      if (!inputError) await onsave();
    } catch (error) { actionError = error instanceof Error ? error.message : String(error); }
    finally { retrying = false; }
  }
  async function copyDraft() {
    try { await navigator.clipboard.writeText(editingText); localNotice = "Draft text copied."; }
    catch { actionError = "Clipboard access was unavailable. Return to editing, select the draft text, and copy it manually."; }
  }
  function keydown(event: KeyboardEvent) {
    // Keep application shortcuts away from the live draft. Textarea selection,
    // clipboard actions, and native undo remain ordinary browser editing.
    event.stopPropagation();
    if (!event.isComposing && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void save();
    }
  }
</script>

<dialog bind:this={dialog} class="page-draft-dialog" data-page-text-draft-dialog aria-labelledby="page-draft-title" onkeydown={keydown}
  oncancel={(event) => { event.preventDefault(); if (discardPending) discardPending = false; else closeAndKeep(); }} onclose={onclose}>
  <h2 id="page-draft-title">Edit page text draft</h2>
  <p class="source-context">{draft.document.name} · {draft.page.name}</p>
  <div class="draft-status" class:save-error={localUnsaved || saveStatus.state === "error"} role="status" data-page-draft-save-state={localUnsaved ? "local-unsaved" : saveStatus.state}>{statusText}</div>
  {#if saveStatus.state === "error"}<p class="error" role="alert">The project could not save this draft: {saveStatus.error ?? "Save failed."} Keep the workspace open and retry.</p>{/if}
  <!-- Keep the same textarea mounted during review so Back retains native undo. -->
  <div class="draft-editing" hidden={!!preview}>
    <p class="help" id="page-draft-help">Select, type, cut, copy, or paste across the page's passages in one text area. Ctrl/Cmd+Z undoes draft typing. The recognized page stays as it is until you review and apply these changes.</p>
    <p class="help">Tables and formulas are visible but protected. Pictures and excluded regions remain boundaries even when their text is not shown. Editing across passages requires a reviewed merge into one manual paragraph; line breaks alone do not create source regions.</p>
    <label class="draft-label">Page text draft<textarea bind:this={editor} data-page-text-draft aria-label="Page text draft" aria-describedby="page-draft-help" value={editingText}
      oninput={(event) => update(event.currentTarget.value)} oncompositionstart={() => (composing = true)} oncompositionend={endComposition}
      disabled={disabled || discardPending} spellcheck="true" dir="auto" rows="18"></textarea></label>
    <div class="draft-details"><span>{editingText.length.toLocaleString()} / 1,000,000 text units</span><span>{composing ? "Finish text composition before reviewing" : "Unapplied page draft"}</span></div>
    <p class="help">Several separate edits can affect the passages between the first and last change. The review shows the full affected span before anything is applied.</p>
  </div>
  {#if preview}
    <div class="review-summary" data-page-text-change-preview>
      <h3 bind:this={reviewHeading} tabindex="-1">Review page changes</h3>
      <p>{preview.regionIds.length} current passage{preview.regionIds.length === 1 ? "" : "s"} → {preview.regions.length} resulting passage{preview.regions.length === 1 ? "" : "s"}</p>
      {#if preview.requiresCombinedCoverage}<p class="warning">This change combines the affected passages into one manual paragraph. Review its text and enclosing source area.</p>{/if}
    </div>
    <div class="text-comparison">
      <label>Current page text<textarea readonly aria-label="Current page text" dir="auto" value={preview.beforeText} rows="9"></textarea></label>
      <label>Proposed page text<textarea readonly aria-label="Proposed page text" dir="auto" value={preview.afterText} rows="9"></textarea></label>
    </div>
    <details class="affected-details"><summary>Affected passages and source coverage</summary>
      <div class="text-comparison">
        <label>Affected current passages<textarea readonly aria-label="Affected current passages" dir="auto" value={preview.affectedBeforeText} rows="5"></textarea></label>
        <label>{preview.requiresCombinedCoverage ? "Combined passage preview" : "Changed passage preview"}<textarea readonly aria-label={preview.requiresCombinedCoverage ? "Combined passage preview" : "Changed passage preview"} dir="auto" value={preview.affectedAfterText} rows="5"></textarea></label>
      </div>
      <svg viewBox={coverageViewBox} role="img" aria-label="Proposed page edit source coverage" class="coverage-preview">
        <image href={imageUrl(preview.page.imagePath)} width={preview.page.width} height={preview.page.height} />
        {#each affected as region (region.id)}<rect class="previous-area" x={region.x} y={region.y} width={region.w} height={region.h} vector-effect="non-scaling-stroke"><title>Previous passage source area</title></rect>{/each}
        {#each preview.regions as region (region.id)}<rect class="proposed-area" x={region.x} y={region.y} width={region.w} height={region.h} vector-effect="non-scaling-stroke"><title>Proposed passage source area</title></rect>{/each}
      </svg>
      <p class="help">Dashed amber outlines: current passages. Solid blue outline: proposed source coverage. These areas describe the chosen edit, not new OCR word measurements.</p>
    </details>
    {#if preview.warnings.length}<ul class="warnings" aria-label="Page edit review notes">{#each preview.warnings as warning}<li>{warning}</li>{/each}</ul>{/if}
    {#if preview.requiresCombinedCoverage}<label class="coverage-choice"><input type="checkbox" aria-label="Use combined source coverage" bind:checked={acceptCombinedCoverage} disabled={disabled || composing || discardPending} /><span>Use combined source coverage. I have reviewed the merged manual paragraph, its source area, and the formatting or link changes above.</span></label>{/if}
    <p class="help">Apply is one undoable project change and reopens the affected review. Original OCR evidence stays separate from manually edited text.</p>
  {/if}
  {#if inputError || actionError}<p class="error" role="alert">{inputError || actionError}</p>{/if}
  {#if localNotice}<p class="local-notice" role="status">{localNotice}</p>{/if}
  {#if discardPending}<section class="discard-confirmation" role="alert" aria-label="Discard page text draft confirmation">
    <h3>Discard this unapplied draft?</h3><p>This removes the saved draft and its edits. It does not apply them to the recognized page. Copy any text you want to keep first.</p>
    <div class="dialog-actions"><button type="button" onclick={() => (discardPending = false)}>Keep draft</button><button type="button" class="danger" disabled={disabled || composing} onclick={discard}>Confirm discard draft</button></div>
  </section>{/if}
  <div class="dialog-actions secondary-actions">
    <button type="button" onclick={copyDraft} disabled={composing}>Copy draft text</button>
    <button type="button" onclick={save} disabled={retrying || composing}>{retrying ? "Saving…" : saveStatus.state === "error" || localUnsaved ? "Retry draft save" : "Save draft"}</button>
    <button type="button" onclick={requestDiscard} disabled={disabled || composing || discardPending}>Discard draft</button>
  </div>
  <div class="dialog-actions primary-actions">
    <button type="button" onclick={closeAndKeep} disabled={composing}>Close and keep draft</button>
    {#if preview}<button type="button" onclick={backToEditing} disabled={disabled || composing || discardPending}>Back to editing</button><button type="button" class="primary" onclick={apply} disabled={disabled || composing || discardPending || (preview.requiresCombinedCoverage && !acceptCombinedCoverage)}>Apply page changes</button>
    {:else}<button type="button" class="primary" onclick={review} disabled={disabled || composing || discardPending || localUnsaved || !!inputError}>Review changes</button>{/if}
  </div>
</dialog>

<style>
  .page-draft-dialog { box-sizing: border-box; width: min(1020px, calc(100vw - 28px)); max-height: calc(100vh - 32px); overflow: auto; padding: 24px; border: 1px solid #71717a; border-radius: 10px; background: #27272a; color: #dbeafe; box-shadow: 0 18px 80px #0009; font: inherit; font-size: 13px; }
  .page-draft-dialog::backdrop { background: #07101dd4; }
  h2 { margin: 0 0 6px; font-size: 21px; } h3 { margin: 0 0 7px; font-size: 15px; }
  .source-context, .help { color: #93c5fd; line-height: 1.65; overflow-wrap: anywhere; }
  .source-context { margin: 4px 0 12px; font-size: 12px; }
  .help { margin: 10px 0; font-size: 12px; }
  .draft-status { display: inline-block; padding: 5px 9px; border: 1px solid #71717a; border-radius: 4px; background: #052e16; color: #86efac; font-size: 11px; }
  .draft-status.save-error { color: #fecaca; background: #27272a; border-color: #71717a; }
  label { display: flex; flex-direction: column; gap: 6px; min-width: 0; color: #bfdbfe; margin: 10px 0; }
  textarea { box-sizing: border-box; display: block; width: 100%; min-width: 0; min-height: 125px; padding: 11px; border: 1px solid #52525b; border-radius: 5px; background: #172554; color: #dbeafe; font: inherit; font-size: 14px; line-height: 1.75; resize: vertical; white-space: pre-wrap; }
  .draft-label textarea { min-height: min(48vh, 420px); }
  .draft-editing[hidden] { display: none; }
  .draft-details { display: flex; flex-wrap: wrap; gap: 6px 20px; justify-content: space-between; color: #93c5fd; font-size: 11px; }
  .text-comparison { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; }
  .review-summary { margin: 18px 0 6px; }
  .review-summary > p { color: #d4d4d8; line-height: 1.6; }
  .review-summary h3:focus { outline: none; }
  .affected-details { margin: 14px 0; padding: 12px; border: 1px solid #52525b; border-radius: 5px; }
  summary { cursor: pointer; color: #bfdbfe; line-height: 1.7; }
  .coverage-preview { display: block; width: 100%; height: 180px; margin-top: 10px; background: #fff; border-radius: 4px; }
  rect { stroke-width: 3px; fill: transparent; }
  .previous-area { stroke: #d97706; stroke-dasharray: 8 5; }
  .proposed-area { stroke: #2563eb; fill: #277be514; }
  .warnings { padding-left: 22px; color: #fcd34d; font-size: 12px; line-height: 1.7; }
  .coverage-choice { flex-direction: row; align-items: flex-start; gap: 10px; padding: 12px; border: 1px solid #71717a; border-radius: 5px; color: #fde68a; background: #27272a; line-height: 1.65; }
  .coverage-choice input { flex: none; margin-top: 4px; }
  .warning, .error, .discard-confirmation { padding: 12px; border: 1px solid #d97706; border-radius: 5px; background: #27272a; color: #fde68a; font-size: 12px; line-height: 1.7; }
  .error { border-color: #71717a; background: #27272a; color: #fecaca; }
  .local-notice { color: #bfdbfe; line-height: 1.6; }
  .discard-confirmation { margin-top: 14px; }
  .discard-confirmation p { margin: 0; }
  .dialog-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 9px; margin-top: 14px; }
  .primary-actions { justify-content: flex-end; padding-top: 14px; border-top: 1px solid #52525b; }
  .secondary-actions { justify-content: flex-start; }
  button { padding: 8px 12px; border: 1px solid #71717a; border-radius: 5px; background: #3f3f46; color: #dbeafe; font: inherit; cursor: pointer; }
  button.primary { background: #2563eb; border-color: #60a5fa; }
  button.danger { background: #3f3f46; border-color: #a1a1aa; }
  button:disabled, textarea:disabled { opacity: .5; cursor: default; }
  button:focus-visible, textarea:focus-visible, input:focus-visible, summary:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
  @media (max-width: 650px) { .page-draft-dialog { padding: 17px; } .text-comparison { grid-template-columns: minmax(0, 1fr); gap: 0; } .dialog-actions button { flex: 1 1 auto; } }
</style>
