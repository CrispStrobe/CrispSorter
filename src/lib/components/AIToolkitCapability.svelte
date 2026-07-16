<script lang="ts">
	import { onMount } from 'svelte';
	import { aitoolkit, AIToolkitClient } from '$lib/aitoolkit';

	let { capability }: { capability: string } = $props();

	let client = $derived(
		new AIToolkitClient($aitoolkit.baseUrl.replace(/\/+$/, ''), $aitoolkit.token),
	);
	let providers = $state<Record<string, string[]>>({});
	let error = $state('');
	let busy = $state(false);
	let out = $state('');
	let imgUrl = $state('');
	let audioUrl = $state('');

	let model = $state('mistral-large-latest');
	let input = $state('');
	let target = $state('German');
	let voice = $state('alloy');

	const LABELS: Record<string, string> = {
		chat: 'AI Chat',
		translate: 'AI Translate',
		vision: 'AI Vision',
		ocr: 'AI OCR',
		transcription: 'AI Transcribe',
		tts: 'AI Speak',
		images: 'AI Images',
		extract: 'AI Extract',
	};

	function firstP(kind: string) {
		return providers[kind]?.[0] ?? '';
	}
	function fail(e: unknown) {
		error = e instanceof Error ? e.message : String(e);
	}
	async function loadProviders() {
		try {
			providers = (await client.providers()) as unknown as Record<string, string[]>;
		} catch {
			providers = {};
		}
	}
	onMount(loadProviders);

	async function run(fn: () => Promise<void>) {
		error = '';
		out = '';
		imgUrl = '';
		audioUrl = '';
		busy = true;
		try {
			await fn();
		} catch (e) {
			fail(e);
		} finally {
			busy = false;
		}
	}

	const send = () =>
		run(async () => {
			if (capability === 'chat') out = (await client.chat(firstP('chat'), model, input)).content;
			else if (capability === 'translate')
				out = (await client.translate(firstP('chat'), model, input, target)).text;
			else if (capability === 'images') {
				const r = await client.generateImage(firstP('image'), model, input);
				const img = r.images?.[0];
				imgUrl = img?.url ?? (img?.b64_json ? `data:image/png;base64,${img.b64_json}` : '');
			} else if (capability === 'tts') {
				audioUrl = URL.createObjectURL(await client.tts(firstP('transcription'), model, voice, input));
			}
		});

	function onFile(e: Event) {
		const f = (e.target as HTMLInputElement).files?.[0];
		if (!f) return;
		run(async () => {
			if (capability === 'vision')
				out = (await client.vision(firstP('vision'), model, f, input || 'Describe this image.')).text;
			else if (capability === 'ocr') out = (await client.ocr(firstP('vision'), model, f)).text;
			else if (capability === 'transcription')
				out = (await client.transcribe(firstP('transcription'), model, f)).text;
			else if (capability === 'extract') out = (await client.extract(f)).text;
		});
	}

	const usesFile = ['vision', 'ocr', 'transcription', 'extract'];
	const accept: Record<string, string> = {
		vision: 'image/*',
		ocr: 'image/*',
		transcription: 'audio/*',
		extract: '',
	};
</script>

<div class="cap-view">
	<h2>{LABELS[capability] ?? capability}</h2>
	{#if !$aitoolkit.token && capability !== 'extract'}
		<p class="muted">Connect + log in on the <strong>AIToolkit</strong> tab to use this.</p>
	{/if}

	{#if capability !== 'extract'}
		<label for="cv-model">Model</label>
		<input id="cv-model" bind:value={model} />
	{/if}

	{#if capability === 'translate'}
		<label for="cv-target">Target language</label>
		<input id="cv-target" bind:value={target} />
	{/if}
	{#if capability === 'tts'}
		<label for="cv-voice">Voice</label>
		<input id="cv-voice" bind:value={voice} />
	{/if}

	{#if capability === 'chat' || capability === 'translate' || capability === 'images' || capability === 'tts'}
		<label for="cv-input">{capability === 'images' ? 'Prompt' : capability === 'chat' ? 'Message' : 'Text'}</label>
		<input id="cv-input" bind:value={input} onkeydown={(e) => e.key === 'Enter' && send()} />
		<button onclick={send} disabled={busy || !$aitoolkit.token}>Run</button>
	{/if}

	{#if capability === 'vision'}
		<label for="cv-prompt">Prompt (optional)</label>
		<input id="cv-prompt" bind:value={input} />
	{/if}

	{#if usesFile.includes(capability)}
		<label for="cv-file">File</label>
		<input id="cv-file" type="file" accept={accept[capability]} onchange={onFile} disabled={busy} />
	{/if}

	{#if out}<pre>{out}</pre>{/if}
	{#if imgUrl}<img src={imgUrl} alt="generated" class="result-img" />{/if}
	{#if audioUrl}<audio controls src={audioUrl}></audio>{/if}
	{#if error}<p class="err">⚠ {error}</p>{/if}
</div>

<style>
	.cap-view { padding: 1rem 1.25rem; max-width: 760px; }
	label { display: block; font-size: 0.8rem; opacity: 0.7; margin: 0.5rem 0 0.2rem; }
	input { padding: 0.45rem 0.55rem; border-radius: 6px; }
	button { margin-top: 0.6rem; }
	pre { white-space: pre-wrap; overflow-x: auto; padding: 0.6rem; border-radius: 6px; margin-top: 0.75rem; }
	.result-img { max-width: 100%; margin-top: 0.75rem; border-radius: 6px; }
	audio { display: block; margin-top: 0.75rem; }
	.err { color: #e03131; }
	.muted { opacity: 0.7; }
</style>
