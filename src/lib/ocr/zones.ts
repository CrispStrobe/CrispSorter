import type { OcrPage, OcrRegion } from "./model";

export interface PixelRectangle {
  x: number;
  y: number;
  w: number;
  h: number;
}

const intersects = (a: PixelRectangle, b: PixelRectangle) =>
  a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y;
// Match the model and native geometry boundary tolerance. Floating point boxes
// at a page edge must still be replaced by a crop of that complete page.
const edgeTolerance = 0.01;
export const zoneContains = (outer: PixelRectangle, inner: PixelRectangle) =>
  inner.x >= outer.x - edgeTolerance &&
  inner.y >= outer.y - edgeTolerance &&
  inner.x + inner.w <= outer.x + outer.w + edgeTolerance &&
  inner.y + inner.h <= outer.y + outer.h + edgeTolerance;
const replaceable = (r: OcrRegion) => !r.excluded && r.kind !== "picture";

/** An empty/blank OCR response is not an instruction to remove reviewed text. */
export function hasRecognizedText(
  regions: readonly Pick<OcrRegion, "text">[],
): boolean {
  return regions.some((region) => region.text.trim().length > 0);
}

/** Native diagnostics must describe a real integer crop of this page. */
export function effectiveRecognitionZone(
  page: OcrPage,
  requested: PixelRectangle,
  value: unknown,
): PixelRectangle {
  if (value === undefined) return requested;
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error(
      "Recognition returned invalid crop geometry. The existing text was preserved.",
    );
  const rect = value as PixelRectangle;
  if (
    ![rect.x, rect.y, rect.w, rect.h].every(Number.isInteger) ||
    rect.x < 0 ||
    rect.y < 0 ||
    rect.w <= 0 ||
    rect.h <= 0 ||
    rect.x + rect.w > page.width ||
    rect.y + rect.h > page.height
  )
    throw new Error(
      "Recognition returned invalid crop geometry. The existing text was preserved.",
    );
  return { x: rect.x, y: rect.y, w: rect.w, h: rect.h };
}

/** Match native crop rounding, then cover whole intersecting text regions. */
export function expandRecognitionZone(
  page: OcrPage,
  requested: PixelRectangle,
): PixelRectangle {
  if (
    ![requested.x, requested.y, requested.w, requested.h].every(
      Number.isFinite,
    ) ||
    requested.w <= 0 ||
    requested.h <= 0
  )
    throw new Error(
      "The recognition zone must have positive, finite dimensions.",
    );
  const x = Math.max(0, Math.floor(requested.x)),
    y = Math.max(0, Math.floor(requested.y));
  let zone = {
    x,
    y,
    w: Math.min(page.width, Math.ceil(requested.x + requested.w)) - x,
    h: Math.min(page.height, Math.ceil(requested.y + requested.h)) - y,
  };
  if (zone.w <= 0 || zone.h <= 0)
    throw new Error("The recognition zone is outside this page.");
  // Expansion can encounter another region. Iterate until every intersecting
  // paragraph is wholly covered; no outside text is discarded by a tiny crop.
  let changed: boolean;
  do {
    changed = false;
    for (const region of page.regions) {
      if (
        !replaceable(region) ||
        !intersects(zone, region) ||
        zoneContains(zone, region)
      )
        continue;
      const left = Math.max(0, Math.floor(Math.min(zone.x, region.x)));
      const top = Math.max(0, Math.floor(Math.min(zone.y, region.y)));
      const right = Math.min(
        page.width,
        Math.ceil(Math.max(zone.x + zone.w, region.x + region.w)),
      );
      const bottom = Math.min(
        page.height,
        Math.ceil(Math.max(zone.y + zone.h, region.y + region.h)),
      );
      const next = { x: left, y: top, w: right - left, h: bottom - top };
      changed =
        changed ||
        next.x !== zone.x ||
        next.y !== zone.y ||
        next.w !== zone.w ||
        next.h !== zone.h;
      zone = next;
    }
  } while (changed);
  return zone;
}

export function regionsInRecognitionZone(page: OcrPage, zone: PixelRectangle): OcrRegion[] {
  return page.regions.filter((region) => replaceable(region) && zoneContains(zone, region));
}

/** Preserve unaffected IDs and their reading order, including excluded content. */
export function mergeZoneRegions(
  page: OcrPage,
  zone: PixelRectangle,
  recognized: OcrRegion[],
): OcrRegion[] {
  // This merge is only for recognition results. Intentional region removal
  // uses deleteRegion; a failed-to-read crop must preserve the original array.
  if (!hasRecognizedText(recognized)) return page.regions;
  const replaced = regionsInRecognitionZone(page, zone);
  const replacedIds = new Set(replaced.map((r) => r.id));
  let insertion = page.regions.findIndex((r) => replacedIds.has(r.id));
  if (insertion < 0)
    insertion = page.regions.findIndex((r) => r.y >= zone.y + zone.h);
  if (insertion < 0) insertion = page.regions.length;
  return [
    ...page.regions.slice(0, insertion).filter((r) => !replacedIds.has(r.id)),
    ...recognized,
    ...page.regions.slice(insertion).filter((r) => !replacedIds.has(r.id)),
  ];
}
