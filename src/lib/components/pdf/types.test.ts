import { describe, expect, it } from 'vitest';
import { moveOrder, normaliseRotation, parseRange, parseSplitRanges } from './types';

describe('normaliseRotation', () => {
    it('maps counter-clockwise turns into the backend domain', () => {
        expect(normaliseRotation(-90)).toBe(270);
        expect(normaliseRotation(-270)).toBe(90);
        expect(normaliseRotation(450)).toBe(90);
        expect(normaliseRotation(360)).toBe(0);
        expect(normaliseRotation(90)).toBe(90);
    });
});

describe('moveOrder', () => {
    it('moves a page one step later or earlier', () => {
        expect(moveOrder(3, [0], 2)).toEqual([1, 0, 2]);
        expect(moveOrder(3, [2], 1)).toEqual([0, 2, 1]);
    });
    it('moves a block to the front or the end, keeping its order', () => {
        expect(moveOrder(5, [3, 1], 0)).toEqual([1, 3, 0, 2, 4]);
        expect(moveOrder(5, [0, 2], 5)).toEqual([1, 3, 4, 0, 2]);
    });
    it('returns null when nothing would change', () => {
        expect(moveOrder(3, [1], 1)).toBeNull();
        expect(moveOrder(3, [1], 2)).toBeNull();
        expect(moveOrder(3, [], 0)).toBeNull();
    });
});

describe('ranges', () => {
    it('parses page ranges to 0-based indices', () => {
        expect(parseRange('1-3, 7')).toEqual([0, 1, 2, 6]);
    });
    it('parses split ranges as [start, exclusive end] and rejects garbage', () => {
        expect(parseSplitRanges('1-5,6-10')).toEqual([[0, 5], [5, 10]]);
        expect(parseSplitRanges('4')).toEqual([[3, 4]]);
        expect(parseSplitRanges('5-2')).toBeNull();
        expect(parseSplitRanges('abc')).toBeNull();
        expect(parseSplitRanges('')).toBeNull();
    });
});
