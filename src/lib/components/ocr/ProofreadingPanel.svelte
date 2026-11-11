<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import {
    createVocabularyEntry, createCorrectionRule, emptyProofreadingData,
    mergeProofreadingData, readProofreadingData,
    type ProofreadingData, type VocabularyEntry, type CorrectionRule, type CorrectionRequest,
  } from "$lib/ocr/proofreading";
  import type { ProofreadingSaveStatus } from "$lib/ocr/proofreadingStore";

  let {
    projectId, projectData, userData, language, userLoadState, userLoadError = "",
    userSaveStatus, userWritable = false, disabled = false, hasPage = false,
    hasRegion = false, selectedPageCount = 0, draftRule = null,
    onproject, onuser, onlanguage, onreload, onretry, onexport, onpreview, onclose,
  }: {
    projectId: string;
    projectData: ProofreadingData;
    userData: ProofreadingData;
    language: string;
    userLoadState: "loading" | "ready" | "error" | "unavailable";
    userLoadError?: string;
    userSaveStatus: ProofreadingSaveStatus;
    userWritable?: boolean;
    disabled?: boolean;
    hasPage?: boolean;
    hasRegion?: boolean;
    selectedPageCount?: number;
    draftRule?: { from: string; to: string; language: string; nonce: number } | null;
    onproject: (data: ProofreadingData, expectedProjectId: string) => void;
    onuser: (data: ProofreadingData) => void;
    onlanguage: (language: string) => void;
    onreload: () => void | Promise<void>;
    onretry: () => void | Promise<void>;
    onexport: (data: ProofreadingData, scope: "project" | "user") => void | Promise<void>;
    onpreview: (rule: CorrectionRule, scope: CorrectionRequest["scope"], includeApproved: boolean) => void;
    onclose: () => void;
  } = $props();

  let scope = $state<"project" | "user">("project");
  let view = $state<"vocabulary" | "rules">("vocabulary");
  let feedback = $state("");
  let fileBusy = $state(false);
  let mounted = true;
  let importGeneration = 0;
  onDestroy(() => { mounted = false; importGeneration++; });
  let importInput = $state<HTMLInputElement>();
  let entryId = $state<string | null>(null);
  let term = $state("");
  let entryLanguage = $state("und");
  let kind = $state<"word" | "protected">("word");
  let exactCase = $state(true);
  let entryQuery = $state("");
  let entryStart = $state(0);
  let ruleId = $state<string | null>(null);
  let ruleName = $state("");
  let from = $state("");
  let to = $state("");
  let ruleLanguage = $state("und");
  let matchCase = $state(true);
  let wholeWord = $state(true);
  let ruleEnabled = $state(true);
  let example = $state("");
  let ruleQuery = $state("");
  let ruleStart = $state(0);
  let previewScope = $state<CorrectionRequest["scope"]>("page");
  let includeApproved = $state(false);
  const pageSize = 50;
  const data = $derived(scope === "project" ? projectData : userData);
  const readOnly = $derived(disabled || fileBusy || (scope === "user" && !userWritable));
  const canPreview = $derived(!disabled && (userLoadState === "ready" || userLoadState === "unavailable") &&
    (previewScope === "project" || (previewScope === "page" && hasPage) ||
      (previewScope === "passage" && hasRegion) || (previewScope === "selected-pages" && selectedPageCount > 0)));
  const filteredEntries = $derived(data.entries.filter((entry) =>
    `${entry.text} ${entry.language} ${entry.kind}`.toLocaleLowerCase().includes(entryQuery.toLocaleLowerCase())));
  const filteredRules = $derived(data.rules.filter((rule) =>
    `${rule.name} ${rule.from} ${rule.to}`.toLocaleLowerCase().includes(ruleQuery.toLocaleLowerCase())));

  $effect(() => {
    void projectId;
    untrack(() => {
      entryId = null; ruleId = null; term = ""; ruleName = ""; from = ""; to = ""; example = "";
      entryLanguage = language; ruleLanguage = language; feedback = "";
    });
  });
  $effect(() => {
    void scope;
    untrack(() => { entryId = null; ruleId = null; feedback = ""; });
  });
  $effect(() => { void entryQuery; void scope; entryStart = 0; });
  $effect(() => { void ruleQuery; void scope; ruleStart = 0; });
  $effect(() => {
    if (entryStart >= filteredEntries.length) entryStart = Math.max(0, Math.floor((filteredEntries.length - 1) / pageSize) * pageSize);
    if (ruleStart >= filteredRules.length) ruleStart = Math.max(0, Math.floor((filteredRules.length - 1) / pageSize) * pageSize);
  });
  $effect(() => {
    const draft = draftRule;
    if (!draft) return;
    untrack(() => {
      view = "rules"; ruleId = null; from = draft.from; to = draft.to;
      ruleName = `${draft.from} → ${draft.to}`; ruleLanguage = draft.language;
      matchCase = true; wholeWord = true; ruleEnabled = true; example = "";
    });
  });

  function commit(next: ProofreadingData, target = scope) {
    if (disabled || (target === "user" && !userWritable)) return;
    const checked = readProofreadingData(next);
    if (target === "project") onproject(checked, projectId); else onuser(checked);
  }
  function safely(action: () => void) {
    try { feedback = ""; action(); }
    catch (error) { feedback = error instanceof Error ? error.message : String(error); }
  }
  function saveEntry() {
    if (readOnly) return;
    safely(() => {
      const next = createVocabularyEntry(term, entryLanguage, kind, exactCase);
      if (entryId) commit({ ...data, entries: data.entries.map((entry) => entry.id === entryId ? { ...next, id: entryId! } : entry) });
      else commit(mergeProofreadingData(data, { ...emptyProofreadingData(), entries: [next] }));
      feedback = `${kind === "protected" ? "Protected entry" : "Vocabulary entry"} ${entryId ? "updated" : "added"}.`;
      term = ""; entryId = null;
    });
  }
  function editEntry(entry: VocabularyEntry) {
    entryId = entry.id; term = entry.text; entryLanguage = entry.language;
    kind = entry.kind; exactCase = entry.caseSensitive; feedback = "";
  }
  function saveRule() {
    if (readOnly) return;
    safely(() => {
      const next = createCorrectionRule({ name: ruleName, from, to, language: ruleLanguage,
        caseSensitive: matchCase, wholeWord, enabled: ruleEnabled, ...(example ? { example } : {}) });
      if (ruleId) commit({ ...data, rules: data.rules.map((rule) => rule.id === ruleId ? { ...next, id: ruleId! } : rule) });
      else commit(mergeProofreadingData(data, { ...emptyProofreadingData(), rules: [next] }));
      feedback = `Correction rule ${ruleId ? "updated" : "added"}. Preview it before applying.`;
      ruleId = null; ruleName = ""; from = ""; to = ""; example = "";
    });
  }
  function editRule(rule: CorrectionRule) {
    ruleId = rule.id; ruleName = rule.name; from = rule.from; to = rule.to;
    ruleLanguage = rule.language; matchCase = rule.caseSensitive;
    wholeWord = rule.wholeWord; ruleEnabled = rule.enabled; example = rule.example ?? ""; feedback = "";
  }
  async function importData(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file || readOnly) { input.value = ""; return; }
    const target = scope, capturedProject = projectId, generation = ++importGeneration;
    fileBusy = true; feedback = "";
    try {
      if (file.size > 4 * 1024 * 1024) throw new Error("Vocabulary import exceeds the 4 MB limit.");
      const text = await file.text();
      if (!mounted || generation !== importGeneration) return;
      if (disabled) throw new Error("A document operation started while reading the file. Import the vocabulary again after it finishes.");
      if (capturedProject !== projectId) throw new Error("The active project changed. Import the vocabulary again into the intended project.");
      if (target === "user" && !userWritable) throw new Error("Load the saved all-project vocabulary before importing changes.");
      const incoming = readProofreadingData(JSON.parse(text));
      commit(mergeProofreadingData(target === "project" ? projectData : userData, incoming), target);
      feedback = `Imported vocabulary and rules into ${target === "project" ? "this project" : "all projects"}. Existing entries were retained.`;
    } catch (error) { if (mounted && generation === importGeneration) feedback = error instanceof Error ? error.message : String(error); }
    finally { if (mounted && generation === importGeneration) fileBusy = false; input.value = ""; }
  }
