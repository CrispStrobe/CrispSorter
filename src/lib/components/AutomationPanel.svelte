<script lang="ts">
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';

    type Rule = {
        name: string;
        enabled: boolean;
        priority: number;
        triggers: unknown[];
        trigger_mode: 'all' | 'any';
        actions: unknown[];
    };

    let rules = $state<Rule[]>([]);
    let selected = $state<Rule | null>(null);
    let editor = $state('');
    let samplePath = $state('');
    let message = $state('');
    let busy = $state(false);

    const blankRule = (): Rule => ({
        name: 'New rule', enabled: false, priority: 100,
        triggers: [{ type: 'extension', patterns: ['pdf'] }],
        trigger_mode: 'all', actions: [{ type: 'ingest' }],
    });

    async function reload() {
        try {
            rules = await invoke<Rule[]>('automation_list_rules');
            if (selected) {
                const current = rules.find((r) => r.name === selected?.name);
                if (current) select(current);
            }
        } catch (e) { message = `Automation load failed: ${e}`; }
    }

    function select(rule: Rule) {
        selected = rule;
        editor = JSON.stringify(rule, null, 2);
        message = '';
    }

    function add() { select(blankRule()); }

    async function saveRule() {
        let rule: Rule;
        try { rule = JSON.parse(editor); } catch (e) { message = `Invalid rule JSON: ${e}`; return; }
        busy = true;
        try {
            rules = await invoke<Rule[]>('automation_save_rule', { rule });
            selected = rules.find((r) => r.name === rule.name) ?? null;
            editor = selected ? JSON.stringify(selected, null, 2) : '';
            message = `Saved “${rule.name}”.`;
        } catch (e) { message = `Automation save failed: ${e}`; }
        finally { busy = false; }
    }

    async function removeRule() {
        if (!selected || !confirm(`Delete automation rule “${selected.name}”?`)) return;
        try {
            rules = await invoke<Rule[]>('automation_delete_rule', { name: selected.name });
            selected = null; editor = ''; message = 'Rule deleted.';
        } catch (e) { message = `Automation delete failed: ${e}`; }
    }

    async function toggle(rule: Rule) {
        try { rules = await invoke<Rule[]>('automation_set_enabled', { name: rule.name, enabled: !rule.enabled }); }
        catch (e) { message = `Automation toggle failed: ${e}`; }
    }

    async function test() {
        if (!samplePath.trim()) { message = 'Choose a sample file path first.'; return; }
        try {
            const actions = await invoke<unknown[]>('automation_test_rule', { filePath: samplePath.trim(), matchAll: false });
            message = actions.length ? `Matching actions: ${JSON.stringify(actions)}` : 'No enabled rule matches this file.';
        } catch (e) { message = `Automation test failed: ${e}`; }
    }

    onMount(reload);
</script>

<div class="section-card">
    <div style="display:flex; align-items:center; justify-content:space-between; gap:8px;">
        <strong>Automation rules</strong>
        <button class="action-btn small" onclick={add}>+ New rule</button>
    </div>
    <p class="hint">Rules are stored locally without credentials. The watcher emits matching actions; actions are never executed silently.</p>
    {#if rules.length}
        <ul style="list-style:none; padding:0; margin:8px 0; display:flex; flex-direction:column; gap:4px;">
            {#each rules as rule (rule.name)}
                <li style="display:flex; align-items:center; gap:6px;">
                    <input type="checkbox" checked={rule.enabled} onchange={() => toggle(rule)} aria-label={`Enable ${rule.name}`} />
                    <button class="action-btn small" style="flex:1; text-align:left;" class:active={selected?.name === rule.name} onclick={() => select(rule)}>
                        {rule.name} <span class="hint">(priority {rule.priority})</span>
                    </button>
                    <button class="action-btn small danger" onclick={() => { selected = rule; removeRule(); }}>×</button>
                </li>
            {/each}
        </ul>
    {/if}
    {#if selected}
        <label for="automation-rule-json">Rule JSON</label>
        <textarea id="automation-rule-json" bind:value={editor} rows="12" spellcheck="false" style="width:100%; font-family:monospace; font-size:0.75rem; margin-top:6px;"></textarea>
        <div style="display:flex; gap:6px; margin-top:8px;">
            <button class="action-btn small" disabled={busy} onclick={saveRule}>Save rule</button>
            <button class="action-btn small" onclick={() => { selected = null; editor = ''; }}>Cancel</button>
        </div>
    {/if}
    <div style="display:flex; gap:6px; margin-top:10px;">
        <input bind:value={samplePath} placeholder="Sample file path" style="flex:1;" />
        <button class="action-btn small" onclick={test}>Test rules</button>
    </div>
    {#if message}<p class="hint" style="margin-top:6px;">{message}</p>{/if}
</div>
