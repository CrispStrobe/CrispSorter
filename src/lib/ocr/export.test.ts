import { describe, expect, it } from "vitest";
import { resolveExportFormat } from "./export";

describe("reviewed archival export selection", () => {
  it("passes PDF/A as an explicit native flag while retaining a PDF filename extension", () => {
    expect(resolveExportFormat("pdfa", { formats: ["text", "pdf", "pdfa"], archival_pdf_export: true }))
      .toEqual({ format: "pdf", pdfa: true });
    expect(resolveExportFormat("pdf", { formats: ["text", "pdf", "pdfa"], archival_pdf_export: true }))
      .toEqual({ format: "pdf", pdfa: false });
  });

  it("rejects absent, stale or inconsistent native capabilities before opening a destination", () => {
    const reason = "PDF/A-2b requires an archival-capable native build.";
    for (const capabilities of [
      { formats: ["text", "pdf"], pdfa_export_reason: reason },
      { formats: ["text", "pdf", "pdfa"], archival_pdf_export: false, pdfa_export_reason: reason },
      { formats: ["text", "pdf"], archival_pdf_export: true, pdfa_export_reason: reason },
    ]) expect(() => resolveExportFormat("pdfa", capabilities)).toThrow(reason);
  });

  it("never labels non-PDF exports as archival", () => {
    for (const format of ["text", "hocr", "alto", "json"]) {
      expect(resolveExportFormat(format, { formats: [format] })).toEqual({ format, pdfa: false });
    }
    expect(() => resolveExportFormat("pdf", { formats: ["text"] })).toThrow(/unavailable/);
  });
});
