import { describe, expect, it } from "vitest";
import { checkExportText, needsBidirectionalPdf, PDF_BIDI_UNAVAILABLE, resolveExportFormat } from "./export";
import { addDocument, applyRecognition, createProject, prepareExport, updatePage, updateRegion } from "./model";

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

  it("preserves ordinary PDF when an older native renderer lacks archival support", () => {
    expect(resolveExportFormat("pdf", { formats: ["text", "pdf"], archival_pdf_export: false }))
      .toEqual({ format: "pdf", pdfa: false });
  });
});

describe("reviewed text PDF availability", () => {
  const capabilities = { formats: ["pdf", "pdfa", "text", "html", "docx"], pdf_bidi_export: false };
  const page = (text: string) => ({ name: "Reviewed page", regions: [{ text }] });

  it("checks RTL scripts, supplementary scripts and directional formatting without rejecting ordinary Unicode", () => {
    for (const text of ["العربية 123 ABC", "שָׁלוֹם", "𞤀𞤢 123 ABC", "Report \u2067العربية\u2069", "\u200eLatin"]) {
      expect(needsBidirectionalPdf(text)).toBe(true);
    }
    for (const text of ["Invoice café é 中文 Ελληνικά 😀", "123 / 42", ""]) {
      expect(needsBidirectionalPdf(text)).toBe(false);
    }
  });

  it("blocks both PDF choices before a destination, using only offered alternatives", () => {
    for (const format of ["pdf", "pdfa"]) {
      const result = checkExportText(format, capabilities, [page("العربية 123 ABC")]);
      expect(result.errors).toEqual([PDF_BIDI_UNAVAILABLE]);
      expect(result.affectedPages).toEqual(["Reviewed page"]);
      expect(result.alternatives).toEqual(["text", "html", "docx"]);
    }
    const mobile = checkExportText("pdf", { formats: ["pdf", "text", "html"] }, [page("שלום")]);
    expect(mobile.alternatives).toEqual(["text", "html"]);
    expect(mobile.errors).toHaveLength(1);
  });

  it("lets selected text formats preserve logical text and leaves other export prerequisites intact", () => {
    const pages = [page("مَرْحَبًا שָׁלוֹם 𞤀𞤢 / 123 é")];
    const before = structuredClone(pages);
    for (const format of ["text", "html", "docx", "json", "hocr", "alto"]) {
      expect(checkExportText(format, capabilities, pages)).toEqual({ errors: [], warnings: [], alternatives: [], affectedPages: [] });
    }
    expect(pages).toEqual(before);
    expect(checkExportText("pdf", { ...capabilities, pdf_bidi_export: true }, [page("שלום ABC")]).errors).toEqual([]);
  });

  it("only examines included regions and the chosen page scope", () => {
    let project = addDocument(createProject("Mixed languages"), { source_uri: "/original.pdf", name: "Original", pages: [
      { image_path: "/one.png", width: 100, height: 100 }, { image_path: "/two.png", width: 100, height: 100 },
    ] });
    for (const [index, text] of ["Invoice café", "العربية"].entries()) {
      project = applyRecognition(project, project.pages[index].id, { width: 100, height: 100, regions: [{ text, x: 1, y: 1, w: 80, h: 10, confidence: 0.9 }] });
    }
    const all = prepareExport(project);
    expect(checkExportText("pdf", capabilities, all.pages).errors).toHaveLength(1);
    const one = prepareExport(project, { pageIds: [project.pages[0].id] });
    expect(checkExportText("pdf", capabilities, one.pages).errors).toEqual([]);
    const second = project.pages[1];
    project = updatePage(project, second.id, { excluded: true });
    expect(checkExportText("pdf", capabilities, prepareExport(project).pages).errors).toEqual([]);
    project = updatePage(project, second.id, { excluded: false });
    project = updateRegion(project, second.id, second.regions[0].id, { excluded: true });
    expect(checkExportText("pdf", capabilities, prepareExport(project).pages).errors).toEqual([]);
  });

  it("keeps supplementary copy available with an explicit search advisory", () => {
    const result = checkExportText("pdfa", capabilities, [page("Reviewed café 中文 😀 é")]);
    expect(result.errors).toEqual([]);
    expect(result.warnings).toHaveLength(1);
    expect(result.warnings[0]).toContain("text search");
    expect(result.warnings[0]).toContain("characters remain in the PDF");
    expect(checkExportText("text", capabilities, [page("😀")]).warnings).toEqual([]);
  });

  it("checks Word input bounds before offering a destination without restricting text alternatives", () => {
    const pages = [page("\t".repeat(100_000))];
    expect(checkExportText("docx", capabilities, pages).errors[0]).toContain("100,000");
    expect(checkExportText("docx", capabilities, pages).alternatives).toEqual(["text", "html"]);
    expect(checkExportText("text", capabilities, pages).errors).toEqual([]);
    expect(checkExportText("docx", capabilities, [page("  العربية\n\nשלום\t  ")]).errors).toEqual([]);
    const unavailable = { formats: ["text", "docx"], docx_export: false, docx_export_reason: "Word requires the OCR text export feature." };
    expect(() => resolveExportFormat("docx", unavailable)).toThrow(unavailable.docx_export_reason);
  });

  it("blocks XML-invalid characters before a destination while retaining exact text and JSON alternatives", () => {
    for (const char of ["\0", "\f", "\ufffe", "\uffff"]) {
      const pages = [page(`Reviewed ${char} العربية`)];
      const available = { ...capabilities, formats: [...capabilities.formats, "json", "hocr", "alto"] };
      for (const format of ["docx", "html", "pdf", "pdfa", "hocr", "alto"]) {
        const check = checkExportText(format, available, pages);
        expect(check.errors[0]).toContain("cannot represent");
        expect(check.alternatives).toEqual(["text", "json"]);
      }
      for (const format of ["text", "json"]) expect(checkExportText(format, available, pages).errors).toEqual([]);
      expect(pages[0].regions[0].text).toBe(`Reviewed ${char} العربية`);
    }
    expect(checkExportText("text", capabilities, [page("\ud800")]).errors[0]).toContain("incomplete Unicode");
    expect(checkExportText("text", capabilities, [page("𞤀😀")]).errors).toEqual([]);
  });
});
