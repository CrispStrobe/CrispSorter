<script lang="ts">
    /**
     * Universal document viewer — content-area-only component.
     *
     * Detects the file format by extension and renders the appropriate
     * sub-viewer.  Parents provide their own wrapper / header / close
     * button; this component fills the content region.
     *
     * Works cross-platform including mobile (no native PDF viewer needed).
     */
    import { uriToPath, extOf, detectKind, type ViewerKind } from './types';
    import PdfViewer from './PdfViewer.svelte';
    import ImageViewer from './ImageViewer.svelte';
    import TextViewer from './TextViewer.svelte';
    import DocxViewer from './DocxViewer.svelte';
    import EpubViewer from './EpubViewer.svelte';
    import HtmlViewer from './HtmlViewer.svelte';
    import CsvViewer from './CsvViewer.svelte';
    import FallbackViewer from './FallbackViewer.svelte';
    import { i18n } from '$lib/i18n.svelte';

    let {
        locationUri = '',
        filename = '',
    }: {
        locationUri: string;
        filename?: string;
    } = $props();

    let path = $derived(uriToPath(locationUri));
    let ext = $derived(extOf(locationUri || filename));
    let kind: ViewerKind = $derived(path ? detectKind(ext) : 'fallback');
</script>

<div class="document-viewer">
    {#if !path}
        <FallbackViewer path="" filename={filename} />
    {:else if kind === 'pdf'}
        <PdfViewer {path} />
    {:else if kind === 'image'}
        <ImageViewer {path} />
    {:else if kind === 'docx'}
        <DocxViewer {path} />
    {:else if kind === 'epub'}
        <EpubViewer {path} {filename} />
    {:else if kind === 'html'}
        <HtmlViewer {path} />
    {:else if kind === 'csv'}
        <CsvViewer {path} />
    {:else if kind === 'text'}
        <TextViewer {path} />
    {:else}
        <FallbackViewer {path} {filename} />
    {/if}
</div>

<style>
    .document-viewer {
        display: flex;
        flex-direction: column;
        flex: 1;
        min-height: 0;
        overflow: hidden;
        background: #0a0a0c;
    }
</style>
