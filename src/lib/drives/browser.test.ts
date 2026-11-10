import { describe, expect, it } from 'vitest';
import {
    availableDriveActions,
    drivePathFromSearchUri,
    joinDrivePath,
    localPathFromSearchUri,
    normalizeDrivePath,
    parentDrivePath,
    pathBaseName,
    supportsCloudVersions,
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

    it('disables local actions for malformed encoded provenance without throwing', () => {
        for (const uri of [
            'crisp+local://host/docs/100%.pdf',
            'crisp+local://host/docs/%Q1.pdf',
            'crisp+local://host/docs/%E2%82.pdf',
            'crisp+local://host/docs/%80.pdf',
            'crisp+local://host/docs/%00.pdf',
            'crisp+local://host',
            '/docs/invalid\0.pdf',
        ]) {
            expect(() => localPathFromSearchUri(uri)).not.toThrow();
            expect(localPathFromSearchUri(uri)).toBeNull();
        }
    });

    it('decodes valid local URIs once and preserves literal percentages in filesystem paths', () => {
        expect(localPathFromSearchUri('crisp+local://host/docs/r%C3%A9sum%C3%A9%20100%25.pdf'))
            .toBe('/docs/résumé 100%.pdf');
        expect(localPathFromSearchUri('crisp+local://host/docs/literal%252Fname.pdf'))
            .toBe('/docs/literal%2Fname.pdf');
        expect(localPathFromSearchUri('/docs/100%.pdf')).toBe('/docs/100%.pdf');
        expect(localPathFromSearchUri('C:\\docs\\100%.pdf')).toBe('C:\\docs\\100%.pdf');
    });

    it('parses registered-drive locations while rejecting invalid provenance', () => {
        expect(drivePathFromSearchUri('crisp+drive://research/docs/r%C3%A9sum%C3%A9%20%252F.pdf'))
            .toEqual({ driveId: 'research', remotePath: '/docs/résumé %2F.pdf' });
        expect(drivePathFromSearchUri('crisp+drive://research/')).toEqual({ driveId: 'research', remotePath: '/' });
        for (const uri of [
            'crisp+drive://research/docs/%', 'crisp+drive://research/docs/%80.pdf',
            'crisp+drive://research/docs/%00.pdf', 'crisp+drive:///docs/file.pdf',
            'crisp+drive://research', '/docs/file.pdf', 'https://example.test/file.pdf',
        ]) expect(drivePathFromSearchUri(uri)).toBeNull();
    });

    it('only enables remote version queries when advertised', () => {
        expect(supportsCloudVersions({ create_dir: false, rename: false, move_path: false, copy: false, delete: false })).toBe(false);
        expect(supportsCloudVersions({ create_dir: false, rename: false, move_path: false, copy: false, delete: false, versions: false })).toBe(false);
        expect(supportsCloudVersions({ create_dir: false, rename: false, move_path: false, copy: false, delete: false, versions: true })).toBe(true);
    });
});
