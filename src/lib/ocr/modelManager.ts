// OCR model manager client: what an OCR task needs, and the explicit,
// cancellable installer (`ocr_models_*` commands, `ocr-model-download` events).
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
/** Same prefix as `client.ts` (kept here so this module stays dependency-light). */
const MODEL_DOWNLOAD_REQUIRED = "MODEL_DOWNLOAD_REQUIRED:";

export interface PlannedModel {
  name: string;
  purpose: string;
  engine: string;
  size_bytes: number | null;
  license: string | null;
  requires_consent: boolean;
  consent_accepted: boolean;
  downloadable: boolean;
  installed: boolean;
  managed: boolean;
  note: string | null;
}

export interface ModelPlan {
  engine: string;
  models: PlannedModel[];
  missing: string[];
  missing_bytes: number;
  free_bytes: number | null;
  offline: boolean;
}

export type InstallPhase = "checking" | "waiting" | "downloading" | "verified" | "complete" | "failed" | "cancelled";
export interface InstallProgress {
  request_id: string | null;
  model: string;
  filename: string;
  index: number;
  count: number;
  downloaded: number;
  total: number | null;
  phase: InstallPhase;
  message: string | null;
}

/** Options the run will use; the backend resolves them with the same policy. */
export interface ModelPlanRequest {
  surface?: "workspace" | "extract";
  config?: Record<string, unknown> | null;
  engine?: string | null;
  language?: string | null;
  cpuPixelBudget?: boolean | null;
  ocrTier?: string | null;
  ocrRecLang?: string | null;
  extras?: ("kie" | "math" | "layout")[];
  /** Plan only the specialist models in `extras`, without a recognizer. */
  extrasOnly?: boolean;
}

export const MODEL_PROGRESS_EVENT = "ocr-model-download";
export const MODEL_DOWNLOAD_CANCELLED = "OCR_MODEL_DOWNLOAD_CANCELLED";

/** A model that blocks the task until it is downloaded. */
export function needsDownload(model: PlannedModel): boolean {
  return model.managed !== false && model.downloadable && !model.installed;
}

export function missingModels(plan: ModelPlan | null | undefined): PlannedModel[] {
  return plan?.models?.filter(needsDownload) ?? [];
}

export function consentRequired(models: PlannedModel[]): PlannedModel[] {
  return models.filter((model) => model.requires_consent && !model.consent_accepted);
}

