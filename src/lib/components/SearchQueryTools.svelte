<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { i18n } from '$lib/i18n.svelte';
    import { buildQuery, quoteQueryLiteral, type QueryBuilderInput } from '$lib/search/queryBuilder';
    let { query = $bindable(''), onapply }: { query: string; onapply?: () => void } = $props();
    let all = $state(''), any = $state(''), phrase = $state(''), excluded = $state('');
    let field = $state<QueryBuilderInput['field']>('');
    let nearLeft = $state(''), nearRight = $state(''), distance = $state<number | undefined>(5), ordered = $state(false);
    let prefix = $state('');
    let words = $state<{ term: string; doc_freq: number }[]>([]);
    let busy = $state(false), error = $state(''), browsed = $state(false);
    let sequence = 0;
    const de = $derived(i18n.lang === 'de');
    const construction = $derived(buildQuery({ all, any, phrase, excluded, field, nearLeft, nearRight, distance: distance ?? null, ordered }));
    const built = $derived(construction.query);
    const builderError = $derived(construction.error === 'operands'
        ? (de ? 'Beide Wörter oder Wortfolgen für die Nähe-Abfrage eingeben.' : 'Enter both words or phrases for the proximity condition.')
        : construction.error === 'distance'
            ? (de ? 'Der Wortabstand muss eine ganze Zahl zwischen 0 und 1024 sein.' : 'Word distance must be a whole number from 0 to 1024.')
            : construction.error === 'length' ? (de ? 'Die Abfrage ist zu lang (maximal 8192 UTF-8-Bytes).' : 'The query is too long (maximum 8192 UTF-8 bytes).') : '');
    function apply() { if (built) { query = built; onapply?.(); } }
    async function browse() {
        const request = ++sequence;
        busy = true; error = ''; browsed = true;
        try {
            const found = await invoke<{ term: string; doc_freq: number }[]>('index_vocabulary', { prefix: prefix.trim(), limit: 50 });
            if (request === sequence) words = found;
        } catch (cause) { if (request === sequence) { error = String(cause); words = []; } }
        finally { if (request === sequence) busy = false; }
    }
    function changePrefix() { sequence += 1; busy = false; browsed = false; words = []; error = ''; }
    function addWord(term: string) { query = query.trim() ? `(${query.trim()}) AND ${quoteQueryLiteral(term)}` : quoteQueryLiteral(term); onapply?.(); }
</script>

