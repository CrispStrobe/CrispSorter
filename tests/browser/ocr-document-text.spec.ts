// Browser integration uses the shipped editor and explicit native fixtures.
// Native inference quality is qualified separately.
import { test, expect, type Locator, type Page } from '@playwright/test';
import { fixtures } from './fixtures';
import type { OcrProject } from '../../src/lib/ocr/model';

const original = 'Warranty expires in November 2031.';

async function savedProject(page: Page): Promise<OcrProject | null> {
    return page.evaluate(async () => {
        const saved = await (window as any).__TAURI_INTERNALS__.invoke('ocr_workspace_load', { projectId: null });
        return saved?.project ?? null;
    });
}

async function openEditor(page: Page) {
    await page.locator('nav.main-nav').getByRole('button', { name: 'OCR', exact: true }).click();
    const workspace = page.getByRole('region', { name: 'OCR document workspace', exact: true });
    const commands = workspace.locator('.command-bar');
    await commands.getByRole('button', { name: 'Add documents', exact: true }).click();
    await expect(workspace.locator('.page-card')).toHaveCount(2);
    await commands.getByRole('button', { name: 'Recognize page', exact: true }).click();
    await expect(workspace.locator('.queue-bar')).toContainText('1 / 1 pages');
    await workspace.getByRole('tab', { name: 'Review', exact: true }).click();
    await workspace.getByRole('button', { name: 'Document', exact: true }).click();
    const pageText = workspace.getByRole('region', { name: 'Page text', exact: true });
    await pageText.getByRole('button', { name: /^Edit passage 1:/ }).click();
    const editor = pageText.getByRole('textbox', { name: 'Text of passage 1', exact: true });
    await expect(editor).toBeFocused();
    return { workspace, commands, pageText, editor };
}

async function caret(editor: Locator, at: number) {
    await editor.evaluate((element, offset) => {
        const node = element.firstChild;
        if (!node || node.nodeType !== Node.TEXT_NODE) throw new Error('Expected a plain-text editor');
        element.ownerDocument.getSelection()!.setBaseAndExtent(node, offset, node, offset);
    }, at);
}

async function selection(editor: Locator) {
    return editor.evaluate(element => {
        const value = element.ownerDocument.getSelection()!;
        return { anchor: value.anchorOffset, focus: value.focusOffset, inside: element.contains(value.anchorNode) && element.contains(value.focusNode) };
    });
}

test.beforeEach(async ({ page }) => {
    await fixtures(page);
    // Wrap either init-script order; the active-lock test defers only its
    // second import, after the editor is already mounted and focused.
    await page.addInitScript(() => {
        const root = window as any;
        const wrapped = new WeakSet<object>();
        const wrap = (value: any) => {
            if (!value || wrapped.has(value)) return value;
            wrapped.add(value);
            const invoke = value.invoke.bind(value);
            value.invoke = async (command: string, args: any = {}, options?: any) => {
                if (command === 'plugin:dialog|open' && root.__documentOpenPath) return root.__documentOpenPath;
                if (command === 'ocr_workspace_import' && root.__holdDocumentImport) {
                    root.__holdDocumentImport = false;
                    await new Promise<void>(resolve => { root.__releaseDocumentImport = resolve; });
                }
                return invoke(command, args, options);
            };
            return value;
        };
        let bridge = wrap(root.__TAURI_INTERNALS__);
        Object.defineProperty(root, '__TAURI_INTERNALS__', {
            configurable: true,
            get: () => bridge,
            set: value => { bridge = wrap(value); },
        });
    });
    await page.goto('/');
    await expect(page.locator('nav.main-nav')).toBeAttached({ timeout: 30000 });
});

test('document typing uses project undo and redo without restoring stale text or moving the caret', async ({ page }) => {
    const { editor } = await openEditor(page);
    await caret(editor, 8);
    await page.keyboard.insertText(' updated');
    await expect(editor).toHaveText('Warranty updated expires in November 2031.');
    expect(await selection(editor)).toEqual({ anchor: 16, focus: 16, inside: true });

    await editor.press('Control+z');
    await expect(editor).toHaveText(original);
    await expect(editor).toBeFocused();
    expect(await selection(editor)).toEqual({ anchor: 8, focus: 8, inside: true });
    await expect.poll(async () => (await savedProject(page))?.pages[0].regions[0]).toMatchObject({
        text: original, originalText: original, confidence: 0.2, review: 'unreviewed',
    });

    await editor.press('Control+y');
    await expect(editor).toHaveText('Warranty updated expires in November 2031.');
    expect(await selection(editor)).toEqual({ anchor: 16, focus: 16, inside: true });
    await editor.press('Control+z');
    await expect(editor).toHaveText(original);
    await page.keyboard.insertText(' renewed');
    await expect(editor).toHaveText('Warranty renewed expires in November 2031.');
    await expect.poll(async () => (await savedProject(page))?.pages[0].regions[0]).toMatchObject({
        text: 'Warranty renewed expires in November 2031.', originalText: original,
        confidence: 0.2, review: 'corrected',
    });
});

