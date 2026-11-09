<script lang="ts">
  import { ChevronRight } from "lucide-svelte";
  import type { OcrRegion } from "$lib/ocr/model";
  import {
    getWordEvidence,
    wordNeedsReview,
    type WordEvidence,
  } from "$lib/ocr/words";

  let {
    region,
    threshold,
    disabled = false,
    onselect,
  }: {
    region: OcrRegion;
    threshold: number;
    disabled?: boolean;
    onselect: (word: WordEvidence) => void | Promise<void>;
  } = $props();

  let original = $state(false);
  let selectedStart = $state<number | null>(null);
  const edited = $derived(region.text !== region.originalText);
  const source = $derived(original && edited ? "original" : "current");
  const words = $derived(getWordEvidence(region, source));
  const flagged = $derived(words.filter((word) => wordNeedsReview(word, threshold)));
  const scored = $derived(words.some((word) => word.confidence !== null));
  const canSelect = $derived(!disabled && source === "current");

  // Offsets belong to one exact text version. Do not carry an active word over
  // a correction, an undo, or a switch between current and original evidence.
  $effect(() => {
    void region.id;
    void region.text;
    void source;
    selectedStart = null;
  });

  function percent(score: number): string {
    return `${Math.round(score * 100)}%`;
  }
  function scoreLabel(word: WordEvidence): string {
    return word.confidence !== null
      ? percent(word.confidence)
      : word.basis === "edited"
        ? "Edited"
        : "Unscored";
  }
  function description(word: WordEvidence): string {
    const evidence =
      word.confidence !== null && word.minConfidence !== null
        ? `Mean original character score: ${percent(word.confidence)}. Lowest character score: ${percent(word.minConfidence)}. A review signal, not a probability of accuracy.`
        : word.basis === "edited"
          ? "The text was edited. Original OCR scores do not measure the corrected text."
          : "Word confidence is unavailable: this result has no aligned character recognition scores.";
    return source === "current"
      ? `${evidence} Select this word in the text editor to correct it.`
      : `${evidence} Original OCR evidence is read-only.`;
  }
  function selectWord(word: WordEvidence) {
    if (!canSelect) return;
    selectedStart = word.start;
    void onselect(word);
  }
  function nextWord() {
    if (!canSelect || !flagged.length) return;
    const next = flagged.find((word) => selectedStart === null || word.start > selectedStart)
      ?? flagged[0];
    selectWord(next);
  }
</script>

<section class="word-review" aria-label="Word confidence for selected region">
  <div class="word-heading">
    <h4>Word confidence</h4>
    <span class="word-count">{flagged.length} to check</span>
    <button
      type="button"
      class="next-word"
      onclick={nextWord}
      disabled={!canSelect || !flagged.length}
      title="Select the next uncertain, unscored, or edited word in this region"
    >Next word to check <ChevronRight size={12} /></button>
  </div>
  {#if edited}
    <div class="evidence-source" aria-label="Word evidence source">
      <button type="button" aria-pressed={source === "current"} onclick={() => (original = false)}>Current text</button>
      <button type="button" aria-pressed={source === "original"} onclick={() => (original = true)}>Original OCR</button>
    </div>
  {/if}
  <p class="word-help">
    {#if edited && source === "current"}
      This text was edited. Original OCR scores do not apply to corrections.
      Select Original OCR to inspect the saved evidence.
    {:else if scored}
      Scores average the original character scores. A word is highlighted when
      any character is below {percent(threshold)}. Scores guide review; they are
      not probabilities of accuracy.
    {:else}
      This OCR result has no aligned character scores, so word confidence is
      unavailable. Inspect the source before accepting the text.
    {/if}
  </p>
  {#if words.length}
    <div class="word-list" dir="auto">
      {#each words as word, index (word.start)}
        {#if source === "current"}
          <button
            type="button"
            class="word"
            class:uncertain={wordNeedsReview(word, threshold)}
            class:selected={selectedStart === word.start}
            class:unscored={word.confidence === null}
            data-word-start={word.start}
            data-word-end={word.end}
            data-word-basis={word.basis}
            data-word-confidence={word.confidence ?? undefined}
            data-word-flagged={wordNeedsReview(word, threshold)}
            aria-label={`Select word ${index + 1}: ${word.text}`}
            title={description(word)}
            disabled={!canSelect}
            onclick={() => selectWord(word)}
          ><span class="word-text">{word.text}</span><span class="word-score">{scoreLabel(word)}</span></button>
        {:else}
          <span
            class="word original-word"
            class:uncertain={wordNeedsReview(word, threshold)}
            class:unscored={word.confidence === null}
            data-word-start={word.start}
            data-word-end={word.end}
            data-word-basis={word.basis}
            data-word-confidence={word.confidence ?? undefined}
            data-word-flagged={wordNeedsReview(word, threshold)}
            title={description(word)}
          ><span class="word-text">{word.text}</span><span class="word-score">{scoreLabel(word)}</span></span>
        {/if}
      {/each}
    </div>
    <p class="word-instruction">
      {source === "current"
        ? "Select a word, then type your correction in the text editor."
        : "Original OCR is read-only. Switch to Current text to make corrections."}
    </p>
  {:else}
    <p class="word-instruction">No words to review. Enter text in the region editor.</p>
  {/if}
</section>

<style>
  .word-review {
    padding: 10px;
    border-top: 1px solid #34465c;
    background: #0d192888;
    min-width: 0;
  }
  .word-heading {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 8px;
  }
  h4 {
    margin: 0;
    color: #d1dfef;
    font-size: 12px;
    font-weight: 600;
  }
  .word-count {
    color: #a1b4ca;
    font-size: 11px;
  }
  .next-word {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    margin-left: auto;
    padding: 4px 0;
    border: 0;
    background: transparent;
    color: #b9d7f6;
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .next-word:disabled {
    color: #7f91a7;
    cursor: default;
  }
  .word-help,
  .word-instruction {
    margin: 7px 0;
    color: #a6b9ce;
    font-size: 11px;
    line-height: 1.55;
  }
  .word-instruction {
    margin-bottom: 0;
  }
  .evidence-source {
    display: flex;
    gap: 4px;
    margin-top: 8px;
  }
  .evidence-source button {
    padding: 4px 7px;
    border: 1px solid #344b65;
    border-radius: 4px;
    background: transparent;
    color: #a6bbd2;
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .evidence-source button[aria-pressed="true"] {
    color: #d1e8ff;
    border-color: #6084ad;
    background: #1e3a55;
  }
  .word-list {
    display: flex;
    flex-wrap: wrap;
    align-items: stretch;
    gap: 5px;
  }
  .word {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    max-width: 100%;
    padding: 5px 7px;
    border: 1px solid #39576b;
    border-radius: 5px;
    background: #163142;
    color: #d0e8ee;
    font: inherit;
    font-size: 12px;
    line-height: 1.5;
    text-align: start;
    cursor: pointer;
  }
  .word-text {
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .word-score {
    color: #a3cad6;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .word.uncertain {
    border-color: #8a682d;
    background: #49361d;
    color: #ffe0a9;
  }
  .word.uncertain .word-score {
    color: #e7c487;
  }
  .word.unscored {
    background: #233247;
    border-color: #60748e;
    color: #d1deef;
  }
  .word.unscored .word-score {
    color: #b1c4db;
  }
  .word.selected {
    outline: 2px solid #93c4ff;
    outline-offset: 1px;
  }
  button:focus-visible {
    outline: 2px solid #b8d9ff;
    outline-offset: 2px;
  }
  .word:disabled,
  .original-word {
    cursor: default;
  }
</style>
