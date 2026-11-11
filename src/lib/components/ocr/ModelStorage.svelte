<script lang="ts">
  import { onDestroy, tick, untrack } from "svelte";
  import { HardDrive, RefreshCw, Trash2, X } from "lucide-svelte";
  import type { WorkspaceClient, ModelStorageReport, ModelStorageEntry } from "$lib/ocr/client";

  let { client, disabled = false, revision = 0, onerror, onnotice }: {
    client: WorkspaceClient;
    disabled?: boolean;
    revision?: number;
    onerror: (error: unknown) => void;
    onnotice: (notice: string) => void;
  } = $props();
  let expanded = $state(false);
  let busy = $state(false);
  let report = $state<ModelStorageReport | null>(null);
  let removing = $state<ModelStorageEntry | null>(null);
  let dialog: HTMLDialogElement | undefined;
  let mounted = true;
  onDestroy(() => { mounted = false; });
  const available = $derived(!!client.modelStorage && !!client.cleanModelStorage && !!client.removeMobileModel);
  function size(bytes: number): string {
    return bytes >= 1024 ** 3 ? `${(bytes / 1024 ** 3).toFixed(2)} GiB` : `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
  }
  async function refresh() {
    if (busy || !client.modelStorage) return;
    busy = true;
    try { const next = await client.modelStorage(); if (mounted) report = next; }
    catch (error) { if (mounted) onerror(error); }
    finally { if (mounted) busy = false; }
  }
  $effect(() => {
    revision;
    untrack(() => { if (expanded) void refresh(); });
  });
  async function toggle() { expanded = !expanded; if (expanded) await refresh(); }
  async function clean() {
    if (disabled || busy || !client.cleanModelStorage) return;
    busy = true;
    try {
      const result = await client.cleanModelStorage();
      if (mounted) { report = result.report; onnotice(`Freed ${size(result.removed_bytes)} of unused OCR downloads.`); }
    } catch (error) { if (mounted) onerror(error); }
    finally { if (mounted) busy = false; }
  }
  async function confirmRemoval(model: ModelStorageEntry) {
    if (disabled || busy || model.in_use || !model.version) return;
    removing = { ...model };
    await tick(); dialog?.showModal();
  }
  async function remove() {
    if (disabled || busy || !removing?.version || !client.removeMobileModel) return;
    const model = removing;
    dialog?.close(); removing = null; busy = true;
    try {
      const result = await client.removeMobileModel(model.name, model.version!);
      if (mounted) { report = result.report; onnotice(`Removed ${model.name} and freed ${size(result.removed_bytes)}. Install it again before using this model for OCR.`); }
    } catch (error) { if (mounted) onerror(error); }
    finally { if (mounted) busy = false; }
  }
</script>

{#if available}
  <div class="model-storage">
    <button class="storage-toggle" onclick={toggle} aria-expanded={expanded} disabled={disabled || busy}>
      <HardDrive size={16} /> Manage model storage
    </button>
    {#if expanded}
      <section aria-label="OCR model storage" aria-busy={busy}>
        <div class="storage-heading">
          <h4>Downloaded models</h4>
          <button class="icon-button" aria-label="Refresh model storage" title="Refresh model storage" onclick={refresh} disabled={disabled || busy}><RefreshCw size={16} /></button>
        </div>
        {#if report}
          <p class="storage-summary">{size(report.bytes)} downloaded · {size(report.reclaimable_bytes)} can be cleaned up</p>
          {#each report.models as model}
            <div class="model-row">
              <div><strong>{model.name}</strong><span>{size(model.bytes)}{model.in_use ? " · In use until app restart" : ""}</span></div>
              {#if model.version}<button class="remove-button" aria-label={`Remove ${model.name}`} onclick={() => confirmRemoval(model)} disabled={disabled || busy || model.in_use}><Trash2 size={14} /> Remove</button>{/if}
            </div>
          {:else}<p class="storage-hint">No models are stored on this device.</p>{/each}
          <button class="clean-button" onclick={clean} disabled={disabled || busy || report.reclaimable_bytes === 0}>Clean unused downloads</button>
          <p class="storage-hint">Cleanup removes interrupted downloads and old versions. Installed models and models used by an open application are kept.</p>
          {#each report.warnings as warning}<p class="storage-warning">{warning}</p>{/each}
        {:else}<p class="storage-hint" role="status">{busy ? "Checking downloaded models…" : "Storage could not be checked. Refresh to try again."}</p>{/if}
        {#if busy && report}<p class="storage-hint" role="status">Updating model storage…</p>{/if}
      </section>
    {/if}
  </div>
{/if}

<dialog bind:this={dialog} class="remove-dialog" aria-labelledby="remove-model-title" onclose={() => { removing = null; }}>
  <div class="storage-heading"><h3 id="remove-model-title">Remove downloaded model?</h3><button class="icon-button" aria-label="Cancel model removal" onclick={() => dialog?.close()}><X size={18} /></button></div>
  <p>Remove <strong>{removing?.name}</strong> from this device? You will need to download it again before using it for OCR. Your documents and reviewed text are kept.</p>
  <div class="dialog-actions"><button onclick={() => dialog?.close()}>Keep model</button><button class="confirm-remove" onclick={remove} disabled={disabled || busy}>Remove model</button></div>
</dialog>

<style>
  .model-storage { margin-top: 12px; min-width: 0; }
  button { display: inline-flex; align-items: center; justify-content: center; gap: 6px; min-height: 40px; border: 1px solid var(--border-color, #3f3f46); border-radius: 7px; padding: 7px 10px; color: inherit; background: transparent; font: inherit; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible { outline: 2px solid var(--accent-color, #60a5fa); outline-offset: 2px; }
  .storage-toggle, .clean-button { width: 100%; }
  section { margin-top: 10px; padding-top: 8px; border-top: 1px solid var(--border-color, #3f3f46); }
  .storage-heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  h4, h3 { margin: 0; font-size: 14px; }
  h3 { font-size: 17px; }
  .icon-button { padding: 8px; min-width: 40px; border: 0; }
  .storage-summary, .storage-hint { font-size: 12px; line-height: 1.5; opacity: .8; }
  .model-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 0; border-bottom: 1px solid var(--border-color, #3f3f46); }
  .model-row > div { min-width: 0; overflow-wrap: anywhere; }
  .model-row strong, .model-row span { display: block; font-size: 12px; }
  .model-row span { opacity: .7; margin-top: 4px; }
  .remove-button { flex-shrink: 0; font-size: 12px; }
  .clean-button { margin-top: 12px; }
  .storage-warning { font-size: 12px; line-height: 1.5; color: var(--warning-color, #eab308); overflow-wrap: anywhere; }
  .remove-dialog {
    position: fixed;
    inset: calc(12px + env(safe-area-inset-top, 0px))
      calc(12px + env(safe-area-inset-right, 0px))
      calc(12px + env(safe-area-inset-bottom, 0px))
      calc(12px + env(safe-area-inset-left, 0px));
    margin: auto;
    width: 420px;
    height: fit-content;
    max-width: calc(100vw - 24px - env(safe-area-inset-left, 0px) - env(safe-area-inset-right, 0px));
    max-height: calc(100vh - 24px - env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px));
    max-height: calc(100dvh - 24px - env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px));
    overflow: auto;
    overscroll-behavior: contain;
    box-sizing: border-box;
    color: var(--text-primary, #e4e4e7);
    background: var(--bg-secondary, #27272a);
    border: 1px solid var(--border-color, #3f3f46);
    border-radius: 12px;
    padding: 20px;
  }
  .remove-dialog button { min-height: 44px; }
  .remove-dialog .icon-button { min-width: 44px; }
  .remove-dialog::backdrop { background: #0009; }
  .remove-dialog p { line-height: 1.6; font-size: 14px; overflow-wrap: anywhere; }
  .dialog-actions { display: flex; justify-content: flex-end; flex-wrap: wrap; gap: 10px; }
  .confirm-remove { color: #fca5a5; border-color: #ef4444; }
  @media (pointer: coarse) { button { min-height: 44px; } }
</style>
