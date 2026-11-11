import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import type {
  ExportPage,
  ImportedDocument,
  ImportedPage,
  OcrProject,
  RecognitionResult,
} from "./model";
import type { SaveReceipt } from "./autosave";
import type { PhotoQuad } from "./photo";
import { loadCapabilities } from "../capabilities";
import { readProviderFile, isProviderUri, PROVIDER_MAX_BYTES, type ProviderFile } from "../mobileProvider";
import { getSetting, saveSetting } from "../store";
import { readProofreadingData, type ProofreadingData } from "./proofreadingData";
import type { OfficeExportOptions } from "./officeExport";
import type { WorkbookOptions } from "./workbookExport";

export interface WorkspaceCapabilities {
  ocr: boolean;
  crispembed: boolean;
  pdf_import: boolean;
  native_document_import?: boolean;
  image_import_formats?: string[];
  pdf_reason?: string;
  formats: string[];
  engines: string[];
  layout: boolean;
  kie: boolean;
  table: boolean;
  math: boolean;
  photo_tools?: boolean;
  camera_import?: boolean;
  mobile_model_store?: boolean;
  default_engine?: string;
  default_language?: string;
  /** Native editable Word structure is available in this build. */
  structured_docx?: boolean;
  /** Saved tables can be exported as literal editable workbook cells. */
  structured_xlsx?: boolean;
  /** Isolated low-memory Qwen2-VL worker is available in this build. */
  cpu_budget?: boolean;
  cpu_budget_pixels?: number;
  reason?: string;
  pdf_export_reason?: string;
  archival_pdf_export?: boolean;
  pdf_bidi_export?: boolean;
  pdf_bidi_export_reason?: string;
  docx_export?: boolean;
  docx_export_reason?: string;
  pdfa_conformance?: "PDF/A-2b";
  pdfa_export_reason?: string;
  reasons?: Record<string, string>;
  system_tesseract?: {
    available: boolean;
    version?: string | null;
    languages: string[];
    reason?: string | null;
  };
}
export interface ModelDownloadProgress {
  name: string;
  filename: string;
  phase: "checking" | "downloading" | "verified" | "complete";
  downloaded: number;
  total: number | null;
  request_id?: string;
}
export interface ModelStorageEntry {
  name: string;
  version: string | null;
  bytes: number;
  reclaimable_bytes: number;
  in_use: boolean;
}
export interface ModelStorageReport {
  bytes: number;
  reclaimable_bytes: number;
  models: ModelStorageEntry[];
  warnings: string[];
}
export interface ModelStorageCleanup {
  removed_bytes: number;
  removed_versions: number;
  report: ModelStorageReport;
}
export interface ImportProgress {
  request_id: string;
  phase: "archiving" | "rendering" | "verifying" | "complete";
  completed: number;
  total: number | null;
}
export interface ImportOptions {
  signal?: AbortSignal;
  onProgress?: (progress: ImportProgress) => void;
}
export function importProgressLabel(progress: ImportProgress): string {
  if (progress.phase === "archiving") return "Preserving the original document…";
  if (progress.phase === "complete") return "Document imported";
  const action = progress.phase === "verifying" ? "Checking" : "Rendering";
  return progress.total ? `${action} pages: ${progress.completed} of ${progress.total}` : `${action} document…`;
}
export function modelDownloadLabel(progress: ModelDownloadProgress): string {
  if (progress.phase === "checking") return `Checking ${progress.name}…`;
  if (progress.phase === "verified") return `Verified ${progress.filename}`;
  if (progress.phase === "complete") return `${progress.name} is installed`;
  const done = (progress.downloaded / (1024 * 1024)).toFixed(1);
  const total = progress.total ? ` / ${(progress.total / (1024 * 1024)).toFixed(1)} MiB` : " MiB";
  return `${progress.filename}: ${done}${total}`;
}
export interface ProjectEnvelope extends SaveReceipt {
  project: OcrProject;
  warnings?: string[];
}
export interface ProjectSummary {
  id: string;
  name: string;
  updated_at: number | string;
  page_count: number;
  revision?: number;
}
export interface RecognitionOptions {
  pagePath: string;
  pageWidthPoints?: number;
  pageHeightPoints?: number;
  config?: Record<string, unknown>;
  engine?: string;
  language?: string;
  zone?: { x: number; y: number; w: number; h: number };
  /** Run Qwen2-VL in the isolated low-memory CPU worker. */
  cpuPixelBudget?: boolean;
}
export interface TransformOptions {
  pagePath: string;
  pageWidthPoints?: number;
  pageHeightPoints?: number;
  operation: "rotate" | "crop" | "split" | "perspective" | "deskew";
  degrees?: number;
  crop?: { x: number; y: number; w: number; h: number };
  splitX?: number;
  quad?: PhotoQuad;
  angle?: number;
}
export interface PhotoAssessment {
  width: number;
  height: number;
  sample_width: number;
  sample_height: number;
  mean_luminance: number;
  contrast: number;
  sharpness: number;
  ink_fraction: number;
  correction_degrees: number | null;
  skew_confidence: number;
  warnings: string[];
}
export interface ExtractionField {
  label: string;
  value: string;
  score: number;
  original_value?: string;
  review?: "unreviewed" | "corrected";
}
export interface ExportResult {
  saved_path: string;
  bytes: number;
  pages: number;
  format: string;
  cancelled?: boolean;
  warnings?: string[];
  sheets?: number;
}
export interface ExportRequest {
  outputPath: string;
  format: string;
  pdfa: boolean;
  pages: ExportPage[];
  overwrite: boolean;
  sourceUris: string[];
  officeOptions?: OfficeExportOptions;
  workbookOptions?: WorkbookOptions;
}
export interface ExportProgress {
  request_id: string;
  phase: "rendering" | "writing" | "committing" | "destination";
  bytes: number;
}
export interface ExportOptions {
  signal?: AbortSignal;
  onProgress?: (progress: ExportProgress) => void;
}
export function exportProgressLabel(progress: ExportProgress): string {
  const bytes = Number.isFinite(progress.bytes) ? Math.max(0, progress.bytes) : 0;
  const size = bytes >= 1024 * 1024 ? `${(bytes / (1024 * 1024)).toFixed(1)} MiB`
    : bytes >= 1024 ? `${(bytes / 1024).toFixed(1)} KiB` : `${Math.floor(bytes)} B`;
  const action = { rendering: "Rendering document", writing: "Writing export",
    committing: "Finishing export", destination: "Saving to a destination" }[progress.phase];
  return `${action} · ${size}`;
}

