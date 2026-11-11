import { describe, expect, it } from 'vitest';
import { MAX_RANGE_PAGES, moveOrder, normaliseRotation, parseRange, parseSplitRanges } from './types';

describe('PDF page selections', () => {
    it('reserves all-pages selection for an empty insertion range', () => {
        expect(parseRange('')).toEqual([]);
        expect(parseRange('   ')).toEqual([]);
        expect(parseRange('1-3, 7, 7')).toEqual([0, 1, 2, 6, 6]);
    });

    it.each(['garbage', '1junk', '1,garbage', ',,', '1,', '1,,2', '0', '-1', '+1', '5-3', '1.5', '1e3', '9007199254740992'])
        ('rejects invalid insertion input %s instead of inserting all or different pages', (input) => {
            expect(parseRange(input)).toBeNull();
        });

    it('rejects oversized expansions before allocating page arrays', () => {
        expect(parseRange(`1-${MAX_RANGE_PAGES + 1}`)).toBeNull();
        expect(parseRange(`1-${MAX_RANGE_PAGES},1`)).toBeNull();
        expect(parseRange('1-9007199254740991')).toBeNull();
    });

    it('validates split bounds against the current document', () => {
        expect(parseSplitRanges('1-3,4,5-7', 7)).toEqual([[0, 3], [3, 4], [4, 7]]);
        for (const input of ['', '0', '5-3', '2,', '1.5', '8', '1-8', '9007199254740992']) {
            expect(parseSplitRanges(input, 7), input).toBeNull();
        }
    });

    it('moves a selected block in source order and handles both list boundaries', () => {
        expect(moveOrder(5, [3, 1, 3], 0)).toEqual([1, 3, 0, 2, 4]);
        expect(moveOrder(5, [0, 2], 5)).toEqual([1, 3, 4, 0, 2]);
        expect(moveOrder(5, [1, 2], 4)).toEqual([0, 3, 1, 2, 4]);
        expect(moveOrder(5, [1, 2], 2)).toBeNull();
        expect(moveOrder(5, [-1, 99, 0.5, NaN], 0)).toBeNull();
        expect(moveOrder(0, [0], 0)).toBeNull();
    });

    it('uses the native rotation domain for clockwise and counter-clockwise turns', () => {
        expect([-450, -90, 0, 90, 360, 450].map(normaliseRotation)).toEqual([270, 270, 0, 90, 0, 90]);
    });
});

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
