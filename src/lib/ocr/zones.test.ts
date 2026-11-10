import { describe, expect, it } from "vitest";
import { addDocument, applyRecognition, createProject } from "./model";
import {
  effectiveRecognitionZone,
  expandRecognitionZone,
  hasRecognizedText,
  mergeZoneRegions,
} from "./zones";

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
  it("rejects malformed native effective crops before altering existing text", () => {
    const page = sample(),
      zone = { x: 0, y: 0, w: 1000, h: 1000 };
    expect(effectiveRecognitionZone(page, zone, undefined)).toEqual(zone);
    expect(effectiveRecognitionZone(page, zone, zone)).toEqual(zone);
    for (const invalid of [
      null,
      {},
      { ...zone, w: 1001 },
      { ...zone, x: NaN },
      { ...zone, h: -1 },
      { ...zone, y: 0.5 },
    ]) {
      expect(() => effectiveRecognitionZone(page, zone, invalid)).toThrow(
        "existing text was preserved",
      );
    }
  });
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
    const replacement = {
      ...page.regions[1],
      id: "partial-paragraph",
      text: "Recognized fragment",
      w: 200,
    };
    const merged = mergeZoneRegions(
      page,
      { x: 100, y: 150, w: 200, h: 100 },
      [replacement],
    );
    expect(merged.filter((region) => region !== replacement)).toEqual(page.regions);
    expect(merged).toContain(replacement);
  });
  it("preserves corrected and approved regions when a full-zone retry finds no text", () => {
    const page = sample();
    page.approved = true;
    page.regions[0].text = "Proofread heading";
    page.regions[0].review = "approved";
    const before = structuredClone(page);
    const zone = { x: 0, y: 0, w: page.width, h: page.height };

    expect(hasRecognizedText([])).toBe(false);
    expect(mergeZoneRegions(page, zone, [])).toBe(page.regions);
    // Some engines return a positioned empty/whitespace region instead of [].
    const blank = [{ ...page.regions[0], id: "empty-retry", text: " \n\t " }];
    expect(hasRecognizedText(blank)).toBe(false);
    expect(mergeZoneRegions(page, zone, blank)).toBe(page.regions);
    expect(page).toEqual(before);
  });
  it("retains excluded and picture regions even inside a recognized zone", () => {
    const page = sample();
    page.regions[0].excluded = true;
    page.regions[1].kind = "picture";
    const replacement = {
      ...page.regions[2],
      id: "new-paragraph",
      text: "Recognized paragraph",
    };
    expect(
      mergeZoneRegions(page, { x: 0, y: 0, w: 1000, h: 1000 }, [replacement]).map(
        (r) => r.id,
      ),
    ).toEqual([...page.regions.slice(0, 2).map((r) => r.id), replacement.id]);
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
  it("replaces page-edge boxes within the accepted floating point tolerance", () => {
    const page = sample();
    const edge = { ...page.regions[0], x: 900, y: 0, w: 100.005, h: 100 };
    page.regions = [edge];
    const replacement = { ...edge, id: "edge-replacement", w: 100 };
    expect(
      mergeZoneRegions(page, { x: 0, y: 0, w: 1000, h: 1000 }, [replacement]),
    ).toEqual([replacement]);
  });
  it("continues expansion to earlier regions after reaching a clipped page edge", () => {
    const page = sample();
    page.regions = [
      { ...page.regions[0], x: 0, y: 50, w: 100, h: 100 },
      { ...page.regions[1], x: 50, y: 0, w: 200, h: 100 },
      { ...page.regions[2], x: 900, y: 0, w: 100.005, h: 100 },
    ];
    expect(
      expandRecognitionZone(page, { x: 100, y: 0, w: 900, h: 10 }),
    ).toEqual({ x: 0, y: 0, w: 1000, h: 150 });
  });
});
