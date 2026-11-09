import { describe, expect, it } from "vitest";
import { captureFileError, initialPhotoQuad, movePhotoCorner, photoQuadError, MAX_CAPTURE_BYTES } from "./photo";

describe("phone photo geometry", () => {
  it("keeps full-image corner coordinates inside the final pixel", () => {
    const quad = initialPhotoQuad(3024, 4032);
    expect(quad[2]).toEqual({ x: 3023, y: 4031 });
    expect(photoQuadError(quad, 3024, 4032)).toBeNull();
    expect(movePhotoCorner(quad, 1, { x: 4000, y: -100 }, 3024, 4032)[1]).toEqual({ x: 3023, y: 0 });
    expect(quad[1]).toEqual({ x: 3023, y: 0 });
  });
  it("accepts a tilted trapezoid but rejects crossing, concavity and nonfinite input", () => {
    const valid = [{ x: 100, y: 50 }, { x: 800, y: 150 }, { x: 950, y: 1300 }, { x: 40, y: 1150 }];
    expect(photoQuadError(valid, 1000, 1400)).toBeNull();
    expect(photoQuadError([valid[0], valid[2], valid[1], valid[3]], 1000, 1400)).toMatch(/clockwise/);
    expect(photoQuadError([valid[0], valid[1], { x: 200, y: 200 }, valid[3]], 1000, 1400)).toMatch(/clockwise/);
    expect(photoQuadError([{ x: NaN, y: 0 }, ...valid.slice(1)], 1000, 1400)).toMatch(/inside/);
    expect(photoQuadError(initialPhotoQuad(2, 2), 2, 2)).not.toBeNull();
  });
  it("gives recoverable capture errors before allocating large IPC payloads", () => {
    expect(captureFileError({ name: "photo.jpg", size: MAX_CAPTURE_BYTES, type: "image/jpeg" })).toBeNull();
    expect(captureFileError({ name: "photo.jpg", size: MAX_CAPTURE_BYTES + 1, type: "image/jpeg" })).toMatch(/32 MiB/);
    expect(captureFileError({ name: "photo.HEIC", size: 10, type: "" })).toMatch(/JPEG or PNG/);
    expect(captureFileError({ name: "photo.png", size: 0, type: "image/png" })).toMatch(/empty/);
  });
  it("accepts phone formats only when this device advertises their decoder", () => {
    for (const format of ["heic", "heif", "avif"]) {
      const file = { name: `photo.${format.toUpperCase()}`, size: 1024, type: "" };
      expect(captureFileError(file, [format])).toBeNull();
      expect(captureFileError(file, [])).toContain(format.toUpperCase());
      expect(captureFileError({ ...file, name: "camera", type: `image/${format}` }, [format])).toBeNull();
    }
    expect(captureFileError({ name: "camera", size: 42, type: "image/heif-sequence" }, ["heif"])).toBeNull();
    expect(captureFileError({ name: "camera", size: 42, type: "application/pdf" }, ["heic"])).toMatch(/photo or page image/);
  });
});
