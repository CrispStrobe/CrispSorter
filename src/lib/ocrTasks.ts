export interface ReviewRegion { text: string; orig: string; confidence: number; x: number; y: number; w: number; h: number }
export interface ReviewPage { ocred: boolean; proofread: boolean; regions: ReviewRegion[] }

/** Automatic retries never replace proofread pages or manual corrections. */
export function weakPages(pages: Map<number, ReviewPage>, threshold: number): number[] {
    return [...pages.entries()].filter(([, page]) => page.ocred && !page.proofread
        && !page.regions.some(r => r.text !== r.orig)
        && (!page.regions.length || page.regions.some(r => !Number.isFinite(r.confidence) || r.confidence < threshold)))
        .map(([index]) => index).sort((a, b) => a - b);
}

export function cropRegion(region: ReviewRegion, width: number, height: number) {
    if (![region.x, region.y, region.w, region.h, width, height].every(Number.isFinite)) throw new Error('Invalid region coordinates');
    const x = Math.max(0, Math.floor(region.x));
    const y = Math.max(0, Math.floor(region.y));
    const right = Math.min(width, Math.ceil(region.x + region.w));
    const bottom = Math.min(height, Math.ceil(region.y + region.h));
    if (right <= x || bottom <= y) throw new Error('Selected region is outside the page');
    return { x, y, w: right - x, h: bottom - y };
}