</script>

<section class="proofreading-panel" aria-label="Vocabulary and rules" data-proofreading-manager>
  <div class="panel-heading"><h3>Vocabulary and rules</h3><button type="button" onclick={onclose} aria-label="Close vocabulary and rules">Close</button></div>
  <p class="hint">Keep specialist terms and identifiers, and reuse literal corrections. Nothing is corrected automatically; OCR scores and approval remain separate.</p>
  <div class="field-grid">
    <label>Vocabulary scope<select bind:value={scope} disabled={disabled || fileBusy}>
      <option value="project">This project</option><option value="user">All projects</option>
    </select></label>
    <label>Active proofreading language<input value={language} maxlength="40" placeholder="en, de, und…"
      onchange={(event) => { onlanguage(event.currentTarget.value); event.currentTarget.value = language; }} disabled={disabled} /></label>
  </div>
  <p class="hint">Use language codes such as en or de. “und” is unspecified; protected entries with “und” apply across languages. All-project entries are saved separately from document undo.</p>
  {#if scope === "project" && data.ignored.length}<div class="notice">
    <span>{data.ignored.length} ignored occurrence{data.ignored.length === 1 ? "" : "s"} saved in this project.</span>
    <button type="button" onclick={() => safely(() => commit({ ...data, ignored: [] }))} disabled={readOnly}>Clear ignored occurrences</button>
  </div>{/if}
  {#if userLoadState === "loading"}<p class="notice">Loading all-project vocabulary…</p>
  {:else if userLoadState === "error"}<div class="notice error" data-proofreading-load-error role="alert">
    <p>All-project vocabulary could not be loaded. Saved data has not been replaced. {userLoadError}</p>
    <button type="button" onclick={() => void onreload()}>Retry loading vocabulary</button>
  </div>
  {:else if userLoadState === "unavailable"}<p class="notice">All-project vocabulary storage is unavailable in this build. Project vocabulary remains available.</p>
  {/if}
  {#if userLoadState === "ready"}<div class="notice" class:error={userSaveStatus.state === "error"} data-proofreading-user-status={userSaveStatus.state} role="status">
    {userSaveStatus.state === "saved" ? "All-project vocabulary saved" : userSaveStatus.state === "saving" ? "Saving all-project vocabulary…" : userSaveStatus.state === "pending" ? "All-project vocabulary has unsaved changes" : `All-project vocabulary save failed: ${userSaveStatus.error ?? "Try saving again."}`}
    {#if userSaveStatus.state === "error"}<button type="button" onclick={() => void onretry()}>Retry saving vocabulary</button>{/if}
  </div>{/if}
  <div class="manager-toolbar">
    <button type="button" aria-pressed={view === "vocabulary"} onclick={() => (view = "vocabulary")}>Vocabulary</button>
    <button type="button" aria-pressed={view === "rules"} onclick={() => (view = "rules")}>Correction rules</button>
    <span class="spacer"></span>
    <input class="file-input" type="file" accept="application/json,.json" bind:this={importInput} onchange={importData} aria-label="Import vocabulary JSON" disabled={readOnly} />
    <button type="button" onclick={() => importInput?.click()} disabled={readOnly}>Import JSON</button>
    <button type="button" onclick={() => void onexport(data, scope)} disabled={disabled || fileBusy || (scope === "user" && userLoadState !== "ready")}>Export JSON</button>
  </div>
  {#if view === "vocabulary"}
    <form onsubmit={(event) => { event.preventDefault(); saveEntry(); }}>
      <label>Vocabulary term<input bind:value={term} maxlength="256" disabled={readOnly} dir="auto" /></label>
      <div class="field-grid">
        <label>Entry language<input bind:value={entryLanguage} maxlength="40" disabled={readOnly} /></label>
        <label>Entry type<select bind:value={kind} disabled={readOnly}><option value="word">Word</option><option value="protected">Protected identifier</option></select></label>
      </div>
      <label class="check"><input type="checkbox" bind:checked={exactCase} disabled={readOnly} />Match exact case</label>
      <div class="actions"><button type="submit" disabled={readOnly || !term.trim()}>{entryId ? "Save entry" : "Add entry"}</button>
        {#if entryId}<button type="button" onclick={() => { entryId = null; term = ""; }}>Cancel entry edit</button>{/if}</div>
    </form>
    <label>Filter vocabulary<input bind:value={entryQuery} type="search" /></label>
    <ul class="entry-list">
      {#each filteredEntries.slice(entryStart, entryStart + pageSize) as entry (entry.id)}<li data-vocabulary-entry={entry.id}>
        <div><strong dir="auto">{entry.text}</strong><span>{entry.kind === "protected" ? "Protected" : "Word"} · {entry.language} · {entry.caseSensitive ? "exact case" : "ignore case"}</span></div>
        <div class="actions"><button type="button" onclick={() => editEntry(entry)} disabled={readOnly} aria-label={`Edit entry: ${entry.text}`}>Edit</button>
          <button type="button" onclick={() => safely(() => commit({ ...data, entries: data.entries.filter((item) => item.id !== entry.id) }))} disabled={readOnly} aria-label={`Remove entry: ${entry.text}`}>Remove</button></div>
      </li>{/each}
    </ul>
    {#if !filteredEntries.length}<p class="hint">No matching vocabulary entries. A custom vocabulary is a set of your terms, not a complete spelling dictionary.</p>{/if}
    {#if filteredEntries.length > pageSize}<div class="pagination"><button type="button" disabled={entryStart === 0} onclick={() => (entryStart = Math.max(0, entryStart - pageSize))}>Previous entries</button><span>{entryStart + 1}–{Math.min(entryStart + pageSize, filteredEntries.length)} of {filteredEntries.length}</span><button type="button" disabled={entryStart + pageSize >= filteredEntries.length} onclick={() => (entryStart += pageSize)}>Next entries</button></div>{/if}
  {:else}
    <form onsubmit={(event) => { event.preventDefault(); saveRule(); }}>
      <label>Rule name<input bind:value={ruleName} maxlength="160" disabled={readOnly} /></label>
      <div class="field-grid">
        <label>Find literal text<textarea bind:value={from} maxlength="1000" rows="2" disabled={readOnly} dir="auto"></textarea></label>
        <label>Replace with<textarea bind:value={to} maxlength="10000" rows="2" disabled={readOnly} dir="auto"></textarea></label>
      </div>
      <label>Rule language<input bind:value={ruleLanguage} maxlength="40" disabled={readOnly} /></label>
      <div class="checks"><label class="check"><input type="checkbox" bind:checked={matchCase} disabled={readOnly} />Match case</label><label class="check"><input type="checkbox" bind:checked={wholeWord} disabled={readOnly} />Whole words only</label><label class="check"><input type="checkbox" bind:checked={ruleEnabled} disabled={readOnly} />Rule enabled</label></div>
      <label>Example or note (optional)<input bind:value={example} maxlength="4000" disabled={readOnly} /></label>
      <p class="hint">Find and replacement are literal strings, including punctuation and line breaks. Rules never run automatically.</p>
      <div class="actions"><button type="submit" disabled={readOnly || !ruleName.trim() || !from || from === to}>{ruleId ? "Save rule" : "Add rule"}</button>
        {#if ruleId}<button type="button" onclick={() => { ruleId = null; ruleName = ""; from = ""; to = ""; example = ""; }}>Cancel rule edit</button>{/if}</div>
    </form>
    <div class="preview-controls">
      <label>Correction scope<select bind:value={previewScope} disabled={disabled}>
        <option value="passage" disabled={!hasRegion}>Selected passage</option><option value="page" disabled={!hasPage}>Current page</option>
        <option value="selected-pages" disabled={!selectedPageCount}>Selected pages ({selectedPageCount})</option><option value="project">Included project</option>
      </select></label>
      <label class="check"><input type="checkbox" bind:checked={includeApproved} disabled={disabled} />Include approved text</label>
    </div>
    <label>Filter rules<input bind:value={ruleQuery} type="search" /></label>
    <ul class="entry-list">
      {#each filteredRules.slice(ruleStart, ruleStart + pageSize) as rule (rule.id)}<li data-correction-rule={rule.id}>
        <div><strong>{rule.name}</strong><span dir="auto">{rule.from} → {rule.to || "(remove text)"}</span><span>{rule.language} · {rule.caseSensitive ? "exact case" : "ignore case"} · {rule.wholeWord ? "whole words" : "within text"}</span></div>
        <label class="check"><input type="checkbox" checked={rule.enabled} disabled={readOnly} aria-label={`Enable rule: ${rule.name}`} onchange={(event) => safely(() => commit({ ...data, rules: data.rules.map((item) => item.id === rule.id ? { ...item, enabled: event.currentTarget.checked } : item) }))} />Enabled</label>
        <div class="actions"><button type="button" onclick={() => onpreview(rule, previewScope, includeApproved)} disabled={!canPreview || !rule.enabled} aria-label={`Preview rule: ${rule.name}`}>Preview rule</button><button type="button" onclick={() => editRule(rule)} disabled={readOnly} aria-label={`Edit rule: ${rule.name}`}>Edit</button><button type="button" onclick={() => safely(() => commit({ ...data, rules: data.rules.filter((item) => item.id !== rule.id) }))} disabled={readOnly} aria-label={`Remove rule: ${rule.name}`}>Remove</button></div>
      </li>{/each}
    </ul>
    {#if !filteredRules.length}<p class="hint">No matching correction rules.</p>{/if}
    {#if filteredRules.length > pageSize}<div class="pagination"><button type="button" disabled={ruleStart === 0} onclick={() => (ruleStart = Math.max(0, ruleStart - pageSize))}>Previous rules</button><span>{ruleStart + 1}–{Math.min(ruleStart + pageSize, filteredRules.length)} of {filteredRules.length}</span><button type="button" disabled={ruleStart + pageSize >= filteredRules.length} onclick={() => (ruleStart += pageSize)}>Next rules</button></div>{/if}
  {/if}
  {#if feedback}<p class="notice" role="status">{feedback}</p>{/if}
</section>

<style>
  .proofreading-panel { padding: 14px; min-height: 0; overflow-y: auto; min-width: 0; color: #d4d4d8; background: #18181b; border-bottom: 1px solid #3f3f46; font-size: 12px; }
  .panel-heading, .manager-toolbar, .actions, .checks, .pagination { display: flex; align-items: center; flex-wrap: wrap; gap: 7px; }
  .panel-heading { justify-content: space-between; }
  h3 { margin: 0; color: #dbeafe; font-size: 14px; }
  .hint { color: #a1a1aa; font-size: 11px; line-height: 1.6; margin: 9px 0; }
  label { display: flex; flex-direction: column; gap: 5px; min-width: 0; margin: 9px 0; color: #d4d4d8; font-size: 11px; }
  .field-grid { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 9px; }
  .check { flex-direction: row; align-items: center; gap: 6px; }
  input:not([type="checkbox"]), select, textarea { box-sizing: border-box; width: 100%; min-width: 0; padding: 6px 7px; border: 1px solid #52525b; border-radius: 4px; background: #18181b; color: #e4e4e7; font: inherit; font-size: 12px; }
  textarea { resize: vertical; line-height: 1.5; }
  input[type="checkbox"] { margin: 0; flex: none; }
  button { padding: 5px 8px; border: 1px solid #52525b; border-radius: 4px; background: #27272a; color: #dbeafe; font: inherit; font-size: 11px; cursor: pointer; }
  button[aria-pressed="true"] { border-color: #60a5fa; background: #3f3f46; }
  button:disabled, input:disabled, select:disabled, textarea:disabled { opacity: .55; cursor: default; }
  button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
  .notice { padding: 9px; margin: 9px 0; border: 1px solid #3f3f46; border-radius: 4px; background: #27272a; color: #bfdbfe; font-size: 11px; line-height: 1.6; overflow-wrap: anywhere; }
  .notice p { margin: 0 0 8px; }
  .notice button { margin-left: 6px; }
  .notice.error { border-color: #71717a; background: #27272a; color: #fecaca; }
  .manager-toolbar { margin: 13px 0; }
  .spacer { flex: 1; }
  .file-input { display: none; }
  form { padding: 10px; border: 1px solid #3f3f46; border-radius: 5px; background: #18181b; }
  .entry-list { list-style: none; padding: 0; margin: 8px 0; }
  .entry-list li { padding: 10px 0; border-bottom: 1px solid #3f3f46; min-width: 0; }
  .entry-list li > div:first-child { display: flex; flex-direction: column; gap: 4px; min-width: 0; overflow-wrap: anywhere; }
  .entry-list strong { color: #dbeafe; font-size: 12px; }
  .entry-list span { color: #a1a1aa; font-size: 10px; white-space: pre-wrap; }
  .entry-list .actions { margin-top: 7px; }
  .preview-controls { margin: 10px 0; padding-bottom: 8px; border-bottom: 1px solid #3f3f46; }
  .pagination { justify-content: space-between; font-size: 10px; }
  @media (max-width: 500px) { .field-grid { grid-template-columns: minmax(0, 1fr); gap: 0; } }
</style>
