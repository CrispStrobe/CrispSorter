<script lang="ts">
    import OcrWorkbench from '../../src/lib/components/OcrWorkbench.svelte';
    import ThirdPartyAiConsent from '../../src/lib/components/ThirdPartyAiConsent.svelte';
    import { createFixtureClient } from './fixture';
    import { installOverlayConsent } from './overlay-fixture';
    const params = new URLSearchParams(location.search);
    const overlayAudit = params.get('scenario') === 'safe-overlays';
    if (overlayAudit) installOverlayConsent();
    const fixture = createFixtureClient(params.get('scenario') ?? 'review');
    // `open` simulates a document sent from another tab ("Open in OCR").
    const initialLocationUri = params.get('open') ?? '';
</script>

<main class:safe-area-frame={overlayAudit}><OcrWorkbench client={fixture.client} initialProject={fixture.initialProject} {initialLocationUri} /></main>
{#if overlayAudit}<ThirdPartyAiConsent />{/if}

<style>
    :global(html), :global(body), :global(#app) { margin:0; width:100%; height:100%; overflow:hidden; }
    :global(*) { box-sizing:border-box; }
    :global(body) { font-family:Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; background:#10151d; color:#edf2f8; }
    main { height:100%; width:100%; min-height:0; }
    /* The component fixture represents the enclosing shell's usable rectangle.
       The browser controls actual CSS env values through its emulation API. */
    main.safe-area-frame { padding:env(safe-area-inset-top, 0px) env(safe-area-inset-right, 0px) env(safe-area-inset-bottom, 0px) env(safe-area-inset-left, 0px); }
    :global(button), :global(input), :global(select), :global(textarea) { font:inherit; }
</style>
