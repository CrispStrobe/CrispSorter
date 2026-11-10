<script lang="ts">
  import type { SourceComponent } from "$lib/ocr/passages";

  let { components, imageUrl }: {
    components: SourceComponent[];
    imageUrl: (path: string) => string;
  } = $props();
  let expanded = $state(false);
  let openComponents = $state<string[]>([]);
  let openEvidence = $state<string[]>([]);
  let openCharacters = $state<string[]>([]);
  let selectedWords = $state<Record<string, number | null>>({});
  let evidenceStarts = $state<Record<string, number>>({});
  let characterStarts = $state<Record<string, number>>({});
  const evidencePageSize = 150;

  function toggled(ids: string[], id: string, open: boolean): string[] {
    return open ? [...new Set([...ids, id])] : ids.filter((value) => value !== id);
  }
  function sourceBox(component: SourceComponent) {
    const index = selectedWords[component.id];
    const word = index === null || index === undefined ? undefined : component.wordEvidence?.words[index];
    return word ?? component.boundsAtCapture;
  }
  function viewBox(component: SourceComponent): string {
    const box = sourceBox(component);
    const x = Math.max(0, box.x - 10), y = Math.max(0, box.y - 6);
    return `${x} ${y} ${Math.min(component.width, box.x + box.w + 10) - x} ${Math.min(component.height, box.y + box.h + 6) - y}`;
  }
  function percent(value: number): string { return `${Math.round(value * 100)}%`; }
</script>

