import type { ExtractionField } from "./client";
import type { OcrPage, OcrRegion } from "./model";
import type { PixelRectangle } from "./zones";

export type ExtractionKind = "fields" | "table" | "formula";
export interface ExtractionSource {
  imagePath: string;
  regionId: string | null;
  bounds: PixelRectangle | null;
  method: "text" | "pixels";
  text: string | null;
}
export interface ExtractionData {
  fields: ExtractionField[];
  tableRows: string[][];
  formula: string;
  labels: string;
  sourceImagePath: string;
  sources: Partial<Record<ExtractionKind, ExtractionSource>>;
  updatedAt: string;
}

/**
 * Keep spreadsheet formulas literal at the CSV boundary. Whitespace and control
 * characters may be ignored by spreadsheet readers, so inspect past them without
 * removing them from the exported value. Plain decimal numbers remain numeric.
 */
export function tableToCsv(rows: readonly (readonly string[])[]): string {
  const leadingBoundaryCharacters = /^[\s\p{Cc}\p{Cf}]+/u;
  const decimalNumber =
    /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?[\s\p{Cc}\p{Cf}]*$/u;
  return rows
    .map((row) =>
      row
        .map((value) => {
          const inspected = value.replace(leadingBoundaryCharacters, "");
          const literal =
            /^[=+\-@]/.test(inspected) && !decimalNumber.test(inspected)
              ? `'${value}`
              : value;
          return `"${literal.replace(/"/g, '""')}"`;
        })
        .join(","),
    )
    .join("\r\n");
}

const pageText = (page: OcrPage) =>
  page.regions
    .filter((r) => !r.excluded && r.kind !== "picture")
    .map((r) => r.text)
    .join("\n");
export function captureExtractionSource(
  page: OcrPage,
  region: OcrRegion | undefined,
  method: "text" | "pixels",
): ExtractionSource {
  return {
    imagePath: page.imagePath,
    regionId: region?.id ?? null,
    bounds: region
      ? { x: region.x, y: region.y, w: region.w, h: region.h }
      : null,
    method,
    text: method === "text" ? (region?.text ?? pageText(page)) : null,
  };
}
export function extractionSourceIsCurrent(
  page: OcrPage,
  source: ExtractionSource,
): boolean {
  if (source.imagePath !== page.imagePath) return false;
  const region = source.regionId
    ? page.regions.find((r) => r.id === source.regionId)
    : undefined;
  if (source.regionId && !region) return false;
  if (source.method === "text")
    return source.text === (region?.text ?? pageText(page));
  return (
    !region ||
    (!!source.bounds &&
      ["x", "y", "w", "h"].every(
        (key) =>
          region[key as keyof PixelRectangle] ===
          source.bounds![key as keyof PixelRectangle],
      ))
  );
}

/** Untrusted project metadata is validated before it reaches editable controls. */
export function readExtraction(page: OcrPage): ExtractionData {
  const empty: ExtractionData = {
    fields: [],
    tableRows: [],
    formula: "",
    labels: "",
    sourceImagePath: page.imagePath,
    sources: {},
    updatedAt: "",
  };
  const raw = page.provenance.extraction;
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return empty;
  const data = raw as Partial<ExtractionData>;
  if (data.sourceImagePath !== page.imagePath) return empty;
  const sources: ExtractionData["sources"] = {};
  for (const kind of ["fields", "table", "formula"] as const) {
    const source = data.sources?.[kind];
    if (
      source &&
      typeof source === "object" &&
      (source.method === "text" || source.method === "pixels") &&
      extractionSourceIsCurrent(page, source)
    )
      sources[kind] = source;
  }
  return JSON.parse(
    JSON.stringify({
      ...empty,
      sources,
      fields:
        sources.fields && Array.isArray(data.fields)
          ? data.fields.filter(
              (f) =>
                f &&
                typeof f.label === "string" &&
                typeof f.value === "string" &&
                Number.isFinite(f.score),
            )
          : [],
      tableRows:
        sources.table && Array.isArray(data.tableRows)
          ? data.tableRows.filter(
              (row) =>
                Array.isArray(row) &&
                row.every((cell) => typeof cell === "string"),
            )
          : [],
      formula:
        sources.formula && typeof data.formula === "string" ? data.formula : "",
      labels: typeof data.labels === "string" ? data.labels : "",
      updatedAt: typeof data.updatedAt === "string" ? data.updatedAt : "",
    }),
  ) as ExtractionData;
}