<details class="query-tools">
    <summary>{de ? 'Suchhilfe, Abfrage-Builder und Wortliste' : 'Query help, builder, and indexed words'}</summary>
    <div class="tools-grid">
        <section aria-label={de ? 'Abfrage erstellen' : 'Query builder'}>
            <h3>{de ? 'Abfrage erstellen' : 'Build a query'}</h3>
            <div class="builder-fields">
                <label>{de ? 'Alle Wörter' : 'All these words'}<input bind:value={all} /></label>
                <label>{de ? 'Eines dieser Wörter' : 'Any of these words'}<input bind:value={any} /></label>
                <label>{de ? 'Genaue Wortfolge' : 'Exact phrase'}<input bind:value={phrase} /></label>
                <label>{de ? 'Ohne diese Wörter' : 'Without these words'}<input bind:value={excluded} /></label>
                <label>{de ? 'Textfeld' : 'Text field'}<select bind:value={field}>
                    <option value="">{de ? 'Alle Textfelder' : 'All text fields'}</option>
                    <option value="title">{de ? 'Titel' : 'Title'}</option><option value="body">{de ? 'Dokumenttext' : 'Body'}</option>
                    <option value="headings">{de ? 'Überschriften' : 'Headings'}</option><option value="translated">{de ? 'Übersetzter Text' : 'Translated body'}</option>
                </select></label>
            </div>
            <fieldset class="proximity-builder">
                <legend>{de ? 'Wörter in der Nähe' : 'Words near each other'}</legend>
                <div class="builder-fields">
                    <label>{de ? 'Erstes Wort oder Wortfolge' : 'First word or phrase'}<input bind:value={nearLeft} /></label>
                    <label>{de ? 'Zweites Wort oder Wortfolge' : 'Second word or phrase'}<input bind:value={nearRight} /></label>
                    <label>{de ? 'Höchstens Wörter dazwischen' : 'Maximum intervening words'}<input type="number" min="0" max="1024" step="1" bind:value={distance} /></label>
                    <label class="order-option"><input type="checkbox" bind:checked={ordered} />{de ? 'In dieser Reihenfolge' : 'In this order'}</label>
                </div>
                <p>{de ? '0 bedeutet direkt benachbart. Mehrere Wörter werden als genaue Wortfolge behandelt.' : '0 means adjacent. Multiple words are treated as an exact phrase.'}</p>
            </fieldset>
            {#if builderError}<p class="error" role="alert">{builderError}</p>{/if}
            {#if built}<code class="query-preview">{built}</code>{/if}
            <button type="button" onclick={apply} disabled={!built}>{de ? 'Abfrage übernehmen' : 'Use this query'}</button>
            <p>{de ? 'Die Abfrage wird übernommen; Suche startet erst mit Suchen.' : 'This fills the query. Press Search when you are ready.'}</p>
        </section>
        <section aria-label={de ? 'Suchsyntax' : 'Query syntax'}>
            <h3>{de ? 'Suchsyntax' : 'Query syntax'}</h3>
            <dl>
                <dt><code>invoice AND NOT draft</code></dt><dd>{de ? 'Beide Bedingungen müssen gelten.' : 'Require a word and exclude another.'}</dd>
                <dt><code>"annual report" OR statement</code></dt><dd>{de ? 'Genaue Wortfolge oder Alternative.' : 'An exact phrase or an alternative.'}</dd>
                <dt><code>title:(report OR review)</code></dt><dd>{de ? 'Auf ein Textfeld beschränken.' : 'Limit the expression to one text field.'}</dd>
                <dt><code>climate w/5 policy</code></dt><dd>{de ? 'Bis zu fünf Wörter dazwischen, beliebige Reihenfolge.' : 'Up to five intervening words, either order.'}</dd>
                <dt><code>climate pre/0 policy</code></dt><dd>{de ? 'Direkt benachbart in dieser Reihenfolge.' : 'Adjacent words in this order.'}</dd>
                <dt><code>report* · colo?r · report~1</code></dt><dd>{de ? 'Platzhalter oder Tippfehlertoleranz (0–2 Änderungen).' : 'Wildcards or typo tolerance (0–2 edits).'}</dd>
            </dl>
            <p>{de ? 'Volltext und Hybrid erzwingen explizite Textbedingungen. Semantisch sucht nach Bedeutung. Nähe-Abfragen erlauben Wörter, Wortfolgen und OR-Gruppen; AND und NOT in einem Nähe-Operanden werden abgelehnt.' : 'Full text and Hybrid enforce explicit text conditions. Semantic mode searches by meaning. Proximity accepts words, phrases and OR groups; AND and NOT inside a proximity operand are rejected.'}</p>
        </section>
        <section aria-label={de ? 'Indizierte Wörter' : 'Indexed words'}>
            <h3>{de ? 'Indizierte Wörter' : 'Indexed words'}</h3>
            <label>{de ? 'Wortanfang' : 'Word prefix'}<input bind:value={prefix} oninput={changePrefix} onkeydown={event => { if (event.key === 'Enter') { event.preventDefault(); void browse(); } }} placeholder={de ? 'z. B. rechn' : 'e.g. report'} /></label>
            <button type="button" onclick={browse} disabled={busy}>{busy ? (de ? 'Lädt…' : 'Loading…') : (de ? 'Wörter anzeigen' : 'Browse words')}</button>
            <p>{de ? 'Bis zu 50 Wörter aus dem lokalen Textindex. Anzahl = lebende indizierte Dokumente; Suchfilter gelten hier nicht.' : 'Up to 50 terms from the local text index. Counts are live indexed documents; search filters do not apply here.'}</p>
            {#if error}<p class="error" role="alert">{error}</p>{/if}
            {#if browsed && !busy && !error && words.length === 0}<p>{de ? 'Keine passenden Wörter.' : 'No matching indexed words.'}</p>{/if}
            <ul class="word-list">{#each words as word (word.term)}<li><button type="button" onclick={() => addWord(word.term)} aria-label={`${de ? 'Wort hinzufügen' : 'Add term'} ${word.term}`}><span>{word.term}</span><span>{word.doc_freq}</span></button></li>{/each}</ul>
        </section>
    </div>
</details>

<style>
    .query-tools { flex-shrink:0;border-bottom:1px solid #27272a;background:#101014;font-size:.8rem;max-height:50vh;overflow:auto; }
    summary { cursor:pointer;padding:9px 16px;color:#a5b4fc; }
    .tools-grid { display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:24px;padding:0 16px 16px; }
    section { min-width:0; } h3 { font-size:.85rem;color:#e4e4e7;margin:8px 0; }
    .builder-fields { display:grid;grid-template-columns:1fr 1fr;gap:8px; }
    .proximity-builder { border:1px solid #3f3f46;border-radius:6px;padding:8px;margin:12px 0 0;min-width:0; }
    .proximity-builder legend { color:#d4d4d8;font-size:.74rem;padding:0 4px; }
    .order-option { flex-direction:row;align-items:center;gap:6px;align-self:end;min-height:34px; }
    .order-option input { width:auto;min-height:0; }
    label { display:flex;flex-direction:column;gap:4px;color:#a1a1aa;font-size:.74rem; }
    input,select { width:100%;box-sizing:border-box;min-width:0;background:#18181b;border:1px solid #52525b;border-radius:5px;padding:7px;color:#f4f4f5; }
    button { margin-top:8px;background:#27272a;border:1px solid #52525b;border-radius:5px;color:#e4e4e7;padding:7px 10px;cursor:pointer; }
    button:disabled { opacity:.45;cursor:default; } button:focus-visible,input:focus-visible,select:focus-visible,summary:focus-visible { outline:2px solid #93c5fd;outline-offset:2px; }
    p,dd { color:#a1a1aa;font-size:.73rem;line-height:1.5; } dd { margin:2px 0 8px; } dt { color:#e4e4e7; } code { overflow-wrap:anywhere; }
    .query-preview { display:block;padding:8px;margin-top:8px;background:#18181b;white-space:pre-wrap; }
    .word-list { list-style:none;padding:0;margin:0;max-height:190px;overflow:auto; }.word-list button { display:flex;justify-content:space-between;width:100%;margin-top:4px;gap:12px;text-align:left; }.word-list span { overflow-wrap:anywhere;min-width:0; }
    .error { color:#fca5a5; } @media(max-width:900px){.tools-grid{grid-template-columns:1fr;gap:12px}.query-tools{max-height:45vh}button,input,select{min-height:40px}}
</style>
