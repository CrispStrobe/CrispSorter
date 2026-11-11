import { describe, expect, it } from 'vitest';
import { nativeExtractionResult } from './nativePath';
import { extractionIngestContent } from './container';

const sourceHash = 'a'.repeat(64);
const entry = { kind: 'zip', index: 3, occurrence: 0, name: 'letters/receipt.txt', sha256: 'b'.repeat(64), size: 30 };
function extracted() {
    return { text: 'Archive summary\nMember: letters/receipt.txt\nThe amount was paid.', ownText: 'Archive summary', ext: 'zip',
        headings: ['letters/receipt.txt'], tags: ['archive:zip'],
        containerSource: { kind: 'zip', sha256: sourceHash, size: 100 },
        containerCoverage: { discovered: 2, extracted: 1, skipped: 1,
            omissions: [{ path: 'encrypted.pdf', reason: 'encrypted' }], omittedDetails: 0 },
        members: [{ entries: [entry], document: { text: 'The amount was paid.', headings: ['Payment'], tags: ['finance'], ext: 'txt',
            metadata: { language: 'en', title: 'Payment record', source_url: 'https://example.test/receipt',
                text_translated: 'Der Betrag wurde bezahlt.', text_translated_lang: 'de' } } }] };
}

describe('foreground container documents', () => {
    it('keeps aggregate text for preview and sends independent content without duplicate root indexing', () => {
        const parsed = nativeExtractionResult(extracted(), 'archive');
        parsed.markdownText = '# Aggregate preview\nThe amount was paid.';
        const input = extractionIngestContent(parsed, sourceHash, () => 'de');
        expect(parsed.text).toContain('The amount was paid.');
        expect(input.fullText).toBe('Archive summary');
        expect(input.ownText).toBe('Archive summary');
        expect(input).not.toHaveProperty('fullTextMd');
        expect(input.containerCoverage?.omissions).toEqual([{ path: 'encrypted.pdf', reason: 'encrypted' }]);
        expect(input.members).toHaveLength(1);
        expect(input.members?.[0].document).toMatchObject({ filename: 'receipt.txt', ext: 'txt', fullText: 'The amount was paid.',
            language: 'en', title: 'Payment record', tags: ['finance'], url: 'https://example.test/receipt',
            textTranslated: 'Der Betrag wurde bezahlt.', textTranslatedLang: 'de' });
        expect(input.members?.[0].document).not.toHaveProperty('members');
    });

    it('binds the extraction to the captured root source bytes before queue submission', () => {
        const result = nativeExtractionResult(extracted(), 'archive');
        expect(() => extractionIngestContent(result, 'c'.repeat(64))).toThrow('container changed');
        expect(extractionIngestContent(result, sourceHash.toUpperCase()).containerSource?.sha256).toBe(sourceHash);
    });

    it('does not let member metadata supply identities, source paths or root fingerprints', () => {
        const response = extracted();
        Object.assign(response.members[0].document.metadata, { ownerId: 'other', docId: 'injected', locationUri: '/other/file',
            sourceHash: 'c'.repeat(64), sourceFingerprint: 'd'.repeat(64) });
        const document = extractionIngestContent(nativeExtractionResult(response, 'archive'), sourceHash).members![0].document;
        for (const key of ['ownerId', 'docId', 'locationUri', 'sourceHash', 'sourceFingerprint']) expect(document).not.toHaveProperty(key);
    });

    it('requires a separate root body and real source receipt whenever members are present', () => {
        for (const key of ['ownText', 'containerSource']) {
            const response: Record<string, unknown> = extracted(); delete response[key];
            expect(() => nativeExtractionResult(response, 'archive')).toThrow('root source receipt or separate root text');
        }
        expect(() => extractionIngestContent({ text: 'aggregate', members: [{ entries: [entry as any], document: { text: 'child' } }] }))
            .toThrow('captured source receipt');
    });

    it('rejects unsafe chains and duplicate logical identities even when physical positions differ', () => {
        const unsafe = extracted(); unsafe.members[0].entries[0] = { ...entry, name: '../escape.txt' };
        expect(() => nativeExtractionResult(unsafe, 'archive')).toThrow('byte receipt');
        const duplicates = extracted(); duplicates.members.push({ ...duplicates.members[0], entries: [{ ...entry, index: 7 }] });
        expect(() => nativeExtractionResult(duplicates, 'archive')).toThrow('duplicate member identities');
        const nested = extracted(); Object.assign(nested.members[0].document, { members: extracted().members });
        expect(() => nativeExtractionResult(nested, 'archive')).toThrow('flattened');
    });

    it('rejects broken coverage and too many members instead of quietly dropping content', () => {
        const response = extracted(); response.containerCoverage.skipped = 2;
        expect(() => nativeExtractionResult(response, 'archive')).toThrow('coverage');
        const oversized = extracted(); oversized.members = Array(2049).fill(oversized.members[0]);
        expect(() => nativeExtractionResult(oversized, 'archive')).toThrow('bounded');
    });

    it('preserves ordinary extraction content and metadata without adding container identities', () => {
        const parsed = nativeExtractionResult({ text: 'Slide text', ext: 'pptx', headings: ['Title'], tags: ['presentation'],
            metadata: { language: 'fr', audio: { duration_seconds: 0 }, image_exif: { camera_make: 'Camera' } } }, 'presentation');
        const input = extractionIngestContent({ ...parsed, markdownText: '# Slide text' });
        expect(input).toMatchObject({ fullText: 'Slide text', fullTextMd: '# Slide text', language: 'fr', audioDurationSeconds: 0,
            imageCameraMake: 'Camera', tags: ['presentation'] });
        expect(input).not.toHaveProperty('members');
        expect(input).not.toHaveProperty('containerSource');
    });
});
