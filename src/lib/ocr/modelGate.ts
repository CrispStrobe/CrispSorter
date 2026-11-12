// One app-wide place that asks the user before OCR models are downloaded.
// The workbench, the inbox and preparation all call `ensureOcrModels`; the
// `ModelDownloadHost` mounted in the shell shows the dialog. Concurrent tasks
// needing the same models share one dialog (and one download).
import { get, writable, type Readable } from "svelte/store";
import { missingModels, requiredModels, type ModelPlan, type ModelPlanRequest, type PlannedModel } from "./modelManager";

export interface ModelRequest {
  id: number;
  key: string;
  models: PlannedModel[];
  plan: ModelPlan | null;
  context: string;
  resolve: (installed: boolean) => void;
}

class ModelGate {
  private currentStore = writable<ModelRequest | null>(null);
  /** The request the dialog host shows, if any. */
  readonly request$: Readable<ModelRequest | null> = { subscribe: this.currentStore.subscribe };
  private queue: ModelRequest[] = [];
  private pending = new Map<string, Promise<boolean>>();
  /** Model sets the user declined in this app session. */
  private declined = new Set<string>();
  private nextId = 1;
  private hosts = 0;

  get current(): ModelRequest | null { return get(this.currentStore); }

  /** The shell's dialog host registers itself; without one, never block. */
  attach(): () => void {
    this.hosts += 1;
    return () => { this.hosts = Math.max(0, this.hosts - 1); };
  }

  get available(): boolean { return this.hosts > 0; }

  /** Ask once per distinct set of models; resolves `true` once installed. */
  request(models: PlannedModel[], plan: ModelPlan | null, context: string, rememberDecline = false): Promise<boolean> {
    if (!models.length) return Promise.resolve(true);
    if (!this.available) return Promise.resolve(false);
    const key = models.map((m) => m.name).sort().join("\n");
    // Background extraction does not ask again for every file after "Not now".
    if (rememberDecline && this.declined.has(key)) return Promise.resolve(false);
    const existing = this.pending.get(key);
    if (existing) return existing;
    const promise = new Promise<boolean>((resolve) => {
      const request: ModelRequest = { id: this.nextId++, key, models, plan, context, resolve };
      if (this.current) this.queue.push(request); else this.currentStore.set(request);
    });
    this.pending.set(key, promise);
    return promise;
  }

  /** Called by the host when the dialog closes. */
  settle(installed: boolean) {
    const request = this.current;
    if (!request) return;
    this.pending.delete(request.key);
    if (installed) this.declined.delete(request.key); else this.declined.add(request.key);
    this.currentStore.set(this.queue.shift() ?? null);
    request.resolve(installed);
  }
}

export const modelGate = new ModelGate();

export interface EnsureResult {
  ok: boolean;
  plan: ModelPlan | null;
  missing: PlannedModel[];
}

/** Check a task's models; when some are missing, ask (unless `interactive`
 * is false) and install them. Plan failures do not block: the task then
 * reports its own, more specific error. */
export async function ensureOcrModels(
  request: ModelPlanRequest,
  options: { context: string; interactive?: boolean; rememberDecline?: boolean },
): Promise<EnsureResult> {
  let plan: ModelPlan | null;
  try { plan = await requiredModels(request); } catch { return { ok: true, plan: null, missing: [] }; }
  const missing = missingModels(plan);
  if (!missing.length) return { ok: true, plan, missing };
  if (options.interactive === false) return { ok: false, plan, missing };
  const installed = await modelGate.request(missing, plan, options.context, options.rememberDecline ?? false);
  return { ok: installed, plan, missing: installed ? [] : missing };
}

/** Ask for models named in a MODEL_DOWNLOAD_REQUIRED error. */
export function askForModels(models: PlannedModel[], context: string): Promise<boolean> {
  return modelGate.request(models, null, context);
}
