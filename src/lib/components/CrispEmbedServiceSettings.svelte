<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { experimentalCrispEmbed, loadExperimentalCrispEmbed, saveExperimentalCrispEmbed } from '$lib/experimentalCrispEmbed';
    let endpoint = $state('');
    let moedern = $state(false);
    let decisions = $state(false);
    let routes = $state<Record<string, boolean>>({});
    let verifiedEndpoint = $state('');
    let message = $state('');
    let busy = $state(false);
    onMount(async () => {
        const config = await loadExperimentalCrispEmbed();
        endpoint = config.endpoint; moedern = config.moedern; decisions = config.decisions;
    });
    async function probe() {
        busy = true; message = '';
        try {
            const report = await invoke<{ routes: Record<string, boolean> }>('crispembed_probe', { endpoint });
            routes = report.routes; verifiedEndpoint = endpoint;
            message = 'Connected. Only capabilities loaded by this server can be enabled.';
        } catch (e) { routes = {}; verifiedEndpoint = ''; message = String(e); }
        finally { busy = false; }
    }
    async function persist() {
        busy = true; message = '';
        try {
            if ((moedern || decisions) && verifiedEndpoint !== endpoint) throw new Error('Test this server before enabling its models.');
            if (moedern && !routes.moedern || decisions && !routes.systemone) throw new Error('This server has not loaded the requested models.');
            await saveExperimentalCrispEmbed({ endpoint: endpoint.trim(), moedern, decisions });
            message = 'Saved. Experimental models remain suggestions; the bundled runtime is unchanged.';
        } catch (e) { message = String(e); } finally { busy = false; }
    }
    async function disable() {
        await saveExperimentalCrispEmbed({ endpoint: '', moedern: false, decisions: false });
        endpoint = ''; moedern = false; decisions = false; routes = {}; verifiedEndpoint = ''; message = 'Experimental services disabled.';
    }
</script>

<details class="service-settings">
    <summary>Experimental CrispEmbed service</summary>
    <p>Connect a newer CrispEmbed server for MoEdern retrieval and SystemOne classification. Selected passages are sent to this server when you use these features. The bundled native runtime stays at the qualified release.</p>
    <label>Server URL <input type="url" bind:value={endpoint} placeholder="http://127.0.0.1:8080" /></label>
    <button disabled={busy || !endpoint} onclick={probe}>Test loaded capabilities</button>
    <label><input type="checkbox" bind:checked={moedern} disabled={!routes.moedern || verifiedEndpoint !== endpoint} /> Use MoEdern to rerank library search</label>
    <label><input type="checkbox" bind:checked={decisions} disabled={!routes.systemone || verifiedEndpoint !== endpoint} /> Offer SystemOne document suggestions</label>
    <button disabled={busy} onclick={persist}>Save experimental settings</button>
    <button disabled={busy} onclick={disable}>Disable services</button>
    {#if message}<p role="status">{message}</p>{/if}
    {#if $experimentalCrispEmbed.moedern || $experimentalCrispEmbed.decisions}<p>Enabled features require this server to remain reachable.</p>{/if}
</details>

<style>
    .service-settings { margin: 16px 0; padding: 12px; border: 1px solid #52525b; border-radius: 8px; }
    summary { cursor: pointer; font-weight: 600; }
    p { font-size: 0.85rem; color: #a1a1aa; }
    label { display: block; margin: 10px 0; }
    input[type='url'] { display: block; width: min(100%, 450px); box-sizing: border-box; }
    button, input[type='url'] { background: #27272a; color: #eee; border: 1px solid #52525b; border-radius: 4px; padding: 6px 10px; }
    button { margin-right: 6px; }
</style>
