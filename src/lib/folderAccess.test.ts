import { describe, expect, it } from 'vitest';
import { folderFromError, pathContains } from './folderAccess';

describe('pathContains', () => {
    it('accepts a folder and its descendants', () => {
        expect(pathContains('/a/texte', '/a/texte')).toBe(true);
        expect(pathContains('/a/texte', '/a/texte/Sorted/Autor/2023')).toBe(true);
    });

    it('rejects a sibling that merely shares a prefix', () => {
        // A string-prefix check would call this contained, and the user
        // would be told a grant worked when it bought nothing.
        expect(pathContains('/a/texte', '/a/texte-backup')).toBe(false);
    });

    it('rejects a descendant used as the ancestor', () => {
        expect(pathContains('/a/texte/Sorted', '/a/texte')).toBe(false);
    });

    it('ignores trailing separators', () => {
        expect(pathContains('/a/texte/', '/a/texte/x')).toBe(true);
    });
});

describe('folderFromError', () => {
    it('extracts the folder the backend named', () => {
        const err =
            'NOT_PERMITTED: /Users/x/Documents/texte/Sorted/Jüster, Markus (Hrsg.)/2023 — the operating system refused, not the folder’s permissions.';
        expect(folderFromError(err)).toBe(
            '/Users/x/Documents/texte/Sorted/Jüster, Markus (Hrsg.)/2023'
        );
    });

    it('returns null for other errors, so the prompt is not offered wrongly', () => {
        expect(folderFromError('NOT_WRITABLE: /x — Read-only file system')).toBeNull();
        expect(folderFromError('SOURCE_NOT_FOUND')).toBeNull();
        expect(folderFromError(undefined)).toBeNull();
    });
});
