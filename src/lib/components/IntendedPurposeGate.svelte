<script module lang="ts">
    // Status is shared across instances. This component can render once per
    // search-result row, and N copies each invoking `intended_purpose_status`
    // would be N identical IPC round-trips for one process-wide fact. One probe,
    // shared reactive result, and `accept()` from any instance updates them all.
    import { invoke as _invoke } from '@tauri-apps/api/core';

    export type Status = {
        acknowledged: boolean;
        version: number;
        statement: string;
        accepted_at_unix: number | null;
    };

    let shared = $state<Status | null>(null);
    let probed = false;

    export async function probeOnce(): Promise<string> {
        if (probed) return '';
        probed = true;
        try {
            shared = await _invoke<Status>('intended_purpose_status');
            return '';
        } catch (e: any) {
            // Older backend without the command: do not invent an
            // acknowledgement, but do not wedge the UI either — Rust is the
            // enforcement point, so failing open here cannot produce
            // unacknowledged output. Allow a later retry.
            probed = false;
            return String(e?.message ?? e);
        }
    }

    export async function acceptShared(): Promise<string> {
        try {
            shared = await _invoke<Status>('intended_purpose_acknowledge');
            return '';
        } catch (e: any) {
            return String(e?.message ?? e ?? 'could not record the acknowledgement');
        }
    }

    export function sharedStatus(): Status | null {
        return shared;
    }
</script>

<script lang="ts">
    // One-time intended-purpose acknowledgement.
    //
    // The Rust side refuses output-producing commands until this is accepted
    // (src-tauri/src/intended_purpose.rs). This is the way to satisfy that from
    // the UI — without it a desktop user would hit a raw error string with no
    // route out but the CLI flag.
    //
    // The statement text comes from Rust rather than living here, so there is
    // one source of truth. A copy in the frontend would drift from the constant
    // the version number refers to, and then "accepted v1" would mean two
    // different texts depending on where you read it.
    import { onMount } from 'svelte';
    import { i18n } from '$lib/i18n.svelte';

    let {
        /// Render as a blocking overlay (the surface produces AI output right
        /// here) rather than only reporting state upward.
        blocking = true,
        onchange = (_acknowledged: boolean) => {},
    }: { blocking?: boolean; onchange?: (acknowledged: boolean) => void } = $props();

    let busy = $state(false);
    let error = $state('');

    const status = $derived(sharedStatus());

    async function accept() {
        busy = true;
        error = '';
        error = await acceptShared();
        onchange(sharedStatus()?.acknowledged ?? false);
        busy = false;
    }

    /// Art 4 (AI literacy). Points at the published copy rather than a bundled
    /// file: the doc is versioned with the source, and a link that opens the
    /// current one beats a snapshot frozen at install time. Failing to open a
    /// browser must not block the acknowledgement, so this reports and returns.
    const LITERACY_URL =
        'https://github.com/CrispStrobe/CrispSorter/blob/main/docs/ai-literacy.md';

    async function openLiteracy() {
        try {
            const { openUrl } = await import('@tauri-apps/plugin-opener');
            await openUrl(LITERACY_URL);
        } catch (e: any) {
            error = `Could not open ${LITERACY_URL}: ${e?.message ?? e}`;
        }
    }

    onMount(async () => {
        error = await probeOnce();
        onchange(sharedStatus()?.acknowledged ?? false);
    });

    const needed = $derived(status !== null && !status.acknowledged);
</script>

{#if needed && blocking}
    <div class="ip-overlay" role="dialog" aria-modal="true" aria-label={i18n.t.intendedPurpose.title}>
        <div class="ip-card">
            <h3>{i18n.t.intendedPurpose.title}</h3>
            <p class="ip-intro">{i18n.t.intendedPurpose.intro}</p>
            <!-- Rendered verbatim from the Rust constant; `white-space: pre-wrap`
                 keeps its paragraph and bullet layout without re-marking it up. -->
            <pre class="ip-statement">{status?.statement}</pre>
            {#if error}<p class="ip-error">{error}</p>{/if}
            <div class="ip-actions">
                <!-- Art 4: the literacy material, from the one screen every
                     operator is guaranteed to see. -->
                <button class="ip-literacy" onclick={openLiteracy}>
                    {i18n.t.intendedPurpose.literacy}
                </button>
                <button class="ip-accept" onclick={accept} disabled={busy}>
                    {i18n.t.intendedPurpose.accept}
                </button>
            </div>
        </div>
    </div>
{:else if needed}
    <!-- Non-blocking form: still needs the accept action, or the notice tells the
         user something is paused without offering any way to un-pause it. -->
    <div class="ip-inline-wrap">
        <p class="ip-inline">{i18n.t.intendedPurpose.blocked}</p>
        <button class="ip-accept" onclick={accept} disabled={busy}>
            {i18n.t.intendedPurpose.accept}
        </button>
    </div>
    {#if error}<p class="ip-error">{error}</p>{/if}
{/if}

<style>
    .ip-overlay {
        position: absolute;
        inset: 0;
        z-index: 50;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 1rem;
        background: color-mix(in srgb, var(--bg, #fff) 82%, transparent);
        backdrop-filter: blur(2px);
    }
    .ip-card {
        max-width: 46rem;
        max-height: 100%;
        overflow: auto;
        padding: 1.1rem 1.3rem;
        border: 1px solid var(--border, #d0d0d0);
        border-radius: 10px;
        background: var(--bg, #fff);
        box-shadow: 0 8px 28px rgba(0, 0, 0, 0.14);
    }
    .ip-card h3 { margin: 0 0 0.3rem; font-size: 1.02rem; }
    .ip-intro { margin: 0 0 0.6rem; color: var(--muted-fg, #666); font-size: 0.86rem; }
    .ip-statement {
        margin: 0 0 0.8rem;
        padding: 0.7rem 0.8rem;
        max-height: 22rem;
        overflow: auto;
        white-space: pre-wrap;
        font-family: inherit;
        font-size: 0.83rem;
        line-height: 1.5;
        border-radius: 6px;
        background: var(--muted-bg, rgba(127, 127, 127, 0.08));
    }
    .ip-error { margin: 0 0 0.5rem; color: #b3261e; font-size: 0.82rem; }
    .ip-actions { display: flex; justify-content: space-between; align-items: center; gap: 0.6rem; }
    /* Reads as the secondary action it is — the accept button stays the one
       thing that looks like the way forward. */
    .ip-literacy {
        padding: 0.42rem 0;
        border: none;
        background: none;
        color: var(--muted-fg, #666);
        text-decoration: underline;
        cursor: pointer;
        font-size: 0.82rem;
        text-align: left;
    }
    .ip-accept {
        padding: 0.42rem 0.9rem;
        border: 1px solid var(--border, #d0d0d0);
        border-radius: 6px;
        cursor: pointer;
        font-size: 0.87rem;
    }
    .ip-accept:disabled { opacity: 0.6; cursor: default; }
    .ip-inline-wrap { display: flex; align-items: center; gap: 0.6rem; flex-wrap: wrap; margin: 0.35rem 0; }
    .ip-inline { margin: 0.3rem 0; color: var(--muted-fg, #666); font-size: 0.82rem; }
</style>
