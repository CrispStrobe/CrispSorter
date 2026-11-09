import { invoke, convertFileSrc } from "@tauri-apps/api/core";
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

export interface WorkspaceCapabilities {
  ocr: boolean;
  crispembed: boolean;
  pdf_import: boolean;
  formats: string[];
  engines: string[];
  layout: boolean;
  kie: boolean;
  table: boolean;
  math: boolean;
  photo_tools?: boolean;
  camera_import?: boolean;
  reason?: string;
  pdf_export_reason?: string;
  reasons?: Record<string, string>;
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
}
export interface ExportRequest {
  outputPath: string;
  format: string;
  pdfa: boolean;
  pages: ExportPage[];
  overwrite: boolean;
  sourceUris: string[];
}

/** Explicit adapter makes the complete review workflow testable without model downloads. */
export interface WorkspaceClient {
  capabilities(): Promise<WorkspaceCapabilities>;
  list(): Promise<ProjectSummary[]>;
  load(projectId?: string): Promise<ProjectEnvelope | null>;
  save(project: OcrProject, expectedRevision: number): Promise<ProjectEnvelope>;
  importDocument(locationUri: string): Promise<ImportedDocument>;
  importImage?(name: string, bytes: Uint8Array): Promise<ImportedDocument>;
  assessPhoto?(pagePath: string): Promise<PhotoAssessment>;
  recognize(options: RecognitionOptions): Promise<RecognitionResult>;
  transform(options: TransformOptions): Promise<{ pages: ImportedPage[] }>;
  prepare(
    pagePath: string,
    config: Record<string, unknown>,
    pageWidthPoints?: number,
    pageHeightPoints?: number,
  ): Promise<ImportedPage>;
  exportDocument(request: ExportRequest): Promise<ExportResult>;
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
}

const providerNames = new Map<string, string>();
const mobileExports = new Map<string, string>();
const documentExtensions = new Set(["pdf", "png", "jpg", "jpeg", "tif", "tiff", "bmp", "webp"]);

/** Raw IPC avoids expanding a camera JPEG into millions of JavaScript numbers.
 * Header metadata is UTF-8 encoded so provider identities stay byte-exact. */
function importPayload(command: string, name: string, bytes: Uint8Array, sourceUri?: string): Promise<ImportedDocument> {
  if (!bytes.byteLength || bytes.byteLength > 32 * 1024 * 1024) return Promise.reject(new Error("Choose a nonempty document no larger than 32 MiB."));
  const metadata = new TextEncoder().encode(JSON.stringify({ name, ...(sourceUri ? { sourceUri } : {}) }));
  if (metadata.byteLength > 12 * 1024) return Promise.reject(new Error("The document name or provider identity is too long."));
  const encoded = btoa(String.fromCharCode(...metadata));
  return invoke(command, bytes, { headers: { "x-ocr-metadata": encoded } });
}

/** The native picker owns destination access; the rendered file stays private
 * until the user chooses where to save it. Completion means the copy finished. */
async function completeMobileExport<T extends { saved_path: string; bytes: number; cancelled?: boolean }>(path: string, render: () => Promise<T>): Promise<T> {
  const name = mobileExports.get(path);
  if (!name) return render();
  try {
    const result = await render();
    const saved = await invoke<{ cancelled: boolean; saved_path?: string }>("plugin:mobile-provider|export_file", { path: result.saved_path, name });
    return { ...result, cancelled: saved.cancelled, saved_path: saved.saved_path ?? "" };
  } finally {
    mobileExports.delete(path);
    // This command accepts only this app's bounded export staging directory.
    await invoke("ocr_workspace_discard_export", { path }).catch(() => {});
  }
}

export const workspaceClient: WorkspaceClient = {
  capabilities: () => invoke("ocr_workspace_capabilities"),
  list: () => invoke("ocr_workspace_list"),
  load: (projectId) =>
    invoke("ocr_workspace_load", { projectId: projectId ?? null }),
  save: (project, expectedRevision) =>
    invoke("ocr_workspace_save", { project, expectedRevision }),
  importDocument: async (locationUri) => {
    if (isProviderUri(locationUri) && (await loadCapabilities()).mobile) {
      const bytes = await readProviderFile(locationUri);
      const name = providerNames.get(locationUri) ?? "Imported document";
      const result = await importPayload("ocr_workspace_import_bytes", name, bytes, locationUri);
      providerNames.delete(locationUri);
      return result;
    }
    return invoke("ocr_workspace_import", { locationUri });
  },
  importImage: (name, bytes) => importPayload("ocr_workspace_import_image", name, bytes),
  assessPhoto: (pagePath) => invoke("ocr_workspace_assess_photo", { pagePath }),
  recognize: (options) =>
    invoke("ocr_workspace_recognize", {
      ...options,
      config: options.config ?? null,
      engine: options.engine ?? null,
      language: options.language ?? null,
      zone: options.zone ?? null,
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
  prepare: (pagePath, config, pageWidthPoints, pageHeightPoints) =>
    invoke("ocr_workspace_prepare", {
      pagePath,
      config,
      pageWidthPoints,
      pageHeightPoints,
    }),
  exportDocument: (request) => completeMobileExport(request.outputPath, () => invoke<ExportResult>("ocr_workspace_export", { ...request })),
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
        if (!isProviderUri(file.uri) || !documentExtensions.has(extension)) throw new Error(`Unsupported OCR document: ${file.name}. Choose PDF, JPEG, PNG, TIFF, BMP or WebP.`);
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
      const filename = `${name.replace(/[\\/\u0000-\u001f]/g, "_").slice(0, 180) || "document"}.${extension}`;
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
};
