/** Non-secret wire format shared with the Rust drive registry and CLI. */
export interface SmbSettings {
    backend: 'mounted' | 'rust';
    mount_path?: string | null;
    require_encryption: boolean;
}
export function loadSmbSettings(saved?: Partial<SmbSettings> | null): SmbSettings {
    return {
        backend: saved?.backend ?? 'mounted',
        mount_path: saved?.mount_path ?? null,
        require_encryption: saved?.require_encryption ?? true,
    };
}
export function smbSettingsPayload(backend: string, mountPath: string, requireEncryption: boolean): SmbSettings {
    if (backend !== 'mounted' && backend !== 'rust') throw new Error('Choose mounted or rust for SMB.');
    const mount_path = mountPath.trim() || null;
    if (backend === 'mounted' && !mount_path) throw new Error('Choose the existing SMB mount folder.');
    return { backend, mount_path, require_encryption: requireEncryption };
}
