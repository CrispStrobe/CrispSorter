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
}
