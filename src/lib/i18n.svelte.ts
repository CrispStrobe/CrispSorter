export type Language = 'en' | 'de';

export const translations = {
    en: {
        nav: {
            batch: 'Batches',
            history: 'History',
            settings: 'Settings'
        },
        batch: {
            add_files: 'Add',
            text_only: 'Text Only',
            ai_sort: 'AI Sort',
            start_batch: 'Start',
            processing: '...',
            accept_all: 'Check All',
            uncheck_all: 'Check None',
            execute: 'Sort',
            clear_all: 'Clear All',
            empty: 'No files. Drop files or use "Add".',
            status: 'Status',
            file_name: 'File Name',
            title: 'Title',
            author: 'Author',
            actions: 'Actions',
            details: 'Details',
            target_path: 'Target Path',
            extracted_text: 'Text Preview',
            edit_metadata: 'Metadata',
            year: 'Year',
            path_hint: 'Calculated after extraction.',
            extract_hint: 'Run extraction first.',
            confirm_move: 'Sort {count} files?',
            search_placeholder: 'Search...',
            filters: 'Filters',
            filter_type: 'Type',
            filter_size: 'Min KB',
            filter_status: 'Status'
        },
        history: {
            title: 'History',
            subtitle: 'Resume or review past sessions.',
            resume: 'Resume',
            empty: 'No history found.',
            import: 'Import',
            export: 'Export',
            delete_confirm: 'Delete session?'
        },
        settings: {
            providers: 'Providers',
            app_settings: 'App Settings',
            general: 'General',
            save_all: 'Save All',
            export_dir: 'Export Directory',
            browse: 'Browse',
            dir_hint: 'Default: "Sorted" folder next to source.',
            save_txt: 'Save .txt',
            save_txt_hint: 'Always create .txt files.',
            language: 'Language',
            base_url: 'Base URL',
            api_key: 'API Key',
            refresh_models: 'Refresh',
            test_connection: 'Test',
            available_models: 'Available Models',
            no_models: 'No models. Refresh to fetch.',
            saved: 'Saved!',
            key_required: 'API key required.',
            fetch_failed: 'Fetch failed',
            test_success: 'Success!',
            test_error: 'Error',
            select_model: 'Select Model'
        }
    },
    de: {
        nav: {
            batch: 'Batches',
            history: 'Verlauf',
            settings: 'Einstellungen'
        },
        batch: {
            add_files: 'Hinzufügen',
            text_only: 'Nur Text',
            ai_sort: 'KI Sortierung',
            start_batch: 'Start',
            processing: '...',
            accept_all: 'Haken Alle',
            uncheck_all: 'Haken keine',
            execute: 'Sortieren',
            clear_all: 'Alle löschen',
            empty: 'Keine Dateien. Dateien hierher ziehen oder "Hinzufügen".',
            status: 'Status',
            file_name: 'Dateiname',
            title: 'Titel',
            author: 'Autor',
            actions: 'Aktionen',
            details: 'Details',
            target_path: 'Zielpfad',
            extracted_text: 'Text-Vorschau',
            edit_metadata: 'Metadaten',
            year: 'Jahr',
            path_hint: 'Wird berechnet.',
            extract_hint: 'Zuerst extrahieren.',
            confirm_move: '{count} Dateien sortieren?',
            search_placeholder: 'Suchen...',
            filters: 'Filter',
            filter_type: 'Typ',
            filter_size: 'Min KB',
            filter_status: 'Status'
        },
        history: {
            title: 'Verlauf',
            subtitle: 'Sitzungen fortsetzen oder prüfen.',
            resume: 'Fortsetzen',
            empty: 'Kein Verlauf.',
            import: 'Import',
            export: 'Export',
            delete_confirm: 'Löschen?'
        },
        settings: {
            providers: 'Anbieter',
            app_settings: 'App-Einstellungen',
            general: 'Allgemein',
            save_all: 'Speichern',
            export_dir: 'Export-Verzeichnis',
            browse: 'Durchsuchen',
            dir_hint: 'Standard: "Sorted"-Ordner neben der Quelle.',
            save_txt: '.txt speichern',
            save_txt_hint: 'Immer .txt-Dateien erstellen.',
            language: 'Sprache',
            base_url: 'Basis-URL',
            api_key: 'API-Schlüssel',
            refresh_models: 'Aktualisieren',
            test_connection: 'Testen',
            available_models: 'Modelle',
            no_models: 'Keine Modelle. Bitte aktualisieren.',
            saved: 'Gespeichert!',
            key_required: 'Schlüssel erforderlich.',
            fetch_failed: 'Fehler beim Laden',
            test_success: 'Erfolg!',
            test_error: 'Fehler',
            select_model: 'Modell auswählen'
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
