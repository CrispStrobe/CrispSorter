<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { tick } from 'svelte';
    import { i18n } from '$lib/i18n.svelte';
    import { occurrenceRanges, queryTerms } from '$lib/search/inspection';
    import { evidenceRanges, evidenceKindLabel, evidenceFieldLabel, explanationClauseRows, readPassageExplanation,
        type DisplayedPassage, type EvidenceRange, type ExplanationRequest, type PassageExplanation, type QueryIntent } from '$lib/search/explanation';
    import type { SearchMode } from '$lib/search/recipe';
    let { docId, chunkIndex, query, sourceHash = '', ownerId, mode = 'hybrid', queryIntent = 'advanced', fuzzy = false, synonyms = false }:
        { docId: string; chunkIndex: number; query: string; sourceHash?: string; ownerId?: string;
            mode?: SearchMode; queryIntent?: QueryIntent; fuzzy?: boolean; synonyms?: boolean } = $props();
    interface Passage extends DisplayedPassage {
        passage_number?: number; chunk_count?: number;
        previous_chunk_index?: number | null; next_chunk_index?: number | null;
    }
    let passage = $state<Passage | null>(null), loading = $state(false), error = $state('');
    let loadedExplanation = $state<PassageExplanation | null>(null), explanationLoading = $state(false), explanationError = $state('');
    let explanationRetry = $state(0);
    let find = $state(''), active = $state(0), retry = $state(0);
    let inspectedChunk = $state(0), caseSensitive = $state(false), wholeWords = $state(false);
    let root: HTMLElement;
    let pendingFocus: 'previous' | 'next' | null = null;
    const de = $derived(i18n.lang === 'de');
    // Invalidate proof synchronously when selection changes, before async effect
    // cleanup or a new passage response can render offsets from an older text.
    const explanation = $derived(loadedExplanation && passage
        && loadedExplanation.query === query && loadedExplanation.mode === mode && loadedExplanation.query_intent === queryIntent
        && loadedExplanation.fuzzy === fuzzy && loadedExplanation.synonyms === synonyms
        && loadedExplanation.passage.doc_id === docId && loadedExplanation.passage.owner_id === (passage.owner_id ?? ownerId ?? '')
        && loadedExplanation.passage.chunk_index === inspectedChunk && loadedExplanation.passage.text === passage.text
        && (loadedExplanation.passage.source_hash ?? null) === (passage.source_hash ?? null)
        && (loadedExplanation.passage.indexed_at ?? null) === (passage.indexed_at ?? null)
        && loadedExplanation.passage.start_char === passage.start_char && loadedExplanation.passage.end_char === passage.end_char
        ? loadedExplanation : null);
    const proofRanges = $derived(explanation ? evidenceRanges(explanation.passage.text, explanation.spans) : []);
    const usingProof = $derived(Boolean(explanation && mode !== 'vector' && !find.trim()));
    const occurrences = $derived<EvidenceRange[]>(usingProof ? proofRanges : occurrenceRanges(passage?.text || '', find.trim() ? [find.trim()] : mode === 'vector' ? [] : queryTerms(query), 1001,
        find.trim() ? { caseSensitive, wholeWords } : {}).map(range => ({ ...range, evidence: [] })));
    const ranges = $derived(occurrences.slice(0, 1000));
    const clauseRows = $derived(explanationClauseRows(explanation?.clauses ?? null));
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
    $effect(() => {
        const selectedPassage = passage;
        const request: ExplanationRequest = { docId, ownerId: selectedPassage?.owner_id ?? ownerId ?? '',
            chunkIndex: inspectedChunk, query, mode, queryIntent, fuzzy, synonyms };
        explanationRetry;
        let cancelled = false;
        loadedExplanation = null; explanationError = ''; explanationLoading = false;
        if (!selectedPassage || !query.trim()) return;
        explanationLoading = true;
        invoke<unknown>('index_explain_passage', { ...request })
            .then(value => {
                if (!cancelled) loadedExplanation = readPassageExplanation(value, request, selectedPassage);
            })
            .catch(cause => { if (!cancelled) explanationError = String(cause); })
            .finally(() => { if (!cancelled) explanationLoading = false; });
        return () => { cancelled = true; };
    });
    $effect(() => { find; query; caseSensitive; wholeWords; explanation; active = 0; });
    $effect(() => {
        passage; find; query; caseSensitive; wholeWords; explanation;
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
        {#if query.trim()}
            <section class="query-evidence" aria-label={de ? 'Nachweis der Suchabfrage' : 'Query evidence'}>
                <h3>{de ? 'Nachweis der Suchabfrage' : 'Query evidence'}</h3>
                {#if explanationLoading}
                    <p role="status">{de ? 'Indizierte Suchbedingungen werden geprüft…' : 'Checking indexed query conditions…'}</p>
                {:else if explanationError}
                    <p class="evidence-unavailable" role="status">{de ? 'Abfragenachweis nicht verfügbar.' : 'Query evidence unavailable.'} {explanationError}</p>
                    <div class="evidence-actions">
                        <button onclick={() => explanationRetry += 1}>{de ? 'Nachweis erneut laden' : 'Retry query evidence'}</button>
                        <button onclick={() => retry += 1}>{de ? 'Textstelle neu laden' : 'Reload passage'}</button>
                    </div>
                {:else if explanation}
                    {#if explanation.lexical_document_match !== null}
                        <p class:expression-matched={explanation.lexical_document_match} class="expression-status" role="status">
                            {explanation.lexical_document_match
                                ? (de ? 'Die lexikalische Abfrage trifft auf das aktuell indizierte Dokument zu.' : 'The lexical expression matches the current indexed document.')
                                : (de ? 'Die lexikalische Abfrage trifft auf das aktuell indizierte Dokument nicht zu.' : 'The lexical expression does not match the current indexed document.')}
                        </p>
                    {/if}
                    {#if explanation.semantic_evidence}
                        <p class="semantic-evidence">{mode === 'vector'
                            ? (de ? 'Semantische Suche kann Textstellen ohne gemeinsame Suchwörter finden. Diese Ansicht zeigt den gespeicherten Text; einzelne Wörter werden nicht als semantischer Nachweis markiert.' : 'Semantic retrieval can find passages without shared query words. This view shows the stored text; individual words are not attributed to semantic relevance.')
                            : (de ? 'Hybridsuche kann semantische Ähnlichkeit verwenden. Die Markierungen erklären ausschließlich lexikalische Suchbedingungen.' : 'Hybrid retrieval can use semantic similarity. These highlights explain lexical query conditions only.')}</p>
                        {#if inspectedChunk !== chunkIndex}<p>{de ? 'Sie betrachten eine andere gespeicherte Textstelle. Beim Blättern wird die Suche nicht erneut ausgeführt oder bewertet.' : 'You are viewing another stored passage. Passage navigation does not rerun or rescore the search.'}</p>{/if}
                    {/if}
                    {#if explanation.lexical_document_match && !explanation.spans.length}
                        <p>{de ? 'Diese Textstelle enthält keine beitragenden Suchmarkierungen. Die passenden Bedingungen können ein anderes Textfeld oder eine andere gespeicherte Textstelle betreffen.' : 'This passage has no contributing query spans. Matching conditions can refer to another text field or stored passage.'}</p>
                    {/if}
                    {#if explanation.spans_truncated}<p class="evidence-limit" role="status">{de ? 'Weitere passende Textbereiche wurden wegen der Nachweisgrenze ausgelassen.' : 'Additional matching spans were omitted from this bounded explanation.'}</p>{/if}
                    {#if explanation.notes.length}
                        <details class="query-conditions evidence-scope">
                            <summary>{de ? 'Umfang des Nachweises' : 'Evidence scope'}</summary>
                            {#each explanation.notes as note}<p class="evidence-note">{note}</p>{/each}
                        </details>
                    {/if}
                    {#if clauseRows.length}
                        <details class="query-conditions">
                            <summary>{de ? 'Suchbedingungen' : 'Query conditions'} ({clauseRows.length})</summary>
                            {#if explanation.lexical_query !== explanation.query}<p>{de ? 'Ausgeführte lexikalische Abfrage' : 'Executed lexical query'}: <code>{explanation.lexical_query}</code></p>{/if}
                            <ol aria-label={de ? 'Auswertung der Suchbedingungen' : 'Query condition evaluation'}>
                                {#each clauseRows as row (row.key)}
                                    <li class:clause-matched={row.clause.matched} style={`--clause-depth:${Math.min(row.depth, 6)}`}>
                                        <span class="clause-status">{row.clause.matched ? (de ? 'Erfüllt' : 'Matched') : (de ? 'Nicht erfüllt' : 'Not matched')}</span>
                                        <code>{row.clause.label}</code>
                                        {#if row.clause.matched_fields?.length}
                                            <span class="clause-field">{de ? 'Treffer in' : 'Matched in'}: {row.clause.matched_fields.map(field => evidenceFieldLabel(field, de)).join(', ')}</span>
                                        {:else if row.clause.field}<span class="clause-field">{row.clause.field}</span>{/if}
                                    </li>
                                {/each}
                            </ol>
                        </details>
                    {/if}
                {/if}
            </section>
        {/if}
        <div class="passage-tools">
            <label>{de ? 'In Textstelle finden' : 'Find in passage'}<input bind:value={find} placeholder={mode === 'vector' ? (de ? 'Wörtlichen Text finden…' : 'Find literal text…') : explanation ? (de ? 'Leer = Nachweis der Suchabfrage' : 'Empty = query evidence') : (de ? 'Leer = Wörter der Suchabfrage' : 'Empty = query words')} onkeydown={event => { if (event.key === 'Enter' || event.key === 'F3') { event.preventDefault(); void move(event.shiftKey ? -1 : 1); } }} /></label>
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
        <p class="provenance">{usingProof
            ? (de ? 'Markierungen zeigen die nachgewiesenen positiven Suchbedingungen in dieser Textstelle. Das Suchfeld prüft unabhängig davon wörtliche Textvorkommen.' : 'Highlights show verified positive query conditions in this passage. Find inspects literal text occurrences independently.')
            : find.trim()
                ? (de ? 'Markierungen zeigen wörtliche Vorkommen des Suchtexts. Enter / Umschalt+Enter navigiert im Suchfeld.' : 'Highlights show literal occurrences of the find text. Enter / Shift+Enter navigates from the find field.')
                : mode === 'vector'
                    ? (de ? 'Semantische Relevanz wird nicht einzelnen Wörtern zugeschrieben. Mit dem Suchfeld können Sie wörtliche Vorkommen prüfen.' : 'Semantic relevance is not attributed to individual words. Use Find to inspect literal occurrences.')
                    : (de ? 'Markierungen sind Suchworthinweise; ein vollständiger Abfragenachweis ist noch nicht verfügbar.' : 'Highlights are query-word hints; complete query evidence is not available.')}</p>
        {#if passage.truncated}<p>{de ? 'Vorschau auf 256 KiB begrenzt; Zählung gilt nur für diesen Ausschnitt.' : 'Preview limited to 256 KiB; occurrence counts cover only this excerpt.'}</p>{/if}
        <pre class="passage-text">{#each ranges as range, index}{passage.text.slice(index === 0 ? 0 : ranges[index - 1].end, range.start)}<mark class:current={active === index} class:query-proof={usingProof} data-hit={index} data-evidence-kind={range.evidence.map(item => item.kind).join(' ') || undefined} title={range.evidence.map(item => `${evidenceKindLabel(item.kind, de)}: ${item.clause}`).join(' · ') || undefined}>{passage.text.slice(range.start, range.end)}</mark>{/each}{passage.text.slice(ranges.length ? ranges[ranges.length - 1].end : 0)}</pre>
    {/if}
</section>

<style>
    .indexed-passage{padding:12px;min-width:0;color:#e4e4e7;font-size:.8rem;}.passage-tools{display:flex;align-items:flex-end;gap:10px;flex-wrap:wrap;position:sticky;top:0;background:#18181b;padding:8px;border-radius:6px}.passage-tools label{display:flex;flex:1;flex-direction:column;gap:5px;min-width:150px;font-size:.72rem;color:#a1a1aa}.passage-tools input{background:#09090b;border:1px solid #52525b;border-radius:4px;color:#fafafa;padding:8px;min-width:0;width:100%;box-sizing:border-box}.hit-navigation{display:flex;align-items:center;gap:8px;white-space:nowrap}button{color:#e4e4e7;background:#27272a;border:1px solid #52525b;border-radius:5px;padding:7px 11px;cursor:pointer}button:disabled{opacity:.4;cursor:default}button:focus-visible,input:focus-visible{outline:2px solid #93c5fd;outline-offset:2px}.provenance{font-size:.68rem;color:#a1a1aa;overflow-wrap:anywhere;line-height:1.5}.passage-text{white-space:pre-wrap;overflow-wrap:anywhere;font: .82rem/1.7 system-ui;margin:12px 0}mark{background:#854d0e;color:#fef9c3;border-radius:2px}mark.current{background:#facc15;color:#18181b;outline:2px solid #fef08a;outline-offset:1px}
    .passage-navigation{display:flex;align-items:center;justify-content:space-between;gap:8px;margin-bottom:8px}.find-options{display:flex;flex-wrap:wrap;gap:12px;padding:8px;font-size:.72rem;color:#d4d4d8}.find-options label{display:flex;align-items:center;gap:5px}.refresh-notice{padding:8px;border:1px solid #a16207;border-radius:5px;color:#fde68a;line-height:1.5}
    .query-evidence{border:1px solid #3f3f46;border-radius:6px;padding:10px;margin-bottom:10px;font-size:.73rem;line-height:1.5;overflow-wrap:anywhere}.query-evidence h3{font-size:.8rem;margin:0 0 6px;color:#f4f4f5}.query-evidence p{margin:6px 0}.expression-status{color:#fcd34d}.expression-matched{color:#86efac}.semantic-evidence,.evidence-note{color:#c4b5fd}.evidence-unavailable,.evidence-limit{color:#fcd34d}.evidence-actions{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px}.evidence-actions button{font-size:.7rem}.query-conditions{margin-top:8px}.query-conditions summary{cursor:pointer;color:#93c5fd}.query-conditions summary:focus-visible{outline:2px solid #93c5fd;outline-offset:3px}.query-conditions code{white-space:pre-wrap;overflow-wrap:anywhere}.query-conditions ol{list-style:none;margin:8px 0 0;padding:0}.query-conditions li{display:flex;align-items:baseline;flex-wrap:wrap;gap:5px;margin:4px 0;padding-left:calc(var(--clause-depth)*9px);border-left:1px solid #3f3f46}.clause-status{font-size:.65rem;color:#a1a1aa}.clause-matched>.clause-status{color:#86efac}.clause-field{font-size:.65rem;color:#93c5fd}mark.query-proof{background:#164e63;color:#cffafe}mark.query-proof.current{background:#67e8f9;color:#083344;outline-color:#a5f3fc}
</style>
