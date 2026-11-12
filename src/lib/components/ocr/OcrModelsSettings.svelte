<script lang="ts">
  // Settings > OCR models: per engine, which models are installed, their
  // size, and Download / Remove. Downloads use the shared dialog (consent,
  // free space, progress, cancel); nothing downloads without it.
  import { onMount } from "svelte";
  import { Download, HardDrive, RefreshCw, Trash2 } from "lucide-svelte";
  import { formatBytes, modelCatalog, needsDownload, removeModel, totalBytes, type PlannedModel } from "$lib/ocr/modelManager";
  import { askForModels } from "$lib/ocr/modelGate";

  let catalog = $state<{ engine: string; models: PlannedModel[] }[]>([]);
  let loading = $state(false);
  let busy = $state("");
  let error = $state("");
  let notice = $state("");
  let expanded = $state<string | null>(null);

  const ENGINE_LABELS: Record<string, string> = {
    tesseract: "Tesseract (embedded)", tesseract_fraktur: "Tesseract Fraktur", dbnet_trocr: "DBNet + TrOCR",
    parseq: "PARSeq", ppocrv6: "PP-OCRv6", easyocr: "EasyOCR", qwen2vl: "Qwen2-VL (German OCR)",
    got: "GOT-OCR 2", glm: "GLM-OCR", internvl2: "InternVL2", surya: "Surya", ocrs: "ocrs", paddle: "PaddleOCR",
  };
  const label = (engine: string) => ENGINE_LABELS[engine] ?? engine.replaceAll("_", " ");

  async function refresh() {
    loading = true;
    error = "";
    try { catalog = await modelCatalog(); }
    catch (failure) { error = failure instanceof Error ? failure.message : String(failure); catalog = []; }
    finally { loading = false; }
  }
  onMount(() => { void refresh(); });

  async function download(engine: string, models: PlannedModel[]) {
    const missing = models.filter(needsDownload);
    if (!missing.length || busy) return;
    busy = engine;
    notice = "";
    try {
      const installed = await askForModels(missing, `${label(engine)} OCR`);
      if (installed) notice = `${label(engine)} models are ready.`;
      await refresh();
    } finally { busy = ""; }
  }

  async function remove(model: PlannedModel) {
    if (busy) return;
    busy = model.name;
    notice = "";
    error = "";
    try {
      const freed = await removeModel(model.name);
      notice = `Removed ${model.name}${freed ? ` and freed ${formatBytes(freed)}` : ""}. OCR asks before downloading it again.`;
      await refresh();
    } catch (failure) { error = failure instanceof Error ? failure.message : String(failure); }
    finally { busy = ""; }
  }

  function engineState(models: PlannedModel[]): string {
    const missing = models.filter(needsDownload);
    if (!missing.length) return models.every((m) => m.installed) ? "Installed" : "Not managed here";
    return `${missing.length} missing · ${formatBytes(totalBytes(missing))}`;
  }
</script>

{#if catalog.length || loading || error}
  <div class="section-card ocr-models" data-ocr-models-settings>
    <div class="heading">
      <h2><HardDrive size={16} /> OCR models</h2>
      <button class="action-btn small" onclick={refresh} disabled={loading || !!busy} title="Refresh"><RefreshCw size={14} /></button>
    </div>
    <p class="hint">OCR downloads models only after you agree. They are verified with a checksum and shared by the app and the command line.</p>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if notice}<p class="hint" role="status">{notice}</p>{/if}
    {#each catalog as entry (entry.engine)}
      {@const missing = entry.models.filter(needsDownload)}
      <div class="engine">
        <button class="engine-row" aria-expanded={expanded === entry.engine} onclick={() => expanded = expanded === entry.engine ? null : entry.engine}>
          <span class="engine-name">{label(entry.engine)}</span>
          <span class="state" class:missing={missing.length > 0}>{engineState(entry.models)}</span>
        </button>
        {#if missing.length}
          <button class="action-btn small" onclick={() => download(entry.engine, entry.models)} disabled={!!busy} aria-label={`Download ${label(entry.engine)} models`}>
            <Download size={14} /> Download
          </button>
        {/if}
      </div>
      {#if expanded === entry.engine}
        <ul class="models">
          {#each entry.models as model (model.name)}
            <li>
              <div>
                <strong>{model.name}</strong>
                <span>{model.purpose} · {formatBytes(model.size_bytes)}{model.license ? ` · ${model.license}` : ""}{model.requires_consent ? " · consent required" : ""}</span>
                {#if model.note}<span class="note">{model.note}</span>{/if}
              </div>
              {#if model.installed && model.managed}
                <button class="action-btn small" onclick={() => remove(model)} disabled={!!busy} aria-label={`Remove ${model.name}`}><Trash2 size={14} /> Remove</button>
              {:else}
                <span class="state" class:missing={needsDownload(model)}>{model.installed ? "Installed" : needsDownload(model) ? "Missing" : "Unavailable"}</span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .heading { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  h2 { font-size: 1rem; color: #a1a1aa; display: flex; align-items: center; gap: 6px; margin: 0; }
  .hint { font-size: 12px; color: #a1a1aa; line-height: 1.5; }
  .error { font-size: 12px; color: #fca5a5; }
  .engine { display: flex; align-items: center; gap: 8px; padding: 6px 0; border-bottom: 1px solid #27272a; }
  .engine-row { flex: 1; display: flex; justify-content: space-between; gap: 10px; background: transparent; border: 0; color: inherit; font: inherit; cursor: pointer; padding: 4px 0; text-align: left; }
  .engine-name { font-size: 13px; }
  .state { font-size: 12px; color: #86efac; }
  .state.missing { color: #fde68a; }
  .models { list-style: none; margin: 0 0 8px; padding: 0 0 0 12px; }
  .models li { display: flex; justify-content: space-between; align-items: center; gap: 10px; padding: 6px 0; border-bottom: 1px solid #27272a; }
  .models strong { display: block; font-size: 12px; }
  .models span { display: block; font-size: 11px; color: #a1a1aa; margin-top: 2px; overflow-wrap: anywhere; }
  .models .note { color: #d4d4d8; }
</style>
