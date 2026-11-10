<script lang="ts">
  import { structureSegments, type StructureBlock } from "$lib/ocr/structureData";

  let { text, block, onnote }: {
    text: string;
    block: StructureBlock;
    onnote?: (blockId: string) => void;
  } = $props();
  const segments = $derived(structureSegments(text, block));
</script>

{#each segments as segment, index (index)}<span data-structure-segment class:bold={segment.bold} class:italic={segment.italic} class:underline={segment.underline}>{#if segment.noteTargetBlockId && onnote}<button type="button" class="note-link" aria-label={`Follow note ${segment.text}`} title="Show the linked footnote" onclick={() => onnote?.(segment.noteTargetBlockId!)}>{segment.text}</button>{:else}{segment.text}{/if}</span>{/each}

<style>
  span { white-space: pre-wrap; overflow-wrap: anywhere; }
  .bold { font-weight: 700; }
  .italic { font-style: italic; }
  .underline { text-decoration: underline; text-underline-offset: 2px; }
  .note-link { padding: 0; border: 0; background: transparent; color: #82c0ff; font: inherit; text-decoration: underline dotted; text-underline-offset: 3px; cursor: pointer; }
  .note-link:focus-visible { outline: 2px solid #a4d1ff; outline-offset: 2px; }
</style>
