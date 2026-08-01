import { describe, expect, it } from 'vitest';
import {
    availableDriveActions,
    joinDrivePath,
    normalizeDrivePath,
    parentDrivePath,
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
});