test('locking an active document editor rejects late input and preserves its saved correction', async ({ page }) => {
    const { workspace, commands, pageText, editor } = await openEditor(page);
    await editor.fill('Reviewed before import.');
    await expect.poll(async () => (await savedProject(page))?.pages[0].regions[0].text).toBe('Reviewed before import.');
    await editor.evaluate(element => { (window as any).__lockedDocumentEditor = element; });
    await page.evaluate(() => {
        (window as any).__documentOpenPath = '/fixtures/locked.pdf';
        (window as any).__holdDocumentImport = true;
    });
    // Programmatic click deliberately retains focus until the operation locks
    // the editor; a pointer click would blur it before exercising that boundary.
    await commands.getByRole('button', { name: 'Add documents', exact: true }).evaluate(element => (element as HTMLElement).click());
    await expect.poll(() => page.evaluate(() => typeof (window as any).__releaseDocumentImport)).toBe('function');
    await expect(workspace.locator('[data-document-text][contenteditable="plaintext-only"]')).toHaveCount(0);
    await page.evaluate(() => {
        const element = (window as any).__lockedDocumentEditor as HTMLElement;
        if (element.contentEditable !== 'false') throw new Error('Active editor was not locked');
        element.textContent = 'A late input must not replace the review.';
        element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText' }));
    });
    await expect(pageText).toContainText('Reviewed before import.');
    await page.evaluate(() => (window as any).__releaseDocumentImport());
    await expect(workspace.locator('.page-card')).toHaveCount(4);
    await expect.poll(async () => (await savedProject(page))?.pages[0].regions[0]).toMatchObject({
        text: 'Reviewed before import.', originalText: original, review: 'corrected',
    });
});

test('document line breaks and pasted markup stay literal and menu undo restores project history', async ({ page }) => {
    const { editor } = await openEditor(page);
    await editor.fill('First');
    await caret(editor, 5);
    await editor.press('Enter');
    await page.keyboard.insertText('Second');
    await editor.press('Shift+Enter');
    await editor.evaluate(element => {
        const data = new DataTransfer();
        data.setData('text/plain', '<b>literal</b>\nThird');
        data.setData('text/html', '<b>formatted</b>');
        element.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: data }));
    });
    await expect.poll(() => editor.textContent()).toBe('First\nSecond\n<b>literal</b>\nThird');
    expect(await editor.evaluate(element => element.childElementCount)).toBe(0);
    const prevented = await editor.evaluate(element => {
        const event = new InputEvent('beforeinput', { bubbles: true, cancelable: true, inputType: 'historyUndo' });
        element.dispatchEvent(event);
        return event.defaultPrevented;
    });
    expect(prevented).toBe(true);
    await expect(editor).toHaveText(original);
    await expect.poll(async () => (await savedProject(page))?.pages[0].regions[0]).toMatchObject({
        text: original, originalText: original, confidence: 0.2, review: 'unreviewed',
    });
});

test('the editing action reconciles interrupted composition and a lock before the queued DOM update', async ({ page }) => {
    const result = await page.evaluate(async () => {
        // Import the production action through Vite, without copying its logic
        // into a synthetic test component.
        const modulePath = '/src/lib/ocr/documentTextEditing.ts';
        const { editableDocumentText } = await import(/* @vite-ignore */ modulePath);
        const element = document.createElement('div');
        element.setAttribute('role', 'textbox');
        document.body.append(element);
        let model = { key: 'page:region', text: 'base', disabled: false };
        const changes: string[] = [];
        const history: string[] = [];
        const options = () => ({ ...model, caret: 4, readModel: () => model,
            oninput: (text: string) => { changes.push(text); model = { ...model, text }; },
            onhistory: (direction: string) => history.push(direction),
        });
        const action = editableDocumentText(element, options());
        element.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
        element.textContent = 'base仮';
        element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertCompositionText', isComposing: true }));
        action.update(options());
        model = { ...model, text: 'Restored authoritative text' };
        action.update(options());
        const duringComposition = element.textContent;
        element.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '仮' }));
        const afterComposition = element.textContent;

        // No action.update yet: input must consult the current owner directly.
        model = { ...model, disabled: true };
        const before = new InputEvent('beforeinput', { bubbles: true, cancelable: true, inputType: 'insertText', data: 'late' });
        element.dispatchEvent(before);
        const shortcut = new KeyboardEvent('keydown', { bubbles: true, cancelable: true, key: 'z', ctrlKey: true });
        element.dispatchEvent(shortcut);
        element.textContent = 'late stale mutation';
        element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText' }));
        const receipt = { duringComposition, afterComposition, finalText: element.textContent,
            disabled: element.contentEditable, inputPrevented: before.defaultPrevented,
            shortcutPrevented: shortcut.defaultPrevented, changes, history };
        action.destroy();
        element.remove();
        return receipt;
    });
    expect(result).toEqual({
        duringComposition: 'base仮', afterComposition: 'Restored authoritative text',
        finalText: 'Restored authoritative text', disabled: 'false',
        inputPrevented: true, shortcutPrevented: true, changes: ['base仮'], history: [],
    });
});
