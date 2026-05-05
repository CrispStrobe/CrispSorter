import { fetch } from '@tauri-apps/plugin-http';
import { invoke } from '@tauri-apps/api/core';
import { queryWebLLM, getWebLLMLoadedModel } from './webllm';
import { queryORT, getORTLoadedModel } from './ort';
import { flog } from '../log';

export interface LLMProvider {
    id: string;
    name: string;
    baseUrl: string;
    apiKey: string;
    models: string[];
    selectedModel: string;
    isConfigured: boolean;
    authHeader?: string;
    requiresCustomHeaders?: Record<string, string>;
}

export const OPENAI_COMPATIBLE = {
    'openai': 'https://api.openai.com/v1',
    'nebius': 'https://api.studio.nebius.ai/v1',
    'scaleway': 'https://api.scaleway.ai/v1',
    'openrouter': 'https://openrouter.ai/api/v1',
    'mistral': 'https://api.mistral.ai/v1',
    'groq': 'https://api.groq.com/openai/v1',
    'poe': 'https://api.poe.com/v1',
    'ollama': 'http://localhost:11434/v1',
    'llamacpp': 'http://localhost:8080/v1',
    'mlx': 'http://localhost:8000/v1',
    'webllm': 'webllm',
};

export const DEFAULT_PROVIDERS: LLMProvider[] = [
    { id: 'ollama', name: 'Ollama (Local)', baseUrl: 'http://localhost:11434/v1', apiKey: 'ollama', models: [], selectedModel: '', isConfigured: true },
    { id: 'mistralrs', name: 'mistral.rs (Native)', baseUrl: 'local', apiKey: '', models: [], selectedModel: '', isConfigured: true },
    { id: 'llamacpp', name: 'llama.cpp (Sidecar)', baseUrl: 'http://localhost:8080/v1', apiKey: 'no-key', models: [], selectedModel: '', isConfigured: true },
    { id: 'mlx', name: 'MLX (Apple Silicon)', baseUrl: 'http://localhost:8000/v1', apiKey: 'no-key', models: [], selectedModel: '', isConfigured: true },
    { id: 'webllm', name: 'WebLLM (Browser GPU)', baseUrl: 'webllm', apiKey: '', models: [], selectedModel: '', isConfigured: true },
    { id: 'ort', name: 'ORT / Transformers.js', baseUrl: 'ort', apiKey: '', models: [], selectedModel: '', isConfigured: true },
    { id: 'groq', name: 'Groq', baseUrl: 'https://api.groq.com/openai/v1', apiKey: '', models: [], selectedModel: '', isConfigured: false },
    { id: 'openrouter', name: 'OpenRouter', baseUrl: 'https://openrouter.ai/api/v1', apiKey: '', models: [], selectedModel: '', isConfigured: false },
    { id: 'mistral', name: 'Mistral', baseUrl: 'https://api.mistral.ai/v1', apiKey: '', models: [], selectedModel: '', isConfigured: false },
    { id: 'openai', name: 'OpenAI', baseUrl: 'https://api.openai.com/v1', apiKey: '', models: [], selectedModel: '', isConfigured: false },
    { id: 'nebius', name: 'Nebius', baseUrl: 'https://api.studio.nebius.ai/v1', apiKey: '', models: [], selectedModel: '', isConfigured: false },
    { id: 'scaleway', name: 'Scaleway', baseUrl: 'https://api.scaleway.ai/v1', apiKey: '', models: [], selectedModel: '', isConfigured: false },
    { id: 'anthropic', name: 'Anthropic', baseUrl: 'https://api.anthropic.com/v1', apiKey: '', models: [], selectedModel: '', isConfigured: false },
    { id: 'google', name: 'Google (Gemini)', baseUrl: 'https://generativelanguage.googleapis.com/v1beta', apiKey: '', models: [], selectedModel: '', isConfigured: false },
];

export class LLMClient {
    private keys: Record<string, string> = {};
    noThinking: boolean = false;
    llamacppPort: number = 8080;
    mlxPort: number = 8000;
    requestDelayMs: number = 0;
    private serverReadyAt: Record<string, number> = {};

