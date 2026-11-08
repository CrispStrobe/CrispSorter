import { describe, expect, it } from "vitest";
import { addDocument, applyRecognition, createProject } from "./model";
import {
  captureExtractionSource,
  extractionSourceIsCurrent,
  readExtraction,
} from "./extraction";

function sample() {
  let p = addDocument(createProject(), {
    source_uri: "/source",
    name: "Invoice",
    pages: [{ image_path: "/page", width: 100, height: 100 }],
  });
  p = applyRecognition(p, p.pages[0].id, {
    width: 100,
    height: 100,
    regions: [{ text: "Total 125", x: 0, y: 0, w: 80, h: 10 }],
  });
  return p.pages[0];
}
describe("extraction source evidence", () => {
  it("invalidates text-based fields after a correction and restores them on undo", () => {
    const page = sample(),
      source = captureExtractionSource(page, undefined, "text");
    page.provenance.extraction = {
      sourceImagePath: page.imagePath,
      sources: { fields: source },
      fields: [{ label: "total", value: "125", score: 0.9 }],
    };
    expect(readExtraction(page).fields).toHaveLength(1);
    const corrected = {
      ...page,
      regions: [{ ...page.regions[0], text: "Total 175" }],
    };
    expect(readExtraction(corrected).fields).toEqual([]);
    expect(readExtraction(page).fields[0].value).toBe("125");
  });
  it("rejects an extraction from changed pixels or changed crop geometry", () => {
    const page = sample(),
      source = captureExtractionSource(page, page.regions[0], "pixels");
    expect(
      extractionSourceIsCurrent({ ...page, imagePath: "/new" }, source),
    ).toBe(false);
    expect(
      extractionSourceIsCurrent(
        { ...page, regions: [{ ...page.regions[0], x: 1 }] },
        source,
      ),
    ).toBe(false);
  });
  it("isolates malformed persisted cells and field values", () => {
    const page = sample(),
      source = captureExtractionSource(page, undefined, "pixels");
    page.provenance.extraction = {
      sourceImagePath: page.imagePath,
      sources: { table: source, fields: source },
      tableRows: [["a", "b"], 42, ["c", {}]],
      fields: [null, { label: 7 }],
      formula: {},
    };
    expect(readExtraction(page).tableRows).toEqual([["a", "b"]]);
    expect(readExtraction(page).fields).toEqual([]);
  });
});
