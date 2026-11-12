import { invoke, isTauri } from '@tauri-apps/api/core';
import type Tesseract from 'tesseract.js';
import { loadSavedOcrPolicy, ocrPolicyLanguage, type OcrPolicy, type OcrSelection } from './ocrSettings';
import { beginOperation, type OperationLog } from '../operationLog';
import { ensureOcrModels, modelGate, type EnsureResult } from '../ocr/modelGate';
import { plannedFromError, type PlannedModel } from '../ocr/modelManager';

export const MAX_OCR_IMAGE_BYTES = 32 * 1024 * 1024;
export const MAX_OCR_METADATA_BYTES = 64 * 1024;
export const OCR_WORKER_IDLE_MS = 30_000;
export interface OcrTextResult { text: string; engine: string; warnings: string[]; }
export interface OcrSessionOptions { ocr?: OcrSelection; signal?: AbortSignal; }
export type OcrImage = Uint8Array | Blob | HTMLCanvasElement;
export interface OcrSession {
    readonly policy: OcrPolicy;
    recognize(image: OcrImage, name: string): Promise<OcrTextResult>;
}

/** Reject promptly while retaining handlers on a late native/PDF result. */
export function withExtractionAbort<T>(work: Promise<T>, signal?: AbortSignal, cancel?: () => void): Promise<T> {
    if (!signal) return work;
    return new Promise<T>((resolve, reject) => {
        const aborted = () => {
            try { cancel?.(); } catch { /* Preserve the cancellation reason. */ }
            reject(signal.reason ?? new DOMException('Extraction cancelled.', 'AbortError'));
        };
        if (signal.aborted) aborted();
        else signal.addEventListener('abort', aborted, { once: true });
        work.then(resolve, reject).finally(() => signal.removeEventListener('abort', aborted));
    });
}

interface BrowserWorker { language: string; promise: Promise<Tesseract.Worker>; }
let browserWorker: BrowserWorker | undefined;
let workerQueue: Promise<void> = Promise.resolve();
let idleTimer: ReturnType<typeof setTimeout> | undefined;

function clearIdleTimer() {
    if (idleTimer !== undefined) clearTimeout(idleTimer);
    idleTimer = undefined;
}

async function retireWorker(worker: BrowserWorker) {
    const operation = beginOperation('ocr.worker', { backend: 'browser', language: worker.language });
    if (browserWorker === worker) browserWorker = undefined;
    try { await (await worker.promise).terminate(); operation.complete(); }
    catch (error) { operation.fail(error); /* Failed initialization/termination leaves no reusable worker. */ }
}

function queueWorker<T>(task: () => Promise<T>): Promise<T> {
    clearIdleTimer();
    const result = workerQueue.then(() => { clearIdleTimer(); return task(); });
    workerQueue = result.then(() => undefined, () => undefined);
    return result;
}

/** Release the shared browser worker at shutdown; normal jobs retain it briefly. */
export function disposeOcrWorkers(): Promise<void> {
    return queueWorker(async () => {
        if (browserWorker) await retireWorker(browserWorker);
    });
}

function scheduleIdleCleanup() {
    clearIdleTimer();
    idleTimer = setTimeout(() => { void disposeOcrWorkers(); }, OCR_WORKER_IDLE_MS);
    // Node test/tool clients should not be kept alive by the browser cache.
    (idleTimer as ReturnType<typeof setTimeout> & { unref?: () => void }).unref?.();
}

function browserLanguage(policy: OcrPolicy): string {
    if (policy.engine !== 'auto') throw new Error(`OCR engine '${policy.engine}' requires the native app. Browser OCR supports Automatic using Tesseract.js only.`);
    if (policy.cpuPixelBudget || policy.ocrTier !== 'auto' || policy.ocrRecLang !== 'auto') {
        throw new Error('The selected native OCR tier, script model, or CPU profile is unavailable in a browser. Use the native app or choose Automatic browser OCR.');
    }
    const config = policy.config;
    if (config && (config.enabled === true || (Array.isArray(config.stages) ? config.stages.length > 0 : config.stages != null)
        || Object.keys(config).some(key => !['enabled', 'stages', 'engine', 'language', 'cpu_pixel_budget'].includes(key)))) {
        throw new Error('The selected OCR pipeline requires the native app. Browser OCR cannot run its stages or preprocessing.');
    }
    const language = policy.language === 'auto' ? policy.defaultLanguage : policy.language;
    if (!/^[a-z][a-z0-9_]*(?:\+[a-z][a-z0-9_]*)*$/i.test(language)) throw new Error('Browser OCR needs Tesseract language codes such as eng, deu, or eng+deu.');
    return language;
}

