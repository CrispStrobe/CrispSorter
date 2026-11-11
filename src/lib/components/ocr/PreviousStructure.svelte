<script lang="ts">
  import { readStructureHistory, type StructureSnapshot } from "$lib/ocr/structureHistory";
  import StructureText from "./StructureText.svelte";

  let { value, imageUrl }: { value: unknown; imageUrl: (path: string) => string } = $props();
  let expanded = $state(false);
  let selected = $state(0);
  let windowStart = $state(0);
  let openImages = $state<string[]>([]);
  const size = 100;
  const snapshotCount = $derived(value && typeof value === "object" && "snapshots" in value && Array.isArray(value.snapshots) ? value.snapshots.length : 0);
  const history = $derived(expanded ? readStructureHistory(value) : undefined);
  const snapshots = $derived(history?.snapshots.slice().reverse() ?? []);
  const snapshot = $derived(snapshots[selected]);
  const regions = $derived(new Map(snapshot?.regions.map((region) => [region.id, region]) ?? []));
  $effect(() => {
    if (selected >= snapshots.length) selected = 0;
    if (snapshot && windowStart >= snapshot.structure.blocks.length) windowStart = 0;
  });
  function choose(index: number) { selected = index; windowStart = 0; openImages = []; }
  function sourceLabel(item: StructureSnapshot): string {
    return `${item.capturedAt} · ${item.structure.blocks.length} blocks`;
  }
</script>

{#if snapshotCount}<details class="previous-structure" data-previous-document-structure bind:open={expanded}>
  <summary>Previous document structure ({snapshotCount})</summary>
  {#if expanded && snapshot}
    <p>These snapshots retain reviewed text and formatting from before source blocks were replaced. They are read-only history, separate from current OCR confidence.</p>
    <label>Previous structure snapshot<select aria-label="Previous structure snapshot" value={selected} onchange={(event) => choose(Number(event.currentTarget.value))}>{#each snapshots as item, index}<option value={index}>{sourceLabel(item)}</option>{/each}</select></label>
    <p class="source">Captured source: {snapshot.imagePath} · {snapshot.width} × {snapshot.height} pixels</p>
    {#each snapshot.structure.blocks.slice(windowStart, windowStart + size) as block, index (block.id)}
      {@const region = regions.get(block.sourceRegionId)}
      {#if region}<article>
        <h4>{windowStart + index + 1} · {block.role}{region.excluded ? " · Excluded" : ""}</h4>
        <div class="old-text" style:text-align={block.alignment}><StructureText text={region.text} {block} /></div>
        {#if block.altText}<p>Figure description: {block.altText}</p>{/if}
        {#if block.decorative}<p>Decorative figure</p>{/if}
        {#if block.anchorBlockId}<p>Caption source block: {block.anchorBlockId}</p>{/if}
        {#if block.noteReferences?.length}<p>{block.noteReferences.length} preserved note link{block.noteReferences.length === 1 ? "" : "s"}</p>{/if}
        <details class="source-image" ontoggle={(event) => (openImages = event.currentTarget.open ? [...new Set([...openImages, block.id])] : openImages.filter((id) => id !== block.id))}><summary>Previous source image for block {windowStart + index + 1}</summary>
          {#if openImages.includes(block.id)}<svg viewBox={`${region.x} ${region.y} ${region.w} ${region.h}`} role="img" aria-label={`Source area for previous block ${windowStart + index + 1}`}><image href={imageUrl(snapshot.imagePath)} width={snapshot.width} height={snapshot.height} /></svg>{/if}
        </details>
      </article>{/if}
    {/each}
    {#if snapshot.structure.blocks.length > size}<div class="pagination">
      <button type="button" disabled={windowStart === 0} onclick={() => (windowStart = Math.max(0, windowStart - size))}>Previous history blocks</button>
      <span>{windowStart + 1}–{Math.min(windowStart + size, snapshot.structure.blocks.length)} of {snapshot.structure.blocks.length}</span>
      <button type="button" disabled={windowStart + size >= snapshot.structure.blocks.length} onclick={() => (windowStart += size)}>Next history blocks</button>
    </div>{/if}
    <p>History is bounded by snapshot count and size. Undo restores recent editing operations; these records preserve earlier replacements after reopening.</p>
  {/if}
</details>{/if}

<style>
  .previous-structure { margin: 10px 0; padding: 10px; border: 1px solid #52525b; border-radius: 5px; color: #bfdbfe; background: #18181b; font-size: 11px; }
  summary { cursor: pointer; line-height: 1.7; color: #bfdbfe; }
  p { color: #93c5fd; line-height: 1.6; overflow-wrap: anywhere; }
  label { display: flex; flex-direction: column; gap: 6px; }
  select { width: 100%; padding: 7px; border: 1px solid #52525b; border-radius: 4px; background: #18181b; color: #dbeafe; font: inherit; }
  article { padding: 10px 0; border-top: 1px solid #3f3f46; }
  h4 { margin: 0 0 6px; font-size: 11px; }
  .old-text { white-space: pre-wrap; overflow-wrap: anywhere; color: #dbeafe; font-size: 13px; line-height: 1.75; }
  .source-image { margin-top: 8px; }
  svg { display: block; width: 100%; max-height: 160px; background: white; margin-top: 7px; }
  .pagination { display: flex; flex-wrap: wrap; align-items: center; gap: 7px; }
  button { padding: 6px; border: 1px solid #71717a; border-radius: 4px; background: #3f3f46; color: #dbeafe; font: inherit; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  summary:focus-visible, select:focus-visible, button:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
</style>
