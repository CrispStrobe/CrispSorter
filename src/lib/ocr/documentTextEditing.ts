export interface TextSelection { anchor: number; focus: number; }
export interface EditableTextModel { key: string; text: string; disabled: boolean; }
export interface EditableTextOptions extends EditableTextModel {
    caret: number;
    /** Read the current owner before an input event, including before Svelte's
     * queued DOM update. This closes the lock/undo-to-input race. */
    readModel: () => EditableTextModel;
    oninput: (text: string) => void;
    onhistory: (direction: 'undo' | 'redo') => void;
}

function boundary(text: string, offset: number): number {
    const n = Number.isFinite(offset) ? Math.max(0, Math.min(text.length, Math.trunc(offset))) : 0;
    // A caret cannot split a UTF-16 surrogate pair.
    return n > 0 && n < text.length && /[\uD800-\uDBFF]/.test(text[n - 1]) && /[\uDC00-\uDFFF]/.test(text[n]) ? n - 1 : n;
}

/** Keep offsets attached to unchanged surrounding text after external edits.
 * Preserve anchor/focus order, including backward selections. */
export function remapTextSelection(previous: string, next: string, selection: TextSelection): TextSelection {
    let start = 0;
    while (start < previous.length && start < next.length && previous[start] === next[start]) start++;
    start = Math.min(boundary(previous, start), boundary(next, start));
    let oldEnd = previous.length, newEnd = next.length;
    while (oldEnd > start && newEnd > start && previous[oldEnd - 1] === next[newEnd - 1]) { oldEnd--; newEnd--; }
    // Keep a shared low surrogate with its high surrogate in the changed span.
    if (boundary(previous, oldEnd) !== oldEnd || boundary(next, newEnd) !== newEnd) { oldEnd++; newEnd++; }
    const map = (offset: number) => {
        const at = boundary(previous, offset);
        const mapped = at < start ? at : at >= oldEnd ? at + newEnd - oldEnd : start + Math.min(at - start, newEnd - start);
        return boundary(next, mapped);
    };
    return { anchor: map(selection.anchor), focus: map(selection.focus) };
}

export function readTextSelection(element: HTMLElement): TextSelection | null {
    const selection = element.ownerDocument.getSelection();
    if (!selection?.anchorNode || !selection.focusNode || !element.contains(selection.anchorNode) || !element.contains(selection.focusNode)) return null;
    const offset = (node: Node, at: number) => {
        const range = element.ownerDocument.createRange();
        range.setStart(element, 0);
        range.setEnd(node, at);
        return range.toString().length;
    };
    return { anchor: offset(selection.anchorNode, selection.anchorOffset), focus: offset(selection.focusNode, selection.focusOffset) };
}

export function writeTextSelection(element: HTMLElement, value: TextSelection): void {
    const selection = element.ownerDocument.getSelection();
    if (!selection) return;
    const text = element.firstChild;
    const node = text?.nodeType === 3 ? text : element;
    const contents = node === element ? '' : node.textContent ?? '';
    const anchor = boundary(contents, value.anchor), focus = boundary(contents, value.focus);
    if (selection.setBaseAndExtent) selection.setBaseAndExtent(node, anchor, node, focus);
    else {
        selection.removeAllRanges();
        const range = element.ownerDocument.createRange();
        range.setStart(node, anchor);
        range.collapse(true);
        selection.addRange(range);
        selection.extend?.(node, focus);
    }
}

/** A controlled plain-text action. Native typing keeps its DOM/caret; history
 * and other model changes reconcile it without reusing stale editable text. */