async function recognizeBrowser(image: OcrImage, policy: OcrPolicy, signal?: AbortSignal, operation?: OperationLog): Promise<OcrTextResult> {
    const language = browserLanguage(policy);
    operation?.stage('queued');
    return withExtractionAbort(queueWorker(async () => {
        let owner: BrowserWorker | undefined;
        try {
            signal?.throwIfAborted();
            let worker = browserWorker;
            if (worker && worker.language !== language) { await retireWorker(worker); worker = undefined; }
            if (!worker) {
                operation?.stage('loading', { language }, 'info');
                worker = { language, promise: import('tesseract.js').then(module => module.default.createWorker(language)) };
                browserWorker = worker;
            } else operation?.stage('reused', { reused: true });
            owner = worker;
            const cancel = () => { void retireWorker(worker); };
            const ready = await withExtractionAbort(owner.promise, signal, cancel);
            signal?.throwIfAborted();
            operation?.stage('recognizing');
            const input = image instanceof Uint8Array ? new Blob([image]) : image;
            const result = await withExtractionAbort(ready.recognize(input, {}, { text: true }), signal, cancel);
            signal?.throwIfAborted();
            if (typeof result.data?.text !== 'string') throw new Error('Browser OCR returned an invalid text result.');
            return { text: result.data.text.trim(), engine: 'tesseract_js', warnings: [] };
        } catch (error) {
            if (owner && browserWorker === owner) void retireWorker(owner);
            throw error;
        } finally { scheduleIdleCleanup(); }
    }), signal);
}

async function imageBytes(image: OcrImage): Promise<Uint8Array> {
    if (image instanceof Uint8Array) return image;
    const blob = image instanceof Blob ? image : await new Promise<Blob>((resolve, reject) => {
        image.toBlob(value => value ? resolve(value) : reject(new Error('Could not encode the PDF page for OCR.')), 'image/png');
    });
    if (!blob.size || blob.size > MAX_OCR_IMAGE_BYTES) throw new Error('OCR needs a nonempty image no larger than 32 MiB.');
    return new Uint8Array(await blob.arrayBuffer());
}

/** A native OCR session found models that are not installed, and the user
 * did not download them. Callers can pause instead of failing every file. */
export class OcrModelsMissingError extends Error {
    readonly models: PlannedModel[];
    constructor(models: PlannedModel[]) {
        super(`OCR models are not installed: ${models.map(m => m.name).join(', ')}. Download them when asked or in Settings > OCR models.`);
        this.name = 'OcrModelsMissingError';
        this.models = models;
    }
}

/** Inbox/batch: check the saved OCR policy's models once per run. */
export async function ensureInboxOcrModels(context: string): Promise<EnsureResult> {
    if (!isTauri() || !modelGate.available) return { ok: true, plan: null, missing: [] };
    try {
        const policy = await loadSavedOcrPolicy(undefined, true);
        return await ensureOcrModels({ surface: 'extract', ...nativeOptions(policy) }, { context, rememberDecline: false });
    } catch {
        return { ok: true, plan: null, missing: [] };
    }
}

function nativeOptions(policy: OcrPolicy) {
    // An explicit legacy tier owns its language defaults. System Tesseract's
    // Automatic choice also keeps the installed-English native behavior.
    const language = ocrPolicyLanguage(policy);
    return { engine: policy.engine, language, config: policy.config ?? null,
        cpuPixelBudget: policy.cpuPixelBudget, ocrTier: policy.ocrTier, ocrRecLang: policy.ocrRecLang };
}

