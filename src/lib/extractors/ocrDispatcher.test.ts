import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke, isTauri } from '@tauri-apps/api/core';
import Tesseract from 'tesseract.js';
import { getSetting, getStore } from '../store';
import { flog } from '../log';
import { platform } from '../capabilities';
import { createOcrSession, disposeOcrWorkers, MAX_OCR_IMAGE_BYTES, OCR_WORKER_IDLE_MS } from './ocrDispatcher';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(), isTauri: vi.fn() }));
vi.mock('../capabilities', () => ({ platform: vi.fn(() => 'linux') }));
vi.mock('../store', () => ({ getSetting: vi.fn(), getStore: vi.fn() }));
vi.mock('tesseract.js', () => ({ default: { createWorker: vi.fn() } }));
vi.mock('../log', () => ({ flog: vi.fn() }));

let saved: Record<string, unknown>;
let workers: { recognize: ReturnType<typeof vi.fn>; terminate: ReturnType<typeof vi.fn> }[];
const input = new Uint8Array([137, 80, 78, 71]);
const metadata = (index = 0) => JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(
    (vi.mocked(invoke).mock.calls[index][2] as { headers: Record<string, string> }).headers['x-ocr-metadata'],
), character => character.charCodeAt(0))));

beforeEach(() => {
    vi.mocked(flog).mockClear();
    vi.mocked(platform).mockReturnValue('linux');
    saved = {}; workers = [];
    vi.mocked(isTauri).mockReset().mockReturnValue(true);
    vi.mocked(invoke).mockReset().mockResolvedValue({ text: ' Native result ', engine: 'selected-native', warnings: [] });
    vi.mocked(getSetting).mockReset().mockImplementation(async (key, fallback) => saved[key] ?? fallback);
    vi.mocked(getStore).mockReset().mockResolvedValue({ get: async (key: string) => saved[key] } as Awaited<ReturnType<typeof getStore>>);
    vi.mocked(Tesseract.createWorker).mockReset().mockImplementation(async () => {
        const worker = { recognize: vi.fn().mockResolvedValue({ data: { text: ' Browser result ' } }), terminate: vi.fn().mockResolvedValue(undefined) };
        workers.push(worker);
        return worker as unknown as Tesseract.Worker;
    });
});
afterEach(async () => { await disposeOcrWorkers(); vi.useRealTimers(); });

