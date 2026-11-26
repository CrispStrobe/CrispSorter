import { describe, expect, it } from 'vitest';
import { normalizeProviderBase, OPENAI_COMPATIBLE } from './client';

describe('provider base URLs', () => {
    it('migrates exact legacy Nebius bases even under a compatible connection, preserving custom routes', () => {
        expect(normalizeProviderBase('openai', 'https://api.studio.nebius.com/v1/')).toBe('https://api.tokenfactory.nebius.com/v1');
        expect(normalizeProviderBase('nebius', 'https://api.studio.nebius.ai/v1')).toBe('https://api.tokenfactory.nebius.com/v1');
        expect(normalizeProviderBase('nebius', 'https://proxy.example/nebius/v1')).toBe('https://proxy.example/nebius/v1');
        expect(normalizeProviderBase('nebius', 'https://api.studio.nebius.com/custom/v1')).toBe('https://api.studio.nebius.com/custom/v1');
    });
    it('moves a saved plain Gemini v1beta base to its OpenAI-compatible path', () => {
        expect(normalizeProviderBase('google', 'https://generativelanguage.googleapis.com/v1beta')).toBe('https://generativelanguage.googleapis.com/v1beta/openai');
        expect(normalizeProviderBase('google', 'https://generativelanguage.googleapis.com/v1beta/')).toBe('https://generativelanguage.googleapis.com/v1beta/openai');
        expect(normalizeProviderBase('google', 'https://generativelanguage.googleapis.com/v1beta/openai')).toBe('https://generativelanguage.googleapis.com/v1beta/openai');
        expect(normalizeProviderBase('openai', 'https://api.openai.com/v1/')).toBe('https://api.openai.com/v1');
    });
    it('gives Anthropic and Gemini a chat endpoint', () => {
        expect(OPENAI_COMPATIBLE.anthropic).toBe('https://api.anthropic.com/v1');
        expect(OPENAI_COMPATIBLE.google).toBe('https://generativelanguage.googleapis.com/v1beta/openai');
    });
});
