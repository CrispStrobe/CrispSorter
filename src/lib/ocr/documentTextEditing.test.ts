import { describe, expect, it } from 'vitest';
import { remapTextSelection } from './documentTextEditing';

describe('document text selection reconciliation', () => {
    it('leaves native typing selections unchanged when the model catches up', () => {
        expect(remapTextSelection('a😀bc', 'a😀bc', { anchor: 3, focus: 1 })).toEqual({ anchor: 3, focus: 1 });
    });

    it('keeps the selection on unchanged text after an insertion before it', () => {
        expect(remapTextSelection('abc def', 'abc nice def', { anchor: 4, focus: 7 })).toEqual({ anchor: 9, focus: 12 });
    });

    it('maps a caret back through undo without retaining the deleted draft', () => {
        expect(remapTextSelection('Hello brave world', 'Hello world', { anchor: 12, focus: 12 })).toEqual({ anchor: 6, focus: 6 });
    });

    it('preserves the direction of backward selections', () => {
        expect(remapTextSelection('Hello brave world', 'Hello world', { anchor: 17, focus: 6 })).toEqual({ anchor: 11, focus: 6 });
    });

    it('clamps a selection inside a replaced span to the replacement', () => {
        expect(remapTextSelection('abc12345xyz', 'abcABxyz', { anchor: 4, focus: 7 })).toEqual({ anchor: 4, focus: 5 });
    });

    it('keeps literal line-break offsets when surrounding lines change', () => {
        expect(remapTextSelection('one\ntwo', 'one\nnew\ntwo', { anchor: 4, focus: 7 })).toEqual({ anchor: 8, focus: 11 });
    });

    it('does not place a caret inside a surrogate pair when emoji change', () => {
        expect(remapTextSelection('A😀BC', 'A😁BC', { anchor: 2, focus: 3 })).toEqual({ anchor: 1, focus: 3 });
        expect(remapTextSelection('X😀Y', '😀Y', { anchor: 3, focus: 4 })).toEqual({ anchor: 2, focus: 3 });
    });

    it('handles empty and fully replaced passages', () => {
        expect(remapTextSelection('long text', '短', { anchor: 9, focus: 0 })).toEqual({ anchor: 1, focus: 0 });
        expect(remapTextSelection('text', '', { anchor: 4, focus: 2 })).toEqual({ anchor: 0, focus: 0 });
        expect(remapTextSelection('', 'text', { anchor: 0, focus: 0 })).toEqual({ anchor: 4, focus: 4 });
    });

    it('bounds stale or malformed offsets', () => {
        expect(remapTextSelection('text', 'next', { anchor: -1, focus: 100 })).toEqual({ anchor: 0, focus: 4 });
        expect(remapTextSelection('text', 'next', { anchor: NaN, focus: Infinity })).toEqual({ anchor: 0, focus: 0 });
    });
});