    constructor(keys: Record<string, string> = {}) {
        this.keys = keys;
    }

    setKeys(keys: Record<string, string>) {
        this.keys = keys;
    }

    /** Ensure a local HTTP server is running. Starts it via Tauri if not reachable. */
    async ensureProviderRunning(providerId: string, modelId?: string): Promise<void> {
        if (!['ollama', 'llamacpp', 'mlx'].includes(providerId)) return;

        // Use cached "running" state for 15s to avoid health checks on every query
        const now = Date.now();
        if (now - (this.serverReadyAt[providerId] ?? 0) < 15000) return;

        const healthUrl = providerId === 'ollama'
            ? 'http://localhost:11434/api/tags'
            : providerId === 'mlx'
            ? `http://localhost:${this.mlxPort}/v1/models`
            : `http://localhost:${this.llamacppPort}/health`;

        try {
            const r = await fetch(healthUrl, { method: 'GET', connectTimeout: 1500 });
            if (r.ok) { this.serverReadyAt[providerId] = now; return; }
        } catch { /* not running */ }

        console.log(`[LLMClient] ${providerId} not running — starting...`);
        if (providerId === 'ollama') {
            await invoke('start_ollama');
        } else if (providerId === 'llamacpp' && modelId) {
            await invoke('start_llamacpp_sidecar', { modelPath: modelId, port: this.llamacppPort });
        } else if (providerId === 'mlx' && modelId) {
            await invoke('start_mlx_server', { modelPath: modelId, port: this.mlxPort });
        } else {
            return; // can't start without model path
        }

        // Poll up to 60s for readiness
        for (let i = 0; i < 30; i++) {
            await new Promise(r => setTimeout(r, 2000));
            try {
                const r = await fetch(healthUrl, { connectTimeout: 2000 });
                if (r.ok) { this.serverReadyAt[providerId] = Date.now(); return; }
            } catch { /* keep waiting */ }
        }
        throw new Error(`${providerId} failed to start within 60s`);
    }

    private getRetryAfterMs(headers: Headers, errorText: string): number {
        // Standard Retry-After header (in seconds)
        const retryAfterHeader = headers.get('retry-after') || headers.get('Retry-After');
        if (retryAfterHeader) {
            const secs = parseFloat(retryAfterHeader);
            if (!isNaN(secs)) return Math.min(Math.ceil(secs * 1000) + 200, 90_000);
        }
        // Parse from Groq/OpenAI error body: "Please try again in 847.5ms" or "1.5s"
        const msMatch = errorText.match(/try again in ([\d.]+)ms/i);
        if (msMatch) return Math.min(Math.ceil(parseFloat(msMatch[1])) + 500, 90_000);
        const sMatch = errorText.match(/try again in ([\d.]+)s/i);
        if (sMatch) return Math.min(Math.ceil(parseFloat(sMatch[1]) * 1000) + 500, 90_000);
        return 3000; // default 3s
    }

    // Sleep that unblocks immediately when signal fires.
    private abortableSleep(ms: number, signal?: AbortSignal): Promise<void> {
        return new Promise<void>((resolve, reject) => {
            if (signal?.aborted) { reject(new DOMException('Aborted', 'AbortError')); return; }
            const t = setTimeout(resolve, ms);
            signal?.addEventListener('abort', () => { clearTimeout(t); reject(new DOMException('Aborted', 'AbortError')); }, { once: true });
        });
    }

    private stripThinking(text: string): string {
        // <think>…</think>  — DeepSeek R1, Qwen3 leakage
        // <thinking>…</thinking>  — some other models
        return text
            .replace(/<think>[\s\S]*?<\/think>/gi, '')
            .replace(/<thinking>[\s\S]*?<\/thinking>/gi, '')
            .trim();
    }

