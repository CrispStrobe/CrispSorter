import type { ContextPanel } from './panels';

type Listener = (panel: ContextPanel) => void;
const listeners = new Set<Listener>();

export function requestBrowserContext(panel: ContextPanel): void {
    for (const listener of listeners) listener(panel);
}

export function subscribeBrowserContext(listener: Listener): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
}
