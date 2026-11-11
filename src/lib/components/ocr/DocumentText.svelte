<script lang="ts">
  import { Info } from "lucide-svelte";
  import type { OcrPage, OcrRegion } from "$lib/ocr/model";
  import { getWordEvidence, getWordConfidenceBand, type WordConfidenceBand } from "$lib/ocr/words";

  /**
   * The page's recognized text as one editable document, in reading order:
   * one paragraph per passage, words coloured by recognition confidence.
   * Clicking places the caret where you clicked; edits are written back to
   * the passage's region, so the page image and the text stay linked.
   */
  let {
    page,
    threshold,
    selectedId = null,
    disabled = false,
    engine = null,
    onselect,
    onedit,
    onblur,
  }: {
    page: OcrPage;
    threshold: number;
    selectedId?: string | null;
    disabled?: boolean;
    /** Short description of the engine that produced this page, if known. */
    engine?: string | null;
    onselect: (regionId: string) => void;
    onedit: (region: OcrRegion, text: string) => void;
    onblur: () => void;
  } = $props();

  type Piece = { text: string; band: WordConfidenceBand | null; score: number | null };

  const passages = $derived(
    page.regions.filter((r) => !r.excluded && r.kind !== "picture" && r.kind !== "table"),
  );
  const pieces = $derived.by(() => {
    const map = new Map<string, Piece[]>();
    for (const region of passages) {
      const list: Piece[] = [];
      let cursor = 0;
      for (const word of getWordEvidence(region)) {
        if (word.start > cursor) list.push({ text: region.text.slice(cursor, word.start), band: null, score: null });
        const score = word.basis === "characters" ? word.minConfidence : word.confidence;
        list.push({ text: region.text.slice(word.start, word.end), band: getWordConfidenceBand(word, threshold), score });
        cursor = word.end;
      }
      if (cursor < region.text.length) list.push({ text: region.text.slice(cursor), band: null, score: null });
      map.set(region.id, list);
    }
    return map;
  });
  const counts = $derived.by(() => {
    const c = { low: 0, medium: 0, scored: 0 };
    for (const list of pieces.values()) for (const piece of list) {
      if (piece.band === "low") c.low++;
      else if (piece.band === "medium") c.medium++;
      if (piece.band && piece.band !== "unscored") c.scored++;
    }
    return c;
  });
  const omitted = $derived(page.regions.filter((r) => !r.excluded && (r.kind === "picture" || r.kind === "table")).length);

  let root = $state<HTMLElement>();
  let editingId = $state<string | null>(null);
  let caretAt = 0;

  const title = (piece: Piece) =>
    piece.band === "edited" ? "Edited" : piece.score === null ? "No confidence score" : `Confidence ${Math.round(piece.score * 100)}%`;

  /** Text offset of a click inside a rendered passage. */
  function clickOffset(block: HTMLElement, event: MouseEvent): number | null {
    const doc = block.ownerDocument as Document & {
      caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
    };
    let node: Node | null = null;
    let offset = 0;
    if (doc.caretPositionFromPoint) {
      const position = doc.caretPositionFromPoint(event.clientX, event.clientY);
      if (position) ({ offsetNode: node, offset } = position);
    } else if (doc.caretRangeFromPoint) {
      const range = doc.caretRangeFromPoint(event.clientX, event.clientY);
      if (range) { node = range.startContainer; offset = range.startOffset; }
    }
    if (!node || !block.contains(node)) return null;
    const range = doc.createRange();
    range.setStart(block, 0);
    range.setEnd(node, offset);
    return range.toString().length;
  }

  function startEditing(region: OcrRegion, offset: number) {
    onselect(region.id);
    if (disabled) return;
    caretAt = Math.max(0, Math.min(offset, region.text.length));
    editingId = region.id;
  }

  function placeCaret(element: HTMLElement, offset: number) {
    const selection = element.ownerDocument.getSelection();
    if (!selection) return;
    const range = element.ownerDocument.createRange();
    const text = element.firstChild;
    if (text && text.nodeType === Node.TEXT_NODE) range.setStart(text, Math.min(offset, text.textContent?.length ?? 0));
    else range.setStart(element, 0);
    range.collapse(true);
    selection.removeAllRanges();
    selection.addRange(range);
  }

  function caretOffset(element: HTMLElement): number {
    const selection = element.ownerDocument.getSelection();
    if (!selection?.rangeCount) return 0;
    const range = element.ownerDocument.createRange();
    range.setStart(element, 0);
    range.setEnd(selection.getRangeAt(0).startContainer, selection.getRangeAt(0).startOffset);
    return range.toString().length;
  }

  /** The editable paragraph owns its DOM while focused; Svelte does not re-render it. */
  function editable(element: HTMLElement, region: OcrRegion) {
    element.textContent = region.text;
    element.focus();
    placeCaret(element, caretAt);
  }

  function move(index: number, toEnd: boolean) {
    const next = passages[index];
    if (!next) return;
    startEditing(next, toEnd ? next.text.length : 0);
  }

  function keydown(event: KeyboardEvent, region: OcrRegion, index: number) {
    const element = event.currentTarget as HTMLElement;
    if (event.key === "Escape") { event.preventDefault(); element.blur(); return; }
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      // Plain newlines keep the paragraph's text exact (no <br>/<div> markup).
      event.preventDefault();
      element.ownerDocument.execCommand("insertText", false, "\n");
      return;
    }
    const text = element.textContent ?? "";
    if (event.key === "ArrowDown" && caretOffset(element) >= text.length) { event.preventDefault(); move(index + 1, false); }
    else if (event.key === "ArrowUp" && caretOffset(element) === 0) { event.preventDefault(); move(index - 1, true); }
  }

  function input(event: Event, region: OcrRegion) {
    const text = (event.currentTarget as HTMLElement).textContent ?? "";
    if (text !== region.text) onedit(region, text);
  }

  function blur(region: OcrRegion) {
    if (editingId === region.id) editingId = null;
    onblur();
  }

  // Selecting a region on the page image scrolls its paragraph into view.
  $effect(() => {
    const id = selectedId;
    if (!id || !root) return;
    const block = Array.from(root.querySelectorAll<HTMLElement>("[data-passage-id]")).find((el) => el.dataset.passageId === id);
    block?.scrollIntoView({ block: "nearest" });
  });
