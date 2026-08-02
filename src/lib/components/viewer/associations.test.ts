import { describe, expect, it } from 'vitest';
import { normalizeFileAssociations, viewerKindForExtension } from './associations';

describe('file associations', () => {
    it('normalizes safe extension and viewer overrides', () => {
        expect(normalizeFileAssociations({ '.log': 'text', PDF: 'fallback', bad: 'unknown' })).toEqual({
            log: 'text', pdf: 'fallback',
        });
    });

    it('ignores malformed or oversized extension keys', () => {
        expect(normalizeFileAssociations({ 'a'.repeat(17): 'text', '../md': 'text', md: 4 })).toEqual({});
    });

    it('falls back to built-in detection', () => {
        expect(viewerKindForExtension('rs', {})).toBe('text');
        expect(viewerKindForExtension('bin', { bin: 'text' })).toBe('text');
    });
});
