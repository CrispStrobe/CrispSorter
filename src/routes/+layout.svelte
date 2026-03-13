<script>
    let { children } = $props();
</script>

{@render children()}

<style>
    :global(html), :global(body) {
        margin: 0;
        padding: 0;
        width: 100%;
        height: 100%;
        overflow: hidden;
        font-family: 'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
        background-color: #09090b; /* Match dark mode default */
        color: #fafafa;
    }

    :global(*) {
        box-sizing: border-box;
    }

    /* Scrollbar styling for a cleaner look */
    :global(::-webkit-scrollbar) {
        width: 8px;
        height: 8px;
    }
    :global(::-webkit-scrollbar-track) {
        background: transparent;
    }
    :global(::-webkit-scrollbar-thumb) {
        background: #27272a;
        border-radius: 10px;
    }
    :global(::-webkit-scrollbar-thumb:hover) {
        background: #3f3f46;
    }
</style>
