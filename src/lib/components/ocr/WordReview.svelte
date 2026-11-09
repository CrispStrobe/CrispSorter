<script lang="ts">
  import { untrack } from "svelte";
  import { ChevronRight } from "lucide-svelte";
  import type { OcrRegion } from "$lib/ocr/model";
  import {
    getWordEvidence,
    getWordConfidenceBand,
    LOW_WORD_CONFIDENCE,
    wordNeedsReview,
    type WordEvidence,
  } from "$lib/ocr/words";

  let {
    region,
    threshold,
    disabled = false,
    onselect,
    oninspect,
  }: {
    region: OcrRegion;
    threshold: number;
    disabled?: boolean;
    onselect: (word: WordEvidence) => void | Promise<void>;
    oninspect?: (word: WordEvidence | null, source: "current" | "original") => void;
  } = $props();

  let original = $state(false);
  let showScores = $state(false);
  let selectedStart = $state<number | null>(null);
  const manual = $derived(region.confidenceBasis === "manual");
  const edited = $derived(region.text !== region.originalText);
  const source = $derived(original && edited ? "original" : "current");
  const evidenceText = $derived(source === "original" ? region.originalText : region.text);
  const words = $derived(getWordEvidence(region, source));
  const selectedWord = $derived(words.find((word) => word.start === selectedStart));
  const flagged = $derived(words.filter((word) => wordNeedsReview(word, threshold)));
  const scored = $derived(words.some((word) => word.confidence !== null));
  const nativeWords = $derived(words.some((word) => word.basis === "word"));
  const characterScores = $derived(words.some((word) => word.basis === "characters"));
  const canSelect = $derived(!disabled && source === "current");
  const lowThreshold = $derived(Math.min(LOW_WORD_CONFIDENCE, threshold));

  // Offsets belong to one exact text version. Do not carry an active word over
  // a correction, an undo, or a switch between current and original evidence.
  $effect(() => {
    void region.id;
    void region.text;
    void region.originalText;
    const evidenceSource = source;
    selectedStart = null;
    untrack(() => oninspect?.(null, evidenceSource));
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
    const band = getWordConfidenceBand(word, threshold);
    const rating = band === "high" ? "High confidence" : band === "low" ? "Low confidence" : "Needs review";
    const evidence =
      word.basis === "word"
        ? word.confidence !== null
          ? `${rating}. Original word recognition score: ${percent(word.confidence)}. Color follows this word score. A review signal, not a probability of accuracy.`
          : "Word confidence is unavailable: the engine did not report a recognition score for this word."
        : word.confidence !== null && word.minConfidence !== null
        ? `${rating}. Mean original character score: ${percent(word.confidence)}. Lowest character score: ${percent(word.minConfidence)}. Color follows the lowest score. A review signal, not a probability of accuracy.`
        : word.basis === "edited"
          ? "The text was edited. Original OCR scores do not measure the corrected text."
          : "Word confidence is unavailable: this result has no verified word score or aligned character recognition scores.";
    const sourceDetail = word.bounds
      ? " Source detail shows the original word image."
      : " Source detail shows the containing region; no verified word location is available.";
    return source === "current"
      ? `${evidence}${sourceDetail} Select this word in the text editor to correct it.`
      : `${evidence}${sourceDetail} ${manual ? "The text before later edits" : "Original OCR evidence"} is read-only.`;
  }
  function selectWord(word: WordEvidence) {
    if (!canSelect) return;
    selectedStart = word.start;
    void onselect(word);
  }
  function inspectOriginal(word: WordEvidence) {
    selectedStart = word.start;
    oninspect?.(word, "original");
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
  <div class="color-legend" role="list" aria-label="Confidence color legend">
    <span class="legend-item high" role="listitem"><span class="swatch" aria-hidden="true"></span>High ≥{percent(threshold)}</span>
    {#if lowThreshold < threshold}<span class="legend-item medium" role="listitem"><span class="swatch" aria-hidden="true"></span>Review {percent(lowThreshold)}–&lt;{percent(threshold)}</span>{/if}
    {#if lowThreshold > 0}<span class="legend-item low" role="listitem"><span class="swatch" aria-hidden="true"></span>Low &lt;{percent(lowThreshold)}</span>{/if}
    <span class="legend-item unscored" role="listitem"><span class="swatch" aria-hidden="true"></span>Unscored</span>
    <span class="legend-item edited" role="listitem"><span class="swatch" aria-hidden="true"></span>Edited</span>
  </div>
  <label class="show-scores"><input type="checkbox" bind:checked={showScores} />Show scores</label>
  {#if edited}
    <div class="evidence-source" aria-label="Word evidence source">
      <button type="button" aria-pressed={source === "current"} onclick={() => (original = false)}>Current text</button>
      <button type="button" aria-pressed={source === "original"} onclick={() => (original = true)}>{manual ? "Before later edits" : "Original OCR"}</button>
    </div>
  {/if}
  <p class="word-help">
    {#if manual}
      This passage was created manually. Its words have no recognition score.
      {#if region.sourceComponents?.length}Source passages retain the original OCR
        text and evidence separately.{/if}
      {#if edited}Before later edits shows the manual passage's starting text.{/if}
    {:else if edited && source === "current"}
      This text was edited. Original OCR scores do not apply to corrections.
      Select Original OCR to inspect the saved evidence.
    {:else if nativeWords}
      Native word colors and percentages use the original word recognition score.
      {#if characterScores}Other scored words use character evidence: color follows
        the lowest score and the percentage is the average.{/if}
      Unscored words have no verified recognition score. Scores guide review;
      they are not probabilities of accuracy.
    {:else if scored}
      Word colors follow the lowest character score; optional percentages show
      the word average. Hover or select a word for both scores. Scores guide
      review; they are not probabilities of accuracy.
    {:else}
      This OCR result has no verified word scores or aligned character scores,
      so word confidence is unavailable. Inspect the source before accepting the text.
    {/if}
  </p>
  {#if words.length}
    <div class="word-list" dir="auto" aria-label="OCR words in reading order">{#each words as word, index (word.start)}<span class="word-separator">{evidenceText.slice(index === 0 ? 0 : words[index - 1].end, word.start)}</span>{#if source === "current"}<button
            type="button"
            class="word"
            class:uncertain={wordNeedsReview(word, threshold)}
            class:selected={selectedStart === word.start}
            data-word-confidence-band={getWordConfidenceBand(word, threshold)}
            data-word-start={word.start}
            data-word-end={word.end}
            data-word-source-start={word.sourceStart}
            data-word-source-end={word.sourceEnd}
            data-word-basis={word.basis}
            data-word-confidence={word.confidence ?? undefined}
            data-word-flagged={wordNeedsReview(word, threshold)}
            aria-label={`Select word ${index + 1}: ${word.text}`}
            aria-describedby={`ocr-word-${region.id}-${source}-${word.start}`}
            title={description(word)}
            disabled={!canSelect}
            onclick={() => selectWord(word)}
          ><span class="word-text">{word.text}</span>{#if showScores}<span class="word-score">{scoreLabel(word)}</span>{/if}</button>{:else}<button
            type="button"
            class="word original-word"
            class:uncertain={wordNeedsReview(word, threshold)}
            class:selected={selectedStart === word.start}
            data-word-confidence-band={getWordConfidenceBand(word, threshold)}
            data-word-start={word.start}
            data-word-end={word.end}
            data-word-source-start={word.sourceStart}
            data-word-source-end={word.sourceEnd}
            data-word-basis={word.basis}
            data-word-confidence={word.confidence ?? undefined}
            data-word-flagged={wordNeedsReview(word, threshold)}
            aria-label={`Inspect ${manual ? "baseline" : "original"} word ${index + 1}: ${word.text}`}
            aria-describedby={`ocr-word-${region.id}-${source}-${word.start}`}
            title={description(word)}
            onclick={() => inspectOriginal(word)}
          ><span class="word-text">{word.text}</span>{#if showScores}<span class="word-score">{scoreLabel(word)}</span>{/if}</button>{/if}{/each}<span class="word-separator">{evidenceText.slice(words[words.length - 1].end)}</span></div>
    <div hidden>{#each words as word (word.start)}<span id={`ocr-word-${region.id}-${source}-${word.start}`}>{description(word)}</span>{/each}</div>
    {#if selectedWord}<p class="word-detail" role="status">{description(selectedWord)}</p>{/if}
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
  .word-instruction,
  .word-detail {
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
    padding: 9px;
    border: 1px solid #34465c;
    border-radius: 5px;
    background: #101b2b;
    color: #d1deef;
    font-size: 14px;
    line-height: 2.2;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    tab-size: 4;
  }
  .word {
    display: inline-block;
    vertical-align: baseline;
    min-width: 0;
    max-width: 100%;
    padding: 1px 3px;
    border: 0;
    border-radius: 3px;
    background: var(--word-bg);
    color: var(--word-fg);
    font: inherit;
    line-height: 1.7;
    text-align: start;
    white-space: normal;
    overflow-wrap: anywhere;
    cursor: pointer;
  }
  .word-text {
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .word-score {
    margin-inline-start: 0.5em;
    color: inherit;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .word[data-word-confidence-band="high"], .legend-item.high {
    --word-fg: #a7f3c7;
    --word-bg: #153b2c;
  }
  .word[data-word-confidence-band="medium"], .legend-item.medium {
    --word-fg: #fde68a;
    --word-bg: #493719;
  }
  .word[data-word-confidence-band="low"], .legend-item.low {
    --word-fg: #fecaca;
    --word-bg: #501f2a;
  }
  .word[data-word-confidence-band="unscored"], .legend-item.unscored {
    --word-fg: #cbd5e1;
    --word-bg: #273448;
  }
  .word[data-word-confidence-band="edited"], .legend-item.edited {
    --word-fg: #bfdbfe;
    --word-bg: #163964;
  }
  .word[data-word-confidence-band="medium"] .word-text {
    text-decoration: underline dotted;
    text-underline-offset: 3px;
  }
  .word[data-word-confidence-band="low"] .word-text {
    text-decoration: underline double;
    text-underline-offset: 3px;
  }
  .color-legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 10px;
    margin-top: 8px;
    font-size: 10px;
    color: #c0d0e2;
  }
  .legend-item {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .swatch {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    background: var(--word-fg);
  }
  .show-scores {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-top: 8px;
    font-size: 11px;
    color: #b7c9dd;
    cursor: pointer;
  }
  .show-scores input {
    accent-color: #93c4ff;
    margin: 0;
  }
  .word.selected {
    outline: 2px solid #93c4ff;
    outline-offset: 1px;
  }
  button:focus-visible {
    outline: 2px solid #b8d9ff;
    outline-offset: 2px;
  }
  .word:disabled {
    cursor: default;
  }
</style>
