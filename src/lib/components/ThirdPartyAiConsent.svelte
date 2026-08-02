<script lang="ts">
    /**
     * Third-party AI data-sharing disclosure — PLAN P36.13, App Review 5.1.2(i).
     *
     * The guideline asks for two things, and this dialog is where both happen:
     * *clearly disclose where personal data will be shared*, and *obtain
     * explicit permission before doing so*.
     *
     * So it names the actual endpoint rather than saying "a third party", and
     * neither button is pre-selected — a dialog whose default is "yes" is not
     * really asking. Mounted once, globally: it registers itself as the
     * prompter for the egress gate, so every path through the LLM client is
     * covered without any call site knowing this component exists.
     */
    import { onMount, onDestroy } from 'svelte';
    import { registerConsentPrompter } from '$lib/llm/thirdPartyConsent';
    import { i18n } from '$lib/i18n.svelte';

    interface PendingRequest {
        providerName: string;
        endpoint: string;
        resolve: (granted: boolean) => void;
    }

    let pending = $state<PendingRequest | null>(null);

    onMount(() => {
        registerConsentPrompter(
            ({ providerName, endpoint }) =>
                new Promise<boolean>((resolve) => {
                    // One at a time. A second request while a dialog is open
                    // would otherwise silently replace the first, leaving its
                    // caller awaiting a promise nobody will ever resolve.
                    if (pending) {
                        resolve(false);
                        return;
                    }
                    pending = { providerName, endpoint, resolve };
                }),
        );
    });

    onDestroy(() => registerConsentPrompter(null));

    function decide(granted: boolean) {
        pending?.resolve(granted);
        pending = null;
    }

    const t = $derived(i18n.t.consent ?? {});
</script>

{#if pending}
    <div
        class="consent-backdrop"
        role="dialog"
        aria-modal="true"
        aria-labelledby="tpai-title"
    >
        <div class="consent-card">
            <h2 id="tpai-title">
                {t.third_party_ai_title ?? 'Send this text to'}
                {pending.providerName}?
            </h2>

            <p class="consent-body">
                {t.third_party_ai_body ??
                    'To answer this, CrispSorter has to send your text — which may include the contents of your documents — to a service run by a third party. It leaves your device.'}
            </p>

            <!-- Naming the endpoint is the "clearly disclose WHERE" half of
                 5.1.2(i). A provider name alone does not tell the user where
                 their bytes are going. -->
            <p class="consent-endpoint">
                <span class="consent-endpoint-label">{t.third_party_ai_endpoint ?? 'Sent to'}</span>
                <code>{pending.endpoint}</code>
            </p>

            <p class="consent-note">
                {t.third_party_ai_note ??
                    'Your choice is remembered for this provider and can be changed in Settings. Local providers (mistral.rs, Ollama, llama.cpp, MLX, WebLLM) never leave your device and are never asked about.'}
            </p>

            <div class="consent-actions">
                <button class="consent-deny" onclick={() => decide(false)}>
                    {t.third_party_ai_deny ?? "Don't send"}
                </button>
                <button class="consent-allow" onclick={() => decide(true)}>
                    {t.third_party_ai_allow ?? 'Send to'}
                    {pending.providerName}
                </button>
            </div>
        </div>
    </div>
{/if}

<style>
    .consent-backdrop {
        position: fixed;
        inset: 0;
        background: #000a;
        display: flex;
        align-items: center;
        justify-content: center;
        z-index: 10000;
        padding: 20px;
    }
    .consent-card {
        background: #1e1e2e;
        border: 1px solid #45475a;
        border-radius: 12px;
        padding: 22px 24px;
        max-width: 520px;
        width: 100%;
        color: #cdd6f4;
        box-shadow: 0 18px 50px #0009;
    }
    h2 {
        margin: 0 0 12px;
        font-size: 1.05rem;
        font-weight: 600;
    }
    .consent-body {
        margin: 0 0 14px;
        line-height: 1.5;
        font-size: 0.9rem;
    }
    .consent-endpoint {
        margin: 0 0 14px;
        font-size: 0.82rem;
        background: #11111b;
        border: 1px solid #313244;
        border-radius: 8px;
        padding: 9px 11px;
        overflow-wrap: anywhere;
    }
    .consent-endpoint-label {
        color: #9399b2;
        margin-right: 8px;
    }
    .consent-endpoint code {
        color: #89b4fa;
    }
    .consent-note {
        margin: 0 0 18px;
        font-size: 0.78rem;
        color: #9399b2;
        line-height: 1.45;
    }
    .consent-actions {
        display: flex;
        gap: 10px;
        justify-content: flex-end;
        flex-wrap: wrap;
    }
    .consent-actions button {
        border-radius: 8px;
        padding: 9px 16px;
        font-size: 0.88rem;
        cursor: pointer;
        border: 1px solid #45475a;
    }
    /* Neither button is visually pre-selected: an "obtain explicit
       permission" dialog that nudges toward yes is not obtaining it. */
    .consent-deny {
        background: #313244;
        color: #cdd6f4;
    }
    .consent-deny:hover {
        background: #45475a;
    }
    .consent-allow {
        background: #1e3a8a;
        color: #cdd6f4;
        border-color: #3b82f6;
    }
    .consent-allow:hover {
        background: #1e40af;
    }
</style>
