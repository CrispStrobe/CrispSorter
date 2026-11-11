<script lang="ts">
  import { tick } from "svelte";
  import { Bold, Italic, Underline, Eraser, ChevronLeft, ChevronRight, ArrowUp, ArrowDown } from "lucide-svelte";
  import { moveRegion, type OcrProject, type OcrRegion } from "$lib/ocr/model";
  import {
    deriveStructure, structureSegments,
    type StructureBlock, type StructureRole, type InlineMarks, type TextRange,
  } from "$lib/ocr/structureData";
  import {
    initializePageStructure, setBlockProperties, setInlineMarks, clearInlineMarks,
    startSection, setSectionColumns, setCaptionAnchor, setFootnoteReference,
  } from "$lib/ocr/structure";
  import { sourceOffsetForTextarea } from "$lib/ocr/text";
  import StructureText from "./StructureText.svelte";
  import StructurePreview from "./StructurePreview.svelte";

  let { project, pageId, selectedId, disabled = false, imageUrl, onselect, onchange, onedit, onblur }: {
    project: OcrProject;
    pageId: string;
    selectedId: string | null;
    disabled?: boolean;
    imageUrl: (path: string) => string;
    onselect: (regionId: string) => void;
    onchange: (next: OcrProject, message: string) => void;
    onedit: (region: OcrRegion, text: string) => void;
    onblur: () => void;
  } = $props();

  type PropertyPatch = Partial<Pick<StructureBlock, "role" | "headingLevel" | "list" | "alignment" | "breakBefore" | "altText" | "decorative">>;
  const textualRoles: StructureRole[] = ["paragraph", "heading", "list-item", "caption", "header", "footer", "footnote"];
  const roleNames: Record<StructureRole, string> = {
    paragraph: "Paragraph", heading: "Heading", "list-item": "List item", figure: "Figure",
    caption: "Caption", table: "Table", formula: "Formula", header: "Running header",
    footer: "Running footer", footnote: "Footnote",
  };
  let editor = $state<HTMLTextAreaElement>();
  let actionError = $state("");
  let rangeState = $state<{ blockId: string; text: string; start: number; end: number } | null>(null);
  let layoutOpen = $state(false);
  let linksOpen = $state(false);
  let previewOpen = $state(false);
  let columnDraft = $state<1 | 2>(1);
  let secondColumnStart = $state("");
  let noteTarget = $state("");
  let altDraft = $state("");
  let decorativeDraft = $state(false);
  let captionQuery = $state("");
  let noteQuery = $state("");
  let columnQuery = $state("");
  const page = $derived(project.pages.find((item) => item.id === pageId));
  const structure = $derived(page ? page.structure ?? deriveStructure(page) : null);
  const regionMap = $derived(new Map(page?.regions.map((item) => [item.id, item]) ?? []));
  const blockPositions = $derived(new Map(structure?.blocks.map((item, index) => [item.id, index]) ?? []));
  const block = $derived(structure?.blocks.find((item) => item.sourceRegionId === selectedId) ?? structure?.blocks[0]);
  const region = $derived(page?.regions.find((item) => item.id === block?.sourceRegionId));
  const index = $derived(structure?.blocks.findIndex((item) => item.id === block?.id) ?? -1);
  const section = $derived(structure?.sections.find((item) => item.id === block?.sectionId));
  const sectionBlocks = $derived(structure?.blocks.filter((item) => item.sectionId === section?.id) ?? []);
  const navigationStart = $derived(Math.max(0, Math.floor(index / 100) * 100));
  const navigationBlocks = $derived(structure?.blocks.slice(navigationStart, navigationStart + 100) ?? []);
  const blockDisabled = $derived(disabled || !!page?.excluded || !!region?.excluded || page?.status === "running");
  const roleChoices = $derived<StructureRole[]>(region?.kind === "picture" ? ["figure"] : region?.kind === "table" ? ["table"] : region?.kind === "formula" ? ["formula"] : textualRoles);
  const canFormat = $derived(!!block && textualRoles.includes(block.role));
  const targets = $derived(structure?.blocks.filter((candidate) => candidate.id !== block?.id && ["figure", "table"].includes(candidate.role) && !regionMap.get(candidate.sourceRegionId)?.excluded) ?? []);
  const notes = $derived(structure?.blocks.filter((candidate) => candidate.id !== block?.id && candidate.role === "footnote" && !regionMap.get(candidate.sourceRegionId)?.excluded) ?? []);
  const visibleTargets = $derived(choices(targets, captionQuery, block?.anchorBlockId));
  const visibleNotes = $derived(choices(notes, noteQuery, noteTarget));
  const visibleColumnStarts = $derived(choices(sectionBlocks.slice(1), columnQuery, secondColumnStart));
  const notices = $derived(Array.isArray(page?.provenance.structureNotices) ? page.provenance.structureNotices.filter((item): item is string => typeof item === "string") : []);
  const activeRange = $derived(rangeState && region && block && rangeState.blockId === block.id && rangeState.text === region.text ? rangeState : null);
  const selectedStyles = $derived.by(() => {
    if (!region || !block || !activeRange || activeRange.start === activeRange.end) return { bold: false, italic: false, underline: false };
    const segments = structureSegments(region.text, block).filter((part) => part.start < activeRange.end && part.end > activeRange.start);
    return { bold: !!segments.length && segments.every((part) => part.bold), italic: !!segments.length && segments.every((part) => part.italic), underline: !!segments.length && segments.every((part) => part.underline) };
  });

  $effect(() => {
    columnDraft = section?.columns ?? 1;
    secondColumnStart = sectionBlocks.find((item) => item.column === 2)?.id ?? "";
  });
  $effect(() => {
    altDraft = block?.altText ?? "";
    decorativeDraft = block?.decorative ?? false;
  });
  $effect(() => {
    const id = block?.id;
    if (rangeState?.blockId !== id) rangeState = null;
    if (!notes.some((note) => note.id === noteTarget)) noteTarget = "";
  });

  function label(candidate: StructureBlock): string {
    const candidateRegion = regionMap.get(candidate.sourceRegionId);
    const position = blockPositions.get(candidate.id) ?? 0;
    const text = candidateRegion?.text.replace(/\s+/gu, " ").trim().slice(0, 48);
    return `${position + 1} · ${roleNames[candidate.role]}${candidateRegion?.excluded ? " · Excluded" : ""}${text ? ` · ${text}` : ""}`;
  }
  function choices(candidates: StructureBlock[], query: string, selected: string | undefined): StructureBlock[] {
    const search = query.trim().toLocaleLowerCase();
    const result: StructureBlock[] = [];
    let selectedCandidate: StructureBlock | undefined;
    for (const candidate of candidates) {
      if (candidate.id === selected) selectedCandidate = candidate;
      if (result.length < 100 && (!search || label(candidate).toLocaleLowerCase().includes(search) || regionMap.get(candidate.sourceRegionId)?.text.toLocaleLowerCase().includes(search))) result.push(candidate);
    }
    if (selectedCandidate && !result.includes(selectedCandidate)) result.unshift(selectedCandidate);
    return result;
  }
  function selectBlock(id: string) {
    const next = structure?.blocks.find((item) => item.id === id);
    if (!next) return;
    actionError = "";
    rangeState = null;
    onselect(next.sourceRegionId);
  }
  function run(action: () => OcrProject, message: string) {
    if (disabled || !page) return;
    actionError = "";
    try { onchange(action(), message); }
    catch (error) { actionError = error instanceof Error ? error.message : String(error); }
  }
  function patch(properties: PropertyPatch, message = "Document structure updated. Review the preview before approving the page.") {
    if (blockDisabled || !block) return;
    run(() => setBlockProperties(project, pageId, block!.id, properties), message);
  }
  function changeRole(role: StructureRole) {
    const properties: PropertyPatch = { role };
    if (role === "heading") properties.headingLevel = block?.headingLevel ?? 1;
    if (role === "list-item") properties.list = block?.list ?? { kind: "bullet", level: 0 };
    patch(properties);
  }
  function captureSelection(): TextRange | null {
    if (!editor || !region || !block) return null;
    const start = sourceOffsetForTextarea(region.text, editor.value, editor.selectionStart);
    const end = sourceOffsetForTextarea(region.text, editor.value, editor.selectionEnd);
    if (start === null || end === null) { rangeState = null; return null; }
    rangeState = { blockId: block.id, text: region.text, start, end };
    return { start, end };
  }
  function selectedRange(): TextRange {
    const range = captureSelection();
    if (!range || range.start === range.end) throw new Error("Select complete text in the passage before applying formatting or a note link.");
    return range;
  }
  async function format(mark: keyof InlineMarks | "clear") {
    if (blockDisabled || !canFormat || !block || !region || !editor) return;
    const current = { id: block.id, regionId: region.id, text: region.text, start: editor.selectionStart, end: editor.selectionEnd };
    try {
      const range = selectedRange();
      const isSet = mark === "clear" ? false : structureSegments(region.text, block).filter((part) => part.start < range.end && part.end > range.start).every((part) => part[mark]);
      run(() => mark === "clear" ? clearInlineMarks(project, pageId, current.id, range) : setInlineMarks(project, pageId, current.id, range, { [mark]: !isSet }), mark === "clear" ? "Selected text formatting cleared." : "Selected text formatting updated.");
      await tick();
      if (editor && block?.id === current.id && region?.text === current.text) {
        editor.focus();
        editor.setSelectionRange(current.start, current.end);
        captureSelection();
      }
    } catch (error) { actionError = error instanceof Error ? error.message : String(error); }
  }
  function linkNote(remove = false) {
    if (blockDisabled || !block || !canFormat) return;
    try {
      const range = selectedRange();
      if (!remove && !noteTarget) throw new Error("Choose the footnote to link to.");
      run(() => setFootnoteReference(project, pageId, block!.id, range, remove ? null : noteTarget), remove ? "Selected note link removed." : "Selected text linked to the chosen footnote.");
    } catch (error) { actionError = error instanceof Error ? error.message : String(error); }
  }
  function edit(text: string) {
    if (!region || blockDisabled) return;
    actionError = "";
    rangeState = null;
    onedit(region, text);
  }
  function editorKeydown(event: KeyboardEvent) {
    if (event.isComposing || !(event.ctrlKey || event.metaKey) || event.altKey) return;
    const mark = ({ b: "bold", i: "italic", u: "underline" } as const)[event.key.toLowerCase() as "b" | "i" | "u"];
    if (!mark || !canFormat) return;
    event.preventDefault();
    event.stopPropagation();
    void format(mark);
  }
  function move(direction: -1 | 1) {
    if (blockDisabled || !region) return;
    run(() => moveRegion(project, pageId, region!.id, direction), "Block reading order updated. Check its section and column in the preview.");
  }
  function applyColumns() {
    if (!section || blockDisabled) return;
    if (columnDraft === 2 && !secondColumnStart) { actionError = "Choose the first block in the second reading column."; return; }
    run(() => setSectionColumns(project, pageId, section!.id, columnDraft === 2 ? secondColumnStart : null), "Section columns updated in the existing reading order.");
  }
  function setListNumber(key: "level" | "start", raw: string) {
    if (!block?.list) return;
    const value = Number(raw);
    if (!raw || !Number.isInteger(value) || value < (key === "level" ? 0 : 1) || value > (key === "level" ? 5 : 1000000)) { actionError = key === "level" ? "List level must be an integer from 0 to 5." : "List start must be an integer from 1 to 1,000,000."; return; }
    patch({ list: { ...block.list, [key]: value } });
  }
  function changeListKind(kind: "bullet" | "number") {
    if (!block?.list) return;
    const previous = structure?.blocks[index - 1];
    const continues = previous?.role === "list-item" && previous.list?.kind === "number" && previous.list.level === block.list.level && previous.sectionId === block.sectionId && previous.column === block.column;
    patch({ list: { kind, level: block.list.level, ...(kind === "number" && !continues ? { start: block.list.start ?? 1 } : {}) } });
  }
  function continueNumbering(checked: boolean) {
    if (!block?.list) return;
    patch({ list: { kind: "number", level: block.list.level, ...(!checked ? { start: block.list.start ?? 1 } : {}) } });
  }
