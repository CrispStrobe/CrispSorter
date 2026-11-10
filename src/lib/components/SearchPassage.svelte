<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { tick } from 'svelte';
    import { i18n } from '$lib/i18n.svelte';
    import { occurrenceRanges, queryTerms } from '$lib/search/inspection';
    let { docId, chunkIndex, query, sourceHash = '' }: { docId: string; chunkIndex: number; query: string; sourceHash?: string } = $props();
    interface Passage { text: string; start_char: number | null; end_char: number | null; offset_unit: string; truncated: boolean }
    let passage = $state<Passage | null>(null), loading = $state(false), error = $state('');
    let find = $state(''), active = $state(0), retry = $state(0);
    let root: HTMLElement;
    const de = $derived(i18n.lang === 'de');
    const ranges = $derived(occurrenceRanges(passage?.text || '', find.trim() ? [find.trim()] : queryTerms(query)));
    $effect(() => {
        const id = docId, index = chunkIndex; retry;
        let cancelled = false;
        passage = null; loading = true; error = ''; active = 0; find = '';
        invoke<Passage>('index_read_passage', { docId: id, chunkIndex: index })
            .then(value => { if (!cancelled) passage = value; })
            .catch(cause => { if (!cancelled) error = String(cause); })
            .finally(() => { if (!cancelled) loading = false; });
        return () => { cancelled = true; };
    });
    $effect(() => { find; query; active = 0; });
    async function move(delta: number) {
        if (!ranges.length) return;
        active = (active + delta + ranges.length) % ranges.length;
        await tick(); root?.querySelector(`[data-hit="${active}"]`)?.scrollIntoView({ block: 'center', behavior: 'smooth' });
    }
</script>

<section class="indexed-passage" aria-label={de ? 'Indizierte Textstelle' : 'Indexed passage'} bind:this={root}>
    {#if loading}<p role="status">{de ? 'Textstelle wird geladen…' : 'Loading indexed passage…'}</p>
    {:else if error}<p role="alert">{error}</p><button onclick={() => retry += 1}>{de ? 'Erneut versuchen' : 'Retry passage'}</button>
    {:else if passage}
        <div class="passage-tools">
            <label>{de ? 'In Textstelle finden' : 'Find in passage'}<input bind:value={find} placeholder={de ? 'Leer = Wörter der Suchabfrage' : 'Empty = query words'} onkeydown={event => { if (event.key === 'Enter' || event.key === 'F3') { event.preventDefault(); void move(event.shiftKey ? -1 : 1); } }} /></label>
            <div class="hit-navigation" role="group" aria-label={de ? 'Texttreffer durchgehen' : 'Text occurrence navigation'}>
                <button onclick={() => move(-1)} disabled={!ranges.length} aria-label={de ? 'Vorheriger Texttreffer' : 'Previous occurrence'}>↑</button>
                <span aria-live="polite">{ranges.length ? active + 1 : 0} / {ranges.length}{ranges.length === 1000 ? '+' : ''}</span>
                <button onclick={() => move(1)} disabled={!ranges.length} aria-label={de ? 'Nächster Texttreffer' : 'Next occurrence'}>↓</button>
            </div>
        </div>
        <p class="provenance">{de ? 'Gespeicherte Textstelle' : 'Stored passage'} · {de ? 'Chunk' : 'Chunk'} {chunkIndex}
            {#if passage.start_char != null} · UTF-8 {passage.start_char}–{passage.end_char ?? '?'}{/if}
            {#if sourceHash} · <span title={sourceHash}>SHA-256 {sourceHash.slice(0, 12)}…</span>{/if}
        </p>
        <p class="provenance">{de ? 'Markierungen zeigen Textvorkommen, keine semantischen oder unscharfen Treffer. Enter / Umschalt+Enter navigiert im Suchfeld.' : 'Highlights show text occurrences, not semantic or fuzzy matches. Enter / Shift+Enter navigates from the find field.'}</p>
        {#if passage.truncated}<p>{de ? 'Vorschau auf 256 KiB begrenzt; Zählung gilt nur für diesen Ausschnitt.' : 'Preview limited to 256 KiB; occurrence counts cover only this excerpt.'}</p>{/if}
        <pre class="passage-text">{#each ranges as range, index}{passage.text.slice(index === 0 ? 0 : ranges[index - 1].end, range.start)}<mark class:current={active === index} data-hit={index}>{passage.text.slice(range.start, range.end)}</mark>{/each}{passage.text.slice(ranges.length ? ranges[ranges.length - 1].end : 0)}</pre>
    {/if}
</section>

<style>
    .indexed-passage{padding:12px;min-width:0;color:#e4e4e7;font-size:.8rem;}.passage-tools{display:flex;align-items:flex-end;gap:10px;flex-wrap:wrap;position:sticky;top:0;background:#18181b;padding:8px;border-radius:6px}.passage-tools label{display:flex;flex:1;flex-direction:column;gap:5px;min-width:150px;font-size:.72rem;color:#a1a1aa}.passage-tools input{background:#09090b;border:1px solid #52525b;border-radius:4px;color:#fafafa;padding:8px;min-width:0;width:100%;box-sizing:border-box}.hit-navigation{display:flex;align-items:center;gap:8px;white-space:nowrap}button{color:#e4e4e7;background:#27272a;border:1px solid #52525b;border-radius:5px;padding:7px 11px;cursor:pointer}button:disabled{opacity:.4;cursor:default}button:focus-visible,input:focus-visible{outline:2px solid #93c5fd;outline-offset:2px}.provenance{font-size:.68rem;color:#a1a1aa;overflow-wrap:anywhere;line-height:1.5}.passage-text{white-space:pre-wrap;overflow-wrap:anywhere;font: .82rem/1.7 system-ui;margin:12px 0}mark{background:#854d0e;color:#fef9c3;border-radius:2px}mark.current{background:#facc15;color:#18181b;outline:2px solid #fef08a;outline-offset:1px}
</style>