/** Explicit adapter makes the complete review workflow testable without model downloads. */
/** Tesseract needs a concrete language: "automatic" follows the app's own
 *  interface language (German → deu), so German text keeps its umlauts. */
let defaultOcrLanguage = "eng";
/** Set from the app's interface language by the OCR workspace. */
export function setDefaultOcrLanguage(language: string): void { defaultOcrLanguage = language; }
export function recognitionLanguage(engine?: string | null, language?: string | null): string | null {
  const automatic = !language || language === "auto";
  const tesseract = !engine || engine === "auto" || engine === "tesseract";
  if (automatic && tesseract) return defaultOcrLanguage;
  return language ?? null;
}

export interface PrepareOptions {
  /** The user agreed to download the models this preparation needs. */
  allowDownloads?: boolean;
  /** Called with each preparation step as it starts (e.g. NAFNet denoise). */
  onStage?: (stage: string) => void;
}

/** Prefix of the error that asks for download consent; JSON follows. */
export const MODEL_DOWNLOAD_REQUIRED = "MODEL_DOWNLOAD_REQUIRED:";
export interface MissingModel { name: string; purpose: string; license?: string | null }
export function missingModels(error: unknown): MissingModel[] | null {
  const text = error instanceof Error ? error.message : String(error ?? "");
  const at = text.indexOf(MODEL_DOWNLOAD_REQUIRED);
  if (at < 0) return null;
  try {
    const parsed = JSON.parse(text.slice(at + MODEL_DOWNLOAD_REQUIRED.length));
    return Array.isArray(parsed) ? parsed.filter((m) => m && typeof m.name === "string") : null;
  } catch { return null; }
}

