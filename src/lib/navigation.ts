import type { Language } from './i18n.svelte';

export interface NavigationDestination {
    id: string;
    label: string;
    description: string;
    keywords: string[];
}

type DestinationTab = { id: string; label?: string; requires?: string[] };
type DestinationCopy = Record<string, { description: string; keywords: string[] }>;

const descriptions: Record<Language, DestinationCopy> = {
    en: {
        batch: { description: 'Add files and extract text or suggested metadata.', keywords: ['import', 'add', 'folder', 'upload', 'extract', 'inbox'] },
        review: { description: 'Check suggestions and preview where accepted files will go.', keywords: ['sort', 'rename', 'organize', 'approve', 'accept', 'move', 'copy', 'metadata'] },
        catalog: { description: 'Find and browse your indexed documents.', keywords: ['search', 'find', 'read', 'catalog', 'library', 'documents'] },
        drives: { description: 'Browse local folders and connected cloud drives.', keywords: ['files', 'folders', 'storage', 'cloud', 'webdav', 'filen', 'sync'] },
        chat: { description: 'Ask questions with sources from your documents.', keywords: ['ask', 'question', 'answer', 'sources', 'evidence', 'conversation'] },
        history: { description: 'Find previous batches and resume a saved session.', keywords: ['resume', 'previous', 'session', 'restore', 'history'] },
        translate: { description: 'Translate a document into another language.', keywords: ['translation', 'language', 'docx', 'word'] },
        ocr: { description: 'Turn scans into text and review recognized pages.', keywords: ['scan', 'scanned', 'recognize', 'image', 'ocr', 'proofread'] },
        pdf: { description: 'Read and edit PDF documents and their pages.', keywords: ['pdf', 'rotate', 'crop', 'redact', 'pages', 'editor'] },
        aitoolkit: { description: 'Connect and inspect your optional AI toolkit.', keywords: ['ai', 'service', 'toolkit'] },
        'ai:chat': { description: 'Chat using your connected AI service.', keywords: ['conversation', 'assistant', 'service'] },
        'ai:translate': { description: 'Translate text using your connected AI service.', keywords: ['translation', 'language', 'service'] },
        'ai:vision': { description: 'Ask your connected AI service about an image.', keywords: ['image', 'photo', 'picture', 'describe', 'vision'] },
        'ai:ocr': { description: 'Recognize image text using your connected AI service.', keywords: ['scan', 'text', 'recognition', 'ocr'] },
        'ai:transcription': { description: 'Transcribe audio using your connected AI service.', keywords: ['audio', 'speech', 'voice', 'transcribe'] },
        'ai:tts': { description: 'Turn text into speech using your connected AI service.', keywords: ['speech', 'voice', 'speak', 'audio', 'read aloud'] },
        'ai:images': { description: 'Generate images using your connected AI service.', keywords: ['generate', 'image', 'picture', 'create'] },
        'ai:extract': { description: 'Extract structured information using your connected AI service.', keywords: ['fields', 'data', 'structured', 'extraction'] },
        settings: { description: 'Configure models, extraction, indexing and app preferences.', keywords: ['setup', 'model', 'models', 'provider', 'api', 'key', 'preferences', 'language', 'privacy', 'credentials'] },
        logs: { description: 'Open activity logs to investigate a problem.', keywords: ['error', 'errors', 'failure', 'diagnostics', 'debug', 'activity'] },
    },
    de: {
        batch: { description: 'Dateien hinzufügen und Text oder Metadaten extrahieren.', keywords: ['import', 'importieren', 'hinzufügen', 'ordner', 'hochladen', 'extrahieren', 'eingang'] },
        review: { description: 'Vorschläge prüfen und Ziele akzeptierter Dateien ansehen.', keywords: ['sortieren', 'umbenennen', 'organisieren', 'prüfen', 'akzeptieren', 'verschieben', 'kopieren', 'metadaten'] },
        catalog: { description: 'Indizierte Dokumente finden und durchsuchen.', keywords: ['suchen', 'finden', 'lesen', 'katalog', 'bibliothek', 'dokumente'] },
        drives: { description: 'Lokale Ordner und verbundene Cloud-Laufwerke durchsuchen.', keywords: ['dateien', 'ordner', 'speicher', 'cloud', 'webdav', 'filen', 'synchronisieren'] },
        chat: { description: 'Fragen stellen und Quellen in Ihren Dokumenten ansehen.', keywords: ['fragen', 'frage', 'antwort', 'quellen', 'belege', 'unterhaltung'] },
        history: { description: 'Frühere Stapel finden und gespeicherte Sitzungen fortsetzen.', keywords: ['fortsetzen', 'sitzung', 'wiederherstellen', 'verlauf', 'früher'] },
        translate: { description: 'Ein Dokument in eine andere Sprache übersetzen.', keywords: ['übersetzung', 'übersetzen', 'sprache', 'docx', 'word'] },
        ocr: { description: 'Scans in Text umwandeln und erkannte Seiten prüfen.', keywords: ['scan', 'scannen', 'erkennen', 'bild', 'ocr', 'korrektur'] },
        pdf: { description: 'PDF-Dokumente und ihre Seiten lesen und bearbeiten.', keywords: ['pdf', 'drehen', 'zuschneiden', 'schwärzen', 'seiten', 'editor'] },
        aitoolkit: { description: 'Das optionale KI-Toolkit verbinden und prüfen.', keywords: ['ki', 'dienst', 'toolkit'] },
        'ai:chat': { description: 'Mit dem verbundenen KI-Dienst chatten.', keywords: ['unterhaltung', 'assistent', 'dienst'] },
        'ai:translate': { description: 'Text mit dem verbundenen KI-Dienst übersetzen.', keywords: ['übersetzung', 'sprache', 'dienst'] },
        'ai:vision': { description: 'Dem verbundenen KI-Dienst Fragen zu einem Bild stellen.', keywords: ['bild', 'foto', 'beschreiben', 'vision'] },
        'ai:ocr': { description: 'Bildtext mit dem verbundenen KI-Dienst erkennen.', keywords: ['scan', 'text', 'erkennung', 'ocr'] },
        'ai:transcription': { description: 'Audio mit dem verbundenen KI-Dienst transkribieren.', keywords: ['audio', 'sprache', 'stimme', 'transkribieren'] },
        'ai:tts': { description: 'Text mit dem verbundenen KI-Dienst vorlesen.', keywords: ['sprache', 'stimme', 'sprechen', 'audio', 'vorlesen'] },
        'ai:images': { description: 'Bilder mit dem verbundenen KI-Dienst erstellen.', keywords: ['generieren', 'bild', 'bilder', 'erstellen'] },
        'ai:extract': { description: 'Strukturierte Informationen mit dem verbundenen KI-Dienst extrahieren.', keywords: ['felder', 'daten', 'strukturiert', 'extraktion'] },
        settings: { description: 'Modelle, Extraktion, Suchindex und App-Einstellungen einrichten.', keywords: ['einrichten', 'modell', 'modelle', 'anbieter', 'api', 'schlüssel', 'sprache', 'datenschutz', 'zugangsdaten'] },
        logs: { description: 'Aktivitätsprotokolle zur Fehlersuche öffnen.', keywords: ['fehler', 'diagnose', 'debug', 'aktivität', 'protokoll'] },
    },
};

