<script lang="ts">
  // Shared confirmation + progress dialog for OCR model downloads.
  // Confirm (list, sizes, licence consent, free space) → per-file and overall
  // progress with Cancel → done, or an error with Retry. Resolves `onclose`
  // with `true` only when every listed model is installed.
  import { onDestroy, onMount } from "svelte";
  import { Download, X, AlertTriangle, CheckCircle2 } from "lucide-svelte";
  import {
    consentRequired, formatBytes, initialProgress, installModels, isCancelled, overallProgress,
    reduceProgress, spaceShortfall, totalBytes, type InstallHandle, type ModelPlan, type PlannedModel,
    type ProgressState,
  } from "$lib/ocr/modelManager";

  let { models, plan = null, context = "", onclose }: {
    models: PlannedModel[];
    plan?: ModelPlan | null;
    /** What is waiting for the models, e.g. "Recognize 3 pages". */
    context?: string;
    onclose: (installed: boolean) => void;
  } = $props();

  let dialog: HTMLDialogElement | undefined;
  let stage = $state<"confirm" | "downloading" | "error" | "done">("confirm");
  let consent = $state(false);
  let progress = $state<ProgressState>(initialProgress());
  let error = $state("");
  let handle: InstallHandle | null = null;
  let cancelling = $state(false);
  let closed = false;

  const needsConsent = $derived(consentRequired(models));
  const total = $derived(totalBytes(models));
  const shortfall = $derived(spaceShortfall(plan, total));
  const overall = $derived(overallProgress(models, progress));
  const canDownload = $derived(stage !== "downloading" && (!needsConsent.length || consent) && shortfall === 0 && !plan?.offline);

  onMount(() => { dialog?.showModal(); });
  onDestroy(() => { if (stage === "downloading") void handle?.cancel(); });

  function finish(installed: boolean) {
    if (closed) return;
    closed = true;
    dialog?.close();
    onclose(installed);
  }

  async function download() {
    if (!canDownload) return;
    stage = "downloading";
    error = "";
    cancelling = false;
    progress = initialProgress();
    try {
      handle = await installModels(models.map((m) => m.name), {
        acceptLicenses: consent ? needsConsent.map((m) => m.name) : [],
        onProgress: (event) => { progress = reduceProgress(progress, event); },
      });
      await handle.done;
      stage = "done";
      finish(true);
    } catch (failure) {
      if (isCancelled(failure)) { stage = "confirm"; finish(false); return; }
      error = failure instanceof Error ? failure.message : String(failure);
      stage = "error";
    } finally { handle = null; }
  }

  async function cancel() {
    if (stage === "downloading" && handle) {
      cancelling = true;
      // The partial file is kept; the next download resumes it.
      await handle.cancel();
      return;
    }
    finish(false);
  }

  function percent(value: number): string { return `${Math.round(Math.max(0, Math.min(1, value)) * 100)}%`; }
  function fileFraction(model: PlannedModel): number {
    const file = progress.files[model.name];
    if (!file) return 0;
    if (file.phase === "complete" || file.phase === "verified") return 1;
    const size = file.total ?? model.size_bytes ?? 0;
    return size > 0 ? file.downloaded / size : 0;
  }
  function fileLabel(model: PlannedModel): string {
    const file = progress.files[model.name];
    if (!file) return "Waiting";
    if (file.phase === "complete") return "Ready";
    if (file.phase === "verified") return "Verified";
    if (file.phase === "checking") return "Checking";
    if (file.phase === "waiting") return "Waiting for another CrispSorter process to finish this download";
    if (file.phase === "failed") return "Failed";
    if (file.phase === "cancelled") return "Cancelled";
    return `${formatBytes(file.downloaded)} of ${formatBytes(file.total ?? model.size_bytes)}`;
  }
</script>

