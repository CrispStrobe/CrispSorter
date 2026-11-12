import { describe, expect, it } from 'vitest';
import { formatTransferBytes } from './transferFormatting';

describe('cloud transfer byte counters', () => {
    it.each([
        [0, '0 B'],
        [1023, '1023 B'],
        [1024, '1.0 KB'],
        [1536, '1.5 KB'],
        [1024 ** 2, '1.0 MB'],
        [1024 ** 3, '1.0 GB'],
        [1024 ** 4, '1.0 TB'],
    ])('displays %i bytes as %s', (bytes, expected) => {
        expect(formatTransferBytes(bytes)).toBe(expected);
    });

    it('keeps unknown sizes distinct from an empty transfer', () => {
        expect(formatTransferBytes(null)).toBe('—');
        expect(formatTransferBytes(undefined)).toBe('—');
        expect(formatTransferBytes(0)).toBe('0 B');
    });

    it('uses the same scale for transferred bytes, total size and speed', () => {
        const done = 1536;
        const total = 2 * 1024 ** 2;
        const bytesPerSecond = 1024 ** 2;
        expect(`${formatTransferBytes(done)} / ${formatTransferBytes(total)}`)
            .toBe('1.5 KB / 2.0 MB');
        expect(`${formatTransferBytes(bytesPerSecond)}/s`).toBe('1.0 MB/s');
    });
});