export const navigationCopy = {
    en: {
        open: 'Quick navigation', title: 'Go to a tool or workflow', search: 'Find a tool or workflow',
        placeholder: 'Try “scan”, “models” or “rename”…', close: 'Close quick navigation',
        empty: 'No matching destinations', emptyHint: 'Try a different tool name or task.',
        current: 'Current', navigate: 'Navigate', choose: 'Open', dismiss: 'Close',
        primary: 'Main navigation', mobile: 'Mobile navigation', tools: 'Tools',
        collapse: 'Collapse navigation', expand: 'Expand navigation',
        result: 'destination', results: 'destinations',
    },
    de: {
        open: 'Schnellnavigation', title: 'Werkzeug oder Arbeitsbereich öffnen', search: 'Werkzeug oder Arbeitsbereich suchen',
        placeholder: 'Zum Beispiel „Scan“, „Modelle“ oder „Umbenennen“…', close: 'Schnellnavigation schließen',
        empty: 'Keine passenden Ziele', emptyHint: 'Versuchen Sie einen anderen Werkzeugnamen oder eine andere Aufgabe.',
        current: 'Aktuell', navigate: 'Navigieren', choose: 'Öffnen', dismiss: 'Schließen',
        primary: 'Hauptnavigation', mobile: 'Mobile Navigation', tools: 'Werkzeuge',
        collapse: 'Navigation einklappen', expand: 'Navigation ausklappen',
        result: 'Ziel', results: 'Ziele',
    },
};

/** Reuse the shell's capability requirements so search cannot reveal unavailable tools. */
export function navigationDestinations(
    tabs: readonly DestinationTab[],
    capabilities: Set<string>,
    labels: Record<string, string>,
    language: Language,
): NavigationDestination[] {
    const available = tabs.filter(tab => !tab.requires || tab.requires.every(capability => capabilities.has(capability)));
    return [...available, { id: 'settings' }, { id: 'logs' }].map(tab => {
        const copy = descriptions[language][tab.id] ?? descriptions[language][tab.id.replace(/^ai:/, '')];
        const fallback = descriptions[language].aitoolkit;
        return {
            id: tab.id,
            label: labels[tab.id] ?? tab.label ?? tab.id,
            description: copy?.description ?? fallback.description,
            // Include the English task terms as well: tool names and technical
            // words are often searched in English even with a German interface.
            keywords: [...(copy?.keywords ?? fallback.keywords), ...(descriptions.en[tab.id]?.keywords ?? [])],
        };
    });
}

function normalized(value: string): string {
    return value.toLocaleLowerCase().normalize('NFD').replace(/\p{M}/gu, '').replace(/ß/g, 'ss');
}

/** Every query word must match; label matches rank ahead of descriptive matches. */
export function searchNavigation(destinations: NavigationDestination[], query: string): NavigationDestination[] {
    const terms = normalized(query).trim().split(/\s+/).filter(Boolean);
    if (!terms.length) return destinations;
    return destinations.map((destination, index) => {
        const label = normalized(destination.label);
        const keywords = normalized(destination.keywords.join(' '));
        const description = normalized(destination.description);
        let score = 0;
        for (const term of terms) {
            if (label === term) score += 8;
            else if (label.startsWith(term)) score += 6;
            else if (label.includes(term)) score += 4;
            else if (keywords.includes(term)) score += 2;
            else if (description.includes(term)) score += 1;
            else return { destination, index, score: -1 };
        }
        return { destination, index, score };
    }).filter(result => result.score >= 0)
        .sort((a, b) => b.score - a.score || a.index - b.index)
        .map(result => result.destination);
}

type ShortcutEvent = Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'metaKey' | 'altKey' | 'shiftKey' | 'isComposing' | 'repeat' | 'defaultPrevented'>;

export function isNavigationShortcut(event: ShortcutEvent): boolean {
    return event.key.toLowerCase() === 'k'
        && event.ctrlKey !== event.metaKey
        && !event.altKey && !event.shiftKey && !event.isComposing && !event.repeat && !event.defaultPrevented;
}
