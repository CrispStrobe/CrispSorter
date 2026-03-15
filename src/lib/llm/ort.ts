/**
 * ORT (ONNX Runtime) inference via @huggingface/transformers.
 *
 * Uses WebGPU if available (DirectML on Windows, Metal on macOS), falls back to WASM/CPU.
 * Models are downloaded from HuggingFace on first use and cached in the browser's
 * Cache API / Origin Private File System — no manual download needed.
 */
import { pipeline, env, type TextGenerationPipeline } from '@huggingface/transformers';

// Disable local model search — always load from HuggingFace CDN / cache
env.allowLocalModels = false;

export const ORT_MODELS = [
    { id: 'HuggingFaceTB/SmolLM2-135M-Instruct', name: 'SmolLM2 135M · ~300 MB · CPU/GPU' },
    { id: 'HuggingFaceTB/SmolLM2-360M-Instruct', name: 'SmolLM2 360M · ~700 MB · CPU/GPU' },
    { id: 'Xenova/Phi-3-mini-4k-instruct',        name: 'Phi-3 Mini 3.8B · ~2 GB · GPU recommended' },
];

type OrtDevice = 'webgpu' | 'wasm';

let generator: TextGenerationPipeline | null = null;
let loadedModelId = '';
let loadedDevice: OrtDevice = 'wasm';

async function detectDevice(): Promise<OrtDevice> {
    if (typeof navigator !== 'undefined' && 'gpu' in navigator) {
        const adapter = await (navigator as any).gpu.requestAdapter().catch(() => null);
        if (adapter) {
            const info = await adapter.requestAdapterInfo?.().catch(() => null);
            console.log('[ORT] WebGPU available —', info?.vendor || '(adapter found)');
            return 'webgpu';
        }
    }
    console.log('[ORT] WebGPU not available, using WASM/CPU fallback.');
    return 'wasm';
}

/**
 * Load (or reuse) an ONNX model via transformers.js.
 * Models are cached after first download — subsequent loads are fast.
 */
export async function loadORT(
    modelId: string,
    onProgress?: (p: { status: string; progress?: number; name?: string }) => void,
): Promise<void> {
    if (generator && loadedModelId === modelId) return;
    generator = null;
    loadedModelId = '';

    const device = await detectDevice();
    loadedDevice = device;
    console.log(`[ORT] Loading "${modelId}" on device=${device}`);

    generator = await pipeline('text-generation', modelId, {
        device,
        dtype: device === 'webgpu' ? 'q4' : 'q8',
        progress_callback: onProgress,
    }) as TextGenerationPipeline;

    loadedModelId = modelId;
    console.log(`[ORT] Model ready. Device=${device}`);
}

/**
 * Run a single-turn chat completion. Call loadORT first.
 */
export async function queryORT(prompt: string, systemPrompt?: string): Promise<string> {
    if (!generator) throw new Error('ORT engine not loaded — select and load a model in Settings first.');

    type Msg = { role: 'system' | 'user' | 'assistant'; content: string };
    const messages: Msg[] = [];
    if (systemPrompt) messages.push({ role: 'system', content: systemPrompt });
    messages.push({ role: 'user', content: prompt });

    const output = await generator(messages as any, { max_new_tokens: 512, do_sample: false });
    const result = output as any;
    const generated = Array.isArray(result) ? result[0] : result;
    const genText = generated?.generated_text ?? generated?.text ?? '';
    // Instruct models with chat template return an array of messages
    if (Array.isArray(genText)) {
        const lastAssistant = [...genText].reverse().find((m: any) => m.role === 'assistant');
        return String(lastAssistant?.content ?? '').trim();
    }
    // Plain string — strip legacy [/INST] echo
    return String(genText).split('[/INST]').pop()?.trim() ?? '';
}

export function getORTLoadedModel(): string { return loadedModelId; }
export function getORTDevice(): string { return loadedDevice; }

export function unloadORT(): void {
    generator = null;
    loadedModelId = '';
}
