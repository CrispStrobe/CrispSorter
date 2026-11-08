import { describe, expect, it } from "vitest";
import { addDocument, applyRecognition, createProject } from "./model";
import {
  captureExtractionSource,
  extractionSourceIsCurrent,
  readExtraction,
  tableToCsv,
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

describe("spreadsheet-safe table CSV", () => {
  it("makes formula-leading cells literal without changing source values", () => {
    const rows = Object.freeze([
      Object.freeze([
        '=HYPERLINK("https://example.invalid","Open")',
        "+SUM(A1:A2)",
        "-1+2",
        "@SUM(A1:A2)",
        "=123",
        "'=already literal",
      ]),
    ]);
    expect(tableToCsv(rows)).toBe(
      '"\'=HYPERLINK(""https://example.invalid"",""Open"")","\'+SUM(A1:A2)","\'-1+2","\'@SUM(A1:A2)","\'=123","\'=already literal"',
    );
    expect(rows[0][0]).toBe('=HYPERLINK("https://example.invalid","Open")');
    expect(rows[0][2]).toBe("-1+2");
  });

  it("detects formulas after leading whitespace, controls, and format characters", () => {
    expect(
      tableToCsv([
        [
          " \t=1+2",
          "\r\n+SUM(A1:A2)",
          "\u0000-2+3",
          "\u00a0\u200b@SUM(A1:A2)",
        ],
        ["\ufeff=1", "\u2066=2", " \tordinary text"],
      ]),
    ).toBe(
      '"\' \t=1+2","\'\r\n+SUM(A1:A2)","\'\u0000-2+3","\'\u00a0\u200b@SUM(A1:A2)"\r\n' +
        '"\'\ufeff=1","\'\u2066=2"," \tordinary text"',
    );
  });

  it("preserves signed decimal and exponent numbers, including surrounding whitespace", () => {
    expect(
      tableToCsv([
        [
          "-42",
          "+42",
          "-0",
          "0012",
          "-.25",
          "+.25",
          "-1.",
          "+1.25",
          "-1.2e-3",
          "+2E+4",
        ],
        ["\t -2.0e+3 \r\n", "1.234567890123456789e100", "0"],
      ]),
    ).toBe(
      '"-42","+42","-0","0012","-.25","+.25","-1.","+1.25","-1.2e-3","+2E+4"\r\n' +
        '"\t -2.0e+3 \r\n","1.234567890123456789e100","0"',
    );
    expect(tableToCsv([["-1e2+3", "+0x2A", "-Infinity", "+ 42"]])).toBe(
      '"\'-1e2+3","\'+0x2A","\'-Infinity","\'+ 42"',
    );
  });

  it("quotes commas, quotes, line breaks, and Unicode while preserving row boundaries", () => {
    expect(
      tableToCsv([
        ['Hello,\r\n"世界" 😀', "مرحبا بالعالم", ""],
        ["first\n=not a new cell", "second\rline", '="a,b"'],
      ]),
    ).toBe(
      '"Hello,\r\n""世界"" 😀","مرحبا بالعالم",""\r\n' +
        '"first\n=not a new cell","second\rline","\'=""a,b"""',
    );
    expect(tableToCsv([[""], [], ["last"]])).toBe('""\r\n\r\n"last"');
    expect(tableToCsv([])).toBe("");
  });
});
