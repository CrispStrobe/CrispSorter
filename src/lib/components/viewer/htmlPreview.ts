import { parseFragment } from 'parse5';

/** HTML documents are untrusted data, including locally extracted attachments. */
export const MAX_HTML_PREVIEW_BYTES = 8 * 1024 * 1024;
export const HTML_PREVIEW_SANDBOX = '';
export const HTML_PREVIEW_CSP = "default-src 'none'; script-src 'none'; connect-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src 'none'; media-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'";

interface HtmlNode {
    nodeName: string;
    tagName?: string;
    namespaceURI?: string;
    value?: string;
    childNodes?: HtmlNode[];
    attrs?: { name: string; value: string }[];
}

const allowedTags = new Set(['a', 'abbr', 'address', 'article', 'aside', 'b', 'bdi', 'bdo', 'blockquote',
    'br', 'caption', 'cite', 'code', 'col', 'colgroup', 'dd', 'del', 'details', 'dfn', 'div', 'dl', 'dt',
    'em', 'figcaption', 'figure', 'footer', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'header', 'hr', 'i',
    'img', 'ins', 'kbd', 'li', 'main', 'mark', 'nav', 'ol', 'p', 'pre', 'q', 'rp', 'rt', 'ruby', 's',
    'samp', 'section', 'small', 'span', 'strong', 'sub', 'summary', 'sup', 'table', 'tbody', 'td',
    'tfoot', 'th', 'thead', 'time', 'tr', 'u', 'ul', 'var', 'wbr']);
const droppedSubtrees = new Set(['script', 'noscript', 'style', 'iframe', 'object', 'embed', 'form',
    'input', 'button', 'select', 'textarea', 'template', 'svg', 'math', 'canvas', 'link', 'meta',
    'base', 'audio', 'video', 'source', 'track', 'frameset', 'frame', 'applet']);
const voidTags = new Set(['br', 'col', 'hr', 'img', 'wbr']);
const escape = (value: string) => value.replace(/&/g, '&amp;').replace(/</g, '&lt;')
    .replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');

function safeAttributes(node: HtmlNode): string {
    const output: string[] = [];
    for (const { name, value } of node.attrs ?? []) {
        // Source CSS, links, event handlers, namespaces and navigation are never copied.
        if ((name === 'title' || name === 'lang' || name === 'alt') && value.length <= 2048) {
            output.push(`${name}="${escape(value)}"`);
        } else if (name === 'dir' && /^(ltr|rtl|auto)$/.test(value)) {
            output.push(`dir="${value}"`);
        } else if ((name === 'colspan' || name === 'rowspan') && (node.tagName === 'td' || node.tagName === 'th') &&
            /^[1-9]\d{0,2}$/.test(value)) {
            output.push(`${name}="${value}"`);
        } else if (name === 'scope' && node.tagName === 'th' && /^(row|col|rowgroup|colgroup)$/.test(value)) {
            output.push(`scope="${value}"`);
        } else if (name === 'start' && node.tagName === 'ol' && /^-?\d{1,6}$/.test(value)) {
            output.push(`start="${value}"`);
        } else if (name === 'src' && node.tagName === 'img' && value.length <= 4 * 1024 * 1024 &&
            /^data:image\/(?:png|jpeg|gif|webp);base64,[a-zA-Z0-9+/=\r\n]+$/.test(value)) {
            output.push(`src="${escape(value)}"`);
        }
    }
    return output.length ? ' ' + output.join(' ') : '';
}

/** Render a bounded, inert reading copy inside a sandbox with its own restrictive CSP.
 * The original file is not changed. The output must never be injected into the app DOM.
 */
export function htmlPreviewDocument(raw: string): string {
    if (raw.length > MAX_HTML_PREVIEW_BYTES || new TextEncoder().encode(raw).length > MAX_HTML_PREVIEW_BYTES) {
        throw new Error('This HTML document exceeds the 8 MiB preview limit.');
    }
    const tree = parseFragment(raw) as unknown as HtmlNode;
    let nodes = 0;
    const pending: { node: HtmlNode; depth: number }[] = [{ node: tree, depth: 0 }];
    while (pending.length) {
        const { node, depth } = pending.pop()!;
        if (++nodes > 100_000 || depth > 128) throw new Error('This HTML document is too complex to preview.');
        for (const child of node.childNodes ?? []) pending.push({ node: child, depth: depth + 1 });
    }
    function render(node: HtmlNode): string {
        if (node.nodeName === '#text') return escape(node.value ?? '');
        if (node.nodeName === '#comment') return '';
        const tag = node.tagName;
        if (tag && (droppedSubtrees.has(tag) || node.namespaceURI && node.namespaceURI !== 'http://www.w3.org/1999/xhtml')) return '';
        const children = () => (node.childNodes ?? []).map(render).join('');
        if (!tag || !allowedTags.has(tag)) return children();
        const opening = `<${tag}${safeAttributes(node)}>`;
        return voidTags.has(tag) ? opening : opening + children() + `</${tag}>`;
    }
    return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${HTML_PREVIEW_CSP}"><meta name="referrer" content="no-referrer"><style>
html{color-scheme:dark}body{margin:0;padding:16px;color:#e4e4e7;background:#0a0a0c;font:14px/1.6 system-ui,sans-serif;overflow-wrap:anywhere}
h1,h2,h3,h4{color:#fafafa}a{color:#93c5fd}img{max-width:100%;height:auto}table{border-collapse:collapse;max-width:100%}
th,td{border:1px solid #52525b;padding:4px 8px}pre,code{background:#18181b;color:#d4d4d8}pre{padding:12px;white-space:pre-wrap}blockquote{border-left:3px solid #52525b;margin-left:0;padding-left:16px}
</style></head><body>${render(tree)}</body></html>`;
}
