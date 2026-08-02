import { beforeEach, describe, expect, it, vi } from 'vitest';

// The store is Tauri-backed; swap it for an in-memory map so the gate's logic
// is what is under test rather than the persistence layer.
const settings = new Map<string, unknown>();
vi.mock('../store', () => ({
	getSetting: async (key: string, fallback: unknown = null) =>
		settings.has(key) ? settings.get(key) : fallback,
	saveSetting: async (key: string, value: unknown) => {
		settings.set(key, value);
	}
}));

import {
	ensureThirdPartyConsent,
	grantConsent,
	hasConsent,
	isThirdPartyEgress,
	listConsented,
	registerConsentPrompter,
	revokeConsent,
	ThirdPartyAiConsentRequired
} from './thirdPartyConsent';

beforeEach(() => {
	settings.clear();
	registerConsentPrompter(null);
});

describe('what counts as third-party egress', () => {
	it('remote providers do', () => {
		expect(isThirdPartyEgress('openai', 'https://api.openai.com/v1')).toBe(true);
		expect(isThirdPartyEgress('groq', 'https://api.groq.com/openai/v1')).toBe(true);
		expect(isThirdPartyEgress('anthropic', 'https://api.anthropic.com/v1')).toBe(true);
	});

	it('loopback and in-process providers do not', () => {
		// A server the user runs is still their machine.
		expect(isThirdPartyEgress('ollama', 'http://localhost:11434/v1')).toBe(false);
		expect(isThirdPartyEgress('llamacpp', 'http://127.0.0.1:8080/v1')).toBe(false);
		// `URL.hostname` brackets IPv6 literals, so this only passes if the
		// gate strips them — it did not, at first.
		expect(isThirdPartyEgress('mlx', 'http://[::1]:8000/v1')).toBe(false);
		// Any 127.0.0.0/8 address is loopback, not just .0.1.
		expect(isThirdPartyEgress('llamacpp', 'http://127.0.0.53:8080/v1')).toBe(false);
		// Nothing touches the network at all for these.
		expect(isThirdPartyEgress('mistralrs', 'local')).toBe(false);
		expect(isThirdPartyEgress('webllm', 'webllm')).toBe(false);
		expect(isThirdPartyEgress('ort', 'ort')).toBe(false);
	});

	// The bug an id-based check would have: `baseUrl` is user-editable, so a
	// "local" provider can be re-pointed at someone else's server.
	it('judges by URL, not by provider id', () => {
		expect(isThirdPartyEgress('ollama', 'https://ollama.example.com/v1')).toBe(true);
		expect(isThirdPartyEgress('openai', 'http://localhost:1234/v1')).toBe(false);
	});

	it('treats an unparseable URL as remote', () => {
		// Over-asking is a worse experience; under-asking is a violation.
		expect(isThirdPartyEgress('mystery', 'not a url')).toBe(true);
	});
});

describe('the gate', () => {
	it('lets local providers through without asking', async () => {
		const prompter = vi.fn(async () => true);
		registerConsentPrompter(prompter);
		await ensureThirdPartyConsent('ollama', 'http://localhost:11434/v1');
		expect(prompter).not.toHaveBeenCalled();
	});

	it('refuses a remote provider when there is nobody to ask', async () => {
		await expect(
			ensureThirdPartyConsent('openai', 'https://api.openai.com/v1')
		).rejects.toBeInstanceOf(ThirdPartyAiConsentRequired);
	});

	it('refuses when the user declines, and does not record anything', async () => {
		registerConsentPrompter(async () => false);
		await expect(
			ensureThirdPartyConsent('groq', 'https://api.groq.com/openai/v1')
		).rejects.toBeInstanceOf(ThirdPartyAiConsentRequired);
		expect(await hasConsent('groq')).toBe(false);
	});

	it('proceeds and records permission when the user agrees', async () => {
		registerConsentPrompter(async () => true);
		await ensureThirdPartyConsent('groq', 'https://api.groq.com/openai/v1');
		expect(await hasConsent('groq')).toBe(true);
	});

	it('asks once per provider, not once per request', async () => {
		const prompter = vi.fn(async () => true);
		registerConsentPrompter(prompter);
		await ensureThirdPartyConsent('groq', 'https://api.groq.com/openai/v1');
		await ensureThirdPartyConsent('groq', 'https://api.groq.com/openai/v1');
		expect(prompter).toHaveBeenCalledTimes(1);
	});

	// Consent is per recipient. Agreeing to send text to Groq says nothing
	// about Google, and treating it as blanket permission is exactly what
	// 5.1.2(i) forbids.
	it('does not let one provider stand in for another', async () => {
		registerConsentPrompter(async () => true);
		await ensureThirdPartyConsent('groq', 'https://api.groq.com/openai/v1');
		registerConsentPrompter(null);
		await expect(
			ensureThirdPartyConsent('google', 'https://generativelanguage.googleapis.com/v1beta')
		).rejects.toBeInstanceOf(ThirdPartyAiConsentRequired);
	});

	it('names the endpoint in the error, so the caller can say where it would have gone', async () => {
		const err = await ensureThirdPartyConsent('openai', 'https://api.openai.com/v1').catch(
			(e) => e
		);
		expect(err).toBeInstanceOf(ThirdPartyAiConsentRequired);
		expect(err.endpoint).toBe('https://api.openai.com/v1');
		expect(err.providerName).toBe('OpenAI');
	});
});

describe('revocation', () => {
	it('takes effect, and the gate asks again', async () => {
		await grantConsent('openai');
		expect(await listConsented()).toContain('openai');

		await revokeConsent('openai');
		expect(await hasConsent('openai')).toBe(false);
		await expect(
			ensureThirdPartyConsent('openai', 'https://api.openai.com/v1')
		).rejects.toBeInstanceOf(ThirdPartyAiConsentRequired);
	});

	it('granting is idempotent', async () => {
		await grantConsent('openai');
		await grantConsent('openai');
		expect((await listConsented()).filter((id) => id === 'openai')).toHaveLength(1);
	});
});