export interface WorkspaceClient {
  capabilities(): Promise<WorkspaceCapabilities>;
  list(): Promise<ProjectSummary[]>;
  load(projectId?: string): Promise<ProjectEnvelope | null>;
  save(project: OcrProject, expectedRevision: number): Promise<ProjectEnvelope>;
  importDocument(locationUri: string, options?: ImportOptions): Promise<ImportedDocument>;
  importImage?(name: string, bytes: Uint8Array, options?: ImportOptions): Promise<ImportedDocument>;
  assessPhoto?(pagePath: string): Promise<PhotoAssessment>;
  installMobileModels?(language: string, onProgress?: (progress: ModelDownloadProgress) => void): Promise<string[]>;
  modelStorage?(): Promise<ModelStorageReport>;
  cleanModelStorage?(): Promise<ModelStorageCleanup>;
  removeMobileModel?(name: string, expectedVersion: string): Promise<ModelStorageCleanup>;
  recognize(options: RecognitionOptions): Promise<RecognitionResult>;
  transform(options: TransformOptions): Promise<{ pages: ImportedPage[] }>;
  prepare(
    pagePath: string,
    config: Record<string, unknown>,
    pageWidthPoints?: number,
    pageHeightPoints?: number,
    options?: PrepareOptions,
  ): Promise<ImportedPage>;
  exportDocument(request: ExportRequest, options?: ExportOptions): Promise<ExportResult>;
  extractFields(
    text: string,
    labels: string[],
    threshold: number,
  ): Promise<ExtractionField[]>;
  layoutFields(
    pagePath: string,
    labels: string[],
    threshold: number,
  ): Promise<ExtractionField[]>;
  table(
    pagePath: string,
  ): Promise<{ html: string; rows: number; cols: number }>;
  math(pagePath: string): Promise<string | null>;
  writeText(
    outputPath: string,
    text: string,
    sourceUris: string[],
  ): Promise<{ saved_path: string; bytes: number; cancelled?: boolean }>;
  reindex(sourceUri: string, text: string): Promise<unknown>;
  pickDocuments(): Promise<string[]>;
  pickExport(name: string, extension: string): Promise<string | null>;
  imageUrl(path: string): string;
  loadUserProofreading?(): Promise<ProofreadingData>;
  saveUserProofreading?(data: ProofreadingData): Promise<void>;
}

const providerNames = new Map<string, string>();
const mobileExports = new Map<string, string>();
const documentExtensions = new Set(["pdf", "png", "jpg", "jpeg", "tif", "tiff", "bmp", "webp"]);
let nativeImageFormats = new Set<string>();

async function documentCapabilities(): Promise<WorkspaceCapabilities> {
  const capabilities = await invoke<WorkspaceCapabilities>("ocr_workspace_capabilities");
  nativeImageFormats = new Set();
  if (!capabilities.native_document_import) return { ...capabilities, image_import_formats: [] };
  try {
    const native = await invoke<{ pdf: boolean; image_formats: string[] }>("plugin:mobile-provider|document_capabilities");
    nativeImageFormats = new Set((native.image_formats ?? []).filter(format => ["heic", "heif", "avif"].includes(format)));
    return { ...capabilities, pdf_import: native.pdf === true, image_import_formats: [...nativeImageFormats],
      pdf_reason: native.pdf ? undefined : "PDF rendering is unavailable on this device. Import page images instead." };
  } catch {
    return { ...capabilities, pdf_import: false, image_import_formats: [],
      pdf_reason: "The native document reader is unavailable. Restart the app or import JPEG, PNG, TIFF, BMP, or WebP pages." };
  }
}

function importAborted(): DOMException { return new DOMException("Document import cancelled.", "AbortError"); }
function importFailure(error: unknown): unknown {
  const detail = error instanceof Error ? error.message : String(error);
  // ImportJob::check returns this exact marker. Provider errors and filenames
  // may contain the same text and must keep their actionable original cause.
  return detail === "OCR_IMPORT_CANCELLED" ? importAborted() : error;
}

/** Register first so fast native imports cannot outrun progress/cancellation.
 * A cancelled worker is awaited: staging must not be removed while it writes. */
