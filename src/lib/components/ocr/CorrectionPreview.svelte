<script lang="ts">
  import type { CorrectionPreview } from "$lib/ocr/proofreading";

  let { preview, selectedIds, onselect }: {
    preview: CorrectionPreview;
    selectedIds: string[];
    onselect: (ids: string[]) => void;
  } = $props();
  let start = $state(0);
  const pageSize = 100;
  const selected = $derived(new Set(selectedIds));
  const visible = $derived(preview.matches.slice(start, start + pageSize));
  $effect(() => { void preview; start = 0; });

  function toggle(id: string, checked: boolean) {
    onselect(checked ? [...selectedIds, id] : selectedIds.filter((candidate) => candidate !== id));
  }
</script>

<div class="correction-preview" data-correction-preview>
  <div class="preview-summary">
    <span>{preview.matches.length} matching occurrence{preview.matches.length === 1 ? "" : "s"}</span>
    <span data-skipped-protected={preview.skippedProtected}>{preview.skippedProtected} protected skipped</span>
    <span data-skipped-approved={preview.skippedApproved}>{preview.skippedApproved} approved skipped</span>
    <span data-skipped-excluded={preview.skippedExcluded}>{preview.skippedExcluded} non-text or excluded passages skipped</span>
  </div>
  {#if preview.matches.length}
    <label class="select-all"><input type="checkbox"
      checked={preview.matches.every((match) => selected.has(match.id))}
      onchange={(event) => onselect(event.currentTarget.checked ? preview.matches.map((match) => match.id) : [])} />Select all corrections</label>
    <div class="match-list">
      {#each visible as match, index (match.id)}
        <label class="match">
          <input type="checkbox" checked={selected.has(match.id)}
            aria-label={`Apply correction ${start + index + 1}: ${match.before} → ${match.after || "(remove text)"}`}
            onchange={(event) => toggle(match.id, event.currentTarget.checked)} />
          <span class="match-content">
            <span class="page-name">{match.pageName}</span>
            <span class="context" dir="auto">{match.context}</span>
            <span class="replacement" dir="auto"><del>{match.before}</del><span aria-hidden="true"> → </span><ins>{match.after || "(remove text)"}</ins></span>
          </span>
        </label>
      {/each}
    </div>
    {#if preview.matches.length > pageSize}<div class="pagination">
      <button type="button" disabled={start === 0} onclick={() => (start = Math.max(0, start - pageSize))}>Previous occurrences</button>
      <span>{start + 1}–{Math.min(start + pageSize, preview.matches.length)} of {preview.matches.length}</span>
      <button type="button" disabled={start + pageSize >= preview.matches.length} onclick={() => (start += pageSize)}>Next occurrences</button>
    </div>{/if}
  {:else}
    <p>No eligible occurrences match this scope. Check the literal text, language, protected entries, and approved-text option.</p>
  {/if}
  <p class="preview-note">Only selected occurrences will change. Affected approvals reopen; original OCR evidence remains available. Undo restores the previous text.</p>
</div>

<style>
  .correction-preview { min-width: 0; color: #d4d4d8; font-size: 12px; }
  .preview-summary { display: flex; flex-wrap: wrap; gap: 7px 16px; padding: 10px 0; color: #93c5fd; }
  .select-all { display: flex; align-items: center; gap: 7px; padding: 8px 0; }
  .match-list { max-height: min(48vh, 440px); overflow-y: auto; border: 1px solid #3f3f46; border-radius: 5px; }
  .match { display: flex; align-items: flex-start; gap: 9px; padding: 11px; border-bottom: 1px solid #3f3f46; background: #18181b; }
  .match:last-child { border-bottom: 0; }
  .match input { flex: none; margin-top: 3px; }
  .match-content { display: flex; flex-direction: column; gap: 6px; min-width: 0; overflow-wrap: anywhere; }
  .page-name { color: #bfdbfe; font-size: 11px; }
  .context { color: #a1a1aa; line-height: 1.5; white-space: pre-wrap; }
  .replacement { color: #dbeafe; line-height: 1.6; white-space: pre-wrap; }
  del { color: #fecaca; background: #27272a; }
  ins { color: #bbf7d0; background: #052e16; text-decoration: none; }
  .preview-note { color: #a1a1aa; line-height: 1.6; }
  .pagination { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; padding-top: 8px; font-size: 11px; }
  button { padding: 5px 7px; border: 1px solid #52525b; border-radius: 4px; background: #27272a; color: #dbeafe; font: inherit; cursor: pointer; }
  button:disabled { opacity: .55; cursor: default; }
  button:focus-visible, input:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
</style>