describe('native OCR dispatch', () => {
    it('uses base64 for Android image extraction while preserving Unicode metadata and operation headers', async () => {
        vi.mocked(platform).mockReturnValue('android');
        const bytes = Uint8Array.from({ length: 24 * 1024 + 3 }, (_, i) => i % 256).subarray(1);
        const session = await createOcrSession({ ocr: { engine: 'tesseract', language: 'deu' } });
        await session.recognize(bytes, 'Überblick_日本_📷.png');
        const [command, payload, options] = vi.mocked(invoke).mock.calls[0];
        expect(command).toBe('ocr_extract_image');
        const encoded = payload as { base64: string };
        expect(Object.keys(encoded)).toEqual(['base64']);
        expect(Uint8Array.from(atob(encoded.base64), c => c.charCodeAt(0))).toEqual(bytes);
        expect(metadata()).toMatchObject({ name: 'Überblick_日本_📷.png', options: { engine: 'tesseract', language: 'deu' } });
        expect((options as { headers: Record<string, string> }).headers['x-operation-id']).toBeTruthy();
    });

    it.each(['auto', 'tesseract', 'system_tesseract', 'ocrs', 'paddle', 'dbnet_trocr', 'surya', 'got', 'glm', 'qwen2vl',
        'internvl2', 'parseq', 'deepseek_ocr2', 'pix2struct', 'granite_vision', 'lightonocr', 'qwen3vl',
        'unlimited_ocr', 'minicpm_ocr', 'unified', 'tesseract_fraktur', 'ppocrv6', 'easyocr', 'olmocr', 'lfm2_vl'])(
        'forwards selected engine %s through the same binary endpoint', async engine => {
            const session = await createOcrSession({ ocr: { engine, language: 'fra' } });
            expect(await session.recognize(input, 'Überblick 日本語.png')).toEqual({ text: 'Native result', engine: 'selected-native', warnings: [] });
            expect(invoke).toHaveBeenCalledOnce();
            expect(vi.mocked(invoke).mock.calls[0][0]).toBe('ocr_extract_image');
            expect(vi.mocked(invoke).mock.calls[0][1]).toBe(input);
            expect(metadata()).toMatchObject({ name: 'Überblick 日本語.png', options: { engine, language: 'fra' } });
            const id = (vi.mocked(invoke).mock.calls[0][2] as { headers: Record<string, string> }).headers['x-operation-id'];
            expect(id).toMatch(/^fe-[a-z0-9]+-[a-z0-9]+$/);
            const receipts = vi.mocked(flog).mock.calls.map(([, message]) => message).filter(message => message.includes(`operation_id=${id} `));
            expect(receipts.at(-1)).toContain('phase=completed');
            expect(receipts.join('\n')).not.toMatch(/Überblick|Native result/);
            expect(Tesseract.createWorker).not.toHaveBeenCalled();
        },
    );

    it('captures one immutable saved policy for successive pages and carries custom stages/CPU budget', async () => {
        saved = { ocrEngine: 'qwen2vl', ocrLanguage: 'deu', ocrCpuPixelBudget: true, ocrPipelineEnabled: true,
            ocrPipelineAdvanced: true, ocrStages: [{ engine: 'qwen2vl', rec_model: '/models/custom.gguf' }] };
        const session = await createOcrSession();
        await session.recognize(input, 'first.png');
        saved.ocrEngine = 'got'; saved.ocrLanguage = 'eng';
        await session.recognize(input, 'second.png');
        for (let i = 0; i < 2; i++) expect(metadata(i).options).toMatchObject({ engine: 'qwen2vl', language: 'deu', cpuPixelBudget: true,
            config: { engine: 'qwen2vl', language: 'deu', cpu_pixel_budget: true, stages: [{ rec_model: '/models/custom.gguf' }] } });
    });

    it('uses the interface fallback only for unconfigured embedded recognition', async () => {
        saved.language = 'de';
        const defaultSession = await createOcrSession();
        await defaultSession.recognize(input, 'default.png');
        expect(metadata().options).toMatchObject({ engine: 'auto', language: 'deu', config: { enabled: false, stages: [] } });
        const system = await createOcrSession({ ocr: { engine: 'system_tesseract' } });
        await system.recognize(input, 'system.png');
        expect(metadata(1).options.language).toBe('auto');
    });

    it('propagates native failure without substituting a browser engine', async () => {
        const failure = new Error('Selected model is unavailable');
        vi.mocked(invoke).mockRejectedValue(failure);
        const session = await createOcrSession({ ocr: { engine: 'got' } });
        await expect(session.recognize(input, 'page.png')).rejects.toBe(failure);
        expect(Tesseract.createWorker).not.toHaveBeenCalled();
    });

    it('rejects malformed native results instead of reporting empty success', async () => {
        vi.mocked(invoke).mockResolvedValue({ engine: 'got', warnings: [] });
        const session = await createOcrSession();
        await expect(session.recognize(input, 'page.png')).rejects.toThrow('invalid result');
    });

    it('rejects empty/oversized images and oversized metadata before invoking native OCR', async () => {
        const session = await createOcrSession();
        await expect(session.recognize(new Uint8Array(), 'empty.png')).rejects.toThrow('nonempty image');
        await expect(session.recognize(new Blob([new Uint8Array(MAX_OCR_IMAGE_BYTES + 1)]), 'large.png')).rejects.toThrow('32 MiB');
        await expect(session.recognize(input, 'x'.repeat(64 * 1024))).rejects.toThrow('metadata limit');
        expect(invoke).not.toHaveBeenCalled();
    });

    it('does not start cancelled work and promptly discards a late native response', async () => {
        const controller = new AbortController();
        controller.abort();
        await expect(createOcrSession({ signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
        expect(getSetting).not.toHaveBeenCalled();
        const active = new AbortController();
        let finish!: (value: unknown) => void;
        vi.mocked(invoke).mockImplementation(() => new Promise(resolve => { finish = resolve; }));
        const session = await createOcrSession({ signal: active.signal });
        const result = session.recognize(input, 'page.png');
        const stopped = expect(result).rejects.toThrow('Stop this document');
        await vi.waitFor(() => expect(invoke).toHaveBeenCalledOnce());
        active.abort(new Error('Stop this document'));
        await stopped;
        finish({ text: 'late', engine: 'got', warnings: [] });
    });
});

describe('explicit browser capability and worker lifecycle', () => {
    beforeEach(() => { vi.mocked(isTauri).mockReturnValue(false); });

    it('truthfully reports Tesseract.js and reuses the worker between images', async () => {
        const first = await createOcrSession(); const second = await createOcrSession();
        expect(await first.recognize(input, 'one.png')).toEqual({ text: 'Browser result', engine: 'tesseract_js', warnings: [] });
        await second.recognize(input, 'two.png');
        expect(Tesseract.createWorker).toHaveBeenCalledExactlyOnceWith('eng');
        expect(workers[0].recognize).toHaveBeenCalledTimes(2);
        expect(invoke).not.toHaveBeenCalled(); expect(getSetting).not.toHaveBeenCalled();
    });

    it('serializes concurrent jobs on the reused worker', async () => {
        const session = await createOcrSession();
        await session.recognize(input, 'warmup.png');
        let finish!: (value: unknown) => void;
        workers[0].recognize.mockClear().mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
        const first = session.recognize(input, 'first.png');
        const second = session.recognize(input, 'second.png');
        await vi.waitFor(() => expect(workers[0].recognize).toHaveBeenCalledTimes(1));
        finish({ data: { text: 'first' } });
        expect((await first).text).toBe('first'); expect((await second).text).toBe('Browser result');
        expect(Tesseract.createWorker).toHaveBeenCalledOnce();
    });

    it('releases the previous language worker before creating another language worker', async () => {
        await (await createOcrSession({ ocr: { language: 'eng+deu' } })).recognize(input, 'one.png');
        await (await createOcrSession({ ocr: { language: 'fra' } })).recognize(input, 'two.png');
        expect(Tesseract.createWorker).toHaveBeenNthCalledWith(1, 'eng+deu');
        expect(Tesseract.createWorker).toHaveBeenNthCalledWith(2, 'fra');
        expect(workers[0].terminate).toHaveBeenCalledOnce();
        expect(workers[1].terminate).not.toHaveBeenCalled();
    });

    it('releases idle workers without a permanent per-language cache', async () => {
        vi.useFakeTimers();
        const session = await createOcrSession();
        await session.recognize(input, 'one.png');
        await vi.advanceTimersByTimeAsync(OCR_WORKER_IDLE_MS);
        expect(workers[0].terminate).toHaveBeenCalledOnce();
        await session.recognize(input, 'two.png');
        expect(Tesseract.createWorker).toHaveBeenCalledTimes(2);
    });

    it.each([
        { engine: 'tesseract' }, { engine: 'system_tesseract' }, { engine: 'got' },
        { cpuPixelBudget: true }, { ocrTier: 'tier2' }, { ocrRecLang: 'cjk' },
        { config: { enabled: true } }, { config: { enabled: false, stages: [{ engine: 'got' }] } },
        { config: { denoise: true } },
    ])('rejects unsupported selected native policy %j instead of substituting', async ocr => {
        const session = await createOcrSession({ ocr });
        await expect(session.recognize(input, 'one.png')).rejects.toThrow(/native app|native OCR/);
        expect(Tesseract.createWorker).not.toHaveBeenCalled(); expect(invoke).not.toHaveBeenCalled();
    });

    it('terminates an actively cancelled worker and allows the next job to start', async () => {
        const controller = new AbortController();
        const session = await createOcrSession({ signal: controller.signal });
        await session.recognize(input, 'warmup.png');
        workers[0].recognize.mockImplementationOnce(() => new Promise(() => {}));
        const pending = session.recognize(input, 'slow.png');
        const stopped = expect(pending).rejects.toMatchObject({ name: 'AbortError' });
        await vi.waitFor(() => expect(workers[0].recognize).toHaveBeenCalledTimes(2));
        controller.abort(); await stopped;
        await (await createOcrSession()).recognize(input, 'next.png');
        expect(workers[0].terminate).toHaveBeenCalledOnce();
        expect(Tesseract.createWorker).toHaveBeenCalledTimes(2);
    });

    it('cancels queued work without terminating another document\'s active worker', async () => {
        const session = await createOcrSession();
        await session.recognize(input, 'warmup.png');
        let finish!: (value: unknown) => void;
        workers[0].recognize.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
        const first = session.recognize(input, 'active.png');
        await vi.waitFor(() => expect(workers[0].recognize).toHaveBeenCalledTimes(2));
        const controller = new AbortController();
        const queued = await createOcrSession({ signal: controller.signal });
        const stopped = expect(queued.recognize(input, 'queued.png')).rejects.toMatchObject({ name: 'AbortError' });
        controller.abort(); await stopped;
        expect(workers[0].terminate).not.toHaveBeenCalled();
        finish({ data: { text: 'active completed' } }); await first;
        await session.recognize(input, 'next.png');
        expect(Tesseract.createWorker).toHaveBeenCalledOnce();
    });

    it('evicts a failed worker and does not poison the queue', async () => {
        const session = await createOcrSession();
        await session.recognize(input, 'warmup.png');
        workers[0].recognize.mockRejectedValueOnce(new Error('Worker crashed'));
        await expect(session.recognize(input, 'broken.png')).rejects.toThrow('Worker crashed');
        await session.recognize(input, 'recovery.png');
        expect(workers[0].terminate).toHaveBeenCalledOnce();
        expect(Tesseract.createWorker).toHaveBeenCalledTimes(2);
    });

    it('still releases the worker when the last queued document was cancelled', async () => {
        vi.useFakeTimers();
        const session = await createOcrSession(); await session.recognize(input, 'warmup.png');
        let finish!: (value: unknown) => void;
        workers[0].recognize.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
        const first = session.recognize(input, 'active.png');
        await vi.waitFor(() => expect(workers[0].recognize).toHaveBeenCalledTimes(2));
        const controller = new AbortController();
        const queued = await createOcrSession({ signal: controller.signal });
        const stopped = expect(queued.recognize(input, 'cancelled.png')).rejects.toMatchObject({ name: 'AbortError' });
        controller.abort(); await stopped; finish({ data: { text: 'complete' } }); await first;
        await vi.advanceTimersByTimeAsync(OCR_WORKER_IDLE_MS);
        expect(workers[0].terminate).toHaveBeenCalledOnce();
    });
});
