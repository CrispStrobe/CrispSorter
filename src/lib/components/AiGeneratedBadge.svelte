<script lang="ts">
    // AI Act Art 50(2) disclosure marker for synthetic text.
    //
    // Chat answers, summaries and machine translation are content a model
    // generated, so they are labelled wherever they are shown. Deliberately one
    // shared component rather than an inline span per view: the obligation
    // applies to every generative surface, and a copied span is the thing that
    // gets forgotten when the next one is added.
    //
    // Not needed for synthesised *audio* — CrispASR watermarks that in the
    // signal itself (see docs/ai-act.md).
    import { Sparkles } from 'lucide-svelte';
    import { i18n } from '$lib/i18n.svelte';

    let { compact = false }: { compact?: boolean } = $props();
</script>

<span class="ai-badge" class:compact title={i18n.t.aiDisclosure.tooltip}>
    <Sparkles size={compact ? 11 : 13} aria-hidden="true" />
    <span class="ai-badge-label">{i18n.t.aiDisclosure.badge}</span>
</span>

<style>
    .ai-badge {
        display: inline-flex;
        align-items: center;
        gap: 0.28em;
        padding: 0.1em 0.45em;
        border: 1px solid var(--border, #d0d0d0);
        border-radius: 999px;
        font-size: 0.72rem;
        line-height: 1.5;
        color: var(--muted-fg, #666);
        background: var(--muted-bg, rgba(127, 127, 127, 0.08));
        white-space: nowrap;
        /* Informational, not interactive — must not read as a button. */
        cursor: help;
        user-select: none;
    }
    .ai-badge.compact {
        font-size: 0.66rem;
        padding: 0.05em 0.35em;
    }
    .ai-badge-label {
        letter-spacing: 0.01em;
    }
</style>
