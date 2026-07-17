import { afterEach, describe, expect, it, vi } from 'vitest';
import { AIToolkitClient, capabilitiesFromFeatures } from './aitoolkit';

function stubFetch(status: number, body: unknown) {
	const fn = vi.fn(
		async () =>
			new Response(body === null ? '' : JSON.stringify(body), {
				status,
				headers: { 'content-type': 'application/json' },
			}),
	);
	vi.stubGlobal('fetch', fn);
	return fn;
}

afterEach(() => vi.unstubAllGlobals());

describe('capabilitiesFromFeatures', () => {
	it('maps enabled features to service:* caps', () => {
		const caps = capabilitiesFromFeatures({ chat: true, tts: false, extract: true });
		expect(caps.has('service:chat')).toBe(true);
		expect(caps.has('service:tts')).toBe(false);
		expect(caps.has('service:extract')).toBe(true);
	});
});

describe('AIToolkitClient', () => {
	it('health returns backend info', async () => {
		stubFetch(200, { status: 'ok', backend: 'python-sidecar', version: '0.0.1' });
		expect((await new AIToolkitClient('http://h').health()).backend).toBe('python-sidecar');
	});

	it('login captures the token', async () => {
		stubFetch(200, { username: 'alice', token: 'T' });
		const c = new AIToolkitClient('http://h');
		expect((await c.login('alice', 'pw')).token).toBe('T');
		expect(c.token).toBe('T');
	});

	it('chat sends Bearer and returns content', async () => {
		const f = stubFetch(200, { content: 'hi' });
		const out = await new AIToolkitClient('http://h', 'T').chat('P', 'm', 'x');
		expect(out.content).toBe('hi');
		const init = f.mock.calls[0][1] as RequestInit;
		expect((init.headers as Headers).get('authorization')).toBe('Bearer T');
	});

	it('translate returns text', async () => {
		stubFetch(200, { text: 'HALLO' });
		const out = await new AIToolkitClient('http://h', 'T').translate('P', 'm', 'hi', 'German');
		expect(out.text).toBe('HALLO');
	});

	it('throws with the server detail on error', async () => {
		stubFetch(400, { detail: 'bad request' });
		await expect(new AIToolkitClient('http://h').config()).rejects.toThrow('bad request');
	});

	it('extract uploads multipart (FormData)', async () => {
		const f = stubFetch(200, { text: 'T', extractor: 'builtin-docx' });
		const file = new File([new Uint8Array([1, 2, 3])], 'a.docx');
		const out = await new AIToolkitClient('http://h', 'T').extract(file);
		expect(out.text).toBe('T');
		const init = f.mock.calls[0][1] as RequestInit;
		expect(init.body instanceof FormData).toBe(true);
	});
});
