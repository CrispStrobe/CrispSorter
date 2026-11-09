import { beforeEach, describe, expect, it, vi } from 'vitest';

const { invoke, open, save, loadCapabilities } = vi.hoisted(() => ({
    invoke: vi.fn(), open: vi.fn(), save: vi.fn(), loadCapabilities: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke, convertFileSrc: (path: string) => `asset:${path}` }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open, save }));
vi.mock('../capabilities', () => ({ loadCapabilities }));

import { workspaceClient, type ExportRequest } from './client';
import { PROVIDER_MAX_BYTES } from '../mobileProvider';

const stagedPath = '/app/cache/ocr-exports/inspection.txt';
const exported = { saved_path: stagedPath, bytes: 14, pages: 1, format: 'text' };
const exportRequest = (outputPath: string): ExportRequest => ({
    outputPath, format: 'text', pages: [], pdfa: false, overwrite: false,
    sourceUris: ['content://provider/Original%3ADocument'],
});
const decodeMetadata = (options: { headers: Record<string, string> }) => JSON.parse(
    new TextDecoder().decode(Uint8Array.from(atob(options.headers['x-ocr-metadata']), char => char.charCodeAt(0))),
);

describe('OCR workspace platform transport', () => {
    beforeEach(() => {
        invoke.mockReset(); open.mockReset(); save.mockReset(); loadCapabilities.mockReset();
        loadCapabilities.mockResolvedValue({ mobile: true });
    });

    it('imports the exact provider URI and displayed filename, including unknown provider size', async () => {
        const uri = 'content://documents/document/CaseSensitive%3AA%2FB';
        const imported = { source_uri: uri, name: 'Überprüfung_日本.TIFF', pages: [] };
        invoke.mockImplementation(async (command, args) => {
            if (command === 'plugin:mobile-provider|pick_files') return { cancelled: false, files: [{ uri, name: imported.name, size: -1 }] };
            if (command === 'plugin:mobile-provider|read_file') {
                expect(args).toEqual({ uri });
                return { data: 'AAH/Ag==' };
            }
            if (command === 'ocr_workspace_import_bytes') return imported;
            throw new Error(`Unexpected command: ${command}`);
        });
        expect(await workspaceClient.pickDocuments()).toEqual([uri]);
        expect(await workspaceClient.importDocument(uri)).toEqual(imported);
        const call = invoke.mock.calls.find(([command]) => command === 'ocr_workspace_import_bytes')!;
        expect(call[1]).toBeInstanceOf(Uint8Array);
        expect([...call[1]]).toEqual([0, 1, 255, 2]);
        expect(decodeMetadata(call[2])).toEqual({ name: imported.name, sourceUri: uri });
        expect(open).not.toHaveBeenCalled();
    });

    it('sends camera bytes as raw IPC and round-trips Unicode metadata without expanding numeric arrays', async () => {
        const bytes = new Uint8Array([19, 0, 128, 255, 23]).subarray(1, 4);
        const name = 'Prüfung_日本_📷.jpg';
        const result = { source_uri: '/workspace/photo.png', name, pages: [] };
        invoke.mockResolvedValue(result);
        expect(await workspaceClient.importImage!(name, bytes)).toEqual(result);
        const [command, payload, options] = invoke.mock.calls[0];
        expect(command).toBe('ocr_workspace_import_image');
        expect(payload).toBe(bytes);
        expect([...payload]).toEqual([0, 128, 255]);
        expect(decodeMetadata(options)).toEqual({ name });
        expect(loadCapabilities).not.toHaveBeenCalled();
    });

    it('rejects empty, oversized, and excessive-metadata imports before invoking native code', async () => {
        await expect(workspaceClient.importImage!('empty.jpg', new Uint8Array())).rejects.toThrow(/nonempty/);
        await expect(workspaceClient.importImage!('large.jpg', new Uint8Array(32 * 1024 * 1024 + 1))).rejects.toThrow(/32 MiB/);
        await expect(workspaceClient.importImage!('界'.repeat(5000) + '.jpg', new Uint8Array([1]))).rejects.toThrow(/identity is too long/);
        expect(invoke).not.toHaveBeenCalled();
    });

    it('rejects unsupported and oversized selections before reading provider bytes and reports cancellation', async () => {
        invoke.mockResolvedValueOnce({ cancelled: false, files: [{ uri: 'content://docs/large', name: 'large.pdf', size: PROVIDER_MAX_BYTES + 1 }] });
        await expect(workspaceClient.pickDocuments()).rejects.toThrow(/16 MiB/);
        invoke.mockResolvedValueOnce({ cancelled: false, files: [{ uri: 'content://docs/photo', name: 'photo.heic', size: 200 }] });
        await expect(workspaceClient.pickDocuments()).rejects.toThrow(/Unsupported OCR document/);
        invoke.mockResolvedValueOnce({ cancelled: true, files: [] });
        expect(await workspaceClient.pickDocuments()).toEqual([]);
        expect(invoke.mock.calls.every(([command]) => command === 'plugin:mobile-provider|pick_files')).toBe(true);
    });

    it('finishes the provider copy before reporting a mobile export and discards private staging', async () => {
        const commands: string[] = [];
        let release!: () => void;
        const copy = new Promise<void>(resolve => { release = resolve; });
        invoke.mockImplementation(async (command, args) => {
            commands.push(command);
            if (command === 'ocr_workspace_export_target') return stagedPath;
            if (command === 'ocr_workspace_export') return exported;
            if (command === 'plugin:mobile-provider|export_file') {
                expect(args).toEqual({ path: stagedPath, name: 'Inspection.txt' });
                await copy;
                return { cancelled: false, saved_path: 'content://documents/Export%3AInspection' };
            }
            if (command === 'ocr_workspace_discard_export') return null;
            throw new Error(`Unexpected command: ${command}`);
        });
        const path = await workspaceClient.pickExport('Inspection', 'txt');
        let finished = false;
        const result = workspaceClient.exportDocument(exportRequest(path!)).then(value => { finished = true; return value; });
        await Promise.resolve(); await Promise.resolve();
        expect(finished).toBe(false);
        expect(commands).toEqual(['ocr_workspace_export_target', 'ocr_workspace_export', 'plugin:mobile-provider|export_file']);
        release();
        expect(await result).toEqual({ ...exported, saved_path: 'content://documents/Export%3AInspection', cancelled: false });
        expect(commands.at(-1)).toBe('ocr_workspace_discard_export');
        expect(invoke).toHaveBeenCalledWith('ocr_workspace_export', exportRequest(stagedPath));
    });

    it('reports a cancelled mobile save without a success path and clears staging for text exports too', async () => {
        invoke.mockImplementation(async command => {
            if (command === 'ocr_workspace_export_target') return stagedPath;
            if (command === 'ocr_workspace_write_text') return { saved_path: stagedPath, bytes: 14 };
            if (command === 'plugin:mobile-provider|export_file') return { cancelled: true };
            if (command === 'ocr_workspace_discard_export') return null;
            throw new Error(`Unexpected command: ${command}`);
        });
        const path = await workspaceClient.pickExport('Field values', 'txt');
        expect(await workspaceClient.writeText(path!, 'Reviewed value', ['content://docs/source'])).toEqual({ saved_path: '', bytes: 14, cancelled: true });
        expect(invoke).toHaveBeenLastCalledWith('ocr_workspace_discard_export', { path: stagedPath });
    });

    it.each(['render', 'provider'])('cleans private staging on %s failure without hiding the original error', async failure => {
        invoke.mockImplementation(async command => {
            if (command === 'ocr_workspace_export_target') return stagedPath;
            if (command === 'ocr_workspace_export') {
                if (failure === 'render') throw new Error('Render failed');
                return exported;
            }
            if (command === 'plugin:mobile-provider|export_file') throw new Error('Provider copy failed');
            if (command === 'ocr_workspace_discard_export') throw new Error('Cleanup could not run');
            throw new Error(`Unexpected command: ${command}`);
        });
        const path = await workspaceClient.pickExport('Inspection', 'txt');
        await expect(workspaceClient.exportDocument(exportRequest(path!))).rejects.toThrow(failure === 'render' ? 'Render failed' : 'Provider copy failed');
        expect(invoke).toHaveBeenLastCalledWith('ocr_workspace_discard_export', { path: stagedPath });
        if (failure === 'render') expect(invoke.mock.calls.some(([command]) => command === 'plugin:mobile-provider|export_file')).toBe(false);
    });

    it('keeps desktop file dialogs and direct atomic exports on the desktop path', async () => {
        loadCapabilities.mockResolvedValue({ mobile: false });
        open.mockResolvedValue(['/scans/one.png', '/scans/two.pdf']);
        save.mockResolvedValue('/exports/Inspection.txt');
        invoke.mockResolvedValue({ ...exported, saved_path: '/exports/Inspection.txt' });
        expect(await workspaceClient.pickDocuments()).toEqual(['/scans/one.png', '/scans/two.pdf']);
        expect(await workspaceClient.pickExport('Inspection', 'txt')).toBe('/exports/Inspection.txt');
        await workspaceClient.exportDocument(exportRequest('/exports/Inspection.txt'));
        expect(invoke.mock.calls).toEqual([['ocr_workspace_export', exportRequest('/exports/Inspection.txt')]]);
        expect(open).toHaveBeenCalledOnce();
        expect(save).toHaveBeenCalledOnce();
    });
});
