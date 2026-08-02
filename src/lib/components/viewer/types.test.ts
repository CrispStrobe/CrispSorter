import { describe, expect, it } from 'vitest';
import { detectKind, extOf, uriToPath } from './types';

describe('document viewer path safety', () => {
    it('accepts absolute and crisp local paths', () => {
        expect(uriToPath('/tmp/readme.md')).toBe('/tmp/readme.md');
        expect(uriToPath('crisp+local://host/Users/test/main.rs')).toBe('/Users/test/main.rs');
    });

    it('rejects provider and archive URIs from local editing', () => {
        expect(uriToPath('crisp+drive://drive-1/folder/file.md')).toBeNull();
        expect(uriToPath('crisp+cb-archive://42/hash#folder/file.md')).toBeNull();
        expect(uriToPath('https://example.test/file.txt')).toBeNull();
    });
});

describe('document viewer format detection', () => {
    it('detects text and code extensions case-insensitively', () => {
        expect(extOf('/tmp/README.MD')).toBe('md');
        expect(detectKind('rs')).toBe('text');
        expect(detectKind('svelte')).toBe('text');
        expect(detectKind('csv')).toBe('csv');
    });

    it('keeps unknown binary formats in the fallback viewer', () => {
        expect(detectKind('7z')).toBe('fallback');
        expect(detectKind('mp4')).toBe('fallback');
    });
});
