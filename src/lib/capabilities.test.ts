import { describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import { buildFlags, caps } from './capabilities';

describe('OCR workbench capabilities', () => {
    it('requires CrispEmbed even when a different OCR tier is available', () => {
        const original = get(caps);
        try {
            caps.set({ ...original, ocr: true, ocr_tiers: ['ocrs'], flags: ['build:ocr', 'build:ocr-workbench'] });
            expect(get(buildFlags).has('build:ocr')).toBe(true);
            expect(get(buildFlags).has('build:ocr-workbench')).toBe(false);
            caps.set({ ...original, ocr: true, ocr_tiers: ['crispembed'], flags: ['build:ocr'] });
            expect(get(buildFlags).has('build:ocr-workbench')).toBe(true);
        } finally {
            caps.set(original);
        }
    });
});
