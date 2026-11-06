<script lang="ts">
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';
    import { onMount } from 'svelte';
    import { getSecret, setSecret, deleteSecret } from '../secrets';

    let instance = $state('');
    let clientId = $state('');
    let clientSecret = $state('');
    let username = $state('');
    let password = $state('');
    let busy = $state(false);
    let status = $state('');
    type Progress = { page: number; pages: number; indexed: number; skipped: number };
    onMount(() => {
        let dispose: (() => void) | undefined;
        let alive = true;
        void listen<Progress>('wallabag://progress', event => {
            const p = event.payload;
            status = `Page ${p.page} of ${p.pages}; ${p.indexed} indexed, ${p.skipped} unchanged.`;
        }).then(fn => { if (alive) dispose = fn; else fn(); });
        return () => { alive = false; dispose?.(); };
    });
    // Access the selected secret store only after an explicit button click.
    const secretAccount = 'wallabag:credentials';
    async function saveCredentials() {
        try {
            await setSecret(secretAccount, JSON.stringify({ instance, clientId, clientSecret, username, password }));
            status = 'Credentials saved in the selected secret store.';
        } catch (e) { status = `Could not save credentials: ${String(e)}`; }
    }
    async function loadCredentials() {
        try {
            const value = await getSecret(secretAccount);
            if (!value) { status = 'No saved credentials in the selected store.'; return; }
            const saved = JSON.parse(value);
            instance = saved.instance ?? ''; clientId = saved.clientId ?? '';
            clientSecret = saved.clientSecret ?? ''; username = saved.username ?? ''; password = saved.password ?? '';
            status = 'Credentials loaded from the selected secret store.';
        } catch (e) { status = `Could not load credentials: ${String(e)}`; }
    }
    async function forgetCredentials() {
        try { await deleteSecret(secretAccount); status = 'Saved credentials removed.'; }
        catch (e) { status = `Could not remove credentials: ${String(e)}`; }
    }
    async function sync() {
        busy = true;
        status = 'Signing in and importing articles…';
        try {
            const p = await invoke<Progress>('wallabag_sync', { credentials: {
                instance, client_id: clientId, client_secret: clientSecret, username, password
            }});
            status = `Finished: ${p.indexed} indexed, ${p.skipped} unchanged. Search them in the local Search Index.`;
        } catch (e) {
            status = `Import stopped: ${String(e)}. You can run it again.`;
        } finally {
            password = '';
            clientSecret = '';
            busy = false;
        }
    }
</script>

<div class="section-card">
    <label><strong>Wallabag</strong></label>
    <p class="hint">Import your saved articles into the local search index. No cloud-backup server is needed. Enable and initialise the local index first; turn off vector embeddings for faster full-text indexing.</p>
    <p class="hint">Create an API client in your Wallabag account under Developer → API clients. Credentials are kept in memory unless you explicitly save them in your chosen secret store.</p>
    <label for="wb-instance">Wallabag instance URL</label>
    <input id="wb-instance" type="url" bind:value={instance} placeholder="https://wallabag.example.org" disabled={busy} />
    <label for="wb-client">Client ID</label>
    <input id="wb-client" bind:value={clientId} autocomplete="off" disabled={busy} />
    <label for="wb-secret">Client secret</label>
    <input id="wb-secret" type="password" bind:value={clientSecret} autocomplete="off" disabled={busy} />
    <label for="wb-user">Username</label>
    <input id="wb-user" bind:value={username} autocomplete="off" disabled={busy} />
    <label for="wb-password">Password</label>
    <input id="wb-password" type="password" bind:value={password} autocomplete="off" disabled={busy} />
    <button onclick={sync} disabled={busy || !instance || !clientId || !clientSecret || !username || !password}>
        {busy ? 'Importing…' : 'Import Wallabag articles'}
    </button>
    <div class="credential-actions">
        <button onclick={saveCredentials} disabled={busy || !instance || !clientId || !clientSecret || !username || !password}>Save credentials</button>
        <button onclick={loadCredentials} disabled={busy}>Load saved credentials</button>
        <button onclick={forgetCredentials} disabled={busy}>Remove saved credentials</button>
    </div>
    <p class="hint">These buttons use your selected secret store. Choose a session or encrypted-file store in Settings if you do not want to use a system keychain.</p>
    {#if status}<p role="status" class="hint">{status}</p>{/if}
</div>

<style>
    .section-card { padding: 16px; border: 1px solid #3f3f46; border-radius: 8px; margin: 16px 0; }
    label { display: block; margin-top: 10px; }
    input { width: 100%; box-sizing: border-box; margin-top: 4px; }
    button { margin-top: 14px; }
    .credential-actions { display: flex; flex-wrap: wrap; gap: 8px; }
    .hint { color: #a1a1aa; font-size: 0.85rem; }
</style>
