import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const settings = readFileSync(new URL('./Settings.svelte', import.meta.url), 'utf8');

describe('image embedding settings integration', () => {
    it('defaults to opt-in and persists the checkbox into the index config', () => {
        expect(settings).toMatch(/indexImageIndexingEnabled\s*=\s*\$state<boolean>\(false\)/);
        expect(settings).toMatch(/getSetting\('indexImageIndexingEnabled',\s*false\)/);
        expect(settings).toMatch(/saveSetting\('indexImageIndexingEnabled',\s*indexImageIndexingEnabled\)/);
        expect(settings).toMatch(/image_indexing_enabled:\s*indexImageIndexingEnabled/);
        expect(settings).toMatch(/bind:checked=\{indexImageIndexingEnabled\}/);
    });
});
