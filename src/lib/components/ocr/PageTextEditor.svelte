<script lang="ts">
  import { ScanLine } from "lucide-svelte";
  import type { OcrPage, OcrRegion } from "$lib/ocr/model";

  let {
    page,
    selectedId,
    disabled = false,
    ocrAvailable = false,
    onfocus,
    onedit,
    onblur,
    onrecognize,
  }: {
    page: OcrPage;
    selectedId: string | null;
    disabled?: boolean;
    ocrAvailable?: boolean;
    onfocus: (id: string) => void;
    onedit: (region: OcrRegion, text: string) => void;
    onblur: () => void;
    onrecognize: (region: OcrRegion) => void | Promise<void>;
  } = $props();

  const passages = $derived(
    page.regions
      .map((region, index) => ({ region, number: index + 1 }))
      .filter(({ region }) => !region.excluded && region.kind !== "picture"),
  );
</script>

<section class="page-text-editor" aria-label="Page text editor">
  <p class="editor-help">
    Edit complete sentences and paragraphs, paste replacement text, or add line
    breaks. Each passage stays linked to its source region, in export reading
    order. Changes save automatically and can be undone.
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
        <button
          type="button"
          onclick={() => onrecognize(region)}
          disabled={disabled || !ocrAvailable}
          title="Recognize this source region again and compare the result before replacing text"
        ><ScanLine size={13} /> Re-OCR region</button>
      </div>
      <textarea
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
    color: #a4b9d1;
    font-size: 12px;
    line-height: 1.65;
  }
  .passage {
    border: 1px solid #344b65;
    border-radius: 6px;
    background: #152336;
    min-width: 0;
    overflow: hidden;
  }
  .passage.selected {
    border-color: #7ca7db;
  }
  .passage-heading {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 8px 10px;
    border-bottom: 1px solid #344b65;
  }
  .passage-heading label {
    color: #d2e1f4;
    font-size: 12px;
    font-weight: 600;
  }
  .passage-kind,
  .edited {
    color: #9eb5d0;
    font-size: 10px;
  }
  .edited {
    color: #a4d2ff;
  }
  .passage-heading button {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-left: auto;
    padding: 4px 0;
    border: 0;
    background: transparent;
    color: #b5d6fc;
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .passage-heading button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .passage textarea {
    display: block;
    box-sizing: border-box;
    width: 100%;
    min-width: 0;
    min-height: 130px;
    padding: 10px;
    border: 0;
    border-radius: 0;
    background: #101d2e;
    color: #e1ebf8;
    font: inherit;
    font-size: 14px;
    line-height: 1.7;
    resize: vertical;
  }
  .passage textarea:focus-visible {
    outline: 2px solid #93c4ff;
    outline-offset: -2px;
  }
  .passage-heading button:focus-visible {
    outline: 2px solid #b8d9ff;
    outline-offset: 2px;
  }
</style>
