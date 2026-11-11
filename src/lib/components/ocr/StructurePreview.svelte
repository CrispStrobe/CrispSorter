<script lang="ts">
  import type { OcrPage } from "$lib/ocr/model";
  import type { PageStructure, StructureBlock } from "$lib/ocr/structureData";
  import StructureText from "./StructureText.svelte";

  let { page, structure, imageUrl, onselect }: {
    page: OcrPage;
    structure: PageStructure;
    imageUrl: (path: string) => string;
    onselect: (regionId: string) => void;
  } = $props();
  let windowStart = $state(0);
  const windowSize = 100;
  const regionMap = $derived(new Map(page.regions.map((region) => [region.id, region])));
  const blockPositions = $derived(new Map(structure.blocks.map((block, index) => [block.id, index])));
  const included = $derived(structure.blocks.filter((block) => {
    const region = regionMap.get(block.sourceRegionId);
    return region && !region.excluded;
  }));
  const visible = $derived(included.slice(windowStart, windowStart + windowSize));
  const sections = $derived(structure.sections.filter((section) => visible.some((block) => block.sectionId === section.id)));

  $effect(() => { if (windowStart >= included.length && windowStart) windowStart = 0; });
  function followNote(blockId: string) {
    const target = structure.blocks.find((block) => block.id === blockId);
    if (target) onselect(target.sourceRegionId);
  }
  function blockNumber(block: StructureBlock): number { return (blockPositions.get(block.id) ?? 0) + 1; }
  function listMarker(block: StructureBlock): string {
    if (block.list?.kind !== "number") return "•";
    let number = block.list.start ?? 1;
    const position = blockPositions.get(block.id) ?? 0;
    if (block.list.start === undefined) {
      for (let i = position - 1; i >= 0; i--) {
        const previous = structure.blocks[i];
        if (previous.role !== "list-item" || previous.sectionId !== block.sectionId || previous.column !== block.column || previous.list?.kind !== "number") break;
        if (previous.list.level !== block.list.level) continue;
        number++;
        if (previous.list.start !== undefined) { number += previous.list.start - 1; break; }
      }
    }
    return `${number}.`;
  }
</script>