{#if components.length}<section class="passage-sources" data-passage-sources aria-label="Passage source evidence">
  <details ontoggle={(event) => (expanded = event.currentTarget.open)}>
    <summary>Source components ({components.length})</summary>
    {#if expanded}
      <p class="source-help">These records preserve the source text, image, and OCR evidence from before structural editing. They do not score the current manual passage or infer where its edited words appear.</p>
      {#each components as component, index (component.id)}
        <details class="source-component" data-source-component={component.id}
          ontoggle={(event) => (openComponents = toggled(openComponents, component.id, event.currentTarget.open))}>
          <summary>Source component {index + 1}</summary>
          {#if openComponents.includes(component.id)}
            <p class="source-meta">{component.kind} · {component.confidenceBasis === "recognition" && component.confidence !== null
              ? `Original region score ${percent(component.confidence)}`
              : component.confidenceBasis === "manual" ? "Manual source" : "Region recognition score unavailable"}</p>
            <svg data-source-component-image viewBox={viewBox(component)} role="img" aria-label={`Source image for component ${index + 1}`}>
              <image href={imageUrl(component.imagePath)} width={component.width} height={component.height} />
            </svg>
            <div class="source-meta source-area">
              <span>Recorded region area: x {component.boundsAtCapture.x}, y {component.boundsAtCapture.y}, {component.boundsAtCapture.w} × {component.boundsAtCapture.h} px.</span>
              {#if selectedWords[component.id] !== null && selectedWords[component.id] !== undefined}<button type="button" onclick={() => (selectedWords = { ...selectedWords, [component.id]: null })}>Show recorded area</button>{/if}
            </div>
            <label>{component.confidenceBasis === "manual" ? "Creation text" : "Original OCR text"}
              <textarea data-source-original-text readonly dir="auto" rows="3" value={component.originalText}
                aria-label={`${component.confidenceBasis === "manual" ? "Creation text" : "Original OCR"} for source component ${index + 1}`}></textarea>
            </label>
            {#if component.textAtCapture !== component.originalText}<label>Text when this source was recorded
              <textarea readonly dir="auto" rows="3" value={component.textAtCapture} aria-label={`Text at capture for source component ${index + 1}`}></textarea>
            </label>{/if}
            {#if component.wordEvidence?.words.length}
              <details class="word-evidence" data-source-word-evidence
                ontoggle={(event) => (openEvidence = toggled(openEvidence, component.id, event.currentTarget.open))}>
                <summary>Original word evidence ({component.wordEvidence.words.length})</summary>
                {#if openEvidence.includes(component.id)}
                  {@const start = evidenceStarts[component.id] ?? 0}
                  <p class="source-help">Select a native word to inspect its original image. Scores are recognizer signals, not probabilities of accuracy. Evidence source: {component.wordEvidence.source}.</p>
                  <div class="source-words">
                    {#each component.wordEvidence.words.slice(start, start + evidencePageSize) as word, localIndex (word.start)}
                      {@const wordIndex = start + localIndex}
                      <button type="button"
                        class:active={selectedWords[component.id] === wordIndex}
                        data-source-word-start={word.start}
                        data-source-word-confidence={word.confidence_basis === "recognition" ? word.confidence ?? undefined : undefined}
                        aria-label={`Inspect source component ${index + 1} word ${wordIndex + 1}: ${word.text}`}
                        aria-pressed={selectedWords[component.id] === wordIndex}
                        onclick={() => (selectedWords = { ...selectedWords, [component.id]: wordIndex })}
                      ><span dir="auto">{word.text}</span><small>{word.confidence_basis === "recognition" && word.confidence !== null ? percent(word.confidence) : "Unscored"}</small></button>
                    {/each}
                  </div>
                  {#if component.wordEvidence.words.length > evidencePageSize}<div class="evidence-pagination">
                    <button type="button" disabled={start === 0} onclick={() => (evidenceStarts = { ...evidenceStarts, [component.id]: Math.max(0, start - evidencePageSize) })}>Previous source words</button>
                    <span>{start + 1}–{Math.min(start + evidencePageSize, component.wordEvidence.words.length)} of {component.wordEvidence.words.length}</span>
                    <button type="button" disabled={start + evidencePageSize >= component.wordEvidence.words.length} onclick={() => (evidenceStarts = { ...evidenceStarts, [component.id]: start + evidencePageSize })}>Next source words</button>
                  </div>{/if}
                {/if}
              </details>
            {:else}<p class="source-help">No native word locations were supplied for this source record.</p>{/if}
            {#if component.confidenceBasis === "recognition" && component.charConfidenceUnit === "unicode_codepoint" && component.charConfidenceAligned && component.char_conf.length}
              <details class="character-evidence" data-source-character-evidence
                ontoggle={(event) => (openCharacters = toggled(openCharacters, component.id, event.currentTarget.open))}>
                <summary>Original character evidence ({component.char_conf.length})</summary>
                {#if openCharacters.includes(component.id)}
                  {@const start = characterStarts[component.id] ?? 0}
                  <p class="source-help">Scores describe characters in the recorded original text. No character image locations are inferred.</p>
                  <div class="source-characters">
                    {#each Array.from(component.originalText).slice(start, start + evidencePageSize) as character, localIndex}
                      <span data-source-character-confidence={component.char_conf[start + localIndex]} title={`Original character ${start + localIndex + 1}: ${percent(component.char_conf[start + localIndex])}`}><span dir="auto">{character === " " ? "␠" : character === "\n" ? "↵" : character === "\r" ? "␍" : character === "\t" ? "⇥" : character}</span><small>{percent(component.char_conf[start + localIndex])}</small></span>
                    {/each}
                  </div>
                  {#if component.char_conf.length > evidencePageSize}<div class="evidence-pagination">
                    <button type="button" disabled={start === 0} onclick={() => (characterStarts = { ...characterStarts, [component.id]: Math.max(0, start - evidencePageSize) })}>Previous source characters</button>
                    <span>{start + 1}–{Math.min(start + evidencePageSize, component.char_conf.length)} of {component.char_conf.length}</span>
                    <button type="button" disabled={start + evidencePageSize >= component.char_conf.length} onclick={() => (characterStarts = { ...characterStarts, [component.id]: start + evidencePageSize })}>Next source characters</button>
                  </div>{/if}
                {/if}
              </details>
            {/if}
          {/if}
        </details>
      {/each}
    {/if}
  </details>
</section>{/if}

<style>
  .passage-sources { padding: 10px; border-top: 1px solid #34465c; background: #121e2d; min-width: 0; }
  summary { cursor: pointer; color: #bdd6f2; font-size: 12px; padding: 4px 0; overflow-wrap: anywhere; }
  summary:focus-visible, button:focus-visible, textarea:focus-visible { outline: 2px solid #83b8f4; outline-offset: 2px; }
  .source-help, .source-meta { font-size: 11px; color: #a8bed7; line-height: 1.6; margin: 8px 0; overflow-wrap: anywhere; }
  .source-component { margin-top: 8px; padding: 7px 9px; border: 1px solid #354c65; border-radius: 5px; min-width: 0; }
  .source-area { display: flex; flex-wrap: wrap; gap: 7px; align-items: center; }
  svg { display: block; width: 100%; height: 85px; background: #fff; border-radius: 4px; }
  label { display: flex; flex-direction: column; gap: 5px; margin: 10px 0; font-size: 11px; color: #bfd0e5; }
  textarea { box-sizing: border-box; width: 100%; min-width: 0; padding: 8px; border: 1px solid #344b64; border-radius: 4px; background: #0e1826; color: #dce8f7; font: inherit; font-size: 12px; line-height: 1.6; resize: vertical; }
  .source-words { display: flex; flex-wrap: wrap; gap: 6px; margin: 8px 0; }
  button { padding: 5px 7px; border: 1px solid #425a76; border-radius: 4px; background: #1a2d43; color: #d1e6fe; font: inherit; font-size: 11px; cursor: pointer; max-width: 100%; overflow-wrap: anywhere; }
  button.active { border-color: #e9c670; }
  button:disabled { opacity: .5; cursor: default; }
  .source-words small { margin-left: 7px; color: #a7bed9; white-space: nowrap; }
  .evidence-pagination { display: flex; align-items: center; flex-wrap: wrap; gap: 7px; color: #a7bed9; font-size: 10px; margin: 8px 0; }
  .source-characters { display: flex; flex-wrap: wrap; gap: 5px; margin: 8px 0; }
  .source-characters > span { display: inline-flex; flex-direction: column; align-items: center; gap: 3px; padding: 5px; border: 1px solid #405974; border-radius: 3px; min-width: 24px; color: #d9e7f8; font-size: 12px; }
  .source-characters small { color: #a7bed9; font-size: 9px; }
</style>
