<script lang="ts">
  import type { OcrProject } from "$lib/ocr/model";
  import { extractionObjectFreshness } from "$lib/ocr/extractionObjects";
  import type { TableExtractionObject } from "$lib/ocr/tableOperations";
  import TableGridPreview from "./TableGridPreview.svelte";
  let { project, object, label, imageUrl }: {
    project: OcrProject; object: TableExtractionObject; label: string; imageUrl: (path: string) => string;
  } = $props();
  let sourceId = $state("");
  let imageFailed = $state(false);
  let lastCapture = "";
  const freshness = $derived(extractionObjectFreshness(project, object));
  const capture = $derived(object.captures.find((item) => item.id === sourceId) ?? object.captures[0]);
  const viewBox = $derived.by(() => {
    if (!capture) return "0 0 1 1";
    if (!capture.bounds) return "0 0 " + capture.width + " " + capture.height;
    const x = Math.max(0, capture.bounds.x - 10), y = Math.max(0, capture.bounds.y - 10);
    const right = Math.min(capture.width, capture.bounds.x + capture.bounds.w + 10);
    const bottom = Math.min(capture.height, capture.bounds.y + capture.bounds.h + 10);
    return x + " " + y + " " + Math.max(1, right - x) + " " + Math.max(1, bottom - y);
  });
  $effect(() => {
    const key = object.id + ":" + (capture?.id ?? "");
    if (key !== lastCapture) { lastCapture = key; imageFailed = false; }
  });
</script>

<section class="table-source-preview" data-table-source-object-id={object.id}>
  <div class="source-status" class:historical={!freshness.current}>{freshness.current ? "Current source" : "Historical source"}</div>
  {#if !freshness.current}<p class="source-warning">{freshness.reasons.join(" ")}</p>{/if}
  {#if capture}
    {#if object.captures.length > 1}<label>{label} captured source<select aria-label={label + " captured source"} value={capture.id} onchange={(event) => (sourceId = event.currentTarget.value)}>
      {#each object.captures as item, index (item.id)}<option value={item.id}>Source {index + 1} · {item.regionId ? "region" : "page"}</option>{/each}
    </select></label>{/if}
    <svg class="source-image" role="img" aria-label={label + " captured source image"} viewBox={viewBox}>
      <image href={imageUrl(capture.imagePath)} width={capture.width} height={capture.height} onerror={() => (imageFailed = true)} />
      {#if capture.bounds}<rect x={capture.bounds.x} y={capture.bounds.y} width={capture.bounds.w} height={capture.bounds.h} vector-effect="non-scaling-stroke"><title>Captured source region</title></rect>{/if}
    </svg>
    {#if imageFailed}<p class="source-warning">The captured image could not be loaded. The saved source cells remain available below.</p>{/if}
  {:else}<p class="source-warning">This input has no validated captured image. Its recorded table data is retained.</p>{/if}
  <TableGridPreview table={object.currentData} label={label + " cells"} />
</section>

<style>
  .table-source-preview { min-width: 0; margin-top: 11px; }
  .source-status { display: inline-block; margin-bottom: 9px; padding: 4px 7px; border: 1px solid #45766c; border-radius: 4px; background: #173d35; color: #b5e7d7; font-size: 10px; }
  .source-status.historical { border-color: #967648; background: #3b3121; color: #edd0a0; }
  .source-warning { margin: 8px 0 11px; color: #e8cea3; font-size: 11px; line-height: 1.65; overflow-wrap: anywhere; }
  .source-image { box-sizing: border-box; display: block; width: 100%; height: 130px; margin-bottom: 13px; border: 1px solid #4d6585; border-radius: 5px; background: #fff; }
  rect { fill: none; stroke: #246abc; stroke-width: 2px; }
  label { display: flex; flex-direction: column; gap: 6px; margin: 9px 0; color: #bdd2ed; font-size: 11px; }
  select { box-sizing: border-box; width: 100%; min-width: 0; padding: 8px; border: 1px solid #466282; border-radius: 5px; background: #102036; color: #e0ecfc; font: inherit; }
  select:focus-visible { outline: 2px solid #a6d1ff; outline-offset: 2px; }
</style>
