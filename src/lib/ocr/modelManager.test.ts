import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  consentRequired, formatBytes, initialProgress, installModels, isCancelled, missingModels, needsDownload,
  overallProgress, planSummary, plannedFromError, reduceProgress, requiredModels, spaceShortfall, totalBytes,
  type InstallProgress, type ModelPlan, type PlannedModel,
} from "./modelManager";
import { ensureOcrModels, modelGate } from "./modelGate";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), convertFileSrc: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("../store", () => ({ getSetting: vi.fn(), saveSetting: vi.fn(), getStore: vi.fn() }));

function model(name: string, overrides: Partial<PlannedModel> = {}): PlannedModel {
  return { name, purpose: "text recognition", engine: "tesseract", size_bytes: 10_000_000, license: "apache-2.0",
    requires_consent: false, consent_accepted: false, downloadable: true, installed: false, managed: true, note: null, ...overrides };
}
function plan(models: PlannedModel[], overrides: Partial<ModelPlan> = {}): ModelPlan {
  const missing = models.filter(needsDownload);
  return { engine: "tesseract", models, missing: missing.map((m) => m.name), missing_bytes: totalBytes(missing),
    free_bytes: 50e9, offline: false, ...overrides };
}
function event(model: string, phase: InstallProgress["phase"], downloaded = 0, total: number | null = null): InstallProgress {
  return { request_id: "r", model, filename: `${model}.gguf`, index: 0, count: 2, downloaded, total, phase, message: phase === "failed" ? "boom" : null };
}

describe("model plan helpers", () => {
  it("only managed, downloadable, missing models block a task", () => {
    const models = [model("dbnet-det", { installed: true }), model("tesseract-deu"),
      model("easyocr-english-g2", { downloadable: false }), model("paddleocr", { managed: false })];
    expect(missingModels(plan(models)).map((m) => m.name)).toEqual(["tesseract-deu"]);
    expect(planSummary(plan(models))).toEqual({ ready: false, label: "1 model missing (10 MB)" });
    expect(planSummary(plan([model("a", { installed: true })]))).toEqual({ ready: true, label: "Models ready" });
    // A registered model without a download (EasyOCR) is reported, not "ready".
    expect(planSummary(plan([model("dbnet-det", { installed: true }), model("easyocr-english-g2", { downloadable: false })])))
      .toEqual({ ready: false, label: "1 model unavailable", unavailable: true });
  });

  it("formats sizes and detects a disk-space shortfall", () => {
    expect(formatBytes(1_684_000_000)).toBe("1.7 GB");
    expect(formatBytes(976_000)).toBe("976 KB");
    expect(formatBytes(null)).toBe("unknown size");
    expect(spaceShortfall({ free_bytes: null }, 1e9)).toBe(0);
    expect(spaceShortfall({ free_bytes: 10e9 }, 1e9)).toBe(0);
    expect(spaceShortfall({ free_bytes: 1e9 }, 1e9)).toBe(256 * 1024 * 1024);
  });

  it("lists restricted models that still need consent", () => {
    const models = [model("got-ocr2", { requires_consent: true, license: "tongyi" }), model("glm-ocr", { requires_consent: true, consent_accepted: true }), model("dbnet-det")];
    expect(consentRequired(models).map((m) => m.name)).toEqual(["got-ocr2"]);
  });

  it("folds progress events into per-file and overall progress", () => {
    const models = [model("a", { size_bytes: 100 }), model("b", { size_bytes: 300 })];
    let state = initialProgress();
    state = reduceProgress(state, event("a", "checking"));
    state = reduceProgress(state, event("a", "downloading", 50, 100));
    expect(overallProgress(models, state)).toEqual({ downloaded: 50, total: 400, fraction: 0.125 });
    state = reduceProgress(state, event("a", "complete", 100, 100));
    // The server reports b's real size; it replaces the registry estimate.
    state = reduceProgress(state, event("b", "downloading", 100, 200));
    expect(overallProgress(models, state)).toEqual({ downloaded: 200, total: 300, fraction: 200 / 300 });
    expect(state.current).toBe("b");
    state = reduceProgress(state, event("b", "failed"));
    expect(state.failed).toBe("boom");
    expect(reduceProgress(state, event("b", "cancelled")).cancelled).toBe(true);
  });

  it("reads the models named by a MODEL_DOWNLOAD_REQUIRED error", () => {
    const error = new Error(`MODEL_DOWNLOAD_REQUIRED:${JSON.stringify([{ name: "nafnet-denoise", purpose: "learned denoise", license: "mit", size_bytes: 30000000 }])}`);
    expect(plannedFromError(error)).toEqual([expect.objectContaining({ name: "nafnet-denoise", purpose: "learned denoise", size_bytes: 30_000_000, downloadable: true })]);
    expect(plannedFromError(new Error("other"))).toBeNull();
    expect(isCancelled("OCR_MODEL_DOWNLOAD_CANCELLED")).toBe(true);
  });
});