    async fetchModels(providerId: string, apiKey?: string, baseUrl?: string): Promise<string[]> {
        console.log(`[LLMClient] fetchModels for ${providerId}`);
        if (['mistralrs', 'webllm', 'ort'].includes(providerId)) return [];

        const key = apiKey || this.keys[providerId];
        const base = baseUrl || OPENAI_COMPATIBLE[providerId as keyof typeof OPENAI_COMPATIBLE];
        
        if (!base) {
            console.error(`[LLMClient] Missing Base URL for ${providerId}`);
            throw new Error(`Base URL for ${providerId} is not configured.`);
        }

        try {
            console.log(`[LLMClient] GET ${base}/models`);
            const headers: Record<string, string> = { 'Content-Type': 'application/json' };
            if (key && providerId !== 'ollama') headers['Authorization'] = `Bearer ${key}`;

            const response = await fetch(`${base}/models`, {
                method: 'GET',
                headers: headers,
                connectTimeout: 10000
            });

            if (!response.ok) {
                const errorText = await response.text();
                console.error(`[LLMClient] Fetch models failed: ${response.status} ${errorText}`);
                throw new Error(`HTTP ${response.status}: ${errorText || response.statusText}`);
            }

            const data = await response.json();
            console.log(`[LLMClient] Models received for ${providerId}`);
            
            if (['ollama', 'llamacpp', 'mlx'].includes(providerId)) {
                return data.data ? data.data.map((m: any) => m.id) : data.models?.map((m: any) => m.name) || [];
            }
            if (data.data && Array.isArray(data.data)) {
                return data.data.map((m: any) => m.id).sort();
            }
            return [];
        } catch (error: any) {
            console.error(`[LLMClient] Error in fetchModels:`, error.message);
            throw error;
        }
    }

