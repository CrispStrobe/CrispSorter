import { expect, it } from 'vitest';
import { cropRegion, weakPages } from './ocrTasks';
const region = { text: 'raw', orig: 'raw', confidence: 0.2, x: 10.5, y: 20.2, w: 30.1, h: 40.1 };
it('retries only weak uncorrected pages', () => {
    const pages = new Map([
        [0, { ocred: true, proofread: false, regions: [region] }],
        [1, { ocred: true, proofread: true, regions: [region] }],
        [2, { ocred: true, proofread: false, regions: [{ ...region, text: 'corrected' }] }],
        [3, { ocred: true, proofread: false, regions: [] }],
        [4, { ocred: false, proofread: false, regions: [] }],
    ]);
    expect(weakPages(pages, 0.5)).toEqual([0, 3]);
});
it('keeps crops inside the image and rejects malformed coordinates', () => {
    expect(cropRegion(region, 35, 50)).toEqual({ x: 10, y: 20, w: 25, h: 30 });
    expect(() => cropRegion({ ...region, x: NaN }, 100, 100)).toThrow();
    expect(() => cropRegion({ ...region, x: 200 }, 100, 100)).toThrow();
});
