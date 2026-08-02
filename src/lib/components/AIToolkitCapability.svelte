<script lang="ts">
	// AI Act Art 50 surface. These panels reach a *remote* AIToolkit backend, so
	// none of the in-process guarantees the rest of the app relies on apply here:
	//
	//  · `images` and `tts` return synthetic artifacts that CrispASR never
	//    touched. The backend marks them itself (XMP / ID3 metadata assertion)
	//    and reports per response whether it succeeded — so the notice below
	//    reads that signal instead of assuming a state. For images the marked
	//    copy is `b64_json`, NOT `url`: see `markedImageSrc`.
	//  · `chat`, `translate` and `vision` (captioning) are synthetic text.
	//
	// See docs/ai-act.md § "Remote generative surfaces".
	import { onMount } from 'svelte';
	import { aitoolkit, AIToolkitClient, markedImageSrc } from '$lib/aitoolkit';
	import AiGeneratedBadge from './AiGeneratedBadge.svelte';
	import IntendedPurposeGate from './IntendedPurposeGate.svelte';
	import { i18n } from '$lib/i18n.svelte';

	let { capability }: { capability: string } = $props();

	/// Capabilities that generate content, as opposed to rendering input the
	/// user supplied. `ocr`, `transcription` and `extract` render what is
	/// already in the file, so Art 50(2) does not reach them.
	const GENERATIVE = new Set(['chat', 'translate', 'vision', 'images', 'tts']);
	/// Generated as a file we hand to the user but cannot watermark ourselves.
	const UNMARKABLE_ARTIFACT = new Set(['images', 'tts']);
	let generative = $derived(GENERATIVE.has(capability));

	let client = $derived(
		new AIToolkitClient($aitoolkit.baseUrl.replace(/\/+$/, ''), $aitoolkit.token),
	);
	let providers = $state<Record<string, string[]>>({});
	let error = $state('');
	let busy = $state(false);
	let out = $state('');
	let imgUrl = $state('');
	let audioUrl = $state('');
	/// Whether the artifact on screen actually carries the backend's Art 50(2)
	/// marking. Reported, never assumed: the backend returns `false` for formats
	/// it cannot mark, and claiming a mark that is not there is worse than
	/// admitting the gap.
	let artifactMarked = $state(false);
	/// True when the marking is a watermark in the signal rather than a metadata
	/// assertion — i.e. it survives re-encoding. Only the local CrispASR
	/// transport can offer that; remote providers hand back finished bytes.
	let watermarked = $state(false);
	/// The Art 50(4) deep-fake disclosure text as supplied by the backend.
	let backendDisclosure = $state('');

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
		artifactMarked = false;
		watermarked = false;
		backendDisclosure = '';
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
				const picked = markedImageSrc(r.images?.[0]);
				imgUrl = picked.src;
				artifactMarked = picked.marked;
				// The backend ships the Art 50(4) wording so clients neither invent
				// nor omit one. Prefer it over our own string.
				backendDisclosure = r.disclosure ?? '';
			} else if (capability === 'tts') {
				const spoken = await client.tts(firstP('transcription'), model, voice, input);
				audioUrl = URL.createObjectURL(spoken.blob);
				artifactMarked = spoken.marked;
				watermarked = spoken.markingPath === 'crispasr';
				backendDisclosure = '';
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
	{#if generative}
		<!-- Blocking, like Chat: this surface produces AI output right here, and
		     the completions never reach Rust, so the overlay is the enforcement
		     point rather than a reminder about one elsewhere. -->
		<IntendedPurposeGate />
	{/if}
	<h2>
		{LABELS[capability] ?? capability}
		{#if generative}<AiGeneratedBadge compact />{/if}
	</h2>
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
	{#if imgUrl}<img src={imgUrl} alt="AI-generated image" class="result-img" />{/if}
	{#if audioUrl}<audio controls src={audioUrl}></audio>{/if}
	{#if (imgUrl || audioUrl) && UNMARKABLE_ARTIFACT.has(capability)}
		<!-- The badge above marks the *panel*; this describes the *artifact*, which
		     is the thing that leaves the app. Art 50(2) attaches to the content, so
		     what matters is whether the bytes carry the mark — and the backend
		     reports that per response rather than guaranteeing it. Both states are
		     shown; neither is inferred. -->
		<p class="ai-artifact-note">
			{!artifactMarked
				? i18n.t.aiDisclosure.unmarkedArtifact
				: watermarked
					? i18n.t.aiDisclosure.watermarkedArtifact
					: i18n.t.aiDisclosure.markedArtifact}
		</p>
		{#if backendDisclosure}
			<p class="ai-artifact-note">{backendDisclosure}</p>
		{/if}
	{/if}
	{#if error}<p class="err">⚠ {error}</p>{/if}
</div>

<style>
	/* `position: relative` anchors the IntendedPurposeGate overlay, which is
	   `position: absolute; inset: 0` — without it the overlay escapes to the
	   nearest positioned ancestor and stops covering this panel. */
	.cap-view { padding: 1rem 1.25rem; max-width: 760px; position: relative; }
	h2 { display: flex; align-items: center; gap: 0.45rem; flex-wrap: wrap; }
	.ai-artifact-note {
		margin: 0.4rem 0 0;
		font-size: 0.8rem;
		line-height: 1.45;
		opacity: 0.85;
	}
	label { display: block; font-size: 0.8rem; opacity: 0.7; margin: 0.5rem 0 0.2rem; }
	input { padding: 0.45rem 0.55rem; border-radius: 6px; }
	button { margin-top: 0.6rem; }
	pre { white-space: pre-wrap; overflow-x: auto; padding: 0.6rem; border-radius: 6px; margin-top: 0.75rem; }
	.result-img { max-width: 100%; margin-top: 0.75rem; border-radius: 6px; }
	audio { display: block; margin-top: 0.75rem; }
	.err { color: #e03131; }
	.muted { opacity: 0.7; }
</style>