export function totalBytes(models: PlannedModel[]): number {
  return models.reduce((sum, model) => sum + (Number.isFinite(model.size_bytes) ? Math.max(0, model.size_bytes ?? 0) : 0), 0);
}

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return "unknown size";
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${Math.round(bytes / 1e6)} MB`;
  if (bytes >= 1e3) return `${Math.round(bytes / 1e3)} KB`;
  return `${Math.round(bytes)} B`;
}

/** Short chip text: "Models ready" / "2 models missing (1.7 GB)". */
export function planSummary(plan: ModelPlan | null | undefined): { ready: boolean; label: string; unavailable?: boolean } {
  const missing = missingModels(plan);
  // Registered but not downloadable (e.g. EasyOCR): say so, the run would fail.
  const unavailable = plan?.models?.filter((m) => m.managed !== false && !m.downloadable && !m.installed) ?? [];
  if (plan && !missing.length && unavailable.length) {
    return { ready: false, label: `${unavailable.length} ${unavailable.length === 1 ? "model" : "models"} unavailable`, unavailable: true };
  }
  if (!plan || !missing.length) return { ready: true, label: "Models ready" };
  const size = totalBytes(missing);
  return { ready: false, label: `${missing.length} ${missing.length === 1 ? "model" : "models"} missing${size ? ` (${formatBytes(size)})` : ""}` };
}

/** Free space is short when the download plus a small margin does not fit. */
export function spaceShortfall(plan: Pick<ModelPlan, "free_bytes"> | null | undefined, needed: number): number {
  const free = plan?.free_bytes;
  if (free == null || !Number.isFinite(free)) return 0;
  const margin = 256 * 1024 * 1024;
  return Math.max(0, needed + margin - free);
}

/** Per-file and overall progress for the dialog, folded from events. */
export interface ProgressState {
  files: Record<string, { downloaded: number; total: number | null; phase: InstallPhase; message: string | null }>;
  current: string | null;
  failed: string | null;
  cancelled: boolean;
}

export function initialProgress(): ProgressState {
  return { files: {}, current: null, failed: null, cancelled: false };
}

export function reduceProgress(state: ProgressState, event: InstallProgress): ProgressState {
  const previous = state.files[event.model];
  const done = event.phase === "complete";
  const entry = {
    downloaded: done ? Math.max(event.downloaded, previous?.total ?? 0, previous?.downloaded ?? 0) : event.downloaded,
    total: event.total ?? previous?.total ?? null,
    phase: event.phase,
    message: event.message ?? null,
  };
  if (done && entry.total == null && entry.downloaded > 0) entry.total = entry.downloaded;
  return {
    files: { ...state.files, [event.model]: entry },
    current: done ? state.current === event.model ? null : state.current : event.model,
    failed: event.phase === "failed" ? event.message ?? `${event.model} failed` : state.failed,
    cancelled: state.cancelled || event.phase === "cancelled",
  };
}

/** Overall fraction 0..1: completed models count fully, the rest by bytes,
 * with the registry estimate standing in until the server reports a size. */
export function overallProgress(models: PlannedModel[], state: ProgressState): { downloaded: number; total: number; fraction: number } {
  let downloaded = 0;
  let total = 0;
  for (const model of models) {
    const file = state.files[model.name];
    const size = file?.total ?? model.size_bytes ?? 0;
    total += size;
    downloaded += file?.phase === "complete" ? size : Math.min(file?.downloaded ?? 0, size || Infinity);
  }
  const fraction = total > 0 ? Math.min(1, downloaded / total)
    : models.length ? models.filter((m) => state.files[m.name]?.phase === "complete").length / models.length : 1;
  return { downloaded, total, fraction };
}

function isPlan(value: unknown): value is ModelPlan {
  return !!value && typeof value === "object" && Array.isArray((value as ModelPlan).models);
}

/** The models a task needs. Builds without the manager report nothing missing. */
export async function requiredModels(request: ModelPlanRequest): Promise<ModelPlan | null> {
  const plan = await invoke<unknown>("ocr_models_required", { request });
  return isPlan(plan) ? plan : null;
}

export async function modelCatalog(): Promise<{ engine: string; models: PlannedModel[] }[]> {
  const catalog = await invoke<unknown>("ocr_models_catalog");
  return Array.isArray(catalog) ? catalog.filter((entry) => entry && Array.isArray(entry.models)) : [];
}

export function removeModel(name: string): Promise<number> {
  return invoke<number>("ocr_models_remove", { name });
}

export interface InstallHandle {
  requestId: string;
  done: Promise<string[]>;
  cancel(): Promise<boolean>;
}

/** Start an install; progress events for this request reach `onProgress`. */
export async function installModels(
  names: string[],
  options: { acceptLicenses?: string[]; onProgress?: (progress: InstallProgress) => void } = {},
): Promise<InstallHandle> {
  const requestId = crypto.randomUUID();
  const unlisten = await listen<InstallProgress>(MODEL_PROGRESS_EVENT, (event) => {
    if (event.payload?.request_id === requestId) options.onProgress?.(event.payload);
  });
  const done = invoke<string[]>("ocr_models_install", { names, requestId, acceptLicenses: options.acceptLicenses ?? [] })
    .finally(() => { try { unlisten(); } catch { /* The bridge may already be closed. */ } });
  return {
    requestId,
    done,
    cancel: () => invoke<boolean>("ocr_models_cancel", { requestId }).catch(() => false),
  };
}

/** Models listed in a `MODEL_DOWNLOAD_REQUIRED:` error, as planned models. */
export function plannedFromError(error: unknown): PlannedModel[] | null {
  const text = error instanceof Error ? error.message : String(error ?? "");
  const at = text.indexOf(MODEL_DOWNLOAD_REQUIRED);
  if (at < 0) return null;
  try {
    const parsed = JSON.parse(text.slice(at + MODEL_DOWNLOAD_REQUIRED.length));
    if (!Array.isArray(parsed)) return null;
    return parsed.filter((m) => m && typeof m.name === "string").map((m) => ({
      name: m.name, purpose: String(m.purpose ?? ""), engine: String(m.engine ?? ""),
      size_bytes: typeof m.size_bytes === "number" ? m.size_bytes : null,
      license: typeof m.license === "string" ? m.license : null,
      requires_consent: m.requires_consent === true, consent_accepted: m.consent_accepted === true,
      downloadable: m.downloadable !== false, installed: false, managed: m.managed !== false, note: m.note ?? null,
    }));
  } catch { return null; }
}

export function isCancelled(error: unknown): boolean {
  return String(error instanceof Error ? error.message : error).includes(MODEL_DOWNLOAD_CANCELLED);
}
