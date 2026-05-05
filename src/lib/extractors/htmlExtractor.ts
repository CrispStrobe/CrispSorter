/**
 * HTML / HTM extractor.
 *
 * Strips `<script>` / `<style>` / `<noscript>`, emits readable plain text,
 * and walks `<h1..h6>` to build a Markdown view + heading list.
 *
 * No dependencies — pure DOMParser. Decodes the file with the charset declared
 * in `<meta charset>` if present, otherwise UTF-8.
 */

export interface HtmlExtractionResult {
    text: string;
    markdownText: string;
    headings: string[];
}

function decodeHtml(arrayBuffer: ArrayBuffer): string {
    // First pass with UTF-8 to read the charset meta tag.
    const sniff = new TextDecoder('utf-8', { fatal: false }).decode(arrayBuffer.slice(0, 4096));
    const m = sniff.match(/<meta[^>]+charset\s*=\s*["']?([\w-]+)/i);
    const charset = (m?.[1] ?? 'utf-8').toLowerCase();
    if (charset === 'utf-8' || charset === 'utf8') {
        return new TextDecoder('utf-8').decode(arrayBuffer);
    }
    try {
        return new TextDecoder(charset).decode(arrayBuffer);
    } catch {
        return new TextDecoder('utf-8').decode(arrayBuffer);
    }
}

function nodeToMarkdown(node: Node, out: string[], headings: string[]) {
    if (node.nodeType === Node.TEXT_NODE) {
        const t = node.textContent ?? '';
        if (t.trim()) out.push(t);
        return;
    }
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const el = node as Element;
    const tag = el.tagName.toLowerCase();
    if (tag === 'script' || tag === 'style' || tag === 'noscript' || tag === 'template') return;

    const headingMatch = /^h([1-6])$/.exec(tag);
    if (headingMatch) {
        const level = parseInt(headingMatch[1], 10);
        const txt = (el.textContent ?? '').trim();
        if (txt) {
            out.push('\n\n' + '#'.repeat(level) + ' ' + txt + '\n\n');
            headings.push(txt);
        }
        return;
    }

    if (tag === 'br') { out.push('\n'); return; }
    if (tag === 'p' || tag === 'div' || tag === 'li' || tag === 'tr') {
        out.push('\n');
        for (const child of Array.from(el.childNodes)) nodeToMarkdown(child, out, headings);
        out.push('\n');
        return;
    }

    for (const child of Array.from(el.childNodes)) nodeToMarkdown(child, out, headings);
}

export async function extractHtml(arrayBuffer: ArrayBuffer): Promise<HtmlExtractionResult> {
    const html = decodeHtml(arrayBuffer);

    if (typeof DOMParser === 'undefined') {
        // Fallback: rough regex strip — should never trigger in browser env.
        const text = html.replace(/<style[\s\S]*?<\/style>/gi, '')
            .replace(/<script[\s\S]*?<\/script>/gi, '')
            .replace(/<[^>]+>/g, ' ')
            .replace(/\s+/g, ' ')
            .trim();
        return { text, markdownText: text, headings: [] };
    }

    const parser = new DOMParser();
    const doc = parser.parseFromString(html, 'text/html');

    // Drop noisy tags up-front.
    doc.querySelectorAll('script, style, noscript, template, head').forEach(n => n.remove());

    const out: string[] = [];
    const headings: string[] = [];
    if (doc.body) {
        for (const child of Array.from(doc.body.childNodes)) nodeToMarkdown(child, out, headings);
    }

    const markdownText = out.join('').replace(/\n{3,}/g, '\n\n').trim();
    const text = markdownText.replace(/^#{1,6}\s+/gm, '').trim();
    return { text, markdownText, headings };
}
