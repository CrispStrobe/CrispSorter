<script lang="ts">
  import type { OcrPage, OcrRegion } from "$lib/ocr/model";
  import { getWordEvidence, getWordConfidenceBand, type WordConfidenceBand } from "$lib/ocr/words";

  /**
   * Reading view for proofreading: the page as continuous text, each word
   * coloured by its recognition confidence. Clicking a word selects its
   * region; the selected region's text can be corrected below.
   */
  let {
    page,
    threshold,
    selectedRegionId = null,
    disabled = false,
    onselect,
    onchange,
  }: {
    page: OcrPage;
    threshold: number;
    selectedRegionId?: string | null;
    disabled?: boolean;
    onselect: (regionId: string) => void;
    onchange: (regionId: string, text: string) => void;
  } = $props();

  type Piece = { text: string; band: WordConfidenceBand | null; score: number | null };
  type Block = { region: OcrRegion; pieces: Piece[]; separator: string };

  /** Same visual line when the vertical extents overlap by half the smaller height. */
  function sameLine(a: OcrRegion, b: OcrRegion): boolean {
    const overlap = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
    return overlap > 0.5 * Math.min(a.h, b.h);
  }

  const blocks = $derived.by((): Block[] => {
    const regions = page.regions.filter((r) => !r.excluded && r.kind !== "picture" && r.text.length > 0);
    return regions.map((region, index) => {
      const words = getWordEvidence(region);
      const pieces: Piece[] = [];
      let cursor = 0;
      for (const word of words) {
        if (word.start > cursor) pieces.push({ text: region.text.slice(cursor, word.start), band: null, score: null });
        const score = word.basis === "characters" ? word.minConfidence : word.confidence;
        pieces.push({ text: region.text.slice(word.start, word.end), band: getWordConfidenceBand(word, threshold), score });
        cursor = word.end;
      }
      if (cursor < region.text.length) pieces.push({ text: region.text.slice(cursor), band: null, score: null });
      const previous = index > 0 ? regions[index - 1] : null;
      const separator = !previous ? "" : region.separatorBefore !== undefined ? region.separatorBefore
        : sameLine(previous, region) ? " " : "\n";
      return { region, pieces, separator };
    });
  });

  const selected = $derived(page.regions.find((r) => r.id === selectedRegionId) ?? null);
  const counts = $derived.by(() => {
    const c = { low: 0, medium: 0, unscored: 0 };
    for (const block of blocks) for (const piece of block.pieces) {
      if (piece.band === "low") c.low++; else if (piece.band === "medium") c.medium++; else if (piece.band === "unscored") c.unscored++;
    }
    return c;
  });
  const title = (piece: Piece) => piece.band === "edited" ? "Edited" : piece.score === null ? "No confidence score"
    : `Confidence ${Math.round(piece.score * 100)}%`;
</script>

<div class="confidence-view">
  <p class="confidence-legend">
    <span class="word low">low</span> {counts.low} ·
    <span class="word medium">uncertain</span> {counts.medium} ·
    <span class="word unscored">no score</span> {counts.unscored} — click a word to correct its passage.
  </p>
  <div class="confidence-text" role="list" aria-label="Recognized text coloured by confidence">
    {#each blocks as block (block.region.id)}{block.separator}<span
        role="listitem"
        class="passage"
        class:selected={block.region.id === selectedRegionId}
        ><!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->{#each block.pieces as piece, i (i)}{#if piece.band}<span
              class="word {piece.band}"
              title={title(piece)}
              onclick={() => onselect(block.region.id)}>{piece.text}</span>{:else}{piece.text}{/if}{/each}</span
      >{/each}
  </div>
  {#if selected}
    <label class="confidence-editor">
      <span>Correct this passage</span>
      <textarea
        aria-label="Correct the selected passage"
        rows={Math.min(6, Math.max(2, selected.text.split("\n").length))}
        value={selected.text}
        {disabled}
        onchange={(e) => onchange(selected.id, (e.currentTarget as HTMLTextAreaElement).value)}
      ></textarea>
    </label>
  {/if}
</div>

<style>
  .confidence-view { display: flex; flex-direction: column; gap: 10px; min-height: 0; }
  .confidence-legend { margin: 0; font-size: 11px; color: #a1a1aa; }
  .confidence-text {
    white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.75; font-size: 14px;
    color: #e4e4e7; background: #18181b; border: 1px solid #3f3f46; border-radius: 6px;
    padding: 12px 14px; max-height: 60vh; overflow-y: auto; user-select: text;
  }
  .passage.selected { outline: 1px solid #60a5fa; outline-offset: 2px; border-radius: 3px; background: #1d3a5c55; }
  .word { cursor: pointer; border-radius: 2px; }
  .word:hover { text-decoration: underline; }
  .word.high { color: #e4e4e7; }
  .word.medium { color: #fcd34d; background: #78350f33; }
  .word.low { color: #fca5a5; background: #7f1d1d55; text-decoration: underline wavy #f87171; }
  .word.unscored { color: #93c5fd; }
  .word.edited { color: #86efac; }
  .confidence-legend .word { cursor: default; padding: 0 3px; }
  .confidence-editor { display: flex; flex-direction: column; gap: 4px; font-size: 11px; color: #a1a1aa; }
  .confidence-editor textarea {
    font: inherit; font-size: 14px; color: #e4e4e7; background: #18181b; border: 1px solid #52525b;
    border-radius: 6px; padding: 8px 10px; resize: vertical;
  }
</style>
