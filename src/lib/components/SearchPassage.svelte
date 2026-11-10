<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { tick } from 'svelte';
    import { i18n } from '$lib/i18n.svelte';
    import { occurrenceRanges, queryTerms } from '$lib/search/inspection';
    let { docId, chunkIndex, query, sourceHash = '', ownerId }: { docId: string; chunkIndex: number; query: string; sourceHash?: string; ownerId?: string } = $props();
    interface Passage {
        text: string; start_char: number | null; end_char: number | null; offset_unit: string; truncated: boolean;
        chunk_index?: number; passage_number?: number; chunk_count?: number;
        previous_chunk_index?: number | null; next_chunk_index?: number | null;
        source_hash?: string | null; indexed_at?: number | null;
    }
    let passage = $state<Passage | null>(null), loading = $state(false), error = $state('');
    let find = $state(''), active = $state(0), retry = $state(0);
    let inspectedChunk = $state(0), caseSensitive = $state(false), wholeWords = $state(false);
    let root: HTMLElement;
    let pendingFocus: 'previous' | 'next' | null = null;
    const de = $derived(i18n.lang === 'de');
    const occurrences = $derived(occurrenceRanges(passage?.text || '', find.trim() ? [find.trim()] : queryTerms(query), 1001,
        find.trim() ? { caseSensitive, wholeWords } : {}));
    const ranges = $derived(occurrences.slice(0, 1000));
    const storedHash = $derived(passage?.source_hash);
    const refreshed = $derived(Boolean(sourceHash && passage?.source_hash && sourceHash !== passage.source_hash));
    $effect(() => { docId; ownerId; inspectedChunk = chunkIndex; find = ''; active = 0; pendingFocus = null; });
    $effect(() => {
        const id = docId, owner = ownerId, index = inspectedChunk; retry;
        const navigationFocus = pendingFocus; pendingFocus = null;
        let cancelled = false;
        passage = null; loading = true; error = ''; active = 0;
        invoke<Passage>('index_read_passage', { docId: id, ownerId: owner, chunkIndex: index })
            .then(async value => {
                if (cancelled) return;
                passage = value; loading = false;
                if (navigationFocus) {
                    await tick();
                    if (cancelled) return;
                    const preferred = root?.querySelector<HTMLButtonElement>(`[data-passage-nav="${navigationFocus}"]:not(:disabled)`);
                    (preferred || root?.querySelector<HTMLButtonElement>('[data-passage-nav]:not(:disabled)'))?.focus();
                }
            })
            .catch(cause => { if (!cancelled) error = String(cause); })
            .finally(() => { if (!cancelled) loading = false; });
        return () => { cancelled = true; };
    });
    $effect(() => { find; query; caseSensitive; wholeWords; active = 0; });
    $effect(() => {
        passage; find; query; caseSensitive; wholeWords;
        // Bring the first occurrence into view when the passage or find changes.
        // Scrolling does not steal focus from the caller or the find controls.
        void tick().then(() => root?.querySelector('[data-hit="0"]')?.scrollIntoView({ block: 'nearest' }));
    });
    async function move(delta: number) {
        if (!ranges.length) return;
        active = (active + delta + ranges.length) % ranges.length;
        await tick(); root?.querySelector(`[data-hit="${active}"]`)?.scrollIntoView({ block: 'center', behavior: 'smooth' });
    }
    function navigate(index: number | null | undefined, direction: 'previous' | 'next') {
        if (index == null) return;
        pendingFocus = direction; inspectedChunk = index;
    }
</script>

