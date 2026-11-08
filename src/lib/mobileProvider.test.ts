import { beforeEach, describe, expect, it, vi } from 'vitest';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

import { batchSourceKey, isProviderUri, partitionProviderFiles, pickProviderDocuments, readProviderFile, PROVIDER_MAX_BYTES } from './mobileProvider';

describe('Android document providers', () => {
    beforeEach(() => invoke.mockReset());

    it('keeps opaque URI identity, including case, and retains local path normalization', () => {
        expect(batchSourceKey('content://docs/document/A%3A1')).not.toBe(batchSourceKey('content://docs/document/a%3A1'));
        expect(batchSourceKey('content://docs/document/A%3A1')).toBe('uri:content://docs/document/A%3A1');
        expect(isProviderUri('file:///provider/Case%20Document.txt')).toBe(true);
        expect(batchSourceKey('file:///provider/Case%20Document.txt')).not.toBe(batchSourceKey('file:///provider/case%20Document.txt'));
        expect(batchSourceKey('C:\\Documents\\ONE.PDF')).toBe(batchSourceKey('c:/documents/one.pdf'));
    });

    it('filters unsupported, unsafe and oversized entries while allowing unknown sizes', () => {
        const files = [
            { uri: 'content://docs/tree/a/document/a%3A1', name: 'Research.PDF', size: -1 },
            { uri: 'content://docs/document/a%3A2', name: 'big.txt', size: PROVIDER_MAX_BYTES + 1 },
            { uri: 'content://docs/document/a%3A3', name: 'video.mp4', size: 10 },
            { uri: '/local/doc.txt', name: 'doc.txt', size: 10 },
            { uri: 'file:///provider/doc%20one.txt', name: 'doc one.txt', size: 5 },
        ];
        expect(partitionProviderFiles(files)).toEqual({ accepted: [files[0], files[4]], unsupported: 2, oversized: 1 });
    });

    it('passes through selected document URIs, reports cancellation and reads their bytes', async () => {
        const uri = 'content://docs/tree/primary%3AFolder/document/primary%3AFolder%2Fpaper.txt';
        invoke.mockResolvedValueOnce({ cancelled: false, files: [{ uri, name: 'paper.txt', size: 3 }] });
        expect(await pickProviderDocuments(true)).toEqual({ accepted: [{ uri, name: 'paper.txt', size: 3 }], unsupported: 0, oversized: 0 });
        expect(invoke).toHaveBeenCalledWith('plugin:mobile-provider|pick_folder');
        invoke.mockResolvedValueOnce({ data: 'YQBi' });
        expect([...await readProviderFile(uri)]).toEqual([97, 0, 98]);
        expect(invoke).toHaveBeenLastCalledWith('plugin:mobile-provider|read_file', { uri });
        invoke.mockResolvedValueOnce({ cancelled: true, files: [] });
        expect(await pickProviderDocuments(false)).toBeNull();
    });

    it('rejects local paths before invoking the native reader', async () => {
        await expect(readProviderFile('/tmp/copied.txt')).rejects.toThrow('provider URI');
        expect(invoke).not.toHaveBeenCalled();
    });

    it('reads the exact iOS provider URL through the native bridge', async () => {
        const uri = 'file:///private/var/mobile/provider/Case%20document.txt';
        invoke.mockResolvedValueOnce({ data: 'dGV4dA==' });
        expect([...await readProviderFile(uri)]).toEqual([116, 101, 120, 116]);
        expect(invoke).toHaveBeenCalledWith('plugin:mobile-provider|read_file', { uri });
    });
});
