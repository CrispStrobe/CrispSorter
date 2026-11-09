import { beforeEach, describe, expect, it, vi } from 'vitest';

const { invoke, open, save, loadCapabilities, listen } = vi.hoisted(() => ({
    invoke: vi.fn(), open: vi.fn(), save: vi.fn(), loadCapabilities: vi.fn(), listen: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke, convertFileSrc: (path: string) => `asset:${path}` }));
vi.mock('@tauri-apps/api/event', () => ({ listen }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open, save }));
vi.mock('../capabilities', () => ({ loadCapabilities }));

import { workspaceClient, importProgressLabel, exportProgressLabel, type ExportRequest, type ExportResult, type ExportProgress, type ModelDownloadProgress, type ImportProgress } from './client';
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
function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason: unknown) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

describe('OCR workspace platform transport', () => {
    beforeEach(() => {
        invoke.mockReset(); open.mockReset(); save.mockReset(); loadCapabilities.mockReset(); listen.mockReset();
        loadCapabilities.mockResolvedValue({ mobile: true });
    });

    it('passes the displayed version when removing a model and preserves stale-version failures', async () => {
        const report = { bytes: 12, reclaimable_bytes: 4, models: [], warnings: [] };
        invoke.mockResolvedValueOnce(report);
        expect(await workspaceClient.modelStorage!()).toEqual(report);
        expect(invoke).toHaveBeenLastCalledWith('ocr_workspace_model_storage');
        const cleaned = { removed_bytes: 4, removed_versions: 1, report };
        invoke.mockResolvedValueOnce(cleaned);
        expect(await workspaceClient.cleanModelStorage!()).toEqual(cleaned);
        expect(invoke).toHaveBeenLastCalledWith('ocr_workspace_clean_model_storage');
        const stale = new Error('This model changed after the storage view was opened.');
        invoke.mockRejectedValueOnce(stale);
        await expect(workspaceClient.removeMobileModel!('tesseract-eng', 'displayed-version')).rejects.toBe(stale);
        expect(invoke).toHaveBeenLastCalledWith('ocr_workspace_remove_mobile_model', { name: 'tesseract-eng', expectedVersion: 'displayed-version' });
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
        const copyStarted = deferred<void>();
        invoke.mockImplementation(async (command, args) => {
            commands.push(command);
            if (command === 'ocr_workspace_export_target') return stagedPath;
            if (command === 'ocr_workspace_export') return exported;
            if (command === 'plugin:mobile-provider|export_file') {
                expect(args).toEqual({ path: stagedPath, name: 'Inspection.txt' });
                copyStarted.resolve();
                await copy;
                return { cancelled: false, saved_path: 'content://documents/Export%3AInspection' };
            }
            if (command === 'ocr_workspace_discard_export') return null;
            throw new Error(`Unexpected command: ${command}`);
        });
        const path = await workspaceClient.pickExport('Inspection', 'txt');
        let finished = false;
        const result = workspaceClient.exportDocument(exportRequest(path!)).then(value => { finished = true; return value; });
        await copyStarted.promise;
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

    it('bounds long multilingual export names by UTF-8 bytes and rejects invalid extensions before invoking native code', async () => {
        const path = '/app/cache/ocr-exports/multilingual.pdf';
        invoke.mockImplementation(async command => {
            if (command === 'ocr_workspace_export_target') return path;
            if (command === 'ocr_workspace_export') return { ...exported, saved_path: path, format: 'pdf' };
            if (command === 'plugin:mobile-provider|export_file') return { cancelled: true };
            if (command === 'ocr_workspace_discard_export') return null;
            throw new Error(`Unexpected command: ${command}`);
        });
        expect(await workspaceClient.pickExport('報告📄'.repeat(200), 'pdf')).toBe(path);
        const filename = invoke.mock.calls[0][1].name as string;
        const bytes = new TextEncoder().encode(filename);
        expect(filename.endsWith('.pdf')).toBe(true);
        expect(bytes.byteLength).toBeLessThanOrEqual(240);
        expect(new TextEncoder().encode(filename.slice(0, -4)).byteLength).toBeLessThanOrEqual(200);
        expect(new TextDecoder().decode(bytes)).toBe(filename); // No truncated surrogate pair.
        expect(filename.startsWith('報告📄')).toBe(true);
        await workspaceClient.exportDocument({ ...exportRequest(path), format: 'pdf' });
        expect(invoke).toHaveBeenCalledWith('plugin:mobile-provider|export_file', { path, name: filename });
        invoke.mockClear();
        for (const extension of ['../pdf', '.pdf', 'pdf!', 'abcdefghijklmnop']) {
            await expect(workspaceClient.pickExport('Inspection', extension)).rejects.toThrow(/supported export format/);
        }
        expect(invoke).not.toHaveBeenCalled();
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

    it('subscribes before installing, correlates progress, and releases the listener after completion', async () => {
        let registered!: (unlisten: () => void) => void;
        let installed!: (names: string[]) => void;
        let invoked!: () => void;
        let receive!: (event: { payload: ModelDownloadProgress }) => void;
        const registration = new Promise<() => void>(resolve => { registered = resolve; });
        const nativeResult = new Promise<string[]>(resolve => { installed = resolve; });
        const nativeStarted = new Promise<void>(resolve => { invoked = resolve; });
        const unlisten = vi.fn();
        const progress = vi.fn();
        let requestId = '';
        listen.mockImplementation((event, handler) => {
            expect(event).toBe('crispembed-model-download');
            receive = handler;
            return registration;
        });
        invoke.mockImplementation((command, args) => {
            expect(command).toBe('ocr_workspace_install_mobile_models');
            expect(args.language).toBe('deu');
            expect(args.requestId).toMatch(/^[0-9a-f-]{36}$/i);
            requestId = args.requestId;
            invoked();
            return nativeResult;
        });
        const result = workspaceClient.installMobileModels!('deu', progress);
        expect(invoke).not.toHaveBeenCalled();
        registered(unlisten);
        await nativeStarted;
        const update: ModelDownloadProgress = {
            name: 'tesseract-deu', filename: 'tesseract-deu.gguf', phase: 'downloading',
            downloaded: 1048576, total: 2097152,
        };
        receive({ payload: update });
        receive({ payload: { ...update, request_id: 'another-install' } });
        expect(progress).not.toHaveBeenCalled();
        receive({ payload: { ...update, request_id: requestId } });
        expect(progress).toHaveBeenCalledOnce();
        expect(progress).toHaveBeenCalledWith({ ...update, request_id: requestId });
        expect(unlisten).not.toHaveBeenCalled();
        installed(['dbnet-det', 'tesseract-deu']);
        expect(await result).toEqual(['dbnet-det', 'tesseract-deu']);
        expect(unlisten).toHaveBeenCalledOnce();
        expect(invoke).toHaveBeenCalledOnce();
    });

    it('preserves the native install failure when listener teardown also fails', async () => {
        const failure = new Error('SHA-256 verification failed; previous installation preserved.');
        const unlisten = vi.fn(() => { throw new Error('Event bridge closed'); });
        listen.mockResolvedValue(unlisten);
        invoke.mockRejectedValue(failure);
        await expect(workspaceClient.installMobileModels!('eng')).rejects.toBe(failure);
        expect(unlisten).toHaveBeenCalledOnce();
    });

    it('does not start a model download when progress subscription fails', async () => {
        const failure = new Error('Event bridge unavailable');
        listen.mockRejectedValue(failure);
        await expect(workspaceClient.installMobileModels!('eng')).rejects.toBe(failure);
        expect(invoke).not.toHaveBeenCalled();
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

    it('uses actual platform codecs to enable PDFs and selectable phone formats', async () => {
        invoke.mockImplementation(async command => {
            if (command === 'ocr_workspace_capabilities') return { pdf_import: true, native_document_import: true, formats: ['text'] };
            if (command === 'plugin:mobile-provider|document_capabilities') return { pdf: true, image_formats: ['heic', 'heif', 'not-a-codec'] };
            if (command === 'plugin:mobile-provider|pick_files') return { cancelled: false, files: [{ uri: 'content://docs/heic', name: 'Capture.HEIC', size: 4096 }] };
            throw new Error(`Unexpected command: ${command}`);
        });
        expect(await workspaceClient.capabilities()).toMatchObject({ pdf_import: true, image_import_formats: ['heic', 'heif'] });
        expect(await workspaceClient.pickDocuments()).toEqual(['content://docs/heic']);
        invoke.mockImplementation(async command => {
            if (command === 'ocr_workspace_capabilities') return { pdf_import: true, native_document_import: true };
            throw new Error('Native bridge unavailable');
        });
        expect(await workspaceClient.capabilities()).toMatchObject({ pdf_import: false, image_import_formats: [], pdf_reason: expect.stringMatching(/unavailable/) });
    });

    it('does not advertise native image codecs on desktop', async () => {
        invoke.mockResolvedValue({ pdf_import: true, native_document_import: false });
        expect(await workspaceClient.capabilities()).toMatchObject({ pdf_import: true, image_import_formats: [] });
        expect(invoke).toHaveBeenCalledOnce();
    });

    it('correlates native import progress and waits for cancellation before releasing its listener', async () => {
        const controller = new AbortController();
        const onProgress = vi.fn(), unlisten = vi.fn();
        let receive!: (event: { payload: ImportProgress }) => void;
        let requestId = '';
        let finish!: (document: unknown) => void;
        let started!: () => void;
        const nativeStarted = new Promise<void>(resolve => { started = resolve; });
        const nativeResult = new Promise(resolve => { finish = resolve; });
        listen.mockImplementation(async (event, handler) => {
            expect(event).toBe('ocr-workspace-import'); receive = handler; return unlisten;
        });
        invoke.mockImplementation((command, _payload, headers) => {
            if (command === 'ocr_workspace_import_image') {
                expect(listen).toHaveBeenCalledOnce();
                requestId = decodeMetadata(headers).requestId;
                started(); return nativeResult;
            }
            if (command === 'ocr_workspace_cancel_import') return Promise.resolve();
            throw new Error(`Unexpected command: ${command}`);
        });
        const result = workspaceClient.importImage!('Photo.avif', new Uint8Array([1]), { signal: controller.signal, onProgress });
        const cancelled = expect(result).rejects.toMatchObject({ name: 'AbortError' });
        await nativeStarted;
        expect(requestId).toMatch(/^[0-9a-f-]{36}$/i);
        const progress: ImportProgress = { request_id: requestId, phase: 'rendering', completed: 1, total: 2 };
        receive({ payload: { ...progress, request_id: 'another-import' } });
        expect(onProgress).not.toHaveBeenCalled();
        receive({ payload: progress }); expect(onProgress).toHaveBeenCalledWith(progress);
        expect(importProgressLabel(progress)).toBe('Rendering pages: 1 of 2');
        controller.abort();
        expect(invoke).toHaveBeenCalledWith('ocr_workspace_cancel_import', { requestId });
        expect(unlisten).not.toHaveBeenCalled();
        receive({ payload: progress }); expect(onProgress).toHaveBeenCalledOnce();
        finish({ source_uri: '/archive/photo.avif', name: 'Photo.avif', pages: [] });
        await cancelled;
        expect(unlisten).toHaveBeenCalledOnce();
        receive({ payload: progress }); expect(onProgress).toHaveBeenCalledOnce();
    });

    it('avoids native work when stopped before subscription completes', async () => {
        const controller = new AbortController(); const unlisten = vi.fn();
        let subscribed!: (value: () => void) => void;
        listen.mockReturnValue(new Promise(resolve => { subscribed = resolve; }));
        const result = workspaceClient.importImage!('Photo.jpg', new Uint8Array([1]), { signal: controller.signal });
        controller.abort(); subscribed(unlisten);
        await expect(result).rejects.toMatchObject({ name: 'AbortError' });
        expect(invoke).not.toHaveBeenCalled(); expect(unlisten).toHaveBeenCalledOnce();
        await expect(workspaceClient.importImage!('Photo.jpg', new Uint8Array([1]), { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
        expect(listen).toHaveBeenCalledOnce();
    });

    it('preserves an import failure and removes its progress listener even when teardown fails', async () => {
        const failure = new Error('This PDF is password protected. Import an unlocked copy.');
        listen.mockResolvedValue(() => { throw new Error('Bridge closed'); });
        invoke.mockRejectedValue(failure);
        loadCapabilities.mockResolvedValue({ mobile: false });
        await expect(workspaceClient.importDocument('/scan/locked.pdf', { onProgress: vi.fn() })).rejects.toBe(failure);
    });

    it('subscribes before export, reports correlated byte progress, and ignores late or invalid events', async () => {
        const registration = deferred<() => void>(), nativeStarted = deferred<void>(), native = deferred<ExportResult>();
        const unlisten = vi.fn(), onProgress = vi.fn();
        let receive!: (event: { payload: ExportProgress }) => void;
        let requestId = '';
        listen.mockImplementation((event, handler) => {
            expect(event).toBe('ocr-workspace-export'); receive = handler; return registration.promise;
        });
        invoke.mockImplementation((command, args) => {
            expect(command).toBe('ocr_workspace_export'); requestId = args.requestId;
            expect(args).toEqual({ ...exportRequest('/exports/reviewed.txt'), requestId });
            nativeStarted.resolve(); return native.promise;
        });
        const result = workspaceClient.exportDocument(exportRequest('/exports/reviewed.txt'), { onProgress });
        expect(invoke).not.toHaveBeenCalled();
        registration.resolve(unlisten); await nativeStarted.promise;
        expect(requestId).toMatch(/^[0-9a-f-]{36}$/i);
        const progress: ExportProgress = { request_id: requestId, phase: 'writing', bytes: 1048576 };
        receive({ payload: { ...progress, request_id: 'another-export' } });
        receive({ payload: { ...progress, bytes: Number.NaN } });
        receive({ payload: { ...progress, bytes: -1 } });
        receive({ payload: { ...progress, phase: 'destination' } });
        expect(onProgress).not.toHaveBeenCalled();
        receive({ payload: progress });
        expect(onProgress).toHaveBeenCalledWith(progress);
        expect(exportProgressLabel(progress)).toBe('Writing export · 1.0 MiB');
        expect(unlisten).not.toHaveBeenCalled();
        native.resolve(exported); expect(await result).toBe(exported);
        expect(unlisten).toHaveBeenCalledOnce();
        receive({ payload: progress }); expect(onProgress).toHaveBeenCalledOnce();
    });

    it('waits for native cancellation before releasing its export listener', async () => {
        const controller = new AbortController(), nativeStarted = deferred<void>(), native = deferred<ExportResult>();
        const unlisten = vi.fn(); let requestId = '';
        listen.mockResolvedValue(unlisten);
        invoke.mockImplementation((command, args) => {
            if (command === 'ocr_workspace_export') { requestId = args.requestId; nativeStarted.resolve(); return native.promise; }
            if (command === 'ocr_workspace_cancel_export') return Promise.resolve(true);
            throw new Error(`Unexpected command: ${command}`);
        });
        const result = workspaceClient.exportDocument(exportRequest('/exports/reviewed.txt'), { signal: controller.signal });
        const cancelled = expect(result).rejects.toMatchObject({ name: 'AbortError', message: 'Document export cancelled.' });
        await nativeStarted.promise; controller.abort();
        expect(invoke).toHaveBeenCalledWith('ocr_workspace_cancel_export', { requestId });
        expect(unlisten).not.toHaveBeenCalled();
        native.reject(new Error('OCR_EXPORT_CANCELLED'));
        await cancelled; expect(unlisten).toHaveBeenCalledOnce();
    });

    it.each(['commit-event', 'cancel-refused'])('preserves successful publication when abort loses the %s race', async race => {
        const controller = new AbortController(), nativeStarted = deferred<void>(), native = deferred<ExportResult>();
        const unlisten = vi.fn(), onProgress = vi.fn();
        let receive!: (event: { payload: ExportProgress }) => void;
        let requestId = '';
        listen.mockImplementation(async (_event, handler) => { receive = handler; return unlisten; });
        invoke.mockImplementation((command, args) => {
            if (command === 'ocr_workspace_export') { requestId = args.requestId; nativeStarted.resolve(); return native.promise; }
            if (command === 'ocr_workspace_cancel_export') return Promise.resolve(false);
            throw new Error(`Unexpected command: ${command}`);
        });
        const result = workspaceClient.exportDocument(exportRequest('/exports/reviewed.txt'), { signal: controller.signal, onProgress });
        await nativeStarted.promise;
        receive({ payload: { request_id: requestId, phase: race === 'commit-event' ? 'committing' : 'writing', bytes: 4096 } });
        controller.abort(); await Promise.resolve();
        expect(onProgress).toHaveBeenLastCalledWith({ request_id: requestId, phase: 'committing', bytes: 4096 });
        expect(invoke.mock.calls.filter(([command]) => command === 'ocr_workspace_cancel_export')).toHaveLength(race === 'commit-event' ? 0 : 1);
        const notifications = onProgress.mock.calls.length;
        receive({ payload: { request_id: requestId, phase: 'writing', bytes: 2048 } });
        expect(onProgress).toHaveBeenCalledTimes(notifications);
        expect(unlisten).not.toHaveBeenCalled();
        native.resolve(exported);
        expect(await result).toBe(exported);
        expect(controller.signal.aborted).toBe(true);
        expect(unlisten).toHaveBeenCalledOnce();
    });

    it.each([
        'OCR_EXPORT_CANCELLED',
        'Render OCR PDF: OCR_EXPORT_CANCELLED',
        'Write OCR export: OCR_EXPORT_CANCELLED',
        'Flush OCR export: OCR_EXPORT_CANCELLED',
    ])('recognizes only the exact native cancellation cause %s', async cause => {
        listen.mockResolvedValue(vi.fn());
        for (const failure of [cause, new Error(cause)]) {
            invoke.mockRejectedValueOnce(failure);
            await expect(workspaceClient.exportDocument(exportRequest('/exports/reviewed.txt'), { onProgress: vi.fn() })).rejects.toMatchObject({ name: 'AbortError' });
        }
    });

    it.each([false, true])('preserves marker-like filenames and native error identity with tracked export %s', async tracked => {
        listen.mockResolvedValue(vi.fn());
        for (const cause of [
            'Output already exists: OCR_EXPORT_CANCELLED',
            'Output already exists: /exports/OCR_EXPORT_CANCELLED.pdf. Choose another name or explicitly confirm replacement.',
            'Render OCR PDF: unsupported value OCR_EXPORT_CANCELLED',
            'Read source: OCR_EXPORT_CANCELLED',
        ]) {
            for (const failure of [cause, new Error(cause)]) {
                invoke.mockRejectedValueOnce(failure);
                await expect(workspaceClient.exportDocument(exportRequest('/exports/reviewed.txt'), tracked ? { onProgress: vi.fn() } : undefined)).rejects.toBe(failure);
            }
        }
    });

    it('retains a native export failure even after abort and failed cancellation or listener teardown', async () => {
        const controller = new AbortController(), nativeStarted = deferred<void>(), native = deferred<ExportResult>();
        const failure = new Error('Output volume is full. The existing document is unchanged.');
        const unlisten = vi.fn(() => { throw new Error('Event bridge closed'); });
        listen.mockResolvedValue(unlisten);
        invoke.mockImplementation(command => {
            if (command === 'ocr_workspace_export') { nativeStarted.resolve(); return native.promise; }
            if (command === 'ocr_workspace_cancel_export') return Promise.reject(new Error('Stop command unavailable'));
            throw new Error(`Unexpected command: ${command}`);
        });
        const result = workspaceClient.exportDocument(exportRequest('/exports/reviewed.txt'), { signal: controller.signal });
        const failed = expect(result).rejects.toBe(failure);
        await nativeStarted.promise; controller.abort(); native.reject(failure);
        await failed; expect(unlisten).toHaveBeenCalledOnce();
    });

    it.each(['before-subscribe', 'during-subscribe'])('cleans mobile staging without starting an export aborted %s', async timing => {
        const controller = new AbortController(), registration = deferred<() => void>();
        const unlisten = vi.fn();
        listen.mockReturnValue(registration.promise);
        invoke.mockImplementation(async command => {
            if (command === 'ocr_workspace_export_target') return stagedPath;
            if (command === 'ocr_workspace_discard_export') return null;
            throw new Error(`Unexpected command: ${command}`);
        });
        const path = await workspaceClient.pickExport('Inspection', 'txt');
        if (timing === 'before-subscribe') controller.abort();
        const result = workspaceClient.exportDocument(exportRequest(path!), { signal: controller.signal });
        if (timing === 'during-subscribe') { controller.abort(); registration.resolve(unlisten); }
        await expect(result).rejects.toMatchObject({ name: 'AbortError' });
        expect(listen).toHaveBeenCalledTimes(timing === 'before-subscribe' ? 0 : 1);
        expect(unlisten).toHaveBeenCalledTimes(timing === 'before-subscribe' ? 0 : 1);
        expect(invoke.mock.calls.map(([command]) => command)).toEqual(['ocr_workspace_export_target', 'ocr_workspace_discard_export']);
    });

    it('preserves a failed export subscription and cleans its private staging destination', async () => {
        const failure = new Error('Export progress subscription failed');
        listen.mockRejectedValue(failure);
        invoke.mockImplementation(async command => {
            if (command === 'ocr_workspace_export_target') return stagedPath;
            if (command === 'ocr_workspace_discard_export') throw new Error('Cleanup failed too');
            throw new Error(`Unexpected command: ${command}`);
        });
        const path = await workspaceClient.pickExport('Inspection', 'txt');
        await expect(workspaceClient.exportDocument(exportRequest(path!), { onProgress: vi.fn() })).rejects.toBe(failure);
        expect(invoke.mock.calls.map(([command]) => command)).toEqual(['ocr_workspace_export_target', 'ocr_workspace_discard_export']);
    });

    it.each(['after-render', 'destination-callback'])('skips the mobile picker for an abort %s and removes the completed private stage', async timing => {
        const controller = new AbortController(), unlisten = vi.fn();
        const phases: string[] = [];
        listen.mockResolvedValue(unlisten);
        invoke.mockImplementation(async command => {
            if (command === 'ocr_workspace_export_target') return stagedPath;
            if (command === 'ocr_workspace_export') {
                if (timing === 'after-render') controller.abort();
                return exported;
            }
            if (command === 'ocr_workspace_cancel_export') return false;
            if (command === 'ocr_workspace_discard_export') { expect(unlisten).toHaveBeenCalledOnce(); return null; }
            throw new Error(`Unexpected command: ${command}`);
        });
        const path = await workspaceClient.pickExport('Inspection', 'txt');
        const result = await workspaceClient.exportDocument(exportRequest(path!), {
            signal: controller.signal, onProgress: progress => {
                phases.push(progress.phase);
                if (progress.phase === 'destination' && timing === 'destination-callback') controller.abort();
            },
        });
        expect(result).toEqual({ ...exported, saved_path: '', cancelled: true });
        expect(phases.includes('destination')).toBe(timing === 'destination-callback');
        expect(invoke.mock.calls.some(([command]) => command === 'plugin:mobile-provider|export_file')).toBe(false);
        expect(invoke).toHaveBeenLastCalledWith('ocr_workspace_discard_export', { path: stagedPath });
    });

    it('hands mobile destination control to the OS and keeps staging until the provider copy finishes', async () => {
        const controller = new AbortController(), copyStarted = deferred<void>();
        const provider = deferred<{ cancelled: boolean; saved_path: string }>();
        const unlisten = vi.fn(), onProgress = vi.fn();
        listen.mockResolvedValue(unlisten);
        invoke.mockImplementation((command, args) => {
            if (command === 'ocr_workspace_export_target') return Promise.resolve(stagedPath);
            if (command === 'ocr_workspace_export') return Promise.resolve(exported);
            if (command === 'plugin:mobile-provider|export_file') {
                expect(args).toEqual({ path: stagedPath, name: 'Inspection.txt' });
                expect(onProgress).toHaveBeenLastCalledWith({ request_id: expect.any(String), phase: 'destination', bytes: 14 });
                expect(unlisten).toHaveBeenCalledOnce(); copyStarted.resolve(); return provider.promise;
            }
            if (command === 'ocr_workspace_discard_export') return Promise.resolve();
            throw new Error(`Unexpected command: ${command}`);
        });
        const path = await workspaceClient.pickExport('Inspection', 'txt');
        const result = workspaceClient.exportDocument(exportRequest(path!), { signal: controller.signal, onProgress });
        await copyStarted.promise; controller.abort();
        expect(invoke.mock.calls.some(([command]) => command === 'ocr_workspace_cancel_export')).toBe(false);
        expect(invoke.mock.calls.some(([command]) => command === 'ocr_workspace_discard_export')).toBe(false);
        provider.resolve({ cancelled: false, saved_path: 'content://documents/Reviewed%3AInspection' });
        expect(await result).toEqual({ ...exported, saved_path: 'content://documents/Reviewed%3AInspection', cancelled: false });
        expect(invoke).toHaveBeenLastCalledWith('ocr_workspace_discard_export', { path: stagedPath });
    });
});
