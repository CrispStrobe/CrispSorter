// Browser integration exercises the shipped editor with explicit native IPC
// fixtures. It does not claim native PDF transformation or OCR qualification.
import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

async function pdfFixture(page: Page) {
    await fixtures(page);
    await page.addInitScript(() => {
        const root = window as any;
        root.__pdfCalls = [];
        root.__pdfReleases = {};
        root.__pdfHold = {};
        root.__pdfOpenPath = '/fixtures/first.pdf';
        root.__pdfSavePath = '/fixtures/saved.pdf';
        root.__pdfFormFailure = false;
        root.__pdfSnapshots = [];
        const sessions = new Map<string, any>();
        const callbacks = new Map<number, Function>();
        const listeners: any[] = [];
        let nextSession = 1, nextSnapshot = 1;
        const wait = async (key: string) => {
            if (!root.__pdfHold[key]) return;
            delete root.__pdfHold[key];
            await new Promise<void>(resolve => { root.__pdfReleases[key] = resolve; });
        };
        const clone = (value: any) => JSON.parse(JSON.stringify(value));
        const session = (path: string) => {
            const count = path.includes('second') ? 1 : 2;
            return {
                id: `pdf-${nextSession++}`, source: path,
                info: { page_count: count, pages: Array.from({ length: count }, (_, i) => ({ page_number: i + 1, width_pt: 200, height_pt: 300, rotation: 0 })) },
                origins: Array.from({ length: count }, (_, index) => ({ source: path, index, rotation: 0, dirty: false })),
                can_undo: false, can_redo: false, undo_label: null, modified: false, ops: [],
            };
        };

        // A real tiny two-page PDF lets pdf.js exercise source/edited preview
        // sharing instead of replacing its render calls with test doubles.
        const stream = 'BT /F1 14 Tf 20 260 Td (PDF fixture) Tj ET';
        const objects = [
            '<< /Type /Catalog /Pages 2 0 R >>',
            '<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>',
            '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Resources << /Font << /F1 6 0 R >> >> /Contents 5 0 R >>',
            '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Resources << /Font << /F1 6 0 R >> >> /Contents 5 0 R >>',
            `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
            '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
        ];
        let pdf = '%PDF-1.4\n';
        const offsets = [0];
        objects.forEach((body, index) => { offsets.push(pdf.length); pdf += `${index + 1} 0 obj\n${body}\nendobj\n`; });
        const xref = pdf.length;
        pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
        for (const offset of offsets.slice(1)) pdf += `${String(offset).padStart(10, '0')} 00000 n \n`;
        pdf += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
        const bytes = Array.from(new TextEncoder().encode(pdf));

        root.__pdfDrop = (paths: string[]) => {
            for (const listener of listeners) {
                if (listener.event === 'tauri://drag-drop' && listener.target?.kind === 'Webview') {
                    callbacks.get(listener.handler)?.({ event: listener.event, id: 1, payload: { paths, position: { x: 20, y: 20 } } });
                }
            }
        };
        const wrapped = new WeakSet<object>();
        const wrap = (bridge: any) => {
            if (!bridge || wrapped.has(bridge)) return bridge;
            wrapped.add(bridge);
            const original = bridge.invoke;
            const originalTransform = bridge.transformCallback;
            bridge.transformCallback = (fn: Function, once?: boolean) => {
                const id = originalTransform(fn, once);
                callbacks.set(id, fn);
                return id;
            };
            bridge.invoke = async (command: string, args: any = {}, options?: any) => {
                root.__pdfCalls.push({ command, args: clone(args) });
                if (command === 'plugin:event|listen') listeners.push(args);
                if (command === 'plugin:dialog|open') return root.__pdfOpenPath;
                if (command === 'plugin:dialog|save') { await wait('save-dialog'); return root.__pdfSavePath; }
                if (command === 'plugin:dialog|message') return 'Yes';
                if (command === 'plugin:fs|read_file' && String(args.path).endsWith('.pdf')) return bytes;
                if (command === 'pdf_session_open') {
                    const value = session(args.path);
                    await wait(`open:${args.path}`);
                    sessions.set(value.id, value);
                    return clone(value);
                }
                if (command === 'pdf_session_close') { sessions.delete(args.id); return; }
                if (command === 'pdf_is_encrypted') return false;
                if (command === 'pdf_session_apply') {
                    const value = sessions.get(args.id);
                    await wait('apply');
                    value.ops.push(args.op.kind);
                    value.can_undo = true; value.modified = true;
                    value.origins = value.origins.map((origin: any) => args.op.kind === 'rotate'
                        ? { ...origin, rotation: (origin.rotation + args.op.degrees) % 360 }
                        : { ...origin, dirty: true });
                    return clone(value);
                }
                if (command === 'pdf_session_save') {
                    const value = sessions.get(args.id);
                    value.source = args.outPath ?? value.source;
                    value.modified = false;
                    value.origins = value.origins.map((origin: any) => ({ ...origin, dirty: true }));
                    return value.source;
                }
                if (command === 'pdf_session_state') return clone(sessions.get(args.id));
                if (command === 'pdf_session_export_temp') {
                    const value = sessions.get(args.id);
                    const path = `/fixtures/snapshots/${nextSnapshot++}/${value.source.split('/').pop()}`;
                    root.__pdfSnapshots.push({ path, session: clone(value) });
                    await wait('preview');
                    return path;
                }
                if (command === 'pdf_read_form_fields') {
                    if (root.__pdfFormFailure) throw new Error('Private form /private/password=secret could not be read');
                    return [];
                }
                if (command === 'pdf_export_text' || command === 'pdf_export_docx') return 2;
                if (command === 'platform_capabilities') return { system_share_sheet: false, direct_print: false, platform: 'linux' };
                if (command === 'convert_capabilities') {
                    await wait(`caps:${args.path}`);
                    const ext = String(args.path ?? '').split('.').pop();
                    return { ext, native_reader: ext === 'pptx', anydoc_available: true, convertible: true, rich_output: ext === 'pptx', slide_options: ext === 'pptx', extensions: ['pptx', 'doc'] };
                }
                if (command === 'convert_document') {
                    await wait('convert');
                    return { content: 'OLD CONVERSION CONTENT', headings: [], slides: 1, engine_used: 'native', written_path: args.outPath };
                }
                if (command === 'docx_check') {
                    await wait('docx');
                    return { ok: [], issues: ['OLD DOCUMENT ISSUE'], valid: false };
                }
                return original(command, args, options);
            };
            return bridge;
        };
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', { configurable: true, get: () => bridge, set: value => { bridge = wrap(value); } });
    });
    await page.goto('/');
    await page.locator('nav.main-nav').getByRole('button', { name: 'PDF Tools', exact: true }).click();
    await expect(page.locator('.pe')).toBeVisible();
    await expect.poll(() => page.evaluate(() => (window as any).__pdfCalls.some((c: any) => c.command === 'plugin:event|listen' && c.args.event === 'tauri://drag-drop' && c.args.target?.kind === 'Webview'))).toBe(true);
}

async function calls(page: Page, command: string) {
    return page.evaluate(name => (window as any).__pdfCalls.filter((entry: any) => entry.command === name), command);
}
async function openPdf(page: Page) {
    await page.locator('.pe-toolbar').getByRole('button', { name: 'Open PDF', exact: true }).click();
    await expect(page.locator('.pe-tile')).toHaveCount(2);
    await expect(page.locator('.pe-busy-chip')).toHaveCount(0);
}
async function drop(page: Page, path: string) {
    await page.evaluate(value => (window as any).__pdfDrop([value]), path);
}
async function release(page: Page, key: string) {
    await expect.poll(() => page.evaluate(value => typeof (window as any).__pdfReleases[value], key)).toBe('function');
    await page.evaluate(value => (window as any).__pdfReleases[value](), key);
}

test('consolidated PDF ownership disposes superseded opens and ignores hidden-view shortcuts/drop', async ({ page }) => {
    await pdfFixture(page);
    await page.evaluate(() => { (window as any).__pdfHold['open:/fixtures/first.pdf'] = true; });
    await drop(page, '/fixtures/first.pdf');
    await expect.poll(() => calls(page, 'pdf_session_open')).toHaveLength(1);
    await drop(page, '/fixtures/second.pdf');
    await release(page, 'open:/fixtures/first.pdf');
    await expect(page.locator('.pe-tile')).toHaveCount(1);
    expect((await calls(page, 'pdf_session_close')).map((entry: any) => entry.args.id)).toContain('pdf-1');
    expect((await calls(page, 'pdf_session_open')).map((entry: any) => entry.args.path)).toEqual(['/fixtures/first.pdf', '/fixtures/second.pdf']);
    await page.locator('nav.main-nav').getByRole('button', { name: 'Inbox', exact: true }).click();
    await page.keyboard.press('Delete');
    await drop(page, '/fixtures/hidden.pdf');
    await page.locator('nav.main-nav').getByRole('button', { name: 'PDF Tools', exact: true }).click();
    await expect(page.locator('.pe-tile')).toHaveCount(1);
    expect(await calls(page, 'pdf_session_apply')).toHaveLength(0);
    expect(await calls(page, 'pdf_session_open')).toHaveLength(2);
});

test('a queued document handoff waits for edits and busy shortcuts cannot issue another mutation', async ({ page }) => {
    await pdfFixture(page); await openPdf(page);
    await page.evaluate(() => { (window as any).__pdfHold.apply = true; });
    await page.getByRole('button', { name: 'Rotate right', exact: true }).click();
    await expect.poll(() => calls(page, 'pdf_session_apply')).toHaveLength(1);
    await page.keyboard.press('Control+s');
    await page.keyboard.press('Delete');
    await drop(page, '/fixtures/second.pdf');
    expect(await calls(page, 'pdf_session_save')).toHaveLength(0);
    expect(await calls(page, 'pdf_session_apply')).toHaveLength(1);
    expect(await calls(page, 'pdf_session_open')).toHaveLength(1);
    await release(page, 'apply');
    await expect(page.locator('.pe-tile')).toHaveCount(1);
    expect(await calls(page, 'plugin:dialog|message')).toHaveLength(1);
});

test('form failure makes one attempt, permits an explicit retry, and logs safely at silent verbosity', async ({ page }) => {
    const logs: string[] = [];
    page.on('console', message => logs.push(message.text()));
    await pdfFixture(page); await openPdf(page);
    await page.evaluate(async () => {
        (window as any).__pdfFormFailure = true;
        const path = '/src/lib/log.ts';
        const { setLogVerbosity } = await import(/* @vite-ignore */ path);
        setLogVerbosity('silent');
    });
    await page.locator('.pe-toolbar').getByRole('button', { name: 'Tools', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Form fields', exact: true }).click();
    await expect(page.locator('.pe-err')).toContainText('could not be read');
    await expect(page.getByRole('button', { name: 'Check again', exact: true })).toBeEnabled();
    await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
    expect(await calls(page, 'pdf_read_form_fields')).toHaveLength(1);
    await page.getByRole('button', { name: 'Check again', exact: true }).click();
    await expect.poll(() => calls(page, 'pdf_read_form_fields')).toHaveLength(2);
    await expect(page.getByRole('button', { name: 'Check again', exact: true })).toBeEnabled();
    const failures = logs.filter(message => message.includes('pdf.form phase=failed'));
    expect(failures).toHaveLength(2);
    expect(failures.join('\n')).not.toMatch(/Private|\/private|password|secret/);
});

test('edited previews are shared and Save refreshes them before text-layer export', async ({ page }) => {
    await pdfFixture(page); await openPdf(page);
    await expect(page.locator('.pe-tile img')).toHaveCount(2);
    await page.evaluate(() => { (window as any).__pdfHold.preview = true; });
    await page.locator('.pe-toolbar').getByRole('button', { name: 'Tools', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Watermark', exact: true }).click();
    await page.locator('.dt-bar').getByRole('button', { name: 'Apply', exact: true }).click();
    await expect.poll(() => calls(page, 'pdf_session_export_temp')).toHaveLength(1);
    await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
    expect(await calls(page, 'pdf_session_export_temp')).toHaveLength(1);
    await release(page, 'preview');
    await expect.poll(() => page.evaluate(() => (window as any).__pdfCalls.filter((entry: any) => entry.command === 'plugin:fs|read_file' && entry.args.path.includes('/snapshots/')).length)).toBe(1);
    await page.locator('.pe-toolbar').getByRole('button', { name: 'Save as…', exact: true }).click();
    await expect.poll(() => calls(page, 'pdf_session_state')).toHaveLength(1);
    await expect.poll(() => calls(page, 'pdf_session_export_temp')).toHaveLength(2);
    const saved = await page.evaluate(() => (window as any).__pdfSnapshots.at(-1));
    expect(saved.session.source).toBe('/fixtures/saved.pdf');
    expect(saved.session.modified).toBe(false);
    expect(saved.session.ops).toEqual(['watermark']);
    await page.evaluate(() => { (window as any).__pdfSavePath = '/fixtures/export.txt'; });
    await page.locator('.pe-toolbar').getByRole('button', { name: 'Export', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Plain text (.txt)…', exact: true }).click();
    await expect.poll(() => calls(page, 'pdf_export_text')).toHaveLength(1);
    const exported = (await calls(page, 'pdf_export_text'))[0].args;
    expect(exported).toMatchObject({ outPath: '/fixtures/export.txt', format: 'text' });
    expect(exported.path).toMatch(/^\/fixtures\/snapshots\/\d+\/saved\.pdf$/);
    expect(await calls(page, 'ocr_extract_image')).toHaveLength(0);
});

test('malformed insertion ranges cannot silently turn into all-pages insertion', async ({ page }) => {
    await pdfFixture(page); await openPdf(page);
    await page.getByRole('button', { name: 'Insert pages from another PDF', exact: true }).click();
    await page.locator('.dt-bar').getByRole('button', { name: 'Choose PDF…', exact: true }).click();
    await page.locator('.dt-bar input[type=text]').fill('1junk');
    await page.locator('.dt-bar').getByRole('button', { name: 'Apply', exact: true }).click();
    await expect(page.locator('.pe-err')).toContainText('Enter ranges');
    expect(await calls(page, 'pdf_session_apply')).toHaveLength(0);
});

test('DOCX and converter handoffs cannot publish results from the previous file', async ({ page }) => {
    await pdfFixture(page);
    await drop(page, '/fixtures/old.docx');
    await page.evaluate(() => { (window as any).__pdfHold.docx = true; });
    await page.locator('.pe-drawer').getByRole('button', { name: 'Validate', exact: true }).click();
    await expect.poll(() => calls(page, 'docx_check')).toHaveLength(1);
    await drop(page, '/fixtures/new.docx');
    await release(page, 'docx');
    await expect(page.locator('.pe-drawer .dt-file')).toContainText('new.docx');
    await expect(page.locator('.pe-drawer')).not.toContainText('OLD DOCUMENT ISSUE');
    await page.evaluate(() => { (window as any).__pdfHold['save-dialog'] = true; });
    await page.locator('.pe-drawer').getByRole('button', { name: 'Strip revision IDs', exact: true }).click();
    await expect.poll(() => page.evaluate(() => typeof (window as any).__pdfReleases['save-dialog'])).toBe('function');
    await drop(page, '/fixtures/replacement.docx');
    await release(page, 'save-dialog');
    await expect(page.locator('.pe-drawer .dt-file')).toContainText('replacement.docx');
    expect(await calls(page, 'docx_strip_rsids')).toHaveLength(0);
    await page.evaluate(() => { (window as any).__pdfHold['caps:/fixtures/old.doc'] = true; });
    await drop(page, '/fixtures/old.doc');
    await expect.poll(() => page.evaluate(() => typeof (window as any).__pdfReleases['caps:/fixtures/old.doc'])).toBe('function');
    await drop(page, '/fixtures/new.pptx');
    await release(page, 'caps:/fixtures/old.doc');
    await expect(page.locator('.pe-drawer .ct-path')).toContainText('new.pptx');
    await expect(page.locator('.pe-drawer option[value=docx]')).toBeEnabled();
    await page.evaluate(() => { (window as any).__pdfHold.convert = true; });
    await page.locator('.pe-drawer').getByRole('button', { name: 'Preview', exact: true }).click();
    await expect.poll(() => calls(page, 'convert_document')).toHaveLength(1);
    await drop(page, '/fixtures/final.pptx');
    await release(page, 'convert');
    await expect(page.locator('.pe-drawer .ct-path')).toContainText('final.pptx');
    await expect(page.locator('.pe-drawer')).not.toContainText('OLD CONVERSION CONTENT');
});