async function trackedImport(options: ImportOptions | undefined, run: (requestId?: string) => Promise<ImportedDocument>): Promise<ImportedDocument> {
  if (!options) return run().catch(error => { throw importFailure(error); });
  if (options.signal?.aborted) throw importAborted();
  const requestId = crypto.randomUUID();
  let active = true;
  let started = false;
  const cancel = () => {
    if (started && active) void invoke("ocr_workspace_cancel_import", { requestId }).catch(() => {});
  };
  const unlisten = await listen<ImportProgress>("ocr-workspace-import", event => {
    if (active && !options.signal?.aborted && event.payload.request_id === requestId) options.onProgress?.(event.payload);
  });
  options.signal?.addEventListener("abort", cancel, { once: true });
  try {
    if (options.signal?.aborted) throw importAborted();
    started = true;
    const result = await run(requestId);
    if (options.signal?.aborted) throw importAborted();
    return result;
  } catch (error) {
    if (options.signal?.aborted) throw importAborted();
    throw importFailure(error);
  } finally {
    active = false;
    options.signal?.removeEventListener("abort", cancel);
    try { unlisten(); } catch { /* Preserve the import result after a closed event bridge. */ }
  }
}

function exportAborted(): DOMException { return new DOMException("Document export cancelled.", "AbortError"); }
function exportFailure(error: unknown): unknown {
  const detail = error instanceof Error ? error.message : String(error);
  // Match the native command's exact cancellation causes. A path or another
  // error's context can legally contain the same marker and must stay intact.
  return [
    "OCR_EXPORT_CANCELLED",
    "Render OCR PDF: OCR_EXPORT_CANCELLED",
    "Write OCR export: OCR_EXPORT_CANCELLED",
    "Flush OCR export: OCR_EXPORT_CANCELLED",
  ].includes(detail) ? exportAborted() : error;
}

/** A stop request can lose to atomic publication. Only the native cancellation
 * marker means the export stopped; a completed result remains a success. */
async function trackedExport(request: ExportRequest, options?: ExportOptions, requestId?: string): Promise<ExportResult> {
  if (!options) return invoke<ExportResult>("ocr_workspace_export", { ...request })
    .catch(error => { throw exportFailure(error); });
  if (options.signal?.aborted) throw exportAborted();
  const id = requestId ?? crypto.randomUUID();
  let active = true;
  let started = false;
  let committing = false;
  let lastBytes = 0;
  const cancel = () => {
    if (!started || !active || committing) return;
    void invoke<boolean>("ocr_workspace_cancel_export", { requestId: id }).then(accepted => {
      if (accepted === false && active && !committing) {
        committing = true;
        options.onProgress?.({ request_id: id, phase: "committing", bytes: lastBytes });
      }
    }).catch(() => { /* The export's eventual result retains its original error. */ });
  };
  const unlisten = await listen<ExportProgress>("ocr-workspace-export", event => {
    const progress = event.payload;
    if (!active || !progress || progress.request_id !== id ||
      !["rendering", "writing", "committing"].includes(progress.phase) ||
      !Number.isSafeInteger(progress.bytes) || progress.bytes < 0 ||
      (committing && progress.phase !== "committing")) return;
    lastBytes = progress.bytes;
    if (progress.phase === "committing") committing = true;
    options.onProgress?.(progress);
  });
  options.signal?.addEventListener("abort", cancel, { once: true });
  try {
    if (options.signal?.aborted) throw exportAborted();
    started = true;
    return await invoke<ExportResult>("ocr_workspace_export", { ...request, requestId: id });
  } catch (error) {
    throw exportFailure(error);
  } finally {
    // Keep the listener and any mobile staging alive until the worker stops.
    active = false;
    options.signal?.removeEventListener("abort", cancel);
    try { unlisten(); } catch { /* Preserve the native export result. */ }
  }
}

/** Raw IPC avoids expanding a camera JPEG into millions of JavaScript numbers.
 * Header metadata is UTF-8 encoded so provider identities stay byte-exact. */
function importPayload(command: string, name: string, bytes: Uint8Array, sourceUri?: string, requestId?: string): Promise<ImportedDocument> {
  if (!bytes.byteLength || bytes.byteLength > 32 * 1024 * 1024) return Promise.reject(new Error("Choose a nonempty document no larger than 32 MiB."));
  const metadata = new TextEncoder().encode(JSON.stringify({ name, ...(sourceUri ? { sourceUri } : {}), ...(requestId ? { requestId } : {}) }));
  if (metadata.byteLength > 12 * 1024) return Promise.reject(new Error("The document name or provider identity is too long."));
  const encoded = btoa(String.fromCharCode(...metadata));
  return invoke(command, bytes, { headers: { "x-ocr-metadata": encoded } });
}

