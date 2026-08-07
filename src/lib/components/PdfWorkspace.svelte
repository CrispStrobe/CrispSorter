<script lang="ts">
    // The document tab hosts three surfaces: the P32.1b page editor
    // (arrange, crop, number, annotate — direct manipulation over an edit
    // session), the one-shot PDF tool panel (merge, split, encrypt, sign,
    // sanitise, PDF/A), and the P30 DOCX surgery panel (validate, page
    // geometry, restyle to a template, notes, quotes).  They operate on
    // different mental models — and on different file formats — so they get
    // a segmented control rather than being crammed into one toolbar.

    import { i18n } from '$lib/i18n.svelte';
    import PdfEditor from './PdfEditor.svelte';
    import PdfTools from './PdfTools.svelte';
    import DocxTools from './DocxTools.svelte';
    import ConvertTools from './ConvertTools.svelte';
    import { LayoutGrid, Wrench, FileType2, FileOutput } from 'lucide-svelte';

    let mode = $state<'edit' | 'tools' | 'docx' | 'convert'>('edit');
</script>

<div class="pw">
    <div class="pw-modes">
        <button class="pw-mode" class:active={mode === 'edit'} onclick={() => mode = 'edit'}>
            <LayoutGrid size={13} /> {i18n.t.pdfeditor.mode_edit}
        </button>
        <button class="pw-mode" class:active={mode === 'tools'} onclick={() => mode = 'tools'}>
            <Wrench size={13} /> {i18n.t.pdfeditor.mode_tools}
        </button>
        <button class="pw-mode" class:active={mode === 'docx'} onclick={() => mode = 'docx'}>
            <FileType2 size={13} /> {i18n.t.docxtools.mode_docx}
        </button>
        <button class="pw-mode" class:active={mode === 'convert'} onclick={() => mode = 'convert'}>
            <FileOutput size={13} /> {i18n.t.converttools?.mode_convert ?? 'Convert'}
        </button>
    </div>
    <!-- All four stay mounted: switching modes must not discard an open
         edit session, a half-filled tool panel, a loaded DOCX report, or a
         conversion preview. -->
    <div class="pw-pane" style:display={mode === 'edit' ? 'flex' : 'none'}>
        <PdfEditor />
    </div>
    <div class="pw-pane" style:display={mode === 'tools' ? 'flex' : 'none'}>
        <PdfTools />
    </div>
    <div class="pw-pane" style:display={mode === 'docx' ? 'flex' : 'none'}>
        <DocxTools />
    </div>
    <div class="pw-pane" style:display={mode === 'convert' ? 'flex' : 'none'}>
        <ConvertTools />
    </div>
</div>

<style>
    .pw { display: flex; flex-direction: column; height: 100%; background: #09090b; }
    .pw-modes {
        display: flex; gap: 4px; padding: 6px 10px;
        border-bottom: 1px solid #27272a; background: #0c0c0f;
    }
    .pw-mode {
        display: inline-flex; align-items: center; gap: 5px;
        padding: 5px 11px; font-size: 12px; border-radius: 6px; cursor: pointer;
        border: 1px solid transparent; background: none; color: #a1a1aa;
    }
    .pw-mode:hover { background: #18181b; color: #e4e4e7; }
    .pw-mode.active { background: #18181b; border-color: #27272a; color: #e4e4e7; }
    .pw-pane { flex: 1; min-height: 0; flex-direction: column; }
</style>
