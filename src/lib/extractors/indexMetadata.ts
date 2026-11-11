import type { ExtractionResult } from './index';

export interface IndexMetadata {
    parentDir?: string;
    volumeId?: string;
    url?: string;
    textTranslated?: string;
    textTranslatedLang?: string;
    audioDurationSeconds?: number;
    imageCameraMake?: string;
    imageCameraModel?: string;
}

/** Keep real extractor metadata when foreground text enters either backend. */
export function extractionIndexMetadata(result: ExtractionResult): IndexMetadata {
    const metadata = result.metadata ?? {};
    const text = (...values: unknown[]): string | undefined => values.find(
        (value): value is string => typeof value === 'string' && value.trim().length > 0,
    );
    const fields: IndexMetadata = {
        parentDir: text(metadata.parentDir, metadata.parent_dir),
        volumeId: text(metadata.volumeId, metadata.volume_id),
        url: text(metadata.url, metadata.sourceUrl, metadata.source_url),
        imageCameraMake: text(metadata.imageCameraMake, metadata.image_camera_make, metadata.image_exif?.camera_make),
        imageCameraModel: text(metadata.imageCameraModel, metadata.image_camera_model, metadata.image_exif?.camera_model),
    };
    const duration = [metadata.audioDurationSeconds, metadata.audio_duration_seconds, metadata.audio?.duration_seconds]
        .find((value): value is number => typeof value === 'number' && Number.isFinite(value) && value >= 0);
    if (duration !== undefined) fields.audioDurationSeconds = duration;
    const translated = text(metadata.textTranslated, metadata.text_translated, metadata.translated_text);
    const language = text(metadata.textTranslatedLang, metadata.text_translated_lang, metadata.translated_to_lang);
    if (translated !== undefined && language !== undefined) {
        fields.textTranslated = translated;
        fields.textTranslatedLang = language;
    }
    return Object.fromEntries(Object.entries(fields).filter(([, value]) => value !== undefined));
}
