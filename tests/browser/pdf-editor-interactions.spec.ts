// The unified PDF editor: one surface for page editing, tools, annotation
// and export. The edit-session backend is mocked here (its behaviour is
// covered by the Rust tests in pdf_session.rs); pdf.js renders a real
// generated PDF so thumbnails, zoom and coordinates are exercised for real.
import { expect, test, type Page } from '@playwright/test';
import { fixtures } from './fixtures';

const SHOTS = process.env.PDF_SHOTS_DIR;

/** A small, valid PDF with one labelled page per entry (sizes in points). */
function buildPdf(sizes: [number, number][]): number[] {
    const objs: string[] = [];
    const n = sizes.length;
    // 1 catalog, 2 pages, 3 font, then per page: page obj + content obj
    objs[1] = '<< /Type /Catalog /Pages 2 0 R >>';
    const kids = sizes.map((_, i) => `${4 + i * 2} 0 R`).join(' ');
    objs[2] = `<< /Type /Pages /Kids [${kids}] /Count ${n} >>`;
    objs[3] = '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>';
    sizes.forEach(([w, h], i) => {
        const content = `BT /F1 36 Tf 40 ${h - 80} Td (Page ${i + 1}) Tj ET 0 0 1 RG 4 w 20 20 ${w - 40} ${h - 40} re S`;
        objs[4 + i * 2] = `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${w} ${h}] /Resources << /Font << /F1 3 0 R >> >> /Contents ${5 + i * 2} 0 R >>`;
        objs[5 + i * 2] = `<< /Length ${content.length} >>\nstream\n${content}\nendstream`;
    });
    let out = '%PDF-1.4\n';
    const offsets: number[] = [];
    for (let k = 1; k < objs.length; k++) {
        offsets[k] = out.length;
        out += `${k} 0 obj\n${objs[k]}\nendobj\n`;
    }
    const xref = out.length;
    out += `xref\n0 ${objs.length}\n0000000000 65535 f \n`;
    for (let k = 1; k < objs.length; k++) out += `${String(offsets[k]).padStart(10, '0')} 00000 n \n`;
    out += `trailer\n<< /Size ${objs.length} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
    return Array.from(out, (c) => c.charCodeAt(0));
}

async function pdfFixtures(page: Page) {
    await fixtures(page);
    const bytes = buildPdf([[300, 400], [300, 400], [400, 300]]);
    await page.addInitScript((pdfBytes: number[]) => {
        const root = window as any;
        const callbacks = new Map<number, Function>();
        root.__cbs = callbacks;
        root.__listeners = [] as { event: string; handler: number }[];
        root.__pdfFail = {} as Record<string, string>;

        // Minimal edit session: a stack of page-origin snapshots.
        let history: any[][] = [];
        let cursor = 0;
        let source = '';
        const page = (o: any) => ({ width_pt: 300, height_pt: 400, rotation: o.rotation });
        const state = () => {
            const origins = history[cursor];
            return {
                id: 'session-1', source,
                info: { page_count: origins.length, pages: origins.map((o, i) => ({ page_number: i + 1, ...page(o) })) },
                origins, can_undo: cursor > 0, can_redo: cursor < history.length - 1,
                undo_label: null, modified: cursor > 0, ops: [],
            };
        };
        const apply = (op: any) => {
            const cur = history[cursor].map((o) => ({ ...o }));
            let next = cur;
            switch (op.kind) {
                case 'reorder': next = op.order.map((i: number) => cur[i]); break;
                case 'remove_pages': next = cur.filter((_, i) => !op.pages.includes(i)); break;
                case 'rotate':
                    // The pre-fix backend's exact rule: a raw -90 used to fail here.
                    if (![0, 90, 180, 270].includes(op.degrees)) throw new Error(`degrees must be 0/90/180/270, got ${op.degrees}`);
                    for (const i of op.pages) cur[i].rotation = (cur[i].rotation + op.degrees) % 360;
                    break;
                case 'insert_blank': next.splice(op.position, 0, { source: null, index: null, rotation: 0, dirty: false }); break;
                case 'crop': for (const i of op.pages) cur[i].dirty = true; break;
                case 'annotate': for (const a of op.annotations) cur[a.page].dirty = true; break;
                case 'erase_annotation': cur[op.page].dirty = true; break;
                default: for (const o of cur) o.dirty = true;
            }
            history = [...history.slice(0, cursor + 1), next];
            cursor += 1;
        };

        // Playwright does not guarantee the ordering of separate init scripts.
        // Retain the fixture whether the shared IPC bridge arrives first or last.
        const wrapped = new WeakSet<object>();
        const wrap = (internals: any) => {
            if (!internals || wrapped.has(internals)) return internals;
            wrapped.add(internals);
            const transform = internals.transformCallback;
            internals.transformCallback = (fn: Function, once?: boolean) => {
                const id = transform(fn, once);
                callbacks.set(id, fn);
                return id;
            };
            const original = internals.invoke;
            internals.invoke = async (command: string, args: any = {}, options?: any) => {
                if (root.__pdfFail[command]) {
                    root.__fixtureCalls.push({ command, args });
                    throw new Error(root.__pdfFail[command]);
                }
                if (command === 'plugin:event|listen') {
                    root.__listeners.push({ event: args.event, handler: args.handler });
                    return original(command, args);
                }
                if ((command === 'plugin:fs|read_file') && /\.pdf$/i.test(args.path ?? '')) {
                    root.__fixtureCalls.push({ command, args });
                    return pdfBytes;
                }
                if (!command.startsWith('pdf_') && command !== 'platform_capabilities') return original(command, args, options);
                root.__fixtureCalls.push({ command, args });
                switch (command) {
                    case 'platform_capabilities': return { system_share_sheet: false, direct_print: false, platform: 'linux' };
                    case 'pdf_session_open':
                        source = args.path;
                        history = [[0, 1, 2].map((i) => ({ source, index: i, rotation: 0, dirty: false }))];
                        cursor = 0;
                        return state();
                    case 'pdf_session_apply': apply(args.op); return state();
                    case 'pdf_session_undo': cursor = Math.max(0, cursor - 1); return state();
                    case 'pdf_session_redo': cursor = Math.min(history.length - 1, cursor + 1); return state();
                    case 'pdf_session_state': return state();
                    case 'pdf_session_save': return args.outPath ?? source;
                    case 'pdf_session_export_temp': return '/fixtures/edited.pdf';
                    case 'pdf_session_close': return null;
                    case 'pdf_is_encrypted': return false;
                    case 'pdf_measure_text_region': return { lines: 1, lines_dropped: 0, overflow: false, unsupported_chars: [] };
                    default: return null;
                }
            };
            return internals;
        };
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', {
            configurable: true, get: () => bridge, set: value => { bridge = wrap(value); },
        });
    }, bytes);
}

const calls = (page: Page, command: string) =>
    page.evaluate((c) => (window as any).__fixtureCalls.filter((x: any) => x.command === c).map((x: any) => x.args), command);

async function openEditor(page: Page) {
    await page.goto('/');
    await page.locator('.main-nav').getByRole('button', { name: 'PDF Tools', exact: true }).click();
    await page.getByRole('toolbar', { name: 'PDF' }).getByRole('button', { name: 'Open PDF', exact: true }).click();
    await expect(page.locator('.pe-tile')).toHaveCount(3);
    await expect(page.locator('.pe-tile img')).toHaveCount(3);
    await expect(page.getByTestId('pdf-zoom')).not.toHaveText('100%');
}

const tile = (page: Page, i: number) => page.locator(`.pe-tile[data-index="${i}"]`);

test.describe('PDF editor', () => {
    test.beforeEach(async ({ page }) => {
        await page.setViewportSize({ width: 1440, height: 900 });
        await pdfFixtures(page);
    });

    test('one editor: pages panel, page view, toolbar, and every former mode reachable', async ({ page }) => {
        await openEditor(page);
        // No more mode tabs.
        await expect(page.getByRole('button', { name: 'Edit pages' })).toHaveCount(0);
        await expect(page.locator('.pw-modes')).toHaveCount(0);
        await expect(page.locator('.pe-side')).toBeVisible();
        await expect(page.locator('.pe-view canvas')).toBeVisible();
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/editor-layout.png` });

        // Former "Werkzeuge": in the Tools menu.
        const toolbar = page.getByRole('toolbar', { name: 'PDF' });
        await toolbar.getByRole('button', { name: 'Tools' }).click();
        const tools = page.getByRole('menu', { name: 'Tools' });
        for (const name of ['Merge — append PDFs…', 'Split into files', 'Watermark', 'Document properties', 'Protect with a password…', 'Remove hidden data…', 'Reduce file size']) {
            await expect(tools.getByRole('menuitem', { name })).toBeVisible();
        }
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/tools-menu.png` });
        await tools.getByRole('menuitem', { name: 'Split into files' }).click();
        await expect(page.getByPlaceholder('1-5,6-10')).toBeVisible();

        // Former "DOCX" and "Konvertieren": in the Export menu.
        await toolbar.getByRole('button', { name: 'Export' }).click();
        const exp = page.getByRole('menu', { name: 'Export' });
        await expect(exp.getByRole('menuitem', { name: 'Word document (.docx)…' })).toBeVisible();
        await expect(exp.getByRole('menuitem', { name: 'Pages as PNG images…' })).toBeVisible();
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/export-menu.png` });
        await exp.getByRole('menuitem', { name: 'Word document tools…' }).click();
        const drawer = page.getByRole('dialog', { name: 'Word document tools…' });
        await expect(drawer).toBeVisible();
        await expect(drawer.locator('.dt-toolbar')).toBeVisible();
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/docx-drawer.png` });
        await toolbar.getByRole('button', { name: 'Export' }).click();
        await page.getByRole('menu', { name: 'Export' }).getByRole('menuitem', { name: 'Convert another document (Office, e-book)…' }).click();
        const conv = page.getByRole('dialog', { name: 'Convert another document (Office, e-book)…' });
        await expect(conv.getByRole('button', { name: 'Choose document…' })).toBeVisible();
    });

    test('rotating counter-clockwise sends a valid quarter turn and succeeds', async ({ page }) => {
        const logs: string[] = [];
        page.on('console', (m) => logs.push(m.text()));
        await openEditor(page);
        await tile(page, 1).click();
        await page.getByRole('toolbar', { name: 'PDF' }).getByRole('button', { name: 'Rotate left', exact: true }).click();
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(1);
        const [{ op }] = await calls(page, 'pdf_session_apply');
        expect(op).toEqual({ kind: 'rotate', pages: [1], degrees: 270 });
        await expect(page.getByRole('alert')).toHaveCount(0);
        // Every step lands in the app log.
        await expect.poll(() => logs.some((l) => l.includes('pdf.edit phase=completed'))).toBe(true);
        expect(logs.some((l) => l.includes('pdf.open phase=completed'))).toBe(true);
    });

    test('an error can be dismissed and clears on the next successful action', async ({ page }) => {
        const logs: string[] = [];
        page.on('console', (m) => logs.push(m.text()));
        await openEditor(page);
        await page.evaluate(() => { (window as any).__pdfFail.pdf_session_apply = 'backend said no'; });
        const rotate = page.getByRole('toolbar', { name: 'PDF' }).getByRole('button', { name: 'Rotate right', exact: true });
        await rotate.click();
        const alert = page.getByRole('alert');
        await expect(alert).toContainText('backend said no');
        expect(logs.some((l) => l.includes('pdf.edit phase=failed'))).toBe(true);
        expect(logs.filter((l) => l.includes('pdf.edit ')).join('\n')).not.toContain('backend said no');
        await alert.getByRole('button', { name: 'Dismiss' }).click();
        await expect(alert).toHaveCount(0);

        await rotate.click();
        await expect(page.getByRole('alert')).toContainText('backend said no');
        await page.evaluate(() => { delete (window as any).__pdfFail.pdf_session_apply; });
        await rotate.click();
        await expect(page.getByRole('alert')).toHaveCount(0);
    });

    test('select all and deselect', async ({ page }) => {
        await openEditor(page);
        const selectedTiles = page.locator('.pe-tile[aria-selected="true"]');
        await page.getByRole('button', { name: 'Select all' }).click();
        await expect(selectedTiles).toHaveCount(3);
        await page.getByRole('button', { name: 'Select none' }).click();
        await expect(selectedTiles).toHaveCount(0);
        await tile(page, 2).click();
        await expect(selectedTiles).toHaveCount(1);
        await page.keyboard.press('Escape');
        await expect(selectedTiles).toHaveCount(0);
    });

    test('pages reorder with the per-tile < > buttons and by dragging', async ({ page }) => {
        await openEditor(page);
        await tile(page, 0).hover();
        await tile(page, 0).getByRole('button', { name: 'Move later (Page 1)' }).click();
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(1);
        expect((await calls(page, 'pdf_session_apply'))[0].op).toEqual({ kind: 'reorder', order: [1, 0, 2] });

        await tile(page, 2).hover();
        await tile(page, 2).getByRole('button', { name: 'Move earlier (Page 3)' }).click();
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(2);
        expect((await calls(page, 'pdf_session_apply'))[1].op).toEqual({ kind: 'reorder', order: [0, 2, 1] });

        // Pointer drag: drop the last page onto the top half of the first.
        const from = await tile(page, 2).boundingBox();
        const to = await tile(page, 0).boundingBox();
        await page.mouse.move(from!.x + from!.width / 2, from!.y + from!.height / 2);
        await page.mouse.down();
        await page.mouse.move(from!.x + from!.width / 2, from!.y + from!.height / 2 - 20, { steps: 3 });
        await page.mouse.move(to!.x + to!.width / 2, to!.y + 10, { steps: 8 });
        await expect(tile(page, 0)).toHaveClass(/drop-before/);
        await page.mouse.up();
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(3);
        expect((await calls(page, 'pdf_session_apply'))[2].op).toEqual({ kind: 'reorder', order: [2, 0, 1] });
    });

    test('thumbnail size and page panel width are adjustable and remembered', async ({ page }) => {
        await openEditor(page);
        const before = (await tile(page, 0).boundingBox())!.width;
        await page.getByRole('slider', { name: 'Thumbnail size' }).fill('260');
        await expect.poll(async () => (await tile(page, 0).boundingBox())!.width).toBeGreaterThan(before + 60);
        const divider = page.getByRole('separator', { name: 'Resize the page panel' });
        const d = (await divider.boundingBox())!;
        await page.mouse.move(d.x + 3, d.y + 100);
        await page.mouse.down();
        await page.mouse.move(d.x + 300, d.y + 100, { steps: 5 });
        await page.mouse.up();
        const width = (await page.locator('.pe-side').boundingBox())!.width;
        expect(width).toBeGreaterThan(400);
        const stored = await page.evaluate(() => [localStorage.getItem('crispsorter.pdfEditor.thumbSize'), localStorage.getItem('crispsorter.pdfEditor.sidebarWidth')]);
        expect(stored[0]).toBe('260');
        expect(Number(stored[1])).toBeGreaterThan(400);
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/wide-page-panel.png` });
    });

    test('a file dropped on the editor opens', async ({ page }) => {
        await page.goto('/');
        await page.locator('.main-nav').getByRole('button', { name: 'PDF Tools', exact: true }).click();
        await expect(page.getByText('Drop a PDF here to open it')).toBeVisible();
        await expect.poll(() => page.evaluate(() => (window as any).__listeners.some((l: any) => l.event === 'tauri://drag-drop'))).toBe(true);
        await page.evaluate(() => {
            const root = window as any;
            for (const l of root.__listeners.filter((x: any) => x.event === 'tauri://drag-drop')) {
                root.__cbs.get(l.handler)?.({ event: l.event, id: 1, payload: { paths: ['/fixtures/dropped.pdf'], position: { x: 400, y: 300 } } });
            }
        });
        await expect(page.locator('.pe-tile')).toHaveCount(3);
        expect((await calls(page, 'pdf_session_open'))[0]).toEqual({ path: '/fixtures/dropped.pdf' });
    });

    test('crop opens with the whole page in view and the page view zooms', async ({ page }) => {
        await openEditor(page);
        const toolbar = page.getByRole('toolbar', { name: 'PDF' });
        await page.getByRole('toolbar', { name: 'View' }).getByRole('button', { name: 'Zoom in (Ctrl +)' }).click();
        await page.getByRole('toolbar', { name: 'View' }).getByRole('button', { name: 'Zoom in (Ctrl +)' }).click();
        await page.getByRole('toolbar', { name: 'View' }).getByRole('button', { name: 'Zoom in (Ctrl +)' }).click();
        await page.getByRole('toolbar', { name: 'View' }).getByRole('button', { name: 'Zoom in (Ctrl +)' }).click();
        await page.getByRole('toolbar', { name: 'View' }).getByRole('button', { name: 'Zoom in (Ctrl +)' }).click();
        const zoomed = parseInt(await page.getByTestId('pdf-zoom').innerText(), 10);

        await toolbar.getByRole('button', { name: 'Crop', exact: true }).click();
        await expect.poll(async () => parseInt(await page.getByTestId('pdf-zoom').innerText(), 10)).toBeLessThan(zoomed);
        const view = (await page.locator('.pe-view').boundingBox())!;
        const pg = (await page.locator('.pe-page').boundingBox())!;
        expect(pg.y).toBeGreaterThanOrEqual(view.y);
        expect(pg.y + pg.height).toBeLessThanOrEqual(view.y + view.height + 1);
        // Drag a crop rectangle inside the page; the numbers follow it.
        await page.mouse.move(pg.x + pg.width * 0.25, pg.y + pg.height * 0.25);
        await page.mouse.down();
        await page.mouse.move(pg.x + pg.width * 0.75, pg.y + pg.height * 0.75, { steps: 5 });
        await page.mouse.up();
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/crop-fit-page.png` });
        await page.getByRole('button', { name: 'Apply', exact: true }).click();
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(1);
        const { op } = (await calls(page, 'pdf_session_apply'))[0];
        expect(op.kind).toBe('crop');
        expect(op.pages).toEqual([0]);
        expect(op.x).toBeGreaterThan(60); expect(op.x).toBeLessThan(90);
        expect(op.w).toBeGreaterThan(135); expect(op.w).toBeLessThan(165);
        expect(op.y).toBeGreaterThan(85); expect(op.y).toBeLessThan(115);
        // The cropped page is re-rendered from the edited document.
        await expect.poll(async () => (await calls(page, 'pdf_session_export_temp')).length).toBeGreaterThan(0);

        await page.keyboard.press('Control+0');
        await page.locator('.pe-view').hover();
        const fit = parseInt(await page.getByTestId('pdf-zoom').innerText(), 10);
        await page.mouse.wheel(0, -200);
        await page.keyboard.down('Control');
        await page.mouse.wheel(0, -100);
        await page.keyboard.up('Control');
        await expect.poll(async () => parseInt(await page.getByTestId('pdf-zoom').innerText(), 10)).toBeGreaterThan(fit);
    });

    test('annotation tools draw into the edit session', async ({ page }) => {
        await openEditor(page);
        const toolbar = page.getByRole('toolbar', { name: 'PDF' });
        const pg = async () => (await page.locator('.pe-page').boundingBox())!;

        await toolbar.getByRole('button', { name: 'Highlight', exact: true }).click();
        let b = await pg();
        await page.mouse.move(b.x + 40, b.y + 40);
        await page.mouse.down();
        await page.mouse.move(b.x + 160, b.y + 70, { steps: 4 });
        await page.mouse.up();
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(1);
        let a = (await calls(page, 'pdf_session_apply'))[0].op.annotations[0];
        expect(a).toMatchObject({ page: 0, ann_type: 'highlight', color: '#facc15' });
        expect(a.w).toBeGreaterThan(10);

        await toolbar.getByRole('button', { name: 'Freehand ink', exact: true }).click();
        b = await pg();
        await page.mouse.move(b.x + 50, b.y + 150);
        await page.mouse.down();
        for (let k = 1; k <= 6; k++) await page.mouse.move(b.x + 50 + k * 15, b.y + 150 + (k % 2) * 20);
        await page.mouse.up();
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(2);
        a = (await calls(page, 'pdf_session_apply'))[1].op.annotations[0];
        expect(a.ann_type).toBe('ink');
        expect(a.ink[0].length).toBeGreaterThanOrEqual(8);

        await toolbar.getByRole('button', { name: 'Sticky note', exact: true }).click();
        b = await pg();
        await page.mouse.click(b.x + 100, b.y + 100);
        await page.getByPlaceholder('Annotation text…').fill('Check this');
        await page.getByRole('button', { name: 'Add', exact: true }).click();
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(3);
        a = (await calls(page, 'pdf_session_apply'))[2].op.annotations[0];
        expect(a).toMatchObject({ ann_type: 'note', text: 'Check this', w: 20, h: 20 });

        await toolbar.getByRole('button', { name: 'Eraser — click an annotation to remove it', exact: true }).click();
        b = await pg();
        await page.mouse.click(b.x + 60, b.y + 50);
        await expect.poll(async () => (await calls(page, 'pdf_session_apply')).length).toBe(4);
        expect((await calls(page, 'pdf_session_apply'))[3].op.kind).toBe('erase_annotation');
        if (SHOTS) await page.screenshot({ path: `${SHOTS}/annotate.png` });
    });
});