</script>

<section class="document-text" aria-label="Page text">
  <div class="document-bar">
    {#if counts.low}<span class="chip low" title="Words below the low-confidence threshold">{counts.low} low</span>{/if}
    {#if counts.medium}<span class="chip medium" title="Words below the review threshold">{counts.medium} uncertain</span>{/if}
    {#if !counts.scored && passages.length}<span class="chip" title="This engine reported no word scores for this page">no word scores</span>{/if}
    {#if engine}<span class="engine" title="Recognition engine for this page">{engine}</span>{/if}
    <span class="spacer"></span>
    <span
      class="info"
      tabindex="0"
      role="note"
      aria-label="About the page text"
      title={"Click anywhere in the text to edit it. Each paragraph stays linked to its region on the page image; Enter adds a line break, arrow keys move between paragraphs, Esc finishes. Red words have low confidence, amber words are uncertain, green words were edited."
        + (omitted ? ` ${omitted} table or picture region${omitted === 1 ? " is" : "s are"} edited in Regions or Structure.` : "")}
      ><Info size={14} /></span
    >
  </div>
  <div class="paper" bind:this={root}>
    {#each passages as region, index (region.id)}
      {#if editingId === region.id}
        <div
          class="passage editing {region.kind}"
          data-passage-id={region.id}
          contenteditable="plaintext-only"
          role="textbox"
          tabindex="0"
          aria-multiline="true"
          aria-label={`Text of passage ${index + 1}`}
          spellcheck="true"
          use:editable={region}
          oninput={(e) => input(e, region)}
          onkeydown={(e) => keydown(e, region, index)}
          onblur={() => blur(region)}
        ></div>
      {:else}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="passage {region.kind}"
          class:selected={region.id === selectedId}
          class:empty={!region.text}
          data-passage-id={region.id}
          role="button"
          tabindex="0"
          aria-label={`Edit passage ${index + 1}: ${region.text.slice(0, 60)}`}
          onclick={(e) => startEditing(region, clickOffset(e.currentTarget as HTMLElement, e) ?? region.text.length)}
          onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); startEditing(region, region.text.length); } }}
        >{#each pieces.get(region.id) ?? [] as piece, i (i)}{#if piece.band && piece.band !== "high" && piece.band !== "unscored"}<span
              class="word {piece.band}"
              title={title(piece)}>{piece.text}</span
            >{:else}{piece.text}{/if}{/each}</div>
      {/if}
    {:else}
      <p class="placeholder">No text on this page yet.</p>
    {/each}
  </div>
</section>

<style>
  .document-text { display: flex; flex-direction: column; gap: 6px; min-height: 0; flex: 1; }
  .document-bar { display: flex; align-items: center; gap: 6px; font-size: 11px; color: #a1a1aa; min-height: 22px; }
  .document-bar .spacer { flex: 1; }
  .chip { border-radius: 999px; padding: 1px 8px; background: #27272a; color: #d4d4d8; }
  .chip.low { background: #7f1d1d66; color: #fca5a5; }
  .chip.medium { background: #78350f55; color: #fcd34d; }
  .engine { color: #71717a; }
  .info { display: inline-flex; color: #71717a; cursor: help; border-radius: 4px; }
  .info:hover, .info:focus-visible { color: #d4d4d8; }
  .paper {
    flex: 1; min-height: 0; overflow-y: auto; background: #fafafa; color: #18181b;
    border-radius: 6px; padding: 22px 26px; font-size: 14px; line-height: 1.6;
    box-shadow: 0 1px 2px #0006;
  }
  .passage {
    white-space: pre-wrap; overflow-wrap: anywhere; margin: 0 0 0.75em; padding: 1px 3px;
    border-radius: 3px; cursor: text; outline: none;
  }
  .passage:hover { background: #f4f4f5; }
  .passage.selected { background: #dbeafe; }
  .passage.editing { background: #fff; box-shadow: 0 0 0 1px #60a5fa; cursor: text; }
  .passage.empty::before { content: "(empty)"; color: #a1a1aa; }
  .passage.title { font-size: 1.25em; font-weight: 600; }
  .passage.header, .passage.footer { font-size: 0.85em; color: #52525b; }
  .passage.formula { font-family: ui-monospace, monospace; }
  .word { border-radius: 2px; }
  .word.medium { background: #fde68a; }
  .word.low { background: #fecaca; text-decoration: underline wavy #dc2626; text-underline-offset: 3px; }
  .word.edited { color: #15803d; }
  .placeholder { color: #71717a; margin: 0; }
</style>
