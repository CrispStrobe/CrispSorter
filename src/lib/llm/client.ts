import { fetch } from '@tauri-apps/plugin-http';

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
};

export const DEFAULT_PROVIDERS: LLMProvider[] = [
    { id: 'ollama', name: 'Ollama (Local)', baseUrl: 'http://localhost:11434/v1', apiKey: 'ollama', models: [], selectedModel: '', isConfigured: true },
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

    constructor(keys: Record<string, string> = {}) {
        this.keys = keys;
    }

    setKeys(keys: Record<string, string>) {
        this.keys = keys;
    }

    async fetchModels(providerId: string, apiKey?: string, baseUrl?: string): Promise<string[]> {
        const key = apiKey || this.keys[providerId];
        const base = baseUrl || OPENAI_COMPATIBLE[providerId as keyof typeof OPENAI_COMPATIBLE];
        
        if (!key && providerId !== 'ollama') throw new Error(`API key for ${providerId} is required.`);
        if (!base) throw new Error(`Base URL for ${providerId} is not configured.`);

        try {
            console.log(`[LLMClient] Fetching models from ${base}/models`);
            
            const headers: Record<string, string> = {
                'Content-Type': 'application/json'
            };
            if (key && providerId !== 'ollama') {
                headers['Authorization'] = `Bearer ${key}`;
            }

            const response = await fetch(`${base}/models`, {
                method: 'GET',
                headers: headers,
                connectTimeout: 10000
            });

            if (!response.ok) {
                const errorText = await response.text();
                throw new Error(`HTTP ${response.status}: ${errorText || response.statusText}`);
            }

            const data = await response.json();
            console.log(`[LLMClient] Models received for ${providerId}:`, data);
            
            if (providerId === 'ollama') {
                return data.data ? data.data.map((m: any) => m.id) : data.models?.map((m: any) => m.name) || [];
            }

            if (data.data && Array.isArray(data.data)) {
                return data.data.map((m: any) => m.id).sort();
            }

            return [];
        } catch (error: any) {
            const msg = error instanceof Error ? error.message : String(error);
            console.error(`[LLMClient] Failed to fetch models for ${providerId}:`, msg);
            throw new Error(msg);
        }
    }

    async query(providerId: string, modelId: string, prompt: string, apiKey?: string): Promise<string> {
        const key = apiKey || this.keys[providerId];
        const baseUrl = OPENAI_COMPATIBLE[providerId as keyof typeof OPENAI_COMPATIBLE];

        if (!key && providerId !== 'ollama') throw new Error(`API key for ${providerId} is required.`);
        if (!baseUrl) throw new Error(`Base URL for ${providerId} not found.`);
        
        try {
            const headers: Record<string, string> = {
                'Content-Type': 'application/json'
            };
            if (key && providerId !== 'ollama') {
                headers['Authorization'] = `Bearer ${key}`;
            }

            const response = await fetch(`${baseUrl}/chat/completions`, {
                method: 'POST',
                headers: headers,
                body: JSON.stringify({
                    model: modelId,
                    messages: [{ role: 'user', content: prompt }],
                    temperature: 0.3
                })
            });

            if (!response.ok) {
                const errorText = await response.text();
                throw new Error(`LLM Error (${providerId}): ${errorText || response.statusText}`);
            }

            const data = await response.json();
            return data.choices[0].message.content;
        } catch (error: any) {
            throw new Error(error instanceof Error ? error.message : String(error));
        }
    }
}

export const llmClient = new LLMClient();
