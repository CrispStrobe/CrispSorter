/**
 * Third-party AI data-sharing consent — PLAN P36.13, App Review 5.1.2(i).
 *
 * > You must clearly disclose where personal data will be shared with third
 * > parties, **including with third-party AI**, and obtain explicit permission
 * > before doing so. […] Apps that share user data without user consent […]
 * > may be removed from sale.
 *
 * Two obligations: *disclose where it goes*, and *ask first*. A prompt built
 * from someone's documents is their personal data, and every remote provider
 * here is a third party receiving it.
 *
 * ## What this is not
 *
 * **Not the intended-purpose gate.** That one (`intended_purpose.rs`) asks
 * whether the *use* is one the operator declared. This asks whether the user
 * agreed to the *egress*. Satisfying either does not satisfy the other, and
 * wiring them together would quietly weaken both.
 *
 * **Not the API-key check.** The tempting argument is that a user who pasted
 * an OpenAI key has obviously consented to talking to OpenAI. That is
 * configuration, not permission: keys get set up once, in Settings, often long
 * before the user asks the app to summarise a specific document — and the
 * guideline asks for permission to *share the data*, not to reach the service.
 *
 * ## Shape
 *
 * Modelled on `index::license_consent`: a gate that refuses, a UI that
 * confirms, a retry that proceeds. Consent is per-provider (agreeing to send
 * text to Groq says nothing about Google), persisted, and revocable from
 * Settings.
 */

import { getSetting, saveSetting } from '../store';

const SETTING_KEY = 'thirdPartyAiConsent';

/**
 * Providers whose base URL is decided by us and known to be remote.
 *
 * Deliberately keyed by id *and* re-checked by URL below, because `baseUrl` is
 * user-editable: someone can point the "ollama" provider at a hosted endpoint,
 * and an id-only check would wave that straight through. The URL is the thing
 * that determines whether bytes leave the machine, so the URL is what decides.
 */
export const KNOWN_REMOTE_PROVIDERS: Record<string, string> = {
	groq: 'Groq',
	openrouter: 'OpenRouter',
	mistral: 'Mistral AI',
	openai: 'OpenAI',
	nebius: 'Nebius',
	scaleway: 'Scaleway',
	anthropic: 'Anthropic',
	google: 'Google (Gemini)',
	poe: 'Poe'
};

/** Sentinels for in-process or in-webview inference — nothing leaves the device. */
const NON_NETWORK_BASE_URLS = new Set(['local', 'webllm', 'ort']);

/**
 * Does using this provider send bytes to someone else's server?
 *
 * Loopback is not egress: an Ollama or llama.cpp server the user runs is still
 * their machine, and `com.apple.security.network.client` covers reaching it.
 * The check is on the resolved URL rather than the provider id so that
 * re-pointing a "local" provider at a remote host is caught.
 */
export function isThirdPartyEgress(providerId: string, baseUrl?: string): boolean {
	const url = (baseUrl ?? '').trim();

	if (NON_NETWORK_BASE_URLS.has(url)) return false;
	if (url) {
		let host: string;
		try {
			// `URL.hostname` returns IPv6 literals *bracketed* — `[::1]`, not
			// `::1` — so a bare `host === '::1'` silently misses IPv6 loopback
			// and asks for consent to talk to the user's own machine.
			host = new URL(url).hostname.toLowerCase().replace(/^\[|\]$/g, '');
		} catch {
			// An unparseable base URL is not something to guess about. Treat it
			// as remote: over-asking is a worse user experience, under-asking is
			// a guideline violation.
			return true;
		}
		const isLoopback =
			host === 'localhost' ||
			host === '::1' ||
			host === '0.0.0.0' ||
			// The whole 127.0.0.0/8 block, not just 127.0.0.1.
			/^127\./.test(host) ||
			host.endsWith('.local');
		return !isLoopback;
	}

	// No URL to judge by — fall back to what we know about the id.
	return providerId in KNOWN_REMOTE_PROVIDERS;
}

/** Human-readable name for the disclosure text. */
export function providerDisplayName(providerId: string): string {
	return KNOWN_REMOTE_PROVIDERS[providerId] ?? providerId;
}

/** Thrown by the gate so the UI can show the disclosure and retry. */
export class ThirdPartyAiConsentRequired extends Error {
	readonly providerId: string;
	readonly providerName: string;
	readonly endpoint: string;
	constructor(providerId: string, endpoint: string) {
		const providerName = providerDisplayName(providerId);
		super(
			`Sending this text to ${providerName} needs your permission first ` +
				`(it leaves your device for ${endpoint}).`
		);
		this.name = 'ThirdPartyAiConsentRequired';
		this.providerId = providerId;
		this.providerName = providerName;
		this.endpoint = endpoint;
	}
}

async function consentedProviders(): Promise<string[]> {
	const raw = await getSetting(SETTING_KEY, []);
	return Array.isArray(raw) ? raw.filter((x) => typeof x === 'string') : [];
}

export async function hasConsent(providerId: string): Promise<boolean> {
	return (await consentedProviders()).includes(providerId);
}

/** Record permission for one provider. Idempotent. */
export async function grantConsent(providerId: string): Promise<void> {
	const current = await consentedProviders();
	if (!current.includes(providerId)) {
		await saveSetting(SETTING_KEY, [...current, providerId]);
	}
}

/** Withdraw it again — Settings needs this, and 5.1.2(i) is about ongoing permission, not a one-off click. */
export async function revokeConsent(providerId: string): Promise<void> {
	const current = await consentedProviders();
	await saveSetting(
		SETTING_KEY,
		current.filter((id) => id !== providerId)
	);
}

export async function listConsented(): Promise<string[]> {
	return consentedProviders();
}

/**
 * Asks the user and resolves to their answer. Registered by the UI.
 *
 * Injected rather than imported so this module stays free of Svelte: the CLI
 * and the unit tests import the gate too, and neither has a dialog to show.
 */
export type ConsentPrompter = (request: {
	providerId: string;
	providerName: string;
	endpoint: string;
}) => Promise<boolean>;

let prompter: ConsentPrompter | null = null;

/** Called once by the UI at startup. */
export function registerConsentPrompter(fn: ConsentPrompter | null): void {
	prompter = fn;
}

/**
 * The gate.
 *
 * Resolves silently when nothing leaves the device or permission is already on
 * record. Otherwise it asks — and only throws if there is nobody to ask or the
 * user says no.
 *
 * **Asking here, rather than letting callers catch and re-run, is the point.**
 * The obligation is "obtain explicit permission before doing so", so the check
 * has to sit at the one place every remote request passes. If each caller had
 * to wrap itself, the next caller added would be one nobody remembered to
 * wrap, and that is precisely the omission the guideline penalises.
 */
export async function ensureThirdPartyConsent(
	providerId: string,
	baseUrl?: string
): Promise<void> {
	if (!isThirdPartyEgress(providerId, baseUrl)) return;
	if (await hasConsent(providerId)) return;

	const endpoint = baseUrl ?? providerDisplayName(providerId);
	if (prompter) {
		const granted = await prompter({
			providerId,
			providerName: providerDisplayName(providerId),
			endpoint
		});
		if (granted) {
			await grantConsent(providerId);
			return;
		}
	}
	// No prompter (headless), or the user declined. Either way the data does
	// not leave: refusing is the only safe outcome, and the message says what
	// would have happened and where.
	throw new ThirdPartyAiConsentRequired(providerId, endpoint);
}