<section class="indexed-passage" aria-label={de ? 'Indizierte Textstelle' : 'Indexed passage'} bind:this={root}>
    {#if loading}<p role="status">{de ? 'Textstelle wird geladen…' : 'Loading indexed passage…'}</p>
    {:else if error}<p role="alert">{error}</p><button onclick={() => retry += 1}>{de ? 'Erneut versuchen' : 'Retry passage'}</button>
    {:else if passage}
        {#if passage.chunk_count != null}
            <div class="passage-navigation" role="group" aria-label={de ? 'Gespeicherte Textstellen durchgehen' : 'Stored passage navigation'}>
                <button data-passage-nav="previous" onclick={() => navigate(passage?.previous_chunk_index, 'previous')} disabled={passage.previous_chunk_index == null} aria-label={de ? 'Vorherige gespeicherte Textstelle' : 'Previous stored passage'}>←</button>
                <span role="status">{de ? 'Textstelle' : 'Passage'} {passage.passage_number ?? inspectedChunk + 1} / {passage.chunk_count}</span>
                <button data-passage-nav="next" onclick={() => navigate(passage?.next_chunk_index, 'next')} disabled={passage.next_chunk_index == null} aria-label={de ? 'Nächste gespeicherte Textstelle' : 'Next stored passage'}>→</button>
            </div>
        {/if}
        <div class="passage-tools">
            <label>{de ? 'In Textstelle finden' : 'Find in passage'}<input bind:value={find} placeholder={de ? 'Leer = Wörter der Suchabfrage' : 'Empty = query words'} onkeydown={event => { if (event.key === 'Enter' || event.key === 'F3') { event.preventDefault(); void move(event.shiftKey ? -1 : 1); } }} /></label>
            <div class="hit-navigation" role="group" aria-label={de ? 'Texttreffer durchgehen' : 'Text occurrence navigation'}>
                <button onclick={() => move(-1)} disabled={!ranges.length} aria-label={de ? 'Vorheriger Texttreffer' : 'Previous occurrence'}>↑</button>
                <span aria-live="polite">{ranges.length ? active + 1 : 0} / {ranges.length}{occurrences.length > 1000 ? '+' : ''}</span>
                <button onclick={() => move(1)} disabled={!ranges.length} aria-label={de ? 'Nächster Texttreffer' : 'Next occurrence'}>↓</button>
            </div>
        </div>
        <div class="find-options">
            <label><input type="checkbox" bind:checked={caseSensitive} disabled={!find.trim()} />{de ? 'Groß-/Kleinschreibung' : 'Match case'}</label>
            <label><input type="checkbox" bind:checked={wholeWords} disabled={!find.trim()} />{de ? 'Ganze Wörter' : 'Whole words'}</label>
        </div>
        <p class="provenance">{de ? 'Gespeicherte Textstelle' : 'Stored passage'} · {de ? 'Chunk' : 'Chunk'} {inspectedChunk}
            {#if passage.start_char != null} · UTF-8 {passage.start_char}–{passage.end_char ?? '?'}{/if}
            {#if storedHash} · <span title={storedHash}>SHA-256 {storedHash.slice(0, 12)}…</span>
            {:else} · {de ? 'Gespeicherter Fingerabdruck nicht verfügbar' : 'Stored fingerprint unavailable'}{/if}
        </p>
        {#if refreshed}<p class="refresh-notice" role="status">{de ? 'Der Index wurde seit dieser Suche aktualisiert. Sie sehen die aktuelle gespeicherte Textstelle; erneut suchen aktualisiert die Treffer.' : 'The index changed after this search. This is the current stored passage; search again to refresh the results.'}</p>{/if}
        <p class="provenance">{de ? 'Markierungen zeigen Textvorkommen, keine semantischen oder unscharfen Treffer. Enter / Umschalt+Enter navigiert im Suchfeld.' : 'Highlights show text occurrences, not semantic or fuzzy matches. Enter / Shift+Enter navigates from the find field.'}</p>
        {#if passage.truncated}<p>{de ? 'Vorschau auf 256 KiB begrenzt; Zählung gilt nur für diesen Ausschnitt.' : 'Preview limited to 256 KiB; occurrence counts cover only this excerpt.'}</p>{/if}
        <pre class="passage-text">{#each ranges as range, index}{passage.text.slice(index === 0 ? 0 : ranges[index - 1].end, range.start)}<mark class:current={active === index} data-hit={index}>{passage.text.slice(range.start, range.end)}</mark>{/each}{passage.text.slice(ranges.length ? ranges[ranges.length - 1].end : 0)}</pre>
    {/if}
</section>

<style>
    .indexed-passage{padding:12px;min-width:0;color:#e4e4e7;font-size:.8rem;}.passage-tools{display:flex;align-items:flex-end;gap:10px;flex-wrap:wrap;position:sticky;top:0;background:#18181b;padding:8px;border-radius:6px}.passage-tools label{display:flex;flex:1;flex-direction:column;gap:5px;min-width:150px;font-size:.72rem;color:#a1a1aa}.passage-tools input{background:#09090b;border:1px solid #52525b;border-radius:4px;color:#fafafa;padding:8px;min-width:0;width:100%;box-sizing:border-box}.hit-navigation{display:flex;align-items:center;gap:8px;white-space:nowrap}button{color:#e4e4e7;background:#27272a;border:1px solid #52525b;border-radius:5px;padding:7px 11px;cursor:pointer}button:disabled{opacity:.4;cursor:default}button:focus-visible,input:focus-visible{outline:2px solid #93c5fd;outline-offset:2px}.provenance{font-size:.68rem;color:#a1a1aa;overflow-wrap:anywhere;line-height:1.5}.passage-text{white-space:pre-wrap;overflow-wrap:anywhere;font: .82rem/1.7 system-ui;margin:12px 0}mark{background:#854d0e;color:#fef9c3;border-radius:2px}mark.current{background:#facc15;color:#18181b;outline:2px solid #fef08a;outline-offset:1px}
    .passage-navigation{display:flex;align-items:center;justify-content:space-between;gap:8px;margin-bottom:8px}.find-options{display:flex;flex-wrap:wrap;gap:12px;padding:8px;font-size:.72rem;color:#d4d4d8}.find-options label{display:flex;align-items:center;gap:5px}.refresh-notice{padding:8px;border:1px solid #a16207;border-radius:5px;color:#fde68a;line-height:1.5}
</style>