export function editableDocumentText(element: HTMLElement, initial: EditableTextOptions) {
    let options = initial;
    let model: EditableTextModel = initial;
    let composing = false;
    let externalDuringComposition = false;

    const reconcile = (next: EditableTextModel, force = false) => {
        const previousKey = model.key;
        model = next;
        element.contentEditable = next.disabled ? 'false' : 'plaintext-only';
        element.setAttribute('aria-disabled', String(next.disabled));
        element.setAttribute('aria-readonly', String(next.disabled));
        const before = element.textContent ?? '';
        if (before !== next.text) {
            if (composing && !force && !next.disabled) { externalDuringComposition = true; return; }
            const selection = element.ownerDocument.activeElement === element ? readTextSelection(element) : null;
            element.textContent = next.text;
            if (selection && !next.disabled) {
                writeTextSelection(element, previousKey === next.key
                    ? remapTextSelection(before, next.text, selection)
                    : { anchor: options.caret, focus: options.caret });
            }
        }
        externalDuringComposition = false;
        if (next.disabled && element.ownerDocument.activeElement === element) element.blur();
    };
    const refresh = () => reconcile(options.readModel());

    const input = () => {
        const latest = options.readModel();
        const changedExternally = latest.key !== model.key || latest.text !== model.text;
        if (latest.disabled || changedExternally) {
            const domText = element.textContent ?? '';
            reconcile(latest, latest.disabled);
            if (latest.disabled || domText !== latest.text) return;
        }
        if (externalDuringComposition) return;
        const text = element.textContent ?? '';
        if (text !== model.text) options.oninput(text);
    };

    const insert = (text: string) => {
        refresh();
        if (model.disabled || composing) return;
        const before = element.textContent ?? '';
        const selection = readTextSelection(element) ?? { anchor: before.length, focus: before.length };
        const start = boundary(before, Math.min(selection.anchor, selection.focus));
        const end = boundary(before, Math.max(selection.anchor, selection.focus));
        const next = before.slice(0, start) + text + before.slice(end);
        element.textContent = next;
        writeTextSelection(element, { anchor: start + text.length, focus: start + text.length });
        if (next !== model.text) options.oninput(next);
    };
    const beforeInput = (event: InputEvent) => {
        refresh();
        if (model.disabled) { event.preventDefault(); return; }
        if (event.inputType === 'historyUndo' || event.inputType === 'historyRedo') {
            event.preventDefault();
            if (!composing) options.onhistory(event.inputType === 'historyUndo' ? 'undo' : 'redo');
        } else if (!event.isComposing && !composing && ['insertParagraph', 'insertLineBreak'].includes(event.inputType)) {
            event.preventDefault();
            insert('\n');
        }
    };
    const keydown = (event: KeyboardEvent) => {
        refresh();
        if (model.disabled) { event.preventDefault(); return; }
        if (event.key === 'Enter' && !event.isComposing && !composing && !event.ctrlKey && !event.metaKey && !event.altKey) {
            event.preventDefault();
            insert('\n');
        }
    };
    const paste = (event: ClipboardEvent) => {
        refresh();
        if (model.disabled) { event.preventDefault(); return; }
        if (!event.clipboardData) return;
        event.preventDefault();
        insert(event.clipboardData.getData('text/plain'));
    };
    const compositionStart = () => { refresh(); composing = true; externalDuringComposition = false; };
    const compositionEnd = () => {
        composing = false;
        if (externalDuringComposition) reconcile(options.readModel(), true);
        else input();
    };

    element.textContent = initial.text;
    reconcile(initial);
    if (!initial.disabled) {
        element.focus();
        writeTextSelection(element, { anchor: initial.caret, focus: initial.caret });
    }
    element.addEventListener('input', input);
    element.addEventListener('beforeinput', beforeInput);
    element.addEventListener('keydown', keydown);
    element.addEventListener('paste', paste);
    element.addEventListener('compositionstart', compositionStart);
    element.addEventListener('compositionend', compositionEnd);
    return {
        update(next: EditableTextOptions) { options = next; reconcile(next); },
        destroy() {
            element.removeEventListener('input', input);
            element.removeEventListener('beforeinput', beforeInput);
            element.removeEventListener('keydown', keydown);
            element.removeEventListener('paste', paste);
            element.removeEventListener('compositionstart', compositionStart);
            element.removeEventListener('compositionend', compositionEnd);
        },
    };
}
