import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { buildLicenseConsentDialog } from './licenseConsentMessage';

const settings = readFileSync(new URL('./components/Settings.svelte', import.meta.url), 'utf8');
// i18n.svelte.ts uses runes, which vitest does not compile here; read the
// string table as text. EN is the first occurrence of each key, DE the second.
const i18nSource = readFileSync(new URL('./i18n.svelte.ts', import.meta.url), 'utf8');
const KEYS = ['registry_license_confirm', 'registry_license_terms', 'registry_license_terms_unknown', 'registry_license_title'] as const;
function stringsFor(index: number): Record<(typeof KEYS)[number], string> {
    const out: Record<string, string> = {};
    for (const key of KEYS) {
        const all = [...i18nSource.matchAll(new RegExp(`\\b${key}: '((?:[^'\\\\]|\\\\.)*)'`, 'g'))];
        expect(all.length, key).toBe(2);
        out[key] = all[index][1];
    }
    return out as Record<(typeof KEYS)[number], string>;
}

const GOT = {
    label: 'Tongyi Qianwen Research License',
    spdx: 'tongyi-qianwen-research',
    model_card_url: 'https://huggingface.co/stepfun-ai/GOT-OCR2_0',
};

describe('generic registry licence dialog', () => {
    for (const [lang, index] of [['en', 0], ['de', 1]] as const) {
        const strings = stringsFor(index);

        it(`${lang}: names the selected model, its licence and its terms, not Jina`, () => {
            const d = buildLicenseConsentDialog(strings, 'got-ocr2-f16', GOT);
            expect(d.message).toContain('got-ocr2-f16');
            expect(d.message).toContain('Tongyi Qianwen Research License');
            expect(d.message).toContain(GOT.model_card_url);
            expect(d.title).toContain('Tongyi Qianwen Research License');
            expect(d.message + d.title).not.toMatch(/jina/i);
            expect(d.message + d.title).not.toContain('CC-BY-NC-4.0');
            expect(d.message).not.toMatch(/\{(model|license|url)\}/);
        });

        it(`${lang}: Gemma and Qwen research models get their own text`, () => {
            const gemma = buildLicenseConsentDialog(strings, 'embeddinggemma-300m', {
                label: 'Gemma Terms of Use', spdx: 'gemma', model_card_url: '',
            });
            expect(gemma.message).toContain('embeddinggemma-300m');
            expect(gemma.message).toContain('Gemma Terms of Use');
            expect(gemma.message).toContain(strings.registry_license_terms_unknown);
            expect(gemma.message).not.toMatch(/jina/i);
            const qwen = buildLicenseConsentDialog(strings, 'qwen2vl-3b', {
                label: 'Qwen Research License', spdx: 'qwen-research', model_card_url: 'https://example.invalid/q',
            });
            expect(qwen.message).toContain('qwen2vl-3b');
            expect(qwen.message).toContain('Qwen Research License');
        });
    }

    it('the generic download dialog no longer uses the Jina-specific string', () => {
        const fn = settings.slice(
            settings.indexOf('async function downloadRegistryModel'),
            settings.indexOf('function selectRegistryModel'),
        );
        expect(fn).toContain('buildLicenseConsentDialog(');
        expect(fn).toContain("'embedder_model_license_info'");
        expect(fn).not.toContain('non_commercial_confirm');
    });
});