describe("native commands", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(listen).mockReset();
  });

  it("returns no plan from builds without the manager", async () => {
    vi.mocked(invoke).mockResolvedValue(null);
    expect(await requiredModels({ engine: "tesseract" })).toBeNull();
    expect(invoke).toHaveBeenCalledWith("ocr_models_required", { request: { engine: "tesseract" } });
  });

  it("routes only this request's progress and cancels by request id", async () => {
    let handler: ((e: { payload: InstallProgress }) => void) | undefined;
    const unlisten = vi.fn();
    vi.mocked(listen).mockImplementation(async (_name, fn) => { handler = fn as typeof handler; return unlisten; });
    let finish: (value: string[]) => void = () => {};
    vi.mocked(invoke).mockImplementation((command: string) => command === "ocr_models_install"
      ? new Promise<string[]>((resolve) => { finish = resolve; }) as never : Promise.resolve(true) as never);
    const seen: InstallProgress[] = [];
    const handle = await installModels(["a"], { acceptLicenses: ["a"], onProgress: (p) => seen.push(p) });
    handler?.({ payload: { ...event("a", "downloading", 5, 10), request_id: handle.requestId } });
    handler?.({ payload: { ...event("a", "downloading", 6, 10), request_id: "another" } });
    expect(seen).toHaveLength(1);
    expect(await handle.cancel()).toBe(true);
    expect(invoke).toHaveBeenCalledWith("ocr_models_cancel", { requestId: handle.requestId });
    expect(invoke).toHaveBeenCalledWith("ocr_models_install", { names: ["a"], requestId: handle.requestId, acceptLicenses: ["a"] });
    finish(["/models/a.gguf"]);
    await handle.done;
    expect(unlisten).toHaveBeenCalled();
  });
});

describe("model gate", () => {
  beforeEach(() => { vi.mocked(invoke).mockReset(); });

  it("never blocks without a mounted dialog host", async () => {
    vi.mocked(invoke).mockResolvedValue(plan([model("tesseract-deu")]));
    expect((await ensureOcrModels({ engine: "tesseract" }, { context: "test" })).ok).toBe(false);
    vi.mocked(invoke).mockResolvedValue(plan([model("tesseract-deu", { installed: true })]));
    expect((await ensureOcrModels({ engine: "tesseract" }, { context: "test" })).ok).toBe(true);
    // A failing plan does not block: the task reports its own error.
    vi.mocked(invoke).mockRejectedValue(new Error("no such engine"));
    expect((await ensureOcrModels({ engine: "x" }, { context: "test" })).ok).toBe(true);
  });

  it("asks once for concurrent tasks and remembers a decline for background work", async () => {
    const detach = modelGate.attach();
    try {
      vi.mocked(invoke).mockResolvedValue(plan([model("tesseract-deu")]));
      const first = ensureOcrModels({ engine: "tesseract" }, { context: "page 1" });
      const second = ensureOcrModels({ engine: "tesseract" }, { context: "page 2" });
      await vi.waitFor(() => expect(modelGate.current?.models.map((m) => m.name)).toEqual(["tesseract-deu"]));
      modelGate.settle(false);
      expect((await first).ok).toBe(false);
      expect((await second).ok).toBe(false);
      expect(modelGate.current).toBeNull();
      // Background extraction does not ask again after "Not now" ...
      expect((await ensureOcrModels({ engine: "tesseract" }, { context: "file", rememberDecline: true })).ok).toBe(false);
      expect(modelGate.current).toBeNull();
      // ... but an explicit user action does, and an install clears it.
      const again = ensureOcrModels({ engine: "tesseract" }, { context: "Recognize" });
      await vi.waitFor(() => expect(modelGate.current).not.toBeNull());
      modelGate.settle(true);
      expect(await again).toMatchObject({ ok: true, missing: [] });
    } finally { detach(); }
  });
});
