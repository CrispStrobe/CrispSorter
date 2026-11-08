import type { OcrPage, OcrRegion } from "./model";

export interface PixelRectangle {
  x: number;
  y: number;
  w: number;
  h: number;
}

const intersects = (a: PixelRectangle, b: PixelRectangle) =>
  a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y;
const contains = (outer: PixelRectangle, inner: PixelRectangle) =>
  inner.x >= outer.x &&
  inner.y >= outer.y &&
  inner.x + inner.w <= outer.x + outer.w &&
  inner.y + inner.h <= outer.y + outer.h;
const replaceable = (r: OcrRegion) => !r.excluded && r.kind !== "picture";

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
        contains(zone, region)
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
        next.x !== zone.x ||
        next.y !== zone.y ||
        next.w !== zone.w ||
        next.h !== zone.h;
      zone = next;
    }
  } while (changed);
  return zone;
}

/** Preserve unaffected IDs and their reading order, including excluded content. */
export function mergeZoneRegions(
  page: OcrPage,
  zone: PixelRectangle,
  recognized: OcrRegion[],
): OcrRegion[] {
  const replaced = page.regions.filter(
    (r) => replaceable(r) && contains(zone, r),
  );
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
