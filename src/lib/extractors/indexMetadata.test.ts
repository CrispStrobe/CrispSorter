import { describe, expect, it } from 'vitest';
import { extractionIndexMetadata } from './indexMetadata';

describe('foreground index metadata', () => {
    it('preserves native duration, camera and translation metadata without inventing values', () => {
        expect(extractionIndexMetadata({ text: 'source', metadata: {
            parent_dir: '/source/100%_done', volume_id: 'drive-1', url: 'https://example.test/source',
            audio: { duration_seconds: 0 }, image_exif: { camera_make: 'Make', camera_model: 'Model' },
            translated_text: 'Translated evidence', translated_to_lang: 'en',
        } })).toEqual({ parentDir: '/source/100%_done', volumeId: 'drive-1', url: 'https://example.test/source',
            audioDurationSeconds: 0, imageCameraMake: 'Make', imageCameraModel: 'Model',
            textTranslated: 'Translated evidence', textTranslatedLang: 'en' });
    });

    it('keeps absent or failed extraction metadata absent', () => {
        expect(extractionIndexMetadata({ text: 'plain' })).toEqual({});
        expect(extractionIndexMetadata({ text: 'plain', metadata: {
            audio_duration_seconds: Number.NaN, image_camera_make: {}, text_translated_lang: 'en',
        } })).toEqual({});
    });

    it('accepts camelCase receipts and only actual finite duration numbers', () => {
        expect(extractionIndexMetadata({ text: 'plain', metadata: {
            audioDurationSeconds: '120', audio_duration_seconds: -1, audio: { duration_seconds: 12.5 },
            textTranslated: 'Übersetzung', textTranslatedLang: 'de', imageCameraModel: 'Camera 100%',
        } })).toEqual({ audioDurationSeconds: 12.5, textTranslated: 'Übersetzung', textTranslatedLang: 'de', imageCameraModel: 'Camera 100%' });
    });
});