export async function createOcrSession(options: OcrSessionOptions = {}): Promise<OcrSession> {
    const native = isTauri();
    const session = beginOperation('ocr.session', { backend: native ? 'native' : 'browser' });
    let policy: OcrPolicy;
    try {
        options.signal?.throwIfAborted();
        policy = await loadSavedOcrPolicy(options.ocr, native);
        options.signal?.throwIfAborted();
        if (native && modelGate.available) {
            // Never download during recognition: ask once (shared with other
            // sessions), or fail fast after the user declined this session.
            session.stage('checking');
            const models = await withExtractionAbort(ensureOcrModels({ surface: 'extract', ...nativeOptions(policy) },
                { context: 'Text recognition', rememberDecline: true }), options.signal);
            if (!models.ok) {
                session.stage('awaiting_models', { count: models.missing.length }, 'info');
                throw new OcrModelsMissingError(models.missing);
            }
        }
        session.complete({ engine: policy.engine, language: policy.language, cpuBudget: policy.cpuPixelBudget });
    } catch (error) {
        if (options.signal?.aborted && error === options.signal.reason) session.cancel(); else session.fail(error);
        throw error;
    }
    return {
        policy,
        async recognize(image, name) {
            const operation = beginOperation('ocr.recognition', { backend: native ? 'native' : 'browser',
                engine: policy.engine, language: policy.language, cpuBudget: policy.cpuPixelBudget });
            let dispatched = false;
            try {
                options.signal?.throwIfAborted();
                if ((image instanceof Uint8Array || image instanceof Blob)
                    && (!(image instanceof Blob ? image.size : image.byteLength)
                        || (image instanceof Blob ? image.size : image.byteLength) > MAX_OCR_IMAGE_BYTES)) {
                    throw new Error('OCR needs a nonempty image no larger than 32 MiB.');
                }
                if (!native) {
                    const result = await recognizeBrowser(image, policy, options.signal, operation);
                    operation.complete({ engine: result.engine, characters: result.text.length, warnings: result.warnings.length });
                    return result;
                }
                const metadata = new TextEncoder().encode(JSON.stringify({ name, options: nativeOptions(policy) }));
                if (metadata.byteLength > MAX_OCR_METADATA_BYTES) throw new Error('OCR settings and the image name exceed the 64 KiB metadata limit.');
                // Chunked conversion avoids a spread-argument overflow for advanced settings.
                let binary = '';
                for (let i = 0; i < metadata.length; i += 8192) binary += String.fromCharCode(...metadata.subarray(i, i + 8192));
                const bytes = await withExtractionAbort(imageBytes(image), options.signal);
                if (!bytes.byteLength || bytes.byteLength > MAX_OCR_IMAGE_BYTES) throw new Error('OCR needs a nonempty image no larger than 32 MiB.');
                options.signal?.throwIfAborted();
                operation.stage('dispatching', { bytes: bytes.byteLength });
                dispatched = true;
                const result = await withExtractionAbort(invoke<OcrTextResult>('ocr_extract_image', bytes,
                    { headers: { 'x-ocr-metadata': btoa(binary), 'x-operation-id': operation.id } }), options.signal);
                options.signal?.throwIfAborted();
                if (!result || typeof result.text !== 'string' || typeof result.engine !== 'string' || !result.engine.trim()
                    || !Array.isArray(result.warnings) || result.warnings.some(value => typeof value !== 'string')) {
                    throw new Error('Native OCR returned an invalid result.');
                }
                if (result.warnings.length) operation.stage('warning', { warnings: result.warnings.length }, 'warn');
                const text = result.text.trim();
                operation.complete({ engine: result.engine, characters: text.length, warnings: result.warnings.length });
                return { ...result, text };
            } catch (error) {
                if (options.signal?.aborted && error === options.signal.reason) operation.cancel({ nativeContinues: native && dispatched });
                else operation.fail(error);
                // Native recognition refuses to download; report which models.
                const missing = plannedFromError(error);
                if (missing) throw new OcrModelsMissingError(missing);
                throw error;
            }
        },
    };
}
