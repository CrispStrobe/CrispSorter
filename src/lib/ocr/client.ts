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
  reason?: string;
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
  config?: Record<string, unknown>;
  engine?: string;
  language?: string;
  zone?: { x: number; y: number; w: number; h: number };
}
export interface TransformOptions {
  pagePath: string;
  operation: "rotate" | "crop" | "split";
  degrees?: number;
  crop?: { x: number; y: number; w: number; h: number };
  splitX?: number;
}
export interface ExtractionField {
  label: string;
  value: string;
  score: number;
}
export interface ExportResult {
  saved_path: string;
  bytes: number;
  pages: number;
  format: string;
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
  recognize(options: RecognitionOptions): Promise<RecognitionResult>;
  transform(options: TransformOptions): Promise<{ pages: ImportedPage[] }>;
  prepare(
    pagePath: string,
    config: Record<string, unknown>,
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
  ): Promise<{ saved_path: string; bytes: number }>;
  reindex(sourceUri: string, text: string): Promise<unknown>;
  pickDocuments(): Promise<string[]>;
  pickExport(name: string, extension: string): Promise<string | null>;
  imageUrl(path: string): string;
}

export const workspaceClient: WorkspaceClient = {
  capabilities: () => invoke("ocr_workspace_capabilities"),
  list: () => invoke("ocr_workspace_list"),
  load: (projectId) =>
    invoke("ocr_workspace_load", { projectId: projectId ?? null }),
  save: (project, expectedRevision) =>
    invoke("ocr_workspace_save", { project, expectedRevision }),
  importDocument: (locationUri) =>
    invoke("ocr_workspace_import", { locationUri }),
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
    }),
  prepare: (pagePath, config) =>
    invoke("ocr_workspace_prepare", { pagePath, config }),
  exportDocument: (request) => invoke("ocr_workspace_export", { ...request }),
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
    invoke("ocr_workspace_write_text", {
      outputPath,
      text,
      sourceUris,
      overwrite: false,
    }),
  reindex: (sourceUri, text) =>
    invoke("ocr_workbench_reingest", { locationUri: sourceUri, text }),
  pickDocuments: async () => {
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
  pickExport: (name, extension) =>
    save({
      title: "Export reviewed document",
      defaultPath: `${name}.${extension}`,
      filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
    }),
  imageUrl: convertFileSrc,
};
