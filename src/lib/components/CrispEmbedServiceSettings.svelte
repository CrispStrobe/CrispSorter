<script lang="ts">
    import { i18n } from '../i18n.svelte';
    function tr(en: string, de: string): string { return i18n.lang === 'de' ? de : en; }
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
            message = tr('Connected. Only capabilities loaded by this server can be enabled.', 'Verbunden. Nur auf diesem Server geladene Funktionen können aktiviert werden.');
        } catch (e) { routes = {}; verifiedEndpoint = ''; message = String(e); }
        finally { busy = false; }
    }
    async function persist() {
        busy = true; message = '';
        try {
            if ((moedern || decisions) && verifiedEndpoint !== endpoint) throw new Error(tr('Test this server before enabling its models.', 'Testen Sie diesen Server, bevor Sie seine Modelle aktivieren.'));
            if (moedern && !routes.moedern || decisions && !routes.systemone) throw new Error(tr('This server has not loaded the requested models.', 'Der Server hat die angeforderten Modelle nicht geladen.'));
            await saveExperimentalCrispEmbed({ endpoint: endpoint.trim(), moedern, decisions });
            message = tr('Saved. Experimental models remain suggestions; the bundled runtime is unchanged.', 'Gespeichert. Experimentelle Modelle liefern weiterhin Vorschläge; die mitgelieferte Laufzeit bleibt unverändert.');
        } catch (e) { message = String(e); } finally { busy = false; }
    }
    async function disable() {
        await saveExperimentalCrispEmbed({ endpoint: '', moedern: false, decisions: false });
        endpoint = ''; moedern = false; decisions = false; routes = {}; verifiedEndpoint = ''; message = tr('Experimental services disabled.', 'Experimentelle Dienste deaktiviert.');
    }
</script>

<details class="service-settings">
    <summary>{tr('Experimental CrispEmbed service', 'Experimenteller CrispEmbed-Dienst')}</summary>
    <p>{tr('Connect a newer CrispEmbed server for MoEdern retrieval and SystemOne classification. Selected passages are sent to this server when you use these features. The bundled native runtime stays at the qualified release.', 'Verbinden Sie einen neueren CrispEmbed-Server für MoEdern-Suche und SystemOne-Klassifizierung. Bei Verwendung dieser Funktionen werden ausgewählte Textstellen an diesen Server gesendet. Die mitgelieferte native Laufzeit bleibt auf der freigegebenen Version.')}</p>
    <label>{tr('Server URL', 'Server-URL')} <input type="url" bind:value={endpoint} placeholder="http://127.0.0.1:8080" /></label>
    <button disabled={busy || !endpoint} onclick={probe}>{tr('Test loaded capabilities', 'Geladene Funktionen testen')}</button>
    <label><input type="checkbox" bind:checked={moedern} disabled={!routes.moedern || verifiedEndpoint !== endpoint} /> {tr('Use MoEdern to rerank library search', 'MoEdern zur Neusortierung der Bibliothekssuche verwenden')}</label>
    <label><input type="checkbox" bind:checked={decisions} disabled={!routes.systemone || verifiedEndpoint !== endpoint} /> {tr('Offer SystemOne document suggestions', 'SystemOne-Dokumentvorschläge anbieten')}</label>
    <button disabled={busy} onclick={persist}>{tr('Save experimental settings', 'Experimentelle Einstellungen speichern')}</button>
    <button disabled={busy} onclick={disable}>{tr('Disable services', 'Dienste deaktivieren')}</button>
    {#if message}<p role="status">{message}</p>{/if}
    {#if $experimentalCrispEmbed.moedern || $experimentalCrispEmbed.decisions}<p>{tr('Enabled features require this server to remain reachable.', 'Aktivierte Funktionen erfordern, dass dieser Server erreichbar bleibt.')}</p>{/if}
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
