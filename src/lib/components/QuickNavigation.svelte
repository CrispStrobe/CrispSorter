<script lang="ts">
    import { onMount, tick } from 'svelte';
    import { ArrowRight, Search, X } from 'lucide-svelte';
    import { navigationCopy, searchNavigation, type NavigationDestination } from '$lib/navigation';
    import type { Language } from '$lib/i18n.svelte';

    let { destinations, current, language, onNavigate, onClose }: {
        destinations: NavigationDestination[];
        current: string;
        language: Language;
        onNavigate: (id: string) => void;
        onClose: () => void;
    } = $props();

    let dialog: HTMLDialogElement;
    let input: HTMLInputElement;
    let resultList: HTMLDivElement;
    let query = $state('');
    let selectedIndex = $state(0);
    const copy = $derived(navigationCopy[language]);
    const results = $derived(searchNavigation(destinations, query));

    $effect(() => { results; selectedIndex = 0; });
    $effect(() => {
        selectedIndex;
        if (resultList) {
            tick().then(() => resultList?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' }));
        }
    });

    onMount(() => {
        const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        dialog.showModal();
        input.focus();
        return () => {
            dialog.close();
            if (previous?.isConnected) previous.focus();
        };
    });

    function choose(id: string) {
        onNavigate(id);
        onClose();
    }

    function onSearchKey(event: KeyboardEvent) {
        if (event.isComposing || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault();
            if (results.length) selectedIndex = (selectedIndex + (event.key === 'ArrowDown' ? 1 : -1) + results.length) % results.length;
        } else if (event.key === 'Enter') {
            event.preventDefault();
            if (results[selectedIndex]) choose(results[selectedIndex].id);
        }
    }

    function onBackdropClick(event: MouseEvent) {
        if (event.target !== dialog) return;
        const rect = dialog.getBoundingClientRect();
        if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) onClose();
    }
</script>

<dialog class="quick-navigation" bind:this={dialog} aria-labelledby="quick-navigation-title"
    oncancel={(event) => { event.preventDefault(); onClose(); }} onclick={onBackdropClick}>
    <div class="palette-header">
        <h2 id="quick-navigation-title">{copy.title}</h2>
        <button class="close-button" aria-label={copy.close} onclick={onClose}><X size={18} /></button>
    </div>
    <div class="search-field">
        <Search size={20} aria-hidden="true" />
        <input bind:this={input} bind:value={query} onkeydown={onSearchKey}
            type="search" autocomplete="off" spellcheck="false" role="combobox"
            aria-label={copy.search} aria-expanded="true" aria-controls="navigation-results"
            aria-autocomplete="list" aria-activedescendant={results[selectedIndex] ? `navigation-result-${selectedIndex}` : undefined}
            placeholder={copy.placeholder} />
    </div>
    <div class="results" id="navigation-results" bind:this={resultList} role="listbox" aria-label={copy.open}>
        {#each results as destination, index (destination.id)}
            <button id={`navigation-result-${index}`} class="destination" class:selected={index === selectedIndex}
                role="option" aria-selected={index === selectedIndex} tabindex="-1"
                onclick={() => choose(destination.id)} onpointermove={() => selectedIndex = index}>
                <span class="destination-text">
                    <span class="destination-name">{destination.label}{#if current === destination.id}<span class="current-badge">{copy.current}</span>{/if}</span>
                    <span class="destination-description">{destination.description}</span>
                </span>
                <ArrowRight size={17} aria-hidden="true" />
            </button>
        {/each}
    </div>
    {#if !results.length}
        <div class="empty-state" role="status"><strong>{copy.empty}</strong><span>{copy.emptyHint}</span></div>
    {/if}
    <div class="palette-footer">
        <span><kbd>↑</kbd><kbd>↓</kbd> {copy.navigate}</span>
        <span><kbd>Enter</kbd> {copy.choose}</span>
        <span><kbd>Esc</kbd> {copy.dismiss}</span>
        <span class="result-count" aria-live="polite">{results.length} {results.length === 1 ? copy.result : copy.results}</span>
    </div>
</dialog>

<style>
    .quick-navigation { width: min(620px, calc(100vw - 32px)); max-height: min(680px, calc(100dvh - 48px)); margin: min(15vh, 100px) auto auto; padding: 0; background: #18181b; color: #fafafa; border: 1px solid #52525b; border-radius: 14px; box-shadow: 0 24px 80px #0008; overflow: hidden; }
    .quick-navigation[open] { display: flex; flex-direction: column; }
    .quick-navigation::backdrop { background: #09090bb8; }
    .palette-header { display: flex; align-items: center; gap: 12px; padding: 16px 18px 8px; }
    h2 { flex: 1; font-size: 0.9rem; font-weight: 600; margin: 0; color: #d4d4d8; }
    .close-button { display: inline-flex; align-items: center; justify-content: center; width: 32px; height: 32px; border: 0; border-radius: 6px; background: transparent; color: #a1a1aa; cursor: pointer; }
    .close-button:hover { color: white; background: #27272a; }
    .search-field { display: flex; align-items: center; gap: 10px; padding: 8px 18px 16px; border-bottom: 1px solid #3f3f46; color: #a1a1aa; }
    input { width: 100%; min-width: 0; border: 0; outline: none; padding: 8px 0; background: transparent; color: #fafafa; font: inherit; font-size: 1rem; }
    input::placeholder { color: #a1a1aa; }
    .search-field:focus-within { box-shadow: inset 0 -2px #60a5fa; }
    .results { overflow-y: auto; min-height: 0; padding: 6px; }
    .destination { width: 100%; display: flex; align-items: center; gap: 12px; padding: 12px; border: 1px solid transparent; border-radius: 8px; background: transparent; color: #fafafa; text-align: left; cursor: pointer; }
    .destination.selected { background: #1e3a5f; border-color: #3b82f6; }
    .destination-text { display: flex; flex: 1; flex-direction: column; gap: 5px; min-width: 0; }
    .destination-name { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; font-size: 0.9rem; font-weight: 600; }
    .destination-description { font-size: 0.8rem; line-height: 1.4; color: #a1a1aa; }
    .destination.selected .destination-description { color: #dbeafe; }
    .current-badge { font-size: 0.65rem; padding: 2px 6px; border: 1px solid #52525b; border-radius: 4px; color: #d4d4d8; font-weight: 400; }
    .empty-state { display: flex; flex-direction: column; gap: 8px; text-align: center; padding: 32px 20px; }
    .empty-state span { font-size: 0.85rem; color: #a1a1aa; }
    .palette-footer { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; border-top: 1px solid #3f3f46; padding: 12px 18px; color: #a1a1aa; font-size: 0.7rem; }
    kbd { display: inline-block; font: inherit; border: 1px solid #52525b; border-radius: 3px; padding: 1px 4px; margin-right: 3px; color: #e4e4e7; }
    .result-count { margin-left: auto; }
    button:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
    @media (max-width: 767px) {
        .quick-navigation { margin-top: 16px; max-height: calc(100dvh - 32px); }
        .close-button { width: 40px; height: 40px; }
        .palette-footer { gap: 8px; padding: 10px 12px; }
        .result-count { display: none; }
    }
</style>
