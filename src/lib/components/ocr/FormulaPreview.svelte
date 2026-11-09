<script lang="ts">
  import { renderToString } from "katex";
  import "katex/dist/katex.min.css";

  let { latex }: { latex: string } = $props();
  const preview = $derived.by(() => {
    if (!latex.trim()) return { html: "", error: "" };
    if (latex.length > 10_000) return { html: "", error: "This formula is too long for the visual preview. Its full LaTeX remains editable and can still be copied or saved." };
    try {
      // Model output is never inserted as HTML. Only KaTeX's bounded,
      // untrusted rendering reaches the preview; macros are fresh per render.
      const html = renderToString(latex, {
        displayMode: true,
        throwOnError: true,
        strict: "error",
        trust: false,
        maxExpand: 1_000,
        maxSize: 20,
        output: "htmlAndMathml",
        macros: {},
      });
      if (html.length > 200_000) return { html: "", error: "The rendered formula exceeds the preview limit. Its full LaTeX remains available below." };
      return { html, error: "" };
    } catch (failure) {
      const message = failure instanceof Error ? failure.message : String(failure);
      return { html: "", error: message.slice(0, 400) };
    }
  });
</script>

<section class="formula-preview" aria-label="Rendered formula preview" data-formula-preview>
  <h4>Formula preview</h4>
  {#if preview.error}<p class="preview-error" role="status">Preview unavailable: {preview.error}</p>
  {:else if preview.html}<div class="math-preview">{@html preview.html}</div>
  {:else}<p class="empty-preview">Enter LaTeX to display a preview.</p>{/if}
  <p class="preview-help">Compare the notation with the captured source. A rendered preview does not verify the mathematics.</p>
</section>

<style>
  .formula-preview { margin: 14px 0; padding: 12px; border: 1px solid #415b7b; border-radius: 6px; background: #111e30; }
  h4 { margin: 0 0 10px; color: #c9def8; font-size: 12px; }
  .math-preview { max-height: 320px; overflow: auto; padding: 7px 3px; color: #e5eefb; font-size: 15px; }
  .math-preview :global(.katex-display) { margin: .5em 0; }
  .preview-help, .preview-error, .empty-preview { margin: 8px 0 0; color: #9eb8d6; font-size: 11px; line-height: 1.65; overflow-wrap: anywhere; }
  .preview-error { color: #f4c6a5; }
</style>