<dialog bind:this={dialog} class="model-dialog" aria-labelledby="model-download-title" data-model-download-dialog
  oncancel={(event) => { event.preventDefault(); void cancel(); }}>
  <div class="heading">
    <h3 id="model-download-title"><Download size={18} /> {models.length === 1 ? "Download OCR model?" : "Download OCR models?"}</h3>
    <button class="icon" aria-label="Close" onclick={() => void cancel()} disabled={cancelling}><X size={18} /></button>
  </div>
  {#if context}<p class="context">{context} needs {models.length === 1 ? "a model that is" : "models that are"} not on this computer yet.</p>{/if}

  <ul class="models" aria-label="Models to download">
    {#each models as model (model.name)}
      <li data-model={model.name}>
        <div class="row">
          <div class="name"><strong>{model.name}</strong><span>{model.purpose}{model.license ? ` · ${model.license}` : ""}</span></div>
          <span class="size">{formatBytes(model.size_bytes)}</span>
        </div>
        {#if stage !== "confirm"}
          <div class="bar" role="progressbar" aria-label={`${model.name} download`} aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(fileFraction(model) * 100)}>
            <div class="fill" class:done={progress.files[model.name]?.phase === "complete"} style:width={percent(fileFraction(model))}></div>
          </div>
          <small class="file-state">{fileLabel(model)}</small>
        {/if}
      </li>
    {/each}
  </ul>

  <p class="total"><span>Total download</span><strong>{formatBytes(total)}</strong></p>
  {#if plan?.free_bytes != null}<p class="hint">Free space on the model drive: {formatBytes(plan.free_bytes)}</p>{/if}
  {#if shortfall > 0}<p class="warning" role="alert"><AlertTriangle size={14} /> Not enough free disk space. Free at least {formatBytes(shortfall)} and try again.</p>{/if}
  {#if plan?.offline}<p class="warning" role="alert"><AlertTriangle size={14} /> Offline mode is on. Turn it off to download models.</p>{/if}

  {#if stage === "confirm" && needsConsent.length}
    <label class="consent">
      <input type="checkbox" bind:checked={consent} data-model-consent />
      <span>I accept the licence terms of {needsConsent.map((m) => `${m.name} (${m.license ?? "restricted"})`).join(", ")}. These models are restricted, for example to non-commercial use.</span>
    </label>
  {/if}

  {#if stage === "downloading" || stage === "done"}
    <div class="overall">
      <div class="bar large" role="progressbar" aria-label="Overall download" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(overall.fraction * 100)}>
        <div class="fill" style:width={percent(overall.fraction)}></div>
      </div>
      <p class="hint" role="status">{cancelling ? "Cancelling…" : `${percent(overall.fraction)} · ${formatBytes(overall.downloaded)} of ${formatBytes(overall.total)}`}</p>
    </div>
  {/if}

  {#if stage === "error"}
    <p class="error" role="alert"><AlertTriangle size={14} /> {error}</p>
    <p class="hint">Downloaded parts are kept, so Retry continues where it stopped.</p>
  {/if}

  <p class="hint">Downloads are verified with a checksum before use and stored in the shared model folder.</p>

  <div class="actions">
    {#if stage === "downloading"}
      <button onclick={() => void cancel()} disabled={cancelling}>Cancel download</button>
    {:else if stage === "error"}
      <button onclick={() => finish(false)}>Close</button>
      <button class="primary" onclick={() => void download()} disabled={!canDownload}>Retry</button>
    {:else if stage === "done"}
      <span class="ready"><CheckCircle2 size={14} /> Models ready</span>
    {:else}
      <button onclick={() => finish(false)}>Not now</button>
      <button class="primary" onclick={() => void download()} disabled={!canDownload}>Download {formatBytes(total)}</button>
    {/if}
  </div>
</dialog>

<style>
  .model-dialog {
    position: fixed; margin: auto; width: 480px; height: fit-content;
    max-width: calc(100vw - 24px); max-height: calc(100dvh - 24px); overflow: auto;
    box-sizing: border-box; color: #e4e4e7; background: #18181b;
    border: 1px solid #3f3f46; border-radius: 12px; padding: 18px 20px;
  }
  .model-dialog::backdrop { background: #000a; }
  .heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  h3 { margin: 0; font-size: 16px; display: flex; align-items: center; gap: 8px; }
  .context { margin: 10px 0 4px; font-size: 13px; color: #d4d4d8; line-height: 1.5; }
  .models { list-style: none; margin: 10px 0; padding: 0; border-top: 1px solid #27272a; }
  .models li { padding: 9px 0; border-bottom: 1px solid #27272a; }
  .row { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
  .name { min-width: 0; overflow-wrap: anywhere; }
  .name strong { display: block; font-size: 13px; }
  .name span { display: block; font-size: 11px; color: #a1a1aa; margin-top: 2px; }
  .size { flex: none; font-size: 12px; color: #d4d4d8; font-variant-numeric: tabular-nums; }
  .bar { height: 6px; margin-top: 7px; border-radius: 999px; background: #27272a; overflow: hidden; }
  .bar.large { height: 9px; }
  .fill { height: 100%; background: #60a5fa; transition: width .2s ease; }
  .fill.done { background: #4ade80; }
  .file-state { display: block; margin-top: 4px; font-size: 11px; color: #a1a1aa; font-variant-numeric: tabular-nums; }
  .total { display: flex; justify-content: space-between; margin: 8px 0 2px; font-size: 13px; }
  .hint { font-size: 12px; color: #a1a1aa; line-height: 1.5; margin: 6px 0; }
  .warning, .error { display: flex; gap: 6px; align-items: flex-start; font-size: 12px; line-height: 1.5; margin: 8px 0; overflow-wrap: anywhere; }
  .warning { color: #fbbf24; }
  .error { color: #fca5a5; }
  .consent { display: flex; gap: 8px; align-items: flex-start; margin: 10px 0; padding: 9px; border: 1px solid #a16207; border-radius: 8px; background: #422006; font-size: 12px; line-height: 1.5; }
  .consent input { margin-top: 3px; }
  .overall { margin-top: 10px; }
  .actions { display: flex; justify-content: flex-end; align-items: center; gap: 8px; margin-top: 14px; flex-wrap: wrap; }
  button { display: inline-flex; align-items: center; justify-content: center; gap: 6px; min-height: 36px; padding: 6px 14px; border: 1px solid #3f3f46; border-radius: 7px; background: #27272a; color: inherit; font: inherit; font-size: 13px; cursor: pointer; }
  button:hover:not(:disabled) { background: #3f3f46; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible { outline: 2px solid #60a5fa; outline-offset: 2px; }
  button.primary { background: #2563eb; border-color: #3b82f6; color: white; }
  button.primary:hover:not(:disabled) { background: #1d4ed8; }
  button.icon { min-height: 32px; padding: 6px; border: 0; background: transparent; }
  .ready { display: inline-flex; align-items: center; gap: 6px; color: #4ade80; font-size: 13px; }
</style>