</script>

{#if page && structure}<section class="structure-panel" data-structure-panel aria-label="Document structure editor">
  <p class="help">Assign document meaning and formatting to the existing passages. Reading order follows the region order; source geometry remains available beside the editor.</p>
  {#if !page.structure}
    <div class="initialize"><h3>Set up document structure</h3><p class="help">Start with one block per source region. Then review headings, lists, columns, figures, and notes.</p>
      <button type="button" class="primary" disabled={disabled || page.excluded || page.status === "running"} onclick={() => run(() => initializePageStructure(project, pageId), "Document structure is ready to edit.")}>Enable document structure</button>
    </div>
  {:else if block && region}
    <div class="block-navigation">
      <label class="block-picker">Structure passage<select aria-label="Structure passage" value={block.id} onchange={(event) => selectBlock(event.currentTarget.value)}>{#each navigationBlocks as candidate (candidate.id)}<option value={candidate.id}>{label(candidate)}</option>{/each}</select></label>
      <div class="navigation-actions">
        <button type="button" aria-label="Previous block" disabled={index <= 0} onclick={() => selectBlock(structure.blocks[index - 1].id)}><ChevronLeft size={14} /></button>
        <span>{index + 1} of {structure.blocks.length}</span>
        <button type="button" aria-label="Next block" disabled={index + 1 >= structure.blocks.length} onclick={() => selectBlock(structure.blocks[index + 1].id)}><ChevronRight size={14} /></button>
        <button type="button" aria-label="Move block earlier" title="Move this block earlier in reading order" disabled={blockDisabled || index <= 0} onclick={() => move(-1)}><ArrowUp size={14} /></button>
        <button type="button" aria-label="Move block later" title="Move this block later in reading order" disabled={blockDisabled || index + 1 >= structure.blocks.length} onclick={() => move(1)}><ArrowDown size={14} /></button>
      </div>
      {#if structure.blocks.length > 100}<div class="navigation-actions"><button type="button" disabled={navigationStart === 0} onclick={() => selectBlock(structure.blocks[Math.max(0, navigationStart - 100)].id)}>Previous 100 blocks</button><button type="button" disabled={navigationStart + 100 >= structure.blocks.length} onclick={() => selectBlock(structure.blocks[navigationStart + 100].id)}>Next 100 blocks</button></div><p class="help">The picker shows blocks {navigationStart + 1}–{Math.min(navigationStart + 100, structure.blocks.length)}.</p>{/if}
    </div>
    {#if region.excluded}<p class="warning">This source region is excluded. Include it in Regions before changing its structure.</p>{/if}
    <div class="property-grid">
      <label>Block role<select aria-label="Block role" value={block.role} disabled={blockDisabled} onchange={(event) => changeRole(event.currentTarget.value as StructureRole)}>{#each roleChoices as role}<option value={role}>{roleNames[role]}</option>{/each}</select></label>
      <label>Text alignment<select aria-label="Text alignment" value={block.alignment} disabled={blockDisabled} onchange={(event) => patch({ alignment: event.currentTarget.value as StructureBlock["alignment"] })}><option value="start">Start</option><option value="center">Center</option><option value="end">End</option><option value="justify">Justify</option></select></label>
      {#if block.role === "heading"}<label>Heading level<select aria-label="Heading level" value={block.headingLevel} disabled={blockDisabled} onchange={(event) => patch({ headingLevel: Number(event.currentTarget.value) as StructureBlock["headingLevel"] })}>{#each [1, 2, 3, 4, 5, 6] as level}<option value={level}>Heading {level}</option>{/each}</select></label>{/if}
      <label>Break before<select aria-label="Break before" value={block.breakBefore} disabled={blockDisabled} onchange={(event) => patch({ breakBefore: event.currentTarget.value as StructureBlock["breakBefore"] })}><option value="none">Continue</option><option value="page">Page break</option></select></label>
    </div>
    {#if block.role === "list-item" && block.list}<div class="property-grid list-properties">
      <label>List type<select aria-label="List type" value={block.list.kind} disabled={blockDisabled} onchange={(event) => changeListKind(event.currentTarget.value as "bullet" | "number")}><option value="bullet">Bulleted</option><option value="number">Numbered</option></select></label>
      <label>List level<input type="number" aria-label="List level" min="0" max="5" step="1" value={block.list.level} disabled={blockDisabled} onchange={(event) => setListNumber("level", event.currentTarget.value)} /></label>
      {#if block.list.kind === "number"}<label>List start<input type="number" aria-label="List start" min="1" max="1000000" step="1" value={block.list.start ?? 1} disabled={blockDisabled || block.list.start === undefined} onchange={(event) => setListNumber("start", event.currentTarget.value)} /></label><label class="check-label"><input type="checkbox" aria-label="Continue list numbering" checked={block.list.start === undefined} disabled={blockDisabled} onchange={(event) => continueNumbering(event.currentTarget.checked)} />Continue list numbering</label>{/if}
    </div>{/if}
    {#if block.role !== "figure"}
      <label class="passage-label">Structure passage text<textarea bind:this={editor} data-structure-text aria-label="Structure passage text" value={region.text} disabled={blockDisabled} rows="7" dir="auto" spellcheck="true" oninput={(event) => edit(event.currentTarget.value)} onselect={captureSelection} onkeyup={captureSelection} onmouseup={captureSelection} onkeydown={editorKeydown} onfocus={() => onselect(region!.id)} {onblur}></textarea></label>
      <p class="help">Typing and pasted replacements use plain formatting. Formatting on unchanged text is retained where its range is certain. Ctrl/Cmd+Z restores text and formatting together; Ctrl/Cmd+B, I, or U formats a selected range.</p>
      {#if canFormat}
        <div class="format-toolbar" role="group" aria-label="Selected text formatting">
          <button type="button" aria-label="Bold selection" title="Bold selected text" aria-pressed={selectedStyles.bold} disabled={blockDisabled} onclick={() => format("bold")}><Bold size={15} />Bold</button>
          <button type="button" aria-label="Italic selection" title="Italic selected text" aria-pressed={selectedStyles.italic} disabled={blockDisabled} onclick={() => format("italic")}><Italic size={15} />Italic</button>
          <button type="button" aria-label="Underline selection" title="Underline selected text" aria-pressed={selectedStyles.underline} disabled={blockDisabled} onclick={() => format("underline")}><Underline size={15} />Underline</button>
          <button type="button" aria-label="Clear selection formatting" title="Clear formatting on selected text" disabled={blockDisabled} onclick={() => format("clear")}><Eraser size={15} />Clear</button>
        </div>
        <div class="selected-preview" data-selected-structure-preview style:text-align={block.alignment} aria-label="Selected passage formatting preview"><StructureText text={region.text} {block} onnote={selectBlock} /></div>
      {:else}<p class="help">Use Extract for the table cells or formula object. This text remains available as the passage transcription.</p>{/if}
    {/if}
    <details bind:open={layoutOpen}>
      <summary>Sections and columns</summary>
      {#if layoutOpen && section}
        <p class="help">Section {structure.sections.findIndex((item) => item.id === section.id) + 1} · Reading column {block.column}. Sections group consecutive blocks. Column choices keep their existing reading sequence.</p>
        <button type="button" disabled={blockDisabled || sectionBlocks[0]?.id === block.id} onclick={() => run(() => startSection(project, pageId, block!.id), "A section starts at this block.")}>Start section here</button>
        <div class="property-grid">
          <label>Section columns<select aria-label="Section columns" bind:value={columnDraft} disabled={blockDisabled}><option value={1}>One column</option><option value={2}>Two columns</option></select></label>
          {#if columnDraft === 2}<label>Second column starts at<select aria-label="Second column starts at" bind:value={secondColumnStart} disabled={blockDisabled}><option value="">Choose a block…</option>{#each visibleColumnStarts as candidate (candidate.id)}<option value={candidate.id}>{label(candidate)}</option>{/each}</select></label>{/if}
        </div>
        {#if columnDraft === 2 && sectionBlocks.length > 101}<label>Find second-column start<input type="search" aria-label="Find second-column start" bind:value={columnQuery} placeholder="Block number or text" /></label><p class="help">Up to 100 matching blocks are shown.</p>{/if}
        <button type="button" disabled={blockDisabled || (columnDraft === 2 && !secondColumnStart)} onclick={applyColumns}>Apply section columns</button>
        <p class="help">First and second refer to reading order. A spanning heading or figure can occupy a separate one-column section. These choices do not move source-image boxes.</p>
      {/if}
    </details>
    <details bind:open={linksOpen}>
      <summary>Figure and note links</summary>
      {#if linksOpen}
        {#if block.role === "figure"}
          <label>Figure alternative text<textarea aria-label="Figure alternative text" bind:value={altDraft} maxlength="10000" rows="3" disabled={blockDisabled || decorativeDraft}></textarea></label>
          <label class="check-label"><input type="checkbox" aria-label="Decorative figure" bind:checked={decorativeDraft} disabled={blockDisabled} />Decorative figure</label>
          <button type="button" disabled={blockDisabled} onclick={() => patch({ altText: decorativeDraft ? "" : altDraft, decorative: decorativeDraft }, "Figure description updated.")}>Apply figure description</button>
          <p class="help">Describe the figure's meaning for readers who cannot see it. Mark only nonessential visual decoration as decorative.</p>
        {:else if block.role === "caption"}
          {#if targets.length > 100}<label>Find caption source<input type="search" aria-label="Find caption source" bind:value={captionQuery} placeholder="Block number or text" /></label>{/if}
          <label>Caption source<select aria-label="Caption source" value={block.anchorBlockId ?? ""} disabled={blockDisabled} onchange={(event) => run(() => setCaptionAnchor(project, pageId, block!.id, event.currentTarget.value || null), "Caption source link updated.")}><option value="">No source link</option>{#each visibleTargets as candidate (candidate.id)}<option value={candidate.id}>{label(candidate)}</option>{/each}</select></label>
          <p class="help">Choose the figure or table this caption describes. Nearby position alone does not establish that relationship.</p>
        {/if}
        {#if canFormat}
          {#if notes.length > 100}<label>Find footnote<input type="search" aria-label="Find footnote" bind:value={noteQuery} placeholder="Block number or text" /></label>{/if}
          <label>Footnote target<select aria-label="Footnote target" bind:value={noteTarget} disabled={blockDisabled || !notes.length}><option value="">Choose a footnote…</option>{#each visibleNotes as candidate (candidate.id)}<option value={candidate.id}>{label(candidate)}</option>{/each}</select></label>
          <div class="link-actions"><button type="button" disabled={blockDisabled || !noteTarget} onclick={() => linkNote()}>Link selected text to note</button><button type="button" disabled={blockDisabled || !block.noteReferences?.length} onclick={() => linkNote(true)}>Remove selected note link</button></div>
          <p class="help">Select the reference marker in the passage, then choose its note. First give the note's passage the Footnote role. Self-links and circular note references are rejected.</p>
          {#if block.noteReferences?.length}<ul class="note-links">{#each block.noteReferences as reference}<li><span dir="auto">“{region.text.slice(reference.start, reference.end)}”</span> → <button type="button" onclick={() => selectBlock(reference.targetBlockId)}>{label(structure.blocks.find((item) => item.id === reference.targetBlockId)!)}</button></li>{/each}</ul>{/if}
        {:else if block.role !== "figure"}<p class="help">Captions and note links are authored in text passages. Choose a text passage and set its role above.</p>{/if}
      {/if}
    </details>
    {#if notices.length}<div class="structure-notices" role="status"><h4>Structure changes to review</h4><ul>{#each notices as item}<li>{item}</li>{/each}</ul></div>{/if}
    <details bind:open={previewOpen}><summary>Show structure preview</summary>{#if previewOpen}<StructurePreview {page} {structure} {imageUrl} {onselect} />{/if}</details>
    <p class="help export-help">Structure is saved with the project and JSON export. Check the selected output format's supported formatting before export.</p>
  {:else}<p class="help">Add or recognize source regions before assigning document structure.</p>{/if}
  {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}
</section>{/if}

<style>
  .structure-panel { min-width: 0; color: #dbeafe; font-size: 12px; }
  .help { margin: 7px 0 12px; color: #93c5fd; line-height: 1.65; font-size: 11px; }
  .initialize { padding: 13px; border: 1px solid #71717a; border-radius: 6px; background: #27272a; }
  h3 { margin: 0 0 6px; font-size: 15px; }
  label { display: flex; flex-direction: column; gap: 6px; min-width: 0; margin: 10px 0; color: #bfdbfe; font-size: 11px; }
  select, textarea, input[type="number"], input[type="search"] { box-sizing: border-box; min-width: 0; width: 100%; padding: 8px; border: 1px solid #52525b; border-radius: 4px; background: #172554; color: #dbeafe; font: inherit; }
  textarea { font-size: 14px; line-height: 1.7; resize: vertical; }
  .passage-label { margin-bottom: 6px; }
  .property-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 0 10px; }
  .block-navigation { margin-bottom: 12px; padding-bottom: 12px; border-bottom: 1px solid #52525b; }
  .navigation-actions, .format-toolbar, .link-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 7px; }
  .navigation-actions span { color: #93c5fd; margin-right: auto; font-size: 11px; }
  button { display: inline-flex; align-items: center; justify-content: center; gap: 5px; padding: 7px 9px; border: 1px solid #71717a; border-radius: 4px; background: #3f3f46; color: #dbeafe; font: inherit; font-size: 11px; cursor: pointer; }
  button.primary { background: #2563eb; border-color: #60a5fa; }
  button[aria-pressed="true"] { border-color: #93c5fd; background: #2563eb; }
  button:disabled, select:disabled, textarea:disabled, input:disabled { opacity: .5; cursor: default; }
  button:focus-visible, select:focus-visible, textarea:focus-visible, input:focus-visible, summary:focus-visible { outline: 2px solid #93c5fd; outline-offset: 2px; }
  .selected-preview { padding: 12px; margin: 10px 0 14px; border: 1px solid #52525b; border-radius: 4px; background: #27272a; color: #dbeafe; line-height: 1.8; font-size: 14px; white-space: pre-wrap; overflow-wrap: anywhere; }
  details { margin: 10px 0; padding: 10px; border: 1px solid #52525b; border-radius: 5px; background: #18181b; }
  summary { color: #bfdbfe; cursor: pointer; line-height: 1.6; font-size: 12px; }
  .check-label { flex-direction: row; align-items: center; gap: 8px; }
  .note-links { margin: 10px 0; padding-left: 20px; color: #d4d4d8; }
  .note-links li { margin-bottom: 7px; overflow-wrap: anywhere; }
  .note-links button { border: 0; background: transparent; color: #93c5fd; padding: 0; text-align: start; }
  .warning, .error, .structure-notices { margin: 12px 0; padding: 10px; border: 1px solid #71717a; border-radius: 5px; color: #fcd34d; background: #3f3f46; font-size: 11px; line-height: 1.65; }
  .error { border-color: #71717a; color: #fecaca; background: #27272a; }
  .structure-notices h4 { margin: 0 0 6px; font-size: 12px; }
  .structure-notices ul { padding-left: 18px; margin: 0; }
  .export-help { margin-bottom: 0; }
  @media (max-width: 460px) { .property-grid { grid-template-columns: minmax(0, 1fr); gap: 0; } }
</style>
