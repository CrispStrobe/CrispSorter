import type { WorkspaceCapabilities } from "./client";

/** Keep the public format choice separate from the native PDF/A request flag. */
export function resolveExportFormat(
  selection: string,
  capabilities: Pick<WorkspaceCapabilities, "formats" | "archival_pdf_export" | "pdfa_export_reason">,
): { format: string; pdfa: boolean } {
  if (selection === "pdfa") {
    if (!capabilities.archival_pdf_export || !capabilities.formats.includes("pdfa")) {
      throw new Error(capabilities.pdfa_export_reason ?? "PDF/A-2b export requires the matching archival renderer.");
    }
    return { format: "pdf", pdfa: true };
  }
  if (!capabilities.formats.includes(selection)) throw new Error("This output format is unavailable in this build.");
  return { format: selection, pdfa: false };
}
