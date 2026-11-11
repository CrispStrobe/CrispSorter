<script lang="ts">
  import { ScanLine, Scissors, Combine, WandSparkles } from "lucide-svelte";
  import type { OcrPage, OcrRegion } from "$lib/ocr/model";
  import { passageEligibility } from "$lib/ocr/passages";
  import PassageSources from "./PassageSources.svelte";

  let {
    page,
    selectedId,
    disabled = false,
    ocrAvailable = false,
    onfocus,
    onedit,
    onblur,
    onrecognize,
    onsplit,
    onmerge,
    onrepair,
    imageUrl,
  }: {
    page: OcrPage;
    selectedId: string | null;
    disabled?: boolean;
    ocrAvailable?: boolean;
    onfocus: (id: string) => void;
    onedit: (region: OcrRegion, text: string) => void;
    onblur: () => void;
    onrecognize: (region: OcrRegion) => void | Promise<void>;
    onsplit: (region: OcrRegion, caret: { start: number; end: number; text: string } | null) => void;
    onmerge: (region: OcrRegion) => void;
    onrepair: (region: OcrRegion) => void;
    imageUrl: (path: string) => string;
  } = $props();
  let editorRoot = $state<HTMLElement>();

  const passages = $derived(
    page.regions
      .map((region, index) => ({ region, number: index + 1 }))
      .filter(({ region }) => !region.excluded && region.kind !== "picture"),
  );
  const eligibility = $derived(selectedId ? passageEligibility(page, selectedId) : null);
  function splitAtCaret(region: OcrRegion) {
    const article = Array.from(editorRoot?.querySelectorAll<HTMLElement>("[data-passage-id]") ?? [])
      .find((item) => item.dataset.passageId === region.id);
    const editor = article?.querySelector<HTMLTextAreaElement>("textarea[data-passage-text]");
    onsplit(region, editor ? { start: editor.selectionStart, end: editor.selectionEnd, text: editor.value } : null);
  }
</script>

<section bind:this={editorRoot} class="page-text-editor" aria-label="Page text editor">
  <p class="editor-help">
    <span class="editor-info" tabindex="0" role="note" aria-label="About passage editing"
      title={"Edit complete sentences and paragraphs, paste replacement text, or add line breaks. Each passage stays linked to its source region, in export reading order. Changes save automatically and can be undone."
        + (page.structure ? " Inserted and replaced text uses plain formatting. Unchanged text retains its formatting where its range is certain. Use Structure to review styles, headings, lists, columns, and links." : "")}
      >i</span>
  </p>
  {#each passages as { region, number } (region.id)}
    <article
      class="passage"
      class:selected={selectedId === region.id}
      data-passage-id={region.id}
    >
      <div class="passage-heading">
        <label for={`passage-${page.id}-${region.id}`}>Passage {number}</label>
        <span class="passage-kind">{region.kind}</span>
        {#if region.text !== region.originalText}<span class="edited">Edited</span>{/if}
        {#if region.sharedSourceGroups?.length}<span class="shared-source" data-shared-source-groups={region.sharedSourceGroups.join(" ")} title="Passages in this source family may share image coverage. Spatial exports require their source areas to stop overlapping, or the passages to be merged again.">Shared source area</span>{/if}
        <button
          type="button"
          onclick={() => onrecognize(region)}
          disabled={disabled || !ocrAvailable}
          title="Recognize this source region again and compare the result before replacing text"
        ><ScanLine size={13} /> Re-OCR region</button>
      </div>
      <textarea
        data-passage-text
        id={`passage-${page.id}-${region.id}`}
        aria-label={`Passage ${number} text`}
        value={region.text}
        oninput={(event) => onedit(region, event.currentTarget.value)}
        onfocus={() => onfocus(region.id)}
        {onblur}
        {disabled}
        rows={Math.max(4, Math.min(20, region.text.split("\n").length + Math.ceil(region.text.length / 52)))}
        spellcheck="true"
        dir="auto"
      ></textarea>
      {#if selectedId === region.id}
        <div class="passage-tools" aria-label="Passage editing tools">
          <button type="button" onclick={() => splitAtCaret(region)} disabled={disabled || !eligibility?.splitAllowed} title="Place the caret where the passage should split, then choose the source coverage"><Scissors size={13} /> Split passage</button>
          <button type="button" onclick={() => onmerge(region)} disabled={disabled || !eligibility?.nextMergeId} title={eligibility?.nextMergeId ? "Preview a merge with the immediately following passage" : "Merge needs the immediately following included passage of the same kind"}><Combine size={13} /> Merge next passage</button>
          <button type="button" onclick={() => onrepair(region)} disabled={disabled || page.excluded || region.excluded || !region.text.trim() || !["text", "title", "header", "footer"].includes(region.kind)} title="Review proposed whitespace, line-break, or hyphenation changes"><WandSparkles size={13} /> Repair passage text</button>
        </div>
        {#if region.sourceComponents?.length}<PassageSources components={region.sourceComponents} {imageUrl} />{/if}
      {/if}
    </article>
  {:else}
    <p class="editor-help">There are no included text passages. Draw a text region on the page to add one.</p>
  {/each}
</section>

<style>
  .page-text-editor {
    display: flex;
    flex-direction: column;
    gap: 13px;
    min-width: 0;
  }
  .editor-help {
    margin: 0;
    color: #a1a1aa;
    font-size: 12px;
    line-height: 1.65;
  }
  .passage {
    border: 1px solid #3f3f46;
    border-radius: 6px;
    background: #27272a;
    min-width: 0;
    overflow: hidden;
  }
  .passage.selected {
    border-color: #60a5fa;
  }
  .passage-heading {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 8px 10px;
    border-bottom: 1px solid #3f3f46;
  }
  .passage-heading label {
    color: #bfdbfe;
    font-size: 12px;
    font-weight: 600;
  }
  .passage-kind,
  .edited {
    color: #a1a1aa;
    font-size: 10px;
  }
  .edited {
    color: #bfdbfe;
  }
  .shared-source { color: #fcd34d; font-size: 10px; }
  .passage-heading button {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-left: auto;
    padding: 4px 0;
    border: 0;
    background: transparent;
    color: #bfdbfe;
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .passage-heading button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .passage-tools { display: flex; flex-wrap: wrap; gap: 7px 14px; padding: 9px 10px; border-top: 1px solid #3f3f46; }
  .passage-tools button { display: inline-flex; align-items: center; gap: 5px; padding: 4px 0; border: 0; background: transparent; color: #bfdbfe; font: inherit; font-size: 11px; cursor: pointer; }
  .passage-tools button:disabled { opacity: .5; cursor: default; }
  .passage-tools button:focus-visible { outline: 2px solid #bfdbfe; outline-offset: 2px; }
  .passage textarea {
    display: block;
    box-sizing: border-box;
    width: 100%;
    min-width: 0;
    min-height: 130px;
    padding: 10px;
    border: 0;
    border-radius: 0;
    background: #18181b;
    color: #dbeafe;
    font: inherit;
    font-size: 14px;
    line-height: 1.7;
    resize: vertical;
  }
  .passage textarea:focus-visible {
    outline: 2px solid #93c5fd;
    outline-offset: -2px;
  }
  .passage-heading button:focus-visible {
    outline: 2px solid #bfdbfe;
    outline-offset: 2px;
  }
  .editor-help { display: flex; justify-content: flex-end; margin: 0; }
  .editor-info { width: 16px; height: 16px; border-radius: 50%; border: 1px solid #52525b; color: #a1a1aa; cursor: help;
    font: italic 600 10px/14px Georgia, serif; text-align: center; }
</style>