/** The native picker owns destination access; the rendered file stays private
 * until the user chooses where to save it. Completion means the copy finished. */
async function completeMobileExport<T extends { saved_path: string; bytes: number; cancelled?: boolean }>(
  path: string, render: () => Promise<T>, options?: ExportOptions, onDestination?: (bytes: number) => void,
): Promise<T> {
  const name = mobileExports.get(path);
  if (!name) return render();
  try {
    const result = await render();
    if (options?.signal?.aborted) return { ...result, cancelled: true, saved_path: "" };
    onDestination?.(result.bytes);
    // Unmount or another caller can abort during the progress callback, before
    // the OS owns the destination picker and copy operation.
    if (options?.signal?.aborted) return { ...result, cancelled: true, saved_path: "" };
    const saved = await invoke<{ cancelled: boolean; saved_path?: string }>("plugin:mobile-provider|export_file", { path: result.saved_path, name });
    return { ...result, cancelled: saved.cancelled, saved_path: saved.saved_path ?? "" };
  } finally {
    mobileExports.delete(path);
    // This command accepts only this app's bounded export staging directory.
    await invoke("ocr_workspace_discard_export", { path }).catch(() => {});
  }
}

export const workspaceClient: WorkspaceClient = {
  capabilities: documentCapabilities,
  list: () => invoke("ocr_workspace_list"),
  load: (projectId) =>
    invoke("ocr_workspace_load", { projectId: projectId ?? null }),
  save: (project, expectedRevision) =>
    invoke("ocr_workspace_save", { project, expectedRevision }),
  importDocument: (locationUri, options) => trackedImport(options, async requestId => {
    if (isProviderUri(locationUri) && (await loadCapabilities()).mobile) {
      const bytes = await readProviderFile(locationUri);
      const name = providerNames.get(locationUri) ?? "Imported document";
      if (options?.signal?.aborted) throw importAborted();
      const result = await importPayload("ocr_workspace_import_bytes", name, bytes, locationUri, requestId);
      providerNames.delete(locationUri);
      return result;
    }
    return invoke("ocr_workspace_import", { locationUri, ...(requestId ? { requestId } : {}) });
  }),
  importImage: (name, bytes, options) => trackedImport(options, requestId => importPayload("ocr_workspace_import_image", name, bytes, undefined, requestId)),
  assessPhoto: (pagePath) => invoke("ocr_workspace_assess_photo", { pagePath }),
  modelStorage: () => invoke("ocr_workspace_model_storage"),
  cleanModelStorage: () => invoke("ocr_workspace_clean_model_storage"),
  removeMobileModel: (name, expectedVersion) => invoke("ocr_workspace_remove_mobile_model", { name, expectedVersion }),
  installMobileModels: async (language, onProgress) => {
    const requestId = crypto.randomUUID();
    const unlisten = await listen<ModelDownloadProgress>("crispembed-model-download", (event) => {
      if (event.payload.request_id === requestId) onProgress?.(event.payload);
    });
    try {
      return await invoke<string[]>("ocr_workspace_install_mobile_models", { language, requestId });
    } finally {
      try { unlisten(); } catch { /* Preserve the install result if the event bridge has closed. */ }
    }
  },
  recognize: (options) =>
    invoke("ocr_workspace_recognize", {
      ...options,
      config: options.config ?? null,
      engine: options.engine ?? null,
      language: recognitionLanguage(options.engine, options.language),
      zone: options.zone ?? null,
      cpuPixelBudget: options.cpuPixelBudget ?? null,
    }),
  transform: (options) =>
    invoke("ocr_workspace_transform", {
      ...options,
      degrees: options.degrees ?? null,
      crop: options.crop ?? null,
      splitX: options.splitX ?? null,
      quad: options.quad ?? null,
      angle: options.angle ?? null,
    }),
  prepare: async (pagePath, config, pageWidthPoints, pageHeightPoints, options) => {
    let unlisten: (() => void) | undefined;
    if (options?.onStage) {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<{ page: string; stage: string }>("ocr-workspace-prepare-stage", (event) => {
        if (event.payload?.page === pagePath) options.onStage?.(event.payload.stage);
      });
    }
    try {
      return await invoke("ocr_workspace_prepare", {
        pagePath,
        config,
        pageWidthPoints,
        pageHeightPoints,
        allowDownloads: options?.allowDownloads ?? false,
      });
    } finally { unlisten?.(); }
  },
  exportDocument: (request, options) => {
    const requestId = options ? crypto.randomUUID() : undefined;
    return completeMobileExport(request.outputPath, () => trackedExport(request, options, requestId), options, bytes => {
      if (requestId) options?.onProgress?.({ request_id: requestId, phase: "destination", bytes });
    });
  },
  extractFields: (text, labels, threshold) =>
    invoke("ocr_workspace_extract_fields", { text, labels, threshold }),
  layoutFields: (pagePath, labels, threshold) =>
    invoke("tool_kie_extract", {
      locationUri: pagePath,
      labels,
      threshold,
      lilt: true,
      liltModel: null,
    }),
  table: (pagePath) =>
    invoke("tool_table_extract", {
      locationUri: pagePath,
      ocrModel: null,
      save: false,
    }),
  math: (pagePath) =>
    invoke("tool_math_ocr", { locationUri: pagePath, model: null }),
  writeText: (outputPath, text, sourceUris) =>
    completeMobileExport(outputPath, () => invoke<{ saved_path: string; bytes: number }>("ocr_workspace_write_text", {
      outputPath,
      text,
      sourceUris,
      overwrite: false,
    })),
  reindex: (sourceUri, text) =>
    invoke("ocr_workbench_reingest", { locationUri: sourceUri, text }),
  pickDocuments: async () => {
    if ((await loadCapabilities()).mobile) {
      const selection = await invoke<{ cancelled: boolean; files: ProviderFile[] }>("plugin:mobile-provider|pick_files");
      if (selection.cancelled) return [];
      for (const file of selection.files) {
        const extension = file.name.split(".").pop()?.toLowerCase() ?? "";
        if (!isProviderUri(file.uri) || (!documentExtensions.has(extension) && !nativeImageFormats.has(extension))) throw new Error(`Unsupported OCR document: ${file.name}. Choose PDF, JPEG, PNG, TIFF, BMP, WebP${nativeImageFormats.size ? `, or ${[...nativeImageFormats].map(format => format.toUpperCase()).join("/")}` : ""}.`);
        if (file.size > PROVIDER_MAX_BYTES) throw new Error(`${file.name} exceeds the 16 MiB mobile document-provider limit. Select a smaller document or import page photos.`);
      }
      for (const file of selection.files) providerNames.set(file.uri, file.name);
      return selection.files.map(file => file.uri);
    }
    const selected = await open({
      multiple: true,
      title: "Add documents to OCR project",
      filters: [
        {
          name: "Documents and scans",
          extensions: [
            "pdf",
            "png",
            "jpg",
            "jpeg",
            "tif",
            "tiff",
            "bmp",
            "webp",
          ],
        },
      ],
    });
    return selected ? (Array.isArray(selected) ? selected : [selected]) : [];
  },
  pickExport: async (name, extension) => {
    if ((await loadCapabilities()).mobile) {
      if (!/^[a-z0-9]{1,12}$/i.test(extension)) throw new Error("Choose a supported export format.");
      const cleaned = name.replace(/[\\/\p{Cc}]/gu, "_");
      let base = "";
      const utf8 = new TextEncoder();
      for (const character of cleaned) {
        if (utf8.encode(base + character).byteLength > 200) break;
        base += character;
      }
      const filename = `${base.trim() || "document"}.${extension}`;
      const path = await invoke<string>("ocr_workspace_export_target", { name: filename });
      mobileExports.set(path, filename);
      return path;
    }
    return save({
      title: "Export reviewed document",
      defaultPath: `${name}.${extension}`,
      filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
    });
  },
  imageUrl: convertFileSrc,
  loadUserProofreading: async () => readProofreadingData(await getSetting("ocrProofreadingV1")),
  saveUserProofreading: async (data) => saveSetting("ocrProofreadingV1", readProofreadingData(data)),
};
