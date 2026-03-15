import { CreateMLCEngine, type MLCEngine, type InitProgressReport } from '@mlc-ai/web-llm';

// Pre-built small models suitable for document classification
export const WEBLLM_MODELS = [
    { id: 'Qwen2.5-0.5B-Instruct-q4f16_1-MLC',  name: 'Qwen2.5 0.5B · ~500 MB · fastest' },
    { id: 'Llama-3.2-1B-Instruct-q4f16_1-MLC',   name: 'Llama 3.2 1B · ~800 MB · fast'    },
    { id: 'gemma-2-2b-it-q4f16_1-MLC',            name: 'Gemma 2 2B · ~1.5 GB · balanced'  },
    { id: 'Llama-3.2-3B-Instruct-q4f16_1-MLC',   name: 'Llama 3.2 3B · ~2 GB · quality'   },
    { id: 'Phi-3.5-mini-instruct-q4f16_1-MLC',    name: 'Phi-3.5 Mini 3.8B · ~2.2 GB'      },
];

let engine: MLCEngine | null = null;
let loadedModelId = '';

async function logWebGPUInfo() {
    if (typeof navigator === 'undefined' || !('gpu' in navigator)) {
        console.warn('[WebLLM] WebGPU not available in this browser/WebView.');
        return;
    }
    const adapter = await (navigator as any).gpu.requestAdapter();
    if (!adapter) {
        console.warn('[WebLLM] WebGPU: requestAdapter() returned null — no GPU adapter found.');
        return;
    }
    const info = await adapter.requestAdapterInfo?.();
    const limits = adapter.limits;
    console.group('[WebLLM] WebGPU adapter info');
    if (info) {
        console.log('  Vendor  :', info.vendor);
        console.log('  Device  :', info.device || info.description);
        console.log('  Backend :', info.backend || '(unknown)');
    }
    console.log('  maxBufferSize         :', (limits.maxBufferSize / 1024 / 1024 / 1024).toFixed(2), 'GB');
    console.log('  maxStorageBufferSize  :', (limits.maxStorageBufferBindingSize / 1024 / 1024).toFixed(0), 'MB');
    console.groupEnd();
}

/**
 * Load (or reuse) a WebLLM model. Models are cached in IndexedDB so the
 * download only happens once; subsequent loads just warm up the engine.
 */
export async function loadWebLLM(
    modelId: string,
    onProgress: (report: InitProgressReport) => void,
): Promise<void> {
    if (engine && loadedModelId === modelId) return;
    engine = null;
    loadedModelId = '';
    await logWebGPUInfo();
    console.log(`[WebLLM] Loading model: ${modelId}`);
    engine = await CreateMLCEngine(modelId, { initProgressCallback: onProgress });
    loadedModelId = modelId;
}

/**
 * Run a single-turn chat completion. Call loadWebLLM first.
 */
export async function queryWebLLM(prompt: string, systemPrompt?: string): Promise<string> {
    if (!engine) throw new Error('WebLLM engine not loaded — select and load a model in Settings first.');
    type Msg = { role: 'system' | 'user' | 'assistant'; content: string };
    const messages: Msg[] = [];
    if (systemPrompt) messages.push({ role: 'system', content: systemPrompt });
    messages.push({ role: 'user', content: prompt });
    const reply = await engine.chat.completions.create({ messages });
    return reply.choices[0].message.content ?? '';
}

export function getWebLLMLoadedModel(): string {
    return loadedModelId;
}

export function unloadWebLLM(): void {
    engine = null;
    loadedModelId = '';
}
