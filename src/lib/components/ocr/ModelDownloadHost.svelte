<script lang="ts">
  // Mounted once in the app shell: shows the shared model download dialog
  // whenever an OCR task asks `modelGate` for missing models.
  import { onDestroy } from "svelte";
  import ModelDownloadDialog from "./ModelDownloadDialog.svelte";
  import { modelGate } from "$lib/ocr/modelGate";

  const detach = modelGate.attach();
  onDestroy(detach);
  const request = modelGate.request$;
</script>

{#if $request}
  {#key $request.id}
    <ModelDownloadDialog
      models={$request.models}
      plan={$request.plan}
      context={$request.context}
      onclose={(installed) => modelGate.settle(installed)}
    />
  {/key}
{/if}
