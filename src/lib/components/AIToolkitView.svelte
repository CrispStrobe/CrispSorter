<script lang="ts">
	import { aitoolkit, AIToolkitClient, capabilitiesFromFeatures } from '$lib/aitoolkit';

	let baseUrl = $state($aitoolkit.baseUrl);
	let token = $state<string | null>($aitoolkit.token);
	let username = $state('');
	let password = $state('');

	let connected = $state(false);
	let info = $state('');
	let caps = $state<Set<string>>(new Set());
	let providersChat = $state<string[]>([]);
	let error = $state('');
	let busy = $state(false);
	let sub = $state('chat');

	function client() {
		return new AIToolkitClient(baseUrl.replace(/\/+$/, ''), token);
	}
	function fail(e: unknown) {
		error = e instanceof Error ? e.message : String(e);
	}

	async function connect() {
		error = '';
		busy = true;
		try {
			const c = client();
			const h = await c.health();
			info = `${h.backend} v${h.version}`;
			const cfg = await c.config();
			caps = capabilitiesFromFeatures(cfg.features ?? {});
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
			const c = client();
			const out = await c.login(username, password);
			token = out.token;
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
			providersChat = (await client().providers()).chat ?? [];
		} catch {
			providersChat = [];
		}
	}

	// chat panel
	let chatModel = $state('mistral-large-latest');
	let chatInput = $state('');
	let chatReply = $state('');
	async function sendChat() {
		error = '';
		chatReply = '';
		busy = true;
		try {
			const out = await client().chat(providersChat[0] ?? '', chatModel, chatInput);
			chatReply = out.content;
		} catch (e) {
			fail(e);
		} finally {
			busy = false;
		}
	}

	// extract panel
	let exText = $state('');
	async function onExtract(e: Event) {
		const f = (e.target as HTMLInputElement).files?.[0];
		if (!f) return;
		error = '';
		exText = '';
		busy = true;
		try {
			exText = (await client().extract(f)).text;
		} catch (err) {
			fail(err);
		} finally {
			busy = false;
		}
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
		<label>Backend URL</label>
		<div class="row">
			<input bind:value={baseUrl} placeholder="http://127.0.0.1:8000 or https://vps" />
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
				<button class="cap" class:active={sub === cap.replace('service:', '')} onclick={() => (sub = cap.replace('service:', ''))}>
					{CAP_LABELS[cap] ?? cap}
				</button>
			{/each}
		</div>

		{#if sub === 'chat' && caps.has('service:chat')}
			<section>
				<p class="muted">provider: {providersChat[0] ?? '—'} {token ? '' : '(log in to use)'}</p>
				<input bind:value={chatModel} placeholder="model" />
				<input bind:value={chatInput} placeholder="message" onkeydown={(e) => e.key === 'Enter' && sendChat()} />
				<button onclick={sendChat} disabled={busy || !token || !chatInput}>Send</button>
				{#if chatReply}<pre>{chatReply}</pre>{/if}
			</section>
		{:else if sub === 'extract' && caps.has('service:extract')}
			<section>
				<p class="muted">Upload a file → extracted text.</p>
				<input type="file" onchange={onExtract} disabled={busy} />
				{#if exText}<pre>{exText}</pre>{/if}
			</section>
		{:else}
			<section><p class="muted">Capability <code>{sub}</code> is available on the backend; a panel for it is not wired here yet.</p></section>
		{/if}
	{/if}

	{#if error}<p class="err">⚠ {error}</p>{/if}
</div>

<style>
	.aitoolkit { padding: 1rem 1.25rem; max-width: 760px; }
	.conn { margin-bottom: 1rem; }
	.row { display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; }
	label { display: block; font-size: 0.8rem; opacity: 0.7; margin-bottom: 0.2rem; }
	input { padding: 0.45rem 0.55rem; border-radius: 6px; }
	.caps { display: flex; gap: 0.4rem; flex-wrap: wrap; margin: 0.75rem 0; }
	.cap { padding: 0.35rem 0.7rem; border-radius: 999px; cursor: pointer; }
	.cap.active { font-weight: 600; }
	pre { white-space: pre-wrap; overflow-x: auto; padding: 0.6rem; border-radius: 6px; }
	.ok { color: #37b24d; }
	.err { color: #e03131; }
	.muted { opacity: 0.7; }
	section { margin-top: 0.75rem; }
</style>
