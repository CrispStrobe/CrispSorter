import { fetch } from '@tauri-apps/plugin-http';
import { invoke } from '@tauri-apps/api/core';
import { queryWebLLM, getWebLLMLoadedModel } from './webllm';
import { queryORT, getORTLoadedModel } from './ort';

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

    constructor(keys: Record<string, string> = {}) {
        this.keys = keys;
    }

    setKeys(keys: Record<string, string>) {
        this.keys = keys;
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
        
        const localProviders = ['ollama', 'llamacpp', 'mlx'];
        if (!key && !localProviders.includes(providerId)) {
            console.error(`[LLMClient] Missing API key for ${providerId}`);
            throw new Error(`API key for ${providerId} is required.`);
        }
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

    async query(providerId: string, modelId: string, prompt: string, apiKey?: string, temperature: number = 0.3): Promise<string> {
        console.log(`[LLMClient] query: provider=${providerId}, model=${modelId}`);
        console.log(`[LLMClient] prompt length: ${prompt.length}`);
        
        if (providerId === 'webllm') {
            const loaded = getWebLLMLoadedModel();
            if (!loaded) throw new Error('WebLLM: no model loaded — open Settings and click Load.');
            console.log(`[LLMClient] WebLLM query using model: ${loaded}`);
            const result = await queryWebLLM(prompt);
            return this.stripThinking(result);
        }

        if (providerId === 'ort') {
            const loaded = getORTLoadedModel();
            if (!loaded) throw new Error('ORT: no model loaded — open Settings and click Load.');
            console.log(`[LLMClient] ORT query using model: ${loaded}`);
            const result = await queryORT(prompt);
            return this.stripThinking(result);
        }

        if (providerId === 'mistralrs') {
            console.log(`[LLMClient] Invoking native mistral.rs engine for local model...`);
            const startTime = Date.now();
            try {
                const result = await invoke('run_mistralrs_query', {
                    modelPath: modelId,
                    prompt: prompt,
                    maxTokens: 512,
                    noThinking: this.noThinking,
                });
                const duration = Date.now() - startTime;
                console.log(`[LLMClient] Native query successful in ${duration}ms. Result length: ${String(result).length}`);
                return this.stripThinking(String(result));
            } catch (error: any) {
                const duration = Date.now() - startTime;
                console.error(`[LLMClient] Native query failed after ${duration}ms:`, error);
                throw new Error(`mistral.rs error: ${error}`);
            }
        }

        const key = apiKey || this.keys[providerId];
        const baseUrl = OPENAI_COMPATIBLE[providerId as keyof typeof OPENAI_COMPATIBLE];

        if (!key && !['ollama', 'llamacpp', 'mlx'].includes(providerId)) throw new Error(`API key for ${providerId} is required.`);
        if (!baseUrl) throw new Error(`Base URL for ${providerId} not found.`);
        
        const isLocal = ['ollama', 'llamacpp', 'mlx'].includes(providerId);
        // Local providers: single attempt, long connect timeout (server is either up or not)
        // Remote providers: 3 retries with backoff
        const maxRetries = isLocal ? 1 : 3;
        let lastError = null;

        for (let attempt = 0; attempt < maxRetries; attempt++) {
            try {
                console.log(`[LLMClient] POST ${baseUrl}/chat/completions (Attempt ${attempt + 1})`);
                const headers: Record<string, string> = { 'Content-Type': 'application/json' };
                if (key && !['ollama', 'llamacpp', 'mlx'].includes(providerId)) headers['Authorization'] = `Bearer ${key}`;

                const messages: { role: string; content: string }[] = [];
                if (this.noThinking) messages.push({ role: 'system', content: '/no_think' });
                messages.push({ role: 'user', content: prompt });

                const response = await fetch(`${baseUrl}/chat/completions`, {
                    method: 'POST',
                    headers: headers,
                    body: JSON.stringify({
                        model: modelId,
                        messages,
                        temperature: temperature
                    }),
                    connectTimeout: isLocal ? 30000 : 5000
                });

                if (!response.ok) {
                    const errorText = await response.text();
                    console.error(`[LLMClient] Query failed: ${response.status} ${errorText}`);
                    throw new Error(`LLM Error (${providerId}): ${errorText || response.statusText}`);
                }

                const data = await response.json();
                console.log(`[LLMClient] Query success.`);
                return this.stripThinking(data.choices[0].message.content);
            } catch (error: any) {
                lastError = error;
                console.warn(`[LLMClient] Attempt ${attempt + 1} failed for ${providerId}:`, error.message || error);
                if (attempt < maxRetries - 1) {
                    await new Promise(resolve => setTimeout(resolve, 2000));
                }
            }
        }

        console.error(`[LLMClient] All ${maxRetries} attempts failed for ${providerId}.`);
        const rawMsg = typeof lastError === 'object' ? (lastError.message || JSON.stringify(lastError)) : String(lastError);
        // Surface actionable message for connection refused
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
