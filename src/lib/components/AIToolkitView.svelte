<script lang="ts">
	import { aitoolkit, AIToolkitClient, capabilitiesFromFeatures } from '$lib/aitoolkit';

	let baseUrl = $state($aitoolkit.baseUrl);
	let token = $state<string | null>($aitoolkit.token);
	let username = $state('');
	let password = $state('');

	let connected = $state(false);
	let info = $state('');
	let caps = $state<Set<string>>(new Set());
	let providers = $state<Record<string, string[]>>({});
	let error = $state('');
	let busy = $state(false);
	let sub = $state('chat');

	function client() {
		return new AIToolkitClient(baseUrl.replace(/\/+$/, ''), token);
	}
	function fail(e: unknown) {
		error = e instanceof Error ? e.message : String(e);
	}
	function firstProvider(kind: string): string {
		return providers[kind]?.[0] ?? '';
	}

	async function connect() {
		error = '';
		busy = true;
		try {
			const c = client();
			const h = await c.health();
			info = `${h.backend} v${h.version}`;
			caps = capabilitiesFromFeatures((await c.config()).features ?? {});
			connected = true;
			aitoolkit.set({ baseUrl: baseUrl.replace(/\/+$/, ''), token });
			if (token) await loadProviders();
		} catch (e) {
			connected = false;
			fail(e);
		} finally {
			busy = false;
		}
	}

	async function login() {
		error = '';
		busy = true;
		try {
			token = (await client().login(username, password)).token;
			password = '';
			aitoolkit.set({ baseUrl: baseUrl.replace(/\/+$/, ''), token });
			await loadProviders();
		} catch (e) {
			fail(e);
		} finally {
			busy = false;
		}
	}

	async function loadProviders() {
		try {
			providers = (await client().providers()) as unknown as Record<string, string[]>;
		} catch {
			providers = {};
		}
	}

	// panels
	let model = $state('mistral-large-latest');
	let chatInput = $state('');
	let out = $state('');
	let target = $state('German');

	async function run(fn: () => Promise<string>) {
		error = '';
		out = '';
		busy = true;
		try {
			out = await fn();
		} catch (e) {
			fail(e);
		} finally {
			busy = false;
		}
	}
	const doChat = () => run(async () => (await client().chat(firstProvider('chat'), model, chatInput)).content);
	const doTranslate = () =>
		run(async () => (await client().translate(firstProvider('chat'), model, chatInput, target)).text);
	async function onImage(e: Event, kind: 'vision' | 'ocr') {
		const f = (e.target as HTMLInputElement).files?.[0];
		if (!f) return;
		run(async () =>
			kind === 'vision'
				? (await client().vision(firstProvider('vision'), model, f, chatInput || 'Describe this image.')).text
				: (await client().ocr(firstProvider('vision'), model, f)).text,
		);
	}
	async function onFile(e: Event) {
		const f = (e.target as HTMLInputElement).files?.[0];
		if (!f) return;
		run(async () => (await client().extract(f)).text);
	}

	const CAP_LABELS: Record<string, string> = {
		'service:chat': 'Chat',
		'service:extract': 'Extract',
		'service:transcription': 'Transcribe',
		'service:tts': 'Speak',
		'service:vision': 'Vision',
		'service:ocr': 'OCR',
		'service:images': 'Images',
		'service:translate': 'Translate',
	};
</script>

<div class="aitoolkit">
	<h2>AIToolkit</h2>

	<section class="conn">
		<label for="ai-url">Backend URL</label>
		<div class="row">
			<input id="ai-url" bind:value={baseUrl} placeholder="http://127.0.0.1:8000 or https://vps" />
			<button onclick={connect} disabled={busy}>Connect</button>
			<span class={connected ? 'ok' : 'muted'}>{connected ? `● ${info}` : '○ not connected'}</span>
		</div>
		{#if connected}
			<div class="row" style="margin-top:.5rem">
				<input bind:value={username} placeholder="username" />
				<input type="password" bind:value={password} placeholder="password" />
				<button onclick={login} disabled={busy}>Log in</button>
				{#if token}<span class="ok">signed in</span>{/if}
			</div>
		{/if}
	</section>

	{#if connected}
		<div class="caps">
			{#each [...caps] as cap}
				{@const id = cap.replace('service:', '')}
				<button class="cap" class:active={sub === id} onclick={() => (sub = id)}>{CAP_LABELS[cap] ?? cap}</button>
			{/each}
		</div>

		<section>
			{#if !token && sub !== 'extract'}<p class="muted">Log in to use provider-backed capabilities.</p>{/if}

			{#if sub === 'chat'}
				<input bind:value={model} placeholder="model" />
				<input bind:value={chatInput} placeholder="message" onkeydown={(e) => e.key === 'Enter' && doChat()} />
				<button onclick={doChat} disabled={busy || !token}>Send</button>
			{:else if sub === 'translate'}
				<input bind:value={model} placeholder="model" />
				<input bind:value={target} placeholder="target language" />
				<input bind:value={chatInput} placeholder="text to translate" />
				<button onclick={doTranslate} disabled={busy || !token}>Translate</button>
			{:else if sub === 'vision'}
				<input bind:value={model} placeholder="vision model" />
				<input bind:value={chatInput} placeholder="prompt (optional)" />
				<input type="file" accept="image/*" onchange={(e) => onImage(e, 'vision')} disabled={busy || !token} />
			{:else if sub === 'ocr'}
				<input bind:value={model} placeholder="vision model" />
				<input type="file" accept="image/*" onchange={(e) => onImage(e, 'ocr')} disabled={busy || !token} />
			{:else if sub === 'extract'}
				<p class="muted">Upload a file → extracted text.</p>
				<input type="file" onchange={onFile} disabled={busy} />
			{:else}
				<p class="muted">Capability <code>{sub}</code> is available on the backend; a panel for it is not wired here yet.</p>
			{/if}

			{#if out}<pre>{out}</pre>{/if}
		</section>
	{/if}

	{#if error}<p class="err">⚠ {error}</p>{/if}
</div>

<style>
	.aitoolkit { padding: 1rem 1.25rem; max-width: 760px; }
	.conn { margin-bottom: 1rem; }
	.row { display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; }
	label { display: block; font-size: 0.8rem; opacity: 0.7; margin-bottom: 0.2rem; }
	input { padding: 0.45rem 0.55rem; border-radius: 6px; margin: 0.2rem 0; }
	.caps { display: flex; gap: 0.4rem; flex-wrap: wrap; margin: 0.75rem 0; }
	.cap { padding: 0.35rem 0.7rem; border-radius: 999px; cursor: pointer; }
	.cap.active { font-weight: 600; }
	pre { white-space: pre-wrap; overflow-x: auto; padding: 0.6rem; border-radius: 6px; margin-top: 0.6rem; }
	.ok { color: #37b24d; }
	.err { color: #e03131; }
	.muted { opacity: 0.7; }
	section { margin-top: 0.75rem; }
</style>
