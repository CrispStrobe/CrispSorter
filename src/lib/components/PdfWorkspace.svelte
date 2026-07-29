<script lang="ts">
    // The PDF tab hosts two surfaces: the P32.1b page editor (arrange,
    // crop, number, annotate — direct manipulation over an edit session)
    // and the existing one-shot tool panel (merge, split, encrypt, sign,
    // sanitise, PDF/A).  They operate on different mental models, so they
    // get a segmented control rather than being crammed into one toolbar.

    import { i18n } from '$lib/i18n.svelte';
    import PdfEditor from './PdfEditor.svelte';
    import PdfTools from './PdfTools.svelte';
    import { LayoutGrid, Wrench } from 'lucide-svelte';

    let mode = $state<'edit' | 'tools'>('edit');
</script>

<div class="pw">
    <div class="pw-modes">
        <button class="pw-mode" class:active={mode === 'edit'} onclick={() => mode = 'edit'}>
            <LayoutGrid size={13} /> {i18n.t.pdfeditor.mode_edit}
        </button>
        <button class="pw-mode" class:active={mode === 'tools'} onclick={() => mode = 'tools'}>
            <Wrench size={13} /> {i18n.t.pdfeditor.mode_tools}
        </button>
    </div>
    <!-- Both stay mounted: switching modes must not discard an open edit
         session or a half-filled tool panel. -->
    <div class="pw-pane" style:display={mode === 'edit' ? 'flex' : 'none'}>
        <PdfEditor />
    </div>
    <div class="pw-pane" style:display={mode === 'tools' ? 'flex' : 'none'}>
        <PdfTools />
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
