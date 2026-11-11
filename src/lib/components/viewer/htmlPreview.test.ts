import { describe, expect, it } from 'vitest';
import { parse } from 'parse5';
import { HTML_PREVIEW_CSP, htmlPreviewDocument, MAX_HTML_PREVIEW_BYTES } from './htmlPreview';

function elements(document: string): any[] {
    const found: any[] = [], pending: any[] = [parse(document)];
    while (pending.length) {
        const node = pending.pop();
        if (node.tagName) found.push(node);
        pending.push(...(node.childNodes ?? []));
    }
    return found;
}
const attr = (node: any, name: string) => node.attrs.find((item: any) => item.name === name)?.value;

describe('isolated HTML reading copies', () => {
    it('preserves readable structure and bounded table spans', () => {
        const output = htmlPreviewDocument('<h1>Title &amp; context</h1><table><tr><th scope="col" colspan="2">A</th></tr><tr><td>B</td></tr></table><p dir="rtl"><b>Text</b></p>');
        expect(output).toContain('<h1>Title &amp; context</h1>');
        expect(output).toContain('<th scope="col" colspan="2">A</th>');
        expect(output).toContain('<p dir="rtl"><b>Text</b></p>');
    });
    it('removes active content, handlers, links and remote resources', () => {
        const output = htmlPreviewDocument('<script>bad()</script><img src="https://remote.test/track" onerror="bad()"><a href="javascript:bad()" target="_top">Link</a><iframe srcdoc="bad"></iframe><form action="https://remote.test"><input></form><video src="https://remote.test"></video>');
        expect(output).not.toMatch(/bad\(\)|https:\/\/remote|javascript:|onerror|srcdoc|<iframe|<form|<input|<video/);
        expect(output).toContain('<a>Link</a>');
    });
    it('discards source styles, imports, CSS URLs and base/meta navigation', () => {
        const output = htmlPreviewDocument('<base href="https://remote.test"><meta http-equiv="refresh" content="0; url=https://remote.test"><style>@import "https://remote.test";</style><p style="background:url(https://remote.test)" id="escape" class="external">Read me</p>');
        expect(output).not.toContain('remote.test');
        expect(output).toContain('<p>Read me</p>');
        const nodes = elements(output);
        expect(nodes.filter(node => node.tagName === 'style')).toHaveLength(1);
        const policies = nodes.filter(node => node.tagName === 'meta' && attr(node, 'http-equiv') === 'Content-Security-Policy');
        expect(policies).toHaveLength(1);
        expect(attr(policies[0], 'content')).toBe(HTML_PREVIEW_CSP);
    });
    it('does not turn malformed or namespace markup into active elements after reparsing', () => {
        const output = htmlPreviewDocument('<svg><foreignObject><p onload="bad()">x</p></foreignObject></svg><math><mtext><table><mglyph><style><!--</style><img title="--><img src=x onerror=bad()>"></mtext></math><p title="&quot; onmouseover=&quot;bad()">safe</p>');
        for (const node of elements(output)) {
            expect(['svg', 'math', 'script', 'iframe', 'object', 'form']).not.toContain(node.tagName);
            expect(node.attrs.every((item: any) => !/^on/i.test(item.name))).toBe(true);
        }
        const escaped = htmlPreviewDocument('<p title="&quot; onmouseover=&quot;bad()">safe</p>');
        const paragraph = elements(escaped).find(node => node.tagName === 'p');
        expect(paragraph.attrs).toEqual([{ name: 'title', value: '" onmouseover="bad()' }]);
    });
    it('permits only embedded raster images and removes vector, blob and file URLs', () => {
        const output = htmlPreviewDocument('<img src="data:image/png;base64,AAAA" alt="Embedded"><img src="data:image/svg+xml;base64,AAAA"><img src="blob:secret"><img src="file:///private"><img srcset="https://remote.test/2x 2x">');
        const images = elements(output).filter(node => node.tagName === 'img');
        expect(images.map(node => attr(node, 'src')).filter(Boolean)).toEqual(['data:image/png;base64,AAAA']);
        expect(output).not.toMatch(/srcset|file:\/\/|blob:secret|svg\+xml|remote.test/);
    });
    it('keeps hostile closing tags as text and its policy before source content', () => {
        const output = htmlPreviewDocument('<p>&lt;/body&gt;&lt;script&gt;alert(1)&lt;/script&gt;</p><meta http-equiv="Content-Security-Policy" content="default-src *">');
        expect(output).toContain('&lt;script&gt;');
        expect(output).not.toContain('default-src *');
        expect(output.indexOf('Content-Security-Policy')).toBeLessThan(output.indexOf('<body>'));
    });
    it('rejects byte and structural limits instead of truncating ambiguous markup', () => {
        expect(() => htmlPreviewDocument('x'.repeat(MAX_HTML_PREVIEW_BYTES + 1))).toThrow('8 MiB');
        expect(() => htmlPreviewDocument('é'.repeat(MAX_HTML_PREVIEW_BYTES / 2 + 1))).toThrow('8 MiB');
        expect(() => htmlPreviewDocument('<div>'.repeat(130) + 'deep' + '</div>'.repeat(130))).toThrow('complex');
    });
});
