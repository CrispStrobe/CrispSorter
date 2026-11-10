/** Geometry is always expressed in pixels of the current working image. */
export interface PhotoPoint { x: number; y: number }
export type PhotoQuad = [PhotoPoint, PhotoPoint, PhotoPoint, PhotoPoint];
export const CORNER_NAMES = ["Top left", "Top right", "Bottom right", "Bottom left"] as const;
export const MAX_CAPTURE_BYTES = 32 * 1024 * 1024;

export function initialPhotoQuad(width: number, height: number): PhotoQuad {
  return [{ x: 0, y: 0 }, { x: width - 1, y: 0 }, { x: width - 1, y: height - 1 }, { x: 0, y: height - 1 }];
}

/** Reject crossed, collapsed and concave selections before touching source pixels. */
export function photoQuadError(points: readonly PhotoPoint[], width: number, height: number): string | null {
  if (points.length !== 4 || !Number.isFinite(width) || !Number.isFinite(height) || width < 2 || height < 2)
    return "Choose four corners on a page at least two pixels wide and high.";
  if (points.some(p => !Number.isFinite(p.x) || !Number.isFinite(p.y) || p.x < 0 || p.y < 0 || p.x > width - 1 || p.y > height - 1))
    return "Keep all four corners inside the image.";
  let area = 0;
  for (let i = 0; i < 4; i++) {
    const a = points[i], b = points[(i + 1) % 4], c = points[(i + 2) % 4];
    if ((b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x) <= 0.01)
      return "Corners must run clockwise around the page without crossing.";
    if (Math.hypot(b.x - a.x, b.y - a.y) < 2)
      return "Move the corners farther apart; every edge needs at least two pixels.";
    area += a.x * b.y - a.y * b.x;
  }
  if (area / 2 < 16) return "Select a larger page area.";
  return null;
}

export function movePhotoCorner(quad: PhotoQuad, index: number, point: PhotoPoint, width: number, height: number): PhotoQuad {
  return quad.map((p, i) => i === index ? {
    x: Math.min(width - 1, Math.max(0, point.x)),
    y: Math.min(height - 1, Math.max(0, point.y)),
  } : { ...p }) as PhotoQuad;
}

export function captureFileError(file: Pick<File, "name" | "size" | "type">, deviceFormats: readonly string[] = []): string | null {
  if (file.size === 0) return "The selected photo is empty. Take another photo or select a different image.";
  if (file.size > MAX_CAPTURE_BYTES) return "This photo exceeds 32 MiB. Export a smaller JPEG or PNG and try again.";
  const phoneFormat = /\.(heic|heif|avif)$/i.exec(file.name)?.[1]?.toLowerCase()
    ?? /^image\/(heic|heif|avif)(?:-sequence)?$/i.exec(file.type)?.[1]?.toLowerCase();
  if (phoneFormat && !deviceFormats.includes(phoneFormat))
    return `This device cannot decode ${phoneFormat.toUpperCase()} images. Export this photo as JPEG or PNG first.`;
  if (file.type && !file.type.startsWith("image/")) return "Select a photo or page image.";
  return null;
}
