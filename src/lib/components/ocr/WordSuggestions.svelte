<script lang="ts">
  import type { CorrectionSuggestion, ProofreadingSelection } from "$lib/ocr/proofreading";

  let {
    selection, language, suggestions, errorMessage = "", ignored = false, disabled = false,
    ready = true, userEditable = false, onlanguage, onadd, onignore, onpreview, onremember,
  }: {
    selection: ProofreadingSelection;
    language: string;
    suggestions: CorrectionSuggestion[];
    errorMessage?: string;
    ignored?: boolean;
    disabled?: boolean;
    ready?: boolean;
    userEditable?: boolean;
    onlanguage: (language: string) => void;
    onadd: (kind: "word" | "protected", scope: "project" | "user") => void;
    onignore: () => void;
    onpreview: (suggestion: CorrectionSuggestion) => void;
    onremember: (suggestion: CorrectionSuggestion) => void;
  } = $props();

  let scope = $state<"project" | "user">("project");
</script>

<section class="word-suggestions" aria-label="Proofreading suggestions">
  <h4>Proofreading <span dir="auto">{selection.text}</span></h4>
  <div class="preferences">
    <label>Proofreading language
      <input value={language} maxlength="40" placeholder="en, de, und…"
        onchange={(event) => { onlanguage(event.currentTarget.value); event.currentTarget.value = language; }} disabled={disabled} />
    </label>
    <label>Save selected term in
      <select bind:value={scope} disabled={disabled}>
        <option value="project">This project</option>
        <option value="user" disabled={!userEditable}>All projects</option>
      </select>
    </label>
  </div>
  <div class="actions">
    <button type="button" onclick={() => onadd("word", scope)} disabled={disabled || (scope === "user" && !userEditable)}>Add to vocabulary</button>
    <button type="button" onclick={() => onadd("protected", scope)} disabled={disabled || (scope === "user" && !userEditable)}
      title="Keep this exact identifier out of proofreading suggestions and correction rules in every language">Protect identifier</button>
    <button type="button" onclick={onignore} disabled={disabled || ignored}>Ignore suggestion</button>
  </div>
  <p class="hint">Vocabulary and protection guide proofreading. They do not change OCR scores or approve the text. Language “und” means unspecified.</p>
  {#if !ready}
    <p class="hint">Load the all-project vocabulary before applying suggestions so protected entries can be checked.</p>
  {:else if errorMessage}
    <p class="hint" role="alert">Suggestions could not be checked. {errorMessage}</p>
  {:else if ignored}
    <p class="hint" data-suggestion-ignored>This occurrence is ignored for the current text version. Its OCR evidence still needs normal review.</p>
  {:else if suggestions.length}
    <ul>
      {#each suggestions as suggestion (suggestion.id)}
        <li>
          <div class="candidate">
            <strong dir="auto">{suggestion.text || "(remove text)"}</strong>
            <span>{suggestion.label}{#if suggestion.source === "engine" && suggestion.confidence !== null} · {Math.round(suggestion.confidence * 100)}% native score{/if}</span>
          </div>
          <div class="actions">
            <button type="button" onclick={() => onpreview(suggestion)} disabled={disabled}
              aria-label={`Use suggestion: ${suggestion.text}`}>Use suggestion</button>
            <button type="button" onclick={() => onremember(suggestion)} disabled={disabled}
              aria-label={`Remember correction: ${suggestion.text}`}>Remember correction</button>
          </div>
        </li>
      {/each}
    </ul>
    <p class="hint">Review the proposed replacement before applying it. A suggestion is not a confidence estimate.</p>
  {:else}
    <p class="hint">No conservative suggestion is available for this selection. You can edit the text directly; absence from a custom vocabulary does not mean a word is wrong.</p>
  {/if}
</section>

<style>
  .word-suggestions { padding: 10px; border-top: 1px solid #3f3f46; min-width: 0; background: #18181b; }
  h4 { display: flex; gap: 8px; flex-wrap: wrap; margin: 0 0 9px; font-size: 12px; color: #e4e4e7; }
  h4 span { font-weight: 400; color: #93c5fd; overflow-wrap: anywhere; }
  .preferences { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 8px; }
  label { display: flex; flex-direction: column; gap: 4px; color: #d4d4d8; font-size: 10px; min-width: 0; }
  input, select { box-sizing: border-box; width: 100%; min-width: 0; padding: 5px 6px; border: 1px solid #52525b; border-radius: 4px; background: #18181b; color: #e4e4e7; font: inherit; font-size: 12px; }
  .actions { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; }
  button { padding: 5px 7px; border: 1px solid #52525b; border-radius: 4px; background: #27272a; color: #dbeafe; font: inherit; font-size: 11px; cursor: pointer; }
  button:disabled, input:disabled, select:disabled { opacity: .55; cursor: default; }
  button:focus-visible, input:focus-visible, select:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
  .hint { margin: 8px 0 0; color: #a1a1aa; font-size: 11px; line-height: 1.55; }
  ul { list-style: none; padding: 0; margin: 8px 0 0; }
  li { padding: 8px 0; border-top: 1px solid #3f3f46; }
  .candidate { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .candidate strong { font-size: 13px; font-weight: 600; color: #dbeafe; overflow-wrap: anywhere; }
  .candidate > span { font-size: 10px; color: #a1a1aa; }
  @media (max-width: 500px) { .preferences { grid-template-columns: minmax(0, 1fr); } }
</style>
