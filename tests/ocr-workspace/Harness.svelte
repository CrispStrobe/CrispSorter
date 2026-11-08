<script lang="ts">
    import OcrWorkbench from '../../src/lib/components/OcrWorkbench.svelte';
    import { createFixtureClient } from './fixture';
    const fixture = createFixtureClient(new URLSearchParams(location.search).get('scenario') ?? 'review');
</script>

<main><OcrWorkbench client={fixture.client} initialProject={fixture.initialProject} /></main>

<style>
    :global(html), :global(body), :global(#app) { margin:0; width:100%; height:100%; overflow:hidden; }
    :global(*) { box-sizing:border-box; }
    :global(body) { font-family:Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; background:#10151d; color:#edf2f8; }
    main { height:100%; width:100%; min-height:0; }
    :global(button), :global(input), :global(select), :global(textarea) { font:inherit; }
</style>
