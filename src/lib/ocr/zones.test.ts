import { describe, expect, it } from "vitest";
import { addDocument, applyRecognition, createProject } from "./model";
import { expandRecognitionZone, mergeZoneRegions } from "./zones";

function sample() {
  let project = addDocument(createProject(), {
    source_uri: "/original.png",
    name: "Original",
    pages: [{ image_path: "/managed.png", width: 1000, height: 1000 }],
  });
  project = applyRecognition(project, project.pages[0].id, {
    width: 1000,
    height: 1000,
    regions: [
      { text: "Heading", x: 100, y: 50, w: 300, h: 40 },
      { text: "Paragraph one", x: 100, y: 150, w: 400, h: 100 },
      { text: "Paragraph two", x: 100, y: 300, w: 400, h: 100 },
    ],
  });
  return project.pages[0];
}

describe("safe zone recognition", () => {
  it("expands a partial paragraph selection to preserve the complete text", () => {
    const page = sample();
    expect(
      expandRecognitionZone(page, { x: 110.4, y: 180.2, w: 20.2, h: 20.1 }),
    ).toEqual({ x: 100, y: 150, w: 400, h: 100 });
  });
  it("keeps the original reading-order position of a replaced heading", () => {
    const page = sample(),
      replacement = {
        ...page.regions[0],
        id: "new-heading",
        text: "Correct heading",
      };
    const merged = mergeZoneRegions(
      page,
      expandRecognitionZone(page, { x: 100, y: 50, w: 300, h: 40 }),
      [replacement],
    );
    expect(merged.map((r) => r.text)).toEqual([
      "Correct heading",
      "Paragraph one",
      "Paragraph two",
    ]);
    expect(merged[1]).toBe(page.regions[1]);
  });
  it("leaves partially intersected regions intact when the native effective crop is narrower", () => {
    const page = sample();
    expect(
      mergeZoneRegions(page, { x: 100, y: 150, w: 200, h: 100 }, []),
    ).toEqual(page.regions);
  });
  it("retains excluded and picture regions even inside a recognized zone", () => {
    const page = sample();
    page.regions[0].excluded = true;
    page.regions[1].kind = "picture";
    expect(
      mergeZoneRegions(page, { x: 0, y: 0, w: 1000, h: 1000 }, []).map(
        (r) => r.id,
      ),
    ).toEqual(page.regions.slice(0, 2).map((r) => r.id));
  });
  it("rejects invalid zones and rounds a blank zone to whole native pixels", () => {
    const page = sample();
    expect(
      expandRecognitionZone(page, { x: 600.6, y: 20.4, w: 10.8, h: 10.9 }),
    ).toEqual({ x: 600, y: 20, w: 12, h: 12 });
    expect(() =>
      expandRecognitionZone(page, { x: NaN, y: 0, w: 1, h: 1 }),
    ).toThrow();
  });
});
