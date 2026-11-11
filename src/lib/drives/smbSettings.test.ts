import { describe, expect, it } from 'vitest';
import { loadSmbSettings, smbSettingsPayload } from './smbSettings';

describe('SMB drive form wire settings', () => {
    it('keeps legacy/default security and chooses the existing mounted path', () => {
        expect(loadSmbSettings()).toEqual({backend: 'mounted', mount_path: null, require_encryption: true});
        expect(loadSmbSettings({require_encryption: false}).require_encryption).toBe(false);
    });
    it.each(['mounted', 'rust'])('round-trips %s without credential fields', backend => {
        const settings = smbSettingsPayload(backend, ' /Volumes/Books ', false);
        expect(loadSmbSettings(JSON.parse(JSON.stringify(settings)))).toEqual(settings);
        expect(Object.keys(settings).sort()).toEqual(['backend','mount_path','require_encryption']);
    });
    it('requires a mount only for the mounted backend, preserving Rust as explicit', () => {
        expect(() => smbSettingsPayload('mounted', ' ', true)).toThrow('mount');
        expect(smbSettingsPayload('rust', '', true)).toEqual({backend: 'rust', mount_path: null, require_encryption: true});
        expect(() => smbSettingsPayload('auto', '/mount', true)).toThrow('Choose');
    });
});
