import { describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import { buildFlags, caps } from './capabilities';

describe('OCR workbench capabilities', () => {
    it.each(['crispembed', 'tesseract', 'system_tesseract', 'ocrs', 'paddle'])('offers the Workbench for supported %s builds', (tier) => {
        const original = get(caps);
        try {
            caps.set({ ...original, ocr: true, ocr_tiers: [tier], flags: ['build:ocr'] });
            expect(get(buildFlags).has('build:ocr')).toBe(true);
            expect(get(buildFlags).has('build:ocr-workbench')).toBe(true);
        } finally {
            caps.set(original);
        }
    });

    it.each([
        { ocr: false, ocr_tiers: ['crispembed'] },
        { ocr: false, ocr_tiers: ['ocrs'] },
        { ocr: true, ocr_tiers: [] },
        { ocr: true, ocr_tiers: ['unsupported-engine'] },
    ])('removes stale Workbench flags for unavailable capabilities %j', (unavailable) => {
        const original = get(caps);
        try {
            caps.set({ ...original, ...unavailable, flags: ['build:ocr', 'build:ocr-workbench'] });
            expect(get(buildFlags).has('build:ocr-workbench')).toBe(false);
        } finally { caps.set(original); }
    });
});
