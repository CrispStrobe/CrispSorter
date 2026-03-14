export interface LLMProvider {
    id: string;
    name: string;
    baseUrl: string;
    apiKey: string;
    models: string[];
    isConfigured: boolean;
}

export const defaultProviders: LLMProvider[] = [
    { id: 'ollama', name: 'Ollama (Local)', baseUrl: 'http://localhost:11434', apiKey: 'ollama', models: [], isConfigured: true },
    { id: 'groq', name: 'Groq', baseUrl: 'https://api.groq.com/openai/v1', apiKey: '', models: [], isConfigured: false },
    { id: 'openrouter', name: 'OpenRouter', baseUrl: 'https://openrouter.ai/api/v1', apiKey: '', models: [], isConfigured: false },
    { id: 'mistral', name: 'Mistral', baseUrl: 'https://api.mistral.ai/v1', apiKey: '', models: [], isConfigured: false },
    { id: 'openai', name: 'OpenAI', baseUrl: 'https://api.openai.com/v1', apiKey: '', models: [], isConfigured: false },
    { id: 'nebius', name: 'Nebius', baseUrl: 'https://api.studio.nebius.ai/v1', apiKey: '', models: [], isConfigured: false },
    { id: 'scaleway', name: 'Scaleway', baseUrl: 'https://api.scaleway.ai/v1', apiKey: '', models: [], isConfigured: false },
];
