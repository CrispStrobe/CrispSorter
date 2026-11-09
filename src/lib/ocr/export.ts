import type { WorkspaceCapabilities } from "./client";

export const PDF_BIDI_UNAVAILABLE =
  "PDF export for right-to-left text or directional formatting is unavailable in this build. Choose a text format to preserve the reviewed reading order.";

const MAX_DOCX_TEXT_BYTES = 16 * 1024 * 1024;
const MAX_DOCX_STRUCTURAL_ITEMS = 100_000;

/** Match the qualified native renderer's conservative script/control guard.
 * This is availability preflight, not a Unicode layout implementation.
 * CrispEmbed 9c44a69: src/ocr_render.cpp, needs_bidirectional_layout().
 */
export function needsBidirectionalPdf(text: string): boolean {
  return /[\u0590-\u08ff\ufb1d-\ufdff\ufe70-\ufeff\u{10800}-\u{10fff}\u{1e800}-\u{1efff}\u200e\u200f\u202a-\u202e\u2066-\u2069]/u.test(text);
}

export function checkExportText(
  selection: string,
  capabilities: Pick<WorkspaceCapabilities, "formats" | "pdf_bidi_export" | "pdf_bidi_export_reason" | "docx_export" | "docx_export_reason">,
  pages: readonly { name?: string; regions: readonly { text: string }[] }[],
): { errors: string[]; warnings: string[]; alternatives: string[]; affectedPages: string[] } {
  const result = { errors: [] as string[], warnings: [] as string[], alternatives: [] as string[], affectedPages: [] as string[] };
  if (selection === "docx") {
    if (capabilities.docx_export === false || !capabilities.formats.includes("docx")) {
      result.errors.push(capabilities.docx_export_reason ?? "Word export is unavailable in this build. Choose Plain text or HTML document.");
      result.alternatives = ["text", "html"].filter(format => capabilities.formats.includes(format));
      return result;
    }
    let bytes = 0, items = 0;
    count: for (const page of pages) {
      items += Math.max(page.regions.length, 1);
      for (const region of page.regions) {
        for (const char of region.text) {
          const cp = char.codePointAt(0)!;
          bytes += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
          if (char === "\n" || char === "\r" || char === "\t") items++;
          if (bytes > MAX_DOCX_TEXT_BYTES || items > MAX_DOCX_STRUCTURAL_ITEMS) break count;
        }
      }
    }
    if (bytes > MAX_DOCX_TEXT_BYTES || items > MAX_DOCX_STRUCTURAL_ITEMS) {
      result.errors.push("Word export supports at most 16 MiB of reviewed text and 100,000 paragraphs, tabs, and line breaks per export. Select fewer pages or choose Plain text or HTML document.");
      result.alternatives = ["text", "html"].filter(format => capabilities.formats.includes(format));
    }
    return result;
  }
  if (selection !== "pdf" && selection !== "pdfa") return result;
  if (capabilities.pdf_bidi_export !== true) {
    result.affectedPages = pages.flatMap((page, index) =>
      page.regions.some(region => needsBidirectionalPdf(region.text)) ? [page.name ?? `Page ${index + 1}`] : []);
    if (result.affectedPages.length) {
      result.errors.push(capabilities.pdf_bidi_export_reason ?? PDF_BIDI_UNAVAILABLE);
      result.alternatives = ["text", "html", "docx"].filter(format => capabilities.formats.includes(format));
      return result;
    }
  }
  if (pages.some(page => page.regions.some(region => /[\u{10000}-\u{10ffff}]/u.test(region.text)))) {
    result.warnings.push("Some PDF readers cannot find supplementary characters, such as emoji, with text search. The characters remain in the PDF. Choose Plain text or HTML document when exact text search is required.");
  }
  return result;
}

/** Keep the public format choice separate from the native PDF/A request flag. */
export function resolveExportFormat(
  selection: string,
  capabilities: Pick<WorkspaceCapabilities, "formats" | "archival_pdf_export" | "pdfa_export_reason" | "docx_export" | "docx_export_reason">,
): { format: string; pdfa: boolean } {
  if (selection === "pdfa") {
    if (!capabilities.archival_pdf_export || !capabilities.formats.includes("pdfa")) {
      throw new Error(capabilities.pdfa_export_reason ?? "PDF/A-2b export requires the matching archival renderer.");
    }
    return { format: "pdf", pdfa: true };
  }
  if (selection === "docx" && capabilities.docx_export === false) {
    throw new Error(capabilities.docx_export_reason ?? "Word export is unavailable in this build. Choose Plain text or HTML document.");
  }
  if (!capabilities.formats.includes(selection)) throw new Error("This output format is unavailable in this build.");
  return { format: selection, pdfa: false };
}
