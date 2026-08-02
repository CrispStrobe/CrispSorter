// AIToolkit backend client + connection store (graft). Lets CrispSorter reach an
// AIToolkit sidecar/VPS: probe /api/config for capabilities, log in, and call the
// provider-backed endpoints (chat, extract, …). In the Tauri webview cross-origin
// fetch is unrestricted; in browser dev the sidecar's CORS allows :1420.

import { writable } from 'svelte/store';

export interface AIToolkitState {
	baseUrl: string;
	token: string | null;
}

const KEY = 'aitoolkit.conn';
const defaults: AIToolkitState = { baseUrl: 'http://127.0.0.1:8000', token: null };

function load(): AIToolkitState {
	if (typeof localStorage === 'undefined') return { ...defaults };
	try {
		return { ...defaults, ...JSON.parse(localStorage.getItem(KEY) || '{}') };
	} catch {
		return { ...defaults };
	}
}

export const aitoolkit = writable<AIToolkitState>(load());
aitoolkit.subscribe((s) => {
	if (typeof localStorage !== 'undefined') localStorage.setItem(KEY, JSON.stringify(s));
});

/** Capabilities advertised by the connected backend (drives which AI tabs show). */
export const aitoolkitCaps = writable<Set<string>>(new Set());

/**
 * Pick the image source that carries the Art 50(2) marking.
 *
 * The backend returns the provider's original `url` *and* a marked `b64_json`
 * copy. Preferring `url` — which is what a plain `url ?? b64_json` does — shows
 * and saves the UNMARKED original and silently discards the marking the backend
 * created for exactly this reason. So `b64_json` wins whenever it exists, and
 * the caller is told which one it got rather than left to assume.
 */
export function markedImageSrc(img: { url?: string; b64_json?: string; marked?: boolean } | undefined): {
	src: string;
	marked: boolean;
} {
	if (!img) return { src: '', marked: false };
	if (img.b64_json) {
		// Sniff the base64 prefix: the backend marks PNG and JPEG, and labelling a
		// JPEG as image/png makes some viewers refuse it.
		const mime = img.b64_json.startsWith('/9j/') ? 'image/jpeg' : 'image/png';
		return { src: `data:${mime};base64,${img.b64_json}`, marked: img.marked !== false };
	}
	return { src: img.url ?? '', marked: false };
}

export function capabilitiesFromFeatures(features: Record<string, boolean>): Set<string> {
	const caps = new Set<string>();
	for (const [k, v] of Object.entries(features)) if (v) caps.add(`service:${k}`);
	return caps;
}

export class AIToolkitClient {
	constructor(
		public baseUrl: string,
		public token: string | null = null,
	) {}

	private async req<T>(path: string, init: RequestInit = {}): Promise<T> {
		const headers = new Headers(init.headers);
		if (init.body) headers.set('Content-Type', 'application/json');
		if (this.token) headers.set('Authorization', `Bearer ${this.token}`);
		const res = await fetch(`${this.baseUrl}${path}`, { ...init, headers });
		const text = await res.text();
		const body: any = text ? JSON.parse(text) : null;
		if (!res.ok) throw new Error(body?.detail ?? res.statusText);
		return body as T;
	}

	health() {
		return this.req<{ status: string; backend: string; version: string }>('/api/health');
	}
	config() {
		return this.req<{ features: Record<string, boolean>; defaults?: any }>('/api/config');
	}
	async login(username: string, password: string) {
		const out = await this.req<{ username: string; token: string }>('/api/auth/login', {
			method: 'POST',
			body: JSON.stringify({ username, password }),
		});
		this.token = out.token;
		return out;
	}
	providers() {
		return this.req<{
			chat: string[];
			transcription: string[];
			vision: string[];
			image: string[];
			ocr: string[];
		}>('/api/providers');
	}
	chat(provider: string, model: string, text: string) {
		return this.req<{ content: string }>('/api/chat/completions', {
			method: 'POST',
			body: JSON.stringify({ provider, model, messages: [{ role: 'user', content: text }] }),
		});
	}
	translate(provider: string, model: string, text: string, target: string) {
		return this.req<{ text: string }>('/api/translate/text', {
			method: 'POST',
			body: JSON.stringify({ provider, model, text, target }),
		});
	}

	private async multipart<T>(path: string, file: File, fields: Record<string, string>): Promise<T> {
		const fd = new FormData();
		fd.append('file', file);
		for (const [k, v] of Object.entries(fields)) fd.append(k, v);
		const headers = new Headers();
		if (this.token) headers.set('Authorization', `Bearer ${this.token}`);
		const res = await fetch(`${this.baseUrl}${path}`, { method: 'POST', body: fd, headers });
		if (!res.ok) throw new Error((await res.text()) || res.statusText);
		return res.json();
	}
	extract(file: File) {
		return this.multipart<{ text: string; extractor: string }>('/api/extract', file, {});
	}
	vision(provider: string, model: string, file: File, prompt: string) {
		return this.multipart<{ text: string }>('/api/vision/analyze', file, { provider, model, prompt });
	}
	ocr(provider: string, model: string, file: File) {
		return this.multipart<{ text: string }>('/api/ocr', file, { provider, model });
	}
	transcribe(provider: string, model: string, file: File) {
		return this.multipart<{ text: string }>('/api/transcription/sync', file, { provider, model });
	}
	/// Art 50(2): the backend marks every image it returns and puts the marked
	/// copy in `b64_json` — deliberately, "so the client never has to be trusted
	/// to mark on download". `url` is the provider's ORIGINAL and is unmarked, so
	/// never prefer it; see `markedImageSrc`. `disclosure` ships the Art 50(4)
	/// deep-fake wording so clients neither invent nor omit one.
	generateImage(provider: string, model: string, prompt: string) {
		return this.req<{
			images: { url?: string; b64_json?: string; marked?: boolean }[];
			ai_generated?: boolean;
			digital_source_type?: string;
			disclosure?: string;
		}>('/api/images/generate', {
			method: 'POST',
			body: JSON.stringify({ provider, model, prompt }),
		});
	}
	/// The audio bytes carry the marking in-band, so a saved file stays marked.
	/// Two strengths, and the difference matters to the user:
	///   · `crispasr` — AudioSeal watermark in the signal + C2PA. Survives
	///     re-encoding.
	///   · `provider-metadata` — an XMP chunk (WAV) or ID3v2 frames (MP3) around
	///     a third-party provider's bytes. Machine-readable, but lost on
	///     re-encode.
	/// `X-AI-Marked` reports whether the backend managed to mark at all (it
	/// returns `false` for formats it cannot handle); `X-AI-Marking-Path` says
	/// which of the two it was. Neither is assumed.
	async tts(
		provider: string,
		model: string,
		voice: string,
		text: string,
	): Promise<{ blob: Blob; marked: boolean; markingPath: string }> {
		const headers = new Headers({ 'Content-Type': 'application/json' });
		if (this.token) headers.set('Authorization', `Bearer ${this.token}`);
		const res = await fetch(`${this.baseUrl}/api/tts/synthesize`, {
			method: 'POST',
			headers,
			body: JSON.stringify({ provider, model, voice, text }),
		});
		if (!res.ok) throw new Error((await res.text()) || res.statusText);
		return {
			blob: await res.blob(),
			marked: res.headers.get('X-AI-Marked') === 'true',
			markingPath: res.headers.get('X-AI-Marking-Path') ?? '',
		};
	}
}
