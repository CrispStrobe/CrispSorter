export type Language = 'en' | 'de';

export const translations = {
    en: {
        nav: {
            batch: 'Batch Review',
            history: 'History',
            settings: 'Settings'
        },
        batch: {
            add_files: 'Add Files',
            text_only: 'Text Only',
            ai_sort: 'AI Sort',
            start_batch: 'Start Batch',
            processing: 'Processing...',
            accept_all: 'Accept All',
            execute: 'Execute Sorting',
            clear_all: 'Clear All',
            empty: 'No files added. Use "Add Files" to begin.',
            status: 'Status',
            file_name: 'Original File Name',
            title: 'Suggested Title',
            author: 'Suggested Author',
            actions: 'Actions',
            details: 'Item Details',
            target_path: 'Target Path',
            extracted_text: 'Extracted Text (Preview)',
            edit_metadata: 'Edit Metadata',
            year: 'Year',
            path_hint: 'Will be calculated after extraction.',
            extract_hint: 'Processing required to view text.',
            confirm_move: 'Move {count} files to their sorted locations?',
            search_placeholder: 'Search files...',
            filters: 'Filters',
            filter_type: 'File Type',
            filter_size: 'Min Size (KB)',
            filter_status: 'Status'
        },
        history: {
            title: 'Batch History',
            subtitle: 'Resume previous sorting sessions or review results.',
            resume: 'Resume',
            empty: 'No history found. Start a new batch to see it here.',
            import: 'Import Batch',
            export: 'Export Batch',
            delete_confirm: 'Are you sure you want to delete this session?'
        },
        settings: {
            providers: 'Providers',
            app_settings: 'App Settings',
            general: 'General Settings',
            save_all: 'Save All',
            export_dir: 'Default Export Directory',
            browse: 'Browse',
            dir_hint: 'If empty, files will be saved in a "Sorted" folder next to the source.',
            save_txt: 'Save Extracted Text (.txt)',
            save_txt_hint: 'Always create a .txt file alongside the sorted document.',
            language: 'Language',
            base_url: 'Base URL',
            api_key: 'API Key',
            refresh_models: 'Refresh Models',
            test_connection: 'Test Connection',
            available_models: 'Available Models',
            no_models: 'No models found. Click "Refresh Models" to fetch them.',
            saved: 'Settings saved!',
            key_required: 'Please enter an API key first.',
            fetch_failed: 'Failed to fetch models',
            test_success: 'Success!',
            test_error: 'Error'
        }
    },
    de: {
        nav: {
            batch: 'Stapel-Prüfung',
            history: 'Verlauf',
            settings: 'Einstellungen'
        },
        batch: {
            add_files: 'Dateien hinzufügen',
            text_only: 'Nur Text',
            ai_sort: 'KI Sortierung',
            start_batch: 'Stapel starten',
            processing: 'Verarbeitung...',
            accept_all: 'Alle akzeptieren',
            execute: 'Sortierung ausführen',
            clear_all: 'Alle löschen',
            empty: 'Keine Dateien hinzugefügt. "Dateien hinzufügen" nutzen.',
            status: 'Status',
            file_name: 'Ursprünglicher Dateiname',
            title: 'Vorgeschlagener Titel',
            author: 'Vorgeschlagener Autor',
            actions: 'Aktionen',
            details: 'Element-Details',
            target_path: 'Zielpfad',
            extracted_text: 'Extrahierter Text (Vorschau)',
            edit_metadata: 'Metadaten bearbeiten',
            year: 'Jahr',
            path_hint: 'Wird nach der Extraktion berechnet.',
            extract_hint: 'Verarbeitung erforderlich, um Text zu sehen.',
            confirm_move: '{count} Dateien an ihre sortierten Orte verschieben?',
            search_placeholder: 'Dateien suchen...',
            filters: 'Filter',
            filter_type: 'Dateityp',
            filter_size: 'Min. Größe (KB)',
            filter_status: 'Status'
        },
        history: {
            title: 'Stapel-Verlauf',
            subtitle: 'Frühere Sitzungen fortsetzen oder Ergebnisse prüfen.',
            resume: 'Fortsetzen',
            empty: 'Kein Verlauf gefunden. Starten Sie einen Stapel.',
            import: 'Stapel importieren',
            export: 'Stapel exportieren',
            delete_confirm: 'Sitzung wirklich löschen?'
        },
        settings: {
            providers: 'Anbieter',
            app_settings: 'App-Einstellungen',
            general: 'Allgemein',
            save_all: 'Alles speichern',
            export_dir: 'Standard-Exportverzeichnis',
            browse: 'Durchsuchen',
            dir_hint: 'Wenn leer, werden Dateien im "Sorted"-Ordner neben der Quelle gespeichert.',
            save_txt: 'Extrahierten Text speichern (.txt)',
            save_txt_hint: 'Immer eine .txt-Datei neben dem sortierten Dokument erstellen.',
            language: 'Sprache',
            base_url: 'Basis-URL',
            api_key: 'API-Schlüssel',
            refresh_models: 'Modelle aktualisieren',
            test_connection: 'Verbindung testen',
            available_models: 'Verfügbare Modelle',
            no_models: 'Keine Modelle gefunden. Klicken Sie auf "Aktualisieren".',
            saved: 'Einstellungen gespeichert!',
            key_required: 'Bitte zuerst API-Schlüssel eingeben.',
            fetch_failed: 'Modelle konnten nicht geladen werden',
            test_success: 'Erfolg!',
            test_error: 'Fehler'
        }
    }
};

export class TranslationService {
    lang = $state<Language>('en');
    t = $derived(translations[this.lang]);

    constructor() {}

    setLanguage(l: Language) {
        this.lang = l;
    }
}

export const i18n = new TranslationService();
