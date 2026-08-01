import { describe, expect, it } from 'vitest';
import {
    availableDriveActions,
    joinDrivePath,
    localPathFromSearchUri,
    normalizeDrivePath,
    parentDrivePath,
    pathBaseName,
} from './browser';

describe('drive browser path contract', () => {
    it('normalizes root and nested provider paths', () => {
        expect(normalizeDrivePath('')).toBe('/');
        expect(normalizeDrivePath('///docs//reports/')).toBe('/docs/reports');
        expect(joinDrivePath('/docs/', 'report.pdf')).toBe('/docs/report.pdf');
    });

    it('never climbs above provider root', () => {
        expect(parentDrivePath('/')).toBe('/');
        expect(parentDrivePath('/docs')).toBe('/');
        expect(parentDrivePath('/docs/reports')).toBe('/docs');
    });

    it('requires selection for item actions but not folder creation', () => {
        const capabilities = {
            create_dir: true,
            rename: true,
            move_path: true,
            copy: false,
            delete: true,
            share_links: false,
        };
        expect(availableDriveActions(capabilities, false)).toEqual({
            create_dir: true,
            rename: false,
            move: false,
            copy: false,
            delete: false,
        });
        expect(availableDriveActions(capabilities, true)).toEqual({
            create_dir: true,
            rename: true,
            move: true,
            copy: false,
            delete: true,
        });
    });

    it('preserves local search provenance and rejects remote schemes', () => {
        expect(localPathFromSearchUri('crisp+local://host/%2FUsers%2Falice%2Fpaper.pdf'))
            .toBe('/Users/alice/paper.pdf');
        expect(localPathFromSearchUri('/Users/alice/paper.pdf')).toBe('/Users/alice/paper.pdf');
        expect(localPathFromSearchUri('crisp+drive://drive-1/docs/paper.pdf')).toBeNull();
        expect(localPathFromSearchUri('crisp+cb-archive://archive/paper.pdf')).toBeNull();
        expect(localPathFromSearchUri('https://example.test/paper.pdf')).toBeNull();
        expect(pathBaseName('/Users/alice/paper.pdf')).toBe('paper.pdf');
    });
});
