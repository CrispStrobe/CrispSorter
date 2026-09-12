import { describe, expect, it } from 'vitest';
import { extractText } from './index';

/**
 * A zero-byte file used to reach the per-format parsers, and they disagreed
 * about what to do with one. pdfjs raises "The PDF file is empty"; the epub
 * parser neither resolves nor rejects — it hangs. Since the batch runner
 * awaits extraction, one such file wedged a 1017-item run at item 200 for
 * eight hours, with both the page watchdog and the file timeout having fired
 * (aborting is cooperative, and a parser that never yields never sees the
 * signal).
 *
 * So emptiness is now rejected once, up front, before any format dispatch.
 */
describe('extractText on an empty file', () => {
    const empty = () => new ArrayBuffer(0);

    it('rejects rather than dispatching to a parser', async () => {
        await expect(
            extractText({ name: 'broken.epub', arrayBuffer: empty() })
        ).rejects.toThrow(/empty \(0 bytes\)/);
    });

    it('rejects for every format, not just the one that reported it', async () => {
        for (const name of ['a.pdf', 'b.epub', 'c.docx', 'd.txt']) {
            await expect(
                extractText({ name, arrayBuffer: empty() })
            ).rejects.toThrow(/empty \(0 bytes\)/);
        }
    });

    it('names the file, so the report says which one to go and look at', async () => {
        await expect(
            extractText({ name: 'Fauler Zauber.epub', arrayBuffer: empty() })
        ).rejects.toThrow(/Fauler Zauber\.epub/);
    });
});