    async query(providerId: string, modelId: string, prompt: string, apiKey?: string, temperature: number = 0.3, signal?: AbortSignal): Promise<string> {
        flog('info', `LLM query: provider=${providerId} model=${modelId} prompt_len=${prompt.length}`);

        if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');

        if (providerId === 'webllm') {
            const loaded = getWebLLMLoadedModel();
            if (!loaded) throw new Error('WebLLM: no model loaded — open Settings and click Load.');
            const result = await queryWebLLM(prompt);
            return this.stripThinking(result);
        }

        if (providerId === 'ort') {
            const loaded = getORTLoadedModel();
            if (!loaded) throw new Error('ORT: no model loaded — open Settings and click Load.');
            const result = await queryORT(prompt);
            return this.stripThinking(result);
        }

        if (providerId === 'mistralrs') {
            const startTime = Date.now();
            const invokePromise = invoke<string>('run_mistralrs_query', {
                modelPath: modelId,
                prompt,
                maxTokens: 512,
                noThinking: this.noThinking,
            });
            // Race against abort signal and 3-minute timeout
            const races: Promise<any>[] = [
                invokePromise,
                new Promise<never>((_, reject) =>
                    setTimeout(() => reject(new Error('LLM_TIMEOUT: mistralrs took >3 min')), 3 * 60 * 1000)
                )
            ];
            if (signal) {
                races.push(new Promise<never>((_, reject) => {
                    if (signal.aborted) return reject(new DOMException('Aborted', 'AbortError'));
                    signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), { once: true });
                }));
            }
            try {
                const result = await Promise.race(races);
                flog('info', `mistralrs done in ${Date.now() - startTime}ms, result_len=${String(result).length}`);
                return this.stripThinking(String(result));
            } catch (error: any) {
                flog('error', `mistralrs failed after ${Date.now() - startTime}ms: ${error?.message ?? error}`);
                throw error?.name === 'AbortError' ? error : new Error(`mistral.rs error: ${error}`);
            }
        }

        const key = apiKey || this.keys[providerId];
        const baseUrl = OPENAI_COMPATIBLE[providerId as keyof typeof OPENAI_COMPATIBLE];

        if (!baseUrl) throw new Error(`Base URL for ${providerId} not found.`);

        const isLocal = ['ollama', 'llamacpp', 'mlx'].includes(providerId);
        const maxRetries = isLocal ? 1 : 3;
        // Per-request timeout: 3 min for local (model might be slow), 60s for remote
        const requestTimeoutMs = isLocal ? 3 * 60 * 1000 : 60 * 1000;
        let lastError = null;

        let rateLimitRetries = 0;
        const MAX_RL_RETRIES = 6;

        for (let attempt = 0; attempt < maxRetries; attempt++) {
            if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
            try {
                flog('info', `POST ${baseUrl}/chat/completions (attempt ${attempt + 1})`);
                const headers: Record<string, string> = { 'Content-Type': 'application/json' };
                if (key && !['ollama', 'llamacpp', 'mlx'].includes(providerId)) headers['Authorization'] = `Bearer ${key}`;

                const messages: { role: string; content: string }[] = [];
                if (this.noThinking) messages.push({ role: 'system', content: '/no_think' });
                messages.push({ role: 'user', content: prompt });

                // Combine external abort signal with per-request timeout
                const reqController = new AbortController();
                const timeoutId = setTimeout(() => reqController.abort(new Error('LLM_TIMEOUT')), requestTimeoutMs);
                if (signal) {
                    if (signal.aborted) { clearTimeout(timeoutId); throw new DOMException('Aborted', 'AbortError'); }
                    signal.addEventListener('abort', () => reqController.abort(signal.reason), { once: true });
                }

                let response: Response;
                try {
                    response = await fetch(`${baseUrl}/chat/completions`, {
                        method: 'POST',
                        headers,
                        body: JSON.stringify({ model: modelId, messages, temperature, stream: false }),
                        connectTimeout: isLocal ? 30000 : 15000,
                        signal: reqController.signal,
                    });
                } finally {
                    clearTimeout(timeoutId);
                }

                if (response.status === 429 && rateLimitRetries < MAX_RL_RETRIES) {
                    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
                    rateLimitRetries++;
                    const errorText = await response.text();
                    const waitMs = this.getRetryAfterMs(response.headers, errorText);
                    flog('warn', `Rate limited (429). Waiting ${waitMs}ms (rl-retry ${rateLimitRetries}/${MAX_RL_RETRIES})`);
                    await this.abortableSleep(waitMs, signal);
                    attempt--;
                    continue;
                }

                if (!response.ok) {
                    const errorText = await response.text();
                    flog('error', `LLM HTTP ${response.status} (${providerId}): ${errorText}`);
                    throw new Error(`LLM Error (${providerId}): ${errorText || response.statusText}`);
                }

                const data = await response.json();
                flog('info', `LLM query success (${providerId})`);
                const content = data.choices?.[0]?.message?.content ?? data.choices?.[0]?.message?.reasoning_content ?? '';
                return this.stripThinking(content);
            } catch (error: any) {
                // Don't retry on user abort or timeout — surface immediately
                if (error?.name === 'AbortError' || error?.message?.includes('LLM_TIMEOUT')) {
                    flog('warn', `LLM query aborted/timed out (${providerId}): ${error.message}`);
                    throw error;
                }
                lastError = error;
                flog('warn', `LLM attempt ${attempt + 1} failed (${providerId}): ${error.message || error}`);
                if (attempt < maxRetries - 1) {
                    await this.abortableSleep(2000, signal);
                }
            }
        }

        flog('error', `All ${maxRetries} LLM attempts failed for ${providerId}`);
        const rawMsg = typeof lastError === 'object' ? (lastError?.message || JSON.stringify(lastError)) : String(lastError);
        const errorMsg = rawMsg.includes('error sending request') || rawMsg.includes('Connection refused')
            ? `Server not running (${baseUrl.replace('/v1', '')})`
            : rawMsg;
        throw new Error(errorMsg);
    }

    async pullModel(providerId: string, modelId: string, baseUrl?: string): Promise<ReadableStream<Uint8Array>> {
        // Ollama uses /api/pull, not /v1/pull
        const url = `${(baseUrl || OPENAI_COMPATIBLE[providerId as keyof typeof OPENAI_COMPATIBLE]).replace('/v1', '')}/api/pull`;
        console.log(`[LLMClient] Pulling model ${modelId} from ${url}`);
        
        const response = await fetch(url, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: modelId, stream: true })
        });

        if (!response.ok) {
            const errorText = await response.text();
            throw new Error(`Failed to pull model: ${errorText}`);
        }

        return response.body!;
    }
}

export const llmClient = new LLMClient();
