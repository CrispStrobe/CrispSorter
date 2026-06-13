<script lang="ts">
    /**
     * Reusable tag-cloud filter (Tier 2). Renders the distinct tags of a
     * corpus as count-weighted, clickable chips. Stateless — the parent owns
     * the facet list + the current selection and reacts to `ontoggle`.
     *
     * Designed to drop into BOTH the Übersicht browse and (later) the search
     * results view, so it carries its own scoped styles and takes no global
     * dependencies.
     */
    import { Loader2, Tag, X } from 'lucide-svelte';

    interface TagFacet { tag: string; count: number; }

    let {
        facets = [],
        selected = new Set<string>(),
        loading = false,
        groupEntities = false,
        ontoggle,
        onclear,
    }: {
        facets?: TagFacet[];
        selected?: Set<string>;
        loading?: boolean;
        /**
         * P19 — when true, tags shaped like `"<label>:<text>"` (the GLiNER
         * NER entity tags) are grouped under a per-label header into an
         * "Entities" view; plain tags keep the flat cloud above. Default
         * false leaves the original behaviour untouched.
         */
        groupEntities?: boolean;
        ontoggle: (tag: string) => void;
        onclear?: () => void;
    } = $props();

    // Scale the font size with the count so the cloud reads at a glance.
    // Clamp to a small range; `max` guards the single-tag corpus.
    let maxCount = $derived(Math.max(1, ...facets.map((f) => f.count)));
    function weight(count: number): number {
        // 0.80rem … 1.15rem across the count range (sqrt softens outliers).
        const t = Math.sqrt(count) / Math.sqrt(maxCount);
        return 0.8 + t * 0.35;
    }

    interface FacetGroup { label: string; facets: TagFacet[]; }

    // Plain (non-namespaced) tags shown in the flat cloud.
    let plainFacets = $derived(
        groupEntities ? facets.filter((f) => !f.tag.includes(':')) : facets,
    );
    // Namespaced entity tags, bucketed by the prefix before the first ':'.
    let entityGroups = $derived.by((): FacetGroup[] => {
        if (!groupEntities) return [];
        const buckets = new Map<string, TagFacet[]>();
        for (const f of facets) {
            const i = f.tag.indexOf(':');
            if (i <= 0) continue;
            const label = f.tag.slice(0, i);
            (buckets.get(label) ?? buckets.set(label, []).get(label)!).push(f);
        }
        return [...buckets.entries()]
            .sort((a, b) => a[0].localeCompare(b[0]))
            .map(([label, fs]) => ({ label, facets: fs }));
    });
    // Strip the `label:` prefix for display; the toggle value stays the full tag.
    function entityText(tag: string): string {
        const i = tag.indexOf(':');
        return i >= 0 ? tag.slice(i + 1) : tag;
    }
</script>

<div class="tag-cloud">
    {#if loading && facets.length === 0}
        <span class="tc-muted"><Loader2 size={12} class="spin" /> Tags …</span>
    {:else if facets.length === 0}
        <span class="tc-muted"><Tag size={12} /> Keine Tags</span>
    {:else}
        {#if selected.size > 0 && onclear}
            <button class="tc-chip tc-clear" onclick={() => onclear?.()} title="Tag-Filter leeren">
                <X size={11} /> {selected.size}
            </button>
        {/if}
        {#each plainFacets as f (f.tag)}
            <button
                class="tc-chip"
                class:active={selected.has(f.tag)}
                style="font-size:{weight(f.count)}rem"
                onclick={() => ontoggle(f.tag)}
                title="{f.tag} — {f.count.toLocaleString()} Dokument(e)"
            >
                {f.tag}<span class="tc-count">{f.count.toLocaleString()}</span>
            </button>
        {/each}
    {/if}
</div>
{#if groupEntities && entityGroups.length > 0}
    {#each entityGroups as g (g.label)}
        <div class="tc-group-label">{g.label}</div>
        <div class="tag-cloud">
            {#each g.facets as f (f.tag)}
                <button
                    class="tc-chip"
                    class:active={selected.has(f.tag)}
                    style="font-size:{weight(f.count)}rem"
                    onclick={() => ontoggle(f.tag)}
                    title="{f.tag} — {f.count.toLocaleString()} Dokument(e)"
                >
                    {entityText(f.tag)}<span class="tc-count">{f.count.toLocaleString()}</span>
                </button>
            {/each}
        </div>
    {/each}
{/if}

<style>
    .tag-cloud {
        display: flex;
        flex-wrap: wrap;
        align-items: baseline;
        gap: 6px;
        padding: 8px 10px;
        background: #18181b;
        border: 1px solid #27272a;
        border-radius: 8px;
        max-height: 160px;
        overflow-y: auto;
    }
    .tc-chip {
        display: inline-flex;
        align-items: baseline;
        gap: 4px;
        padding: 2px 8px;
        border: 1px solid #3f3f46;
        border-radius: 999px;
        background: #27272a;
        color: #d4d4d8;
        cursor: pointer;
        line-height: 1.4;
        transition: background 0.12s, border-color 0.12s, color 0.12s;
    }
    .tc-chip:hover { background: #3f3f46; border-color: #52525b; }
    .tc-chip.active {
        background: #1d4ed8;
        border-color: #2563eb;
        color: #fff;
    }
    .tc-count {
        font-size: 0.7em;
        color: #a1a1aa;
        font-variant-numeric: tabular-nums;
    }
    .tc-chip.active .tc-count { color: #bfdbfe; }
    .tc-clear { background: #3f1d1d; border-color: #7f1d1d; color: #fca5a5; }
    .tc-clear:hover { background: #7f1d1d; color: #fff; }
    .tc-muted {
        display: inline-flex;
        align-items: center;
        gap: 5px;
        color: #71717a;
        font-size: 0.82rem;
    }
    .tc-group-label {
        margin: 8px 2px 2px;
        font-size: 0.72rem;
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.04em;
        color: #71717a;
    }
    :global(.spin) { animation: tc-spin 1s linear infinite; }
    @keyframes tc-spin { to { transform: rotate(360deg); } }
</style>
