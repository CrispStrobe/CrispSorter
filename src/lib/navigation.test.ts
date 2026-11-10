import { describe, expect, it } from 'vitest';
import { isNavigationShortcut, navigationDestinations, searchNavigation } from './navigation';

const tabs = [
    { id: 'batch' }, { id: 'review' }, { id: 'catalog' }, { id: 'translate' },
    { id: 'ocr', requires: ['build:ocr-workbench'] },
    { id: 'ai:chat', label: 'AI Chat', requires: ['build:aitoolkit', 'service:chat'] },
];
const labels = { batch: 'Inbox', review: 'Review', catalog: 'Library', translate: 'Translate', ocr: 'OCR', settings: 'Settings', logs: 'Logs' };

describe('quick navigation', () => {
    it('keeps unavailable native and service destinations out of search', () => {
        const destinations = navigationDestinations(tabs, new Set(['service:chat']), labels, 'en');
        expect(destinations.map(item => item.id)).toEqual(['batch', 'review', 'catalog', 'translate', 'settings', 'logs']);
        expect(searchNavigation(destinations, 'scan')).toEqual([]);
        const enabled = navigationDestinations(tabs, new Set(['build:ocr-workbench', 'build:aitoolkit', 'service:chat']), labels, 'en');
        expect(searchNavigation(enabled, 'scan').map(item => item.id)).toEqual(['ocr']);
        expect(searchNavigation(enabled, 'AI Chat')[0].id).toBe('ai:chat');
    });

    it('finds workflows by task rather than requiring the destination name', () => {
        const destinations = navigationDestinations(tabs, new Set(), labels, 'en');
        expect(searchNavigation(destinations, 'rename')[0].id).toBe('review');
        expect(searchNavigation(destinations, 'models')[0].id).toBe('settings');
        expect(searchNavigation(destinations, 'find documents')[0].id).toBe('catalog');
        expect(searchNavigation(destinations, 'completely absent')).toEqual([]);
        expect(searchNavigation(destinations, '   ')).toEqual(destinations);
    });

    it('supports German labels and task terms with or without accents', () => {
        const destinations = navigationDestinations(tabs, new Set(), { ...labels, review: 'Prüfen', translate: 'Übersetzen' }, 'de');
        expect(searchNavigation(destinations, 'prufen')[0].id).toBe('review');
        expect(searchNavigation(destinations, 'UMBenennen')[0].id).toBe('review');
        expect(searchNavigation(destinations, 'ubersetzen')[0].id).toBe('translate');
        expect(searchNavigation(destinations, 'models')[0].id).toBe('settings');
        expect(destinations.find(item => item.id === 'settings')?.description).toContain('Modelle');
    });

    it('prefers exact destination names to incidental words in descriptions', () => {
        const destinations = navigationDestinations(tabs, new Set(), labels, 'en');
        expect(searchNavigation(destinations, 'review')[0].id).toBe('review');
        expect(searchNavigation(destinations, 'library')[0].id).toBe('catalog');
    });
});

describe('quick navigation keyboard shortcut', () => {
    const plain = { key: 'k', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, isComposing: false, repeat: false, defaultPrevented: false };

    it('accepts Ctrl+K and Cmd+K', () => {
        expect(isNavigationShortcut({ ...plain, ctrlKey: true })).toBe(true);
        expect(isNavigationShortcut({ ...plain, metaKey: true, key: 'K' })).toBe(true);
    });

    it('ignores composition, repeats, handled events and conflicting modifiers', () => {
        expect(isNavigationShortcut(plain)).toBe(false);
        for (const extra of ['altKey', 'shiftKey', 'isComposing', 'repeat', 'defaultPrevented', 'metaKey']) {
            expect(isNavigationShortcut({ ...plain, ctrlKey: true, [extra]: true })).toBe(false);
        }
        expect(isNavigationShortcut({ ...plain, ctrlKey: true, key: 'p' })).toBe(false);
    });
});