<section class="structure-preview" data-structure-preview aria-label="Document structure preview">
  <h3>Structure preview</h3>
  <p class="help">Logical reading order and chosen formatting. Source fonts and exact page positions are not reconstructed here. Excluded blocks are omitted.</p>
  {#each sections as section (section.id)}
    <section class="logical-section" aria-label={`Section ${structure.sections.findIndex((item) => item.id === section.id) + 1}`}>
      <div class="section-label">Section {structure.sections.findIndex((item) => item.id === section.id) + 1} · {section.columns === 2 ? "Two reading columns" : "One reading column"}</div>
      <div class="columns" class:two-columns={section.columns === 2}>
        {#each Array.from({ length: section.columns }, (_, index) => index + 1) as column (column)}
          <div class="reading-column" aria-label={`Reading column ${column}`}>
            {#each visible.filter((block) => block.sectionId === section.id && block.column === column) as block (block.id)}
              {@const region = regionMap.get(block.sourceRegionId)}
              {#if region}
                <div class="preview-block" data-structure-block={block.id} data-structure-role={block.role} class:page-break={block.breakBefore === "page"} style:text-align={block.alignment}>
                  {#if block.breakBefore === "page"}<div class="break-label">Page break</div>{/if}
                  <button class="block-label" type="button" onclick={() => onselect(block.sourceRegionId)} aria-label={`Edit structure block ${blockNumber(block)}`}>{blockNumber(block)} · {block.role}</button>
                  {#if block.role === "figure"}
                    <figure>
                      <svg viewBox={`${region.x} ${region.y} ${region.w} ${region.h}`} role="img" aria-label={block.decorative ? "Decorative source figure" : block.altText || "Source figure; alternative text has not been entered"}>
                        <image href={imageUrl(page.imagePath)} width={page.width} height={page.height} />
                      </svg>
                      <figcaption class="figure-description">{block.decorative ? "Decorative figure" : block.altText || "Alternative text needed"}</figcaption>
                    </figure>
                  {:else if block.role === "heading"}
                    <svelte:element this={`h${block.headingLevel ?? 1}`} class="document-heading"><StructureText text={region.text} {block} onnote={followNote} /></svelte:element>
                  {:else if block.role === "list-item"}
                    <div class="list-item" style:padding-inline-start={`${(block.list?.level ?? 0) * 1.2}rem`}><span class="list-marker" aria-hidden="true">{listMarker(block)}</span><span><StructureText text={region.text} {block} onnote={followNote} /></span></div>
                  {:else if block.role === "table" || block.role === "formula"}
                    <pre class="specialized"><StructureText text={region.text} {block} /></pre>
                  {:else}
                    <p class:caption={block.role === "caption"} class:running={block.role === "header" || block.role === "footer"} class:footnote={block.role === "footnote"}><StructureText text={region.text} {block} onnote={followNote} /></p>
                    {#if block.role === "caption" && block.anchorBlockId}
                      {@const anchor = structure.blocks.find((candidate) => candidate.id === block.anchorBlockId)}
                      {#if anchor}<button type="button" class="anchor-link" onclick={() => onselect(anchor.sourceRegionId)}>Caption for {anchor.role} {blockNumber(anchor)}</button>{/if}
                    {/if}
                  {/if}
                </div>
              {/if}
            {/each}
          </div>
        {/each}
      </div>
    </section>
  {:else}<p class="help">No included blocks to preview.</p>{/each}
  {#if included.length > windowSize}<div class="pagination">
    <button type="button" disabled={windowStart === 0} onclick={() => (windowStart = Math.max(0, windowStart - windowSize))}>Previous preview blocks</button>
    <span>{windowStart + 1}–{Math.min(windowStart + windowSize, included.length)} of {included.length}</span>
    <button type="button" disabled={windowStart + windowSize >= included.length} onclick={() => (windowStart += windowSize)}>Next preview blocks</button>
  </div>{/if}
</section>

<style>
  .structure-preview { margin-top: 16px; padding: 12px; border: 1px solid #52525b; border-radius: 6px; background: #18181b; }
  h3 { margin: 0; color: #dbeafe; font-size: 14px; }
  .help, .section-label, .figure-description { color: #93c5fd; font-size: 11px; line-height: 1.6; }
  .help { margin: 8px 0 12px; }
  .logical-section + .logical-section { margin-top: 20px; padding-top: 12px; border-top: 1px dashed #71717a; }
  .section-label { margin-bottom: 7px; }
  .columns { display: grid; grid-template-columns: minmax(0, 1fr); gap: 18px; }
  .columns.two-columns { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .reading-column { min-width: 0; }
  .preview-block { margin-bottom: 14px; min-width: 0; color: #dbeafe; font-size: 13px; line-height: 1.7; overflow-wrap: anywhere; }
  .block-label, .anchor-link { display: block; border: 0; padding: 2px 0; background: transparent; color: #93c5fd; font: inherit; font-size: 10px; cursor: pointer; text-align: start; }
  .block-label { margin-bottom: 3px; }
  .anchor-link { margin-top: 3px; }
  .document-heading { margin: 0; font-size: 1.2em; line-height: 1.5; }
  h1.document-heading { font-size: 1.55em; } h2.document-heading { font-size: 1.35em; }
  p, figure { margin: 0; white-space: pre-wrap; }
  .list-item { display: flex; align-items: baseline; gap: 7px; }
  .list-marker { flex: none; }
  .caption { font-style: italic; }
  .running, .footnote { font-size: .9em; color: #d4d4d8; }
  .specialized { margin: 0; font: inherit; white-space: pre-wrap; }
  figure svg { display: block; max-width: 100%; width: 100%; max-height: 180px; background: #fff; border-radius: 3px; }
  .figure-description { margin-top: 4px; }
  .page-break { border-top: 1px dashed #60a5fa; margin-top: 14px; padding-top: 7px; }
  .break-label { font-size: 10px; color: #d4d4d8; text-align: start; }
  .pagination { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; color: #93c5fd; font-size: 10px; }
  .pagination button { padding: 6px; border: 1px solid #71717a; border-radius: 4px; background: #3f3f46; color: #dbeafe; font: inherit; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
  @media (max-width: 480px) { .columns.two-columns { gap: 10px; } }
</style>
