import { describe, expect, it } from 'vitest';
import { normalizeProviderBase, OPENAI_COMPATIBLE } from './client';

describe('provider base URLs', () => {
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
