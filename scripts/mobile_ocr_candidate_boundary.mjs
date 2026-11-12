// Candidate-only controls for the real app IPC caller. Unit controls for this
// evidence validator do not constitute native or application execution.
import assert from 'node:assert/strict';

export const NATIVE_PIPELINE_RUN_FAILURE = 'Recognize page: CrispEmbed OCR pipeline run failed: OCR pipeline recognition failed; see native diagnostics';

export async function checkCandidateOcrBoundary({ recognize, importBlank, retain, pagePath, baselineConfig }) {
  const disabledCleanup = { enabled: false, denoise: false };
  const negativeConfig = { ...baselineConfig, enabled: true, engine: 'unified', language: null,
    router: false, cleanup_enabled: false, denoise: false, dewarp: false, restore: false,
    sr: false, layout: false, min_chars: 0, min_confidence: 0,
    stages: [{ engine: 'unified', source_type: 'auto', det_model: 'dbnet-det',
      min_chars: 0, min_confidence: 0, cleanup: disabledCleanup }] };
  const negative = { config: negativeConfig, outcome: 'unexpected_success' };
  try {
    negative.result = await recognize({ pagePath, config: negativeConfig, engine: 'unified', language: null,
      zone: null, allowDownloads: false });
  } catch (error) {
    negative.outcome = 'rejected';
    negative.error = String(error).slice(0, 16_384);
  }
  // Preserve the actual result/rejection before asserting. A policy/model gate,
  // initialization-constructor failure, timeout, or successful empty response
  // cannot stand in for the rebuilt wrapper's NULL-full-text error branch.
  await retain('candidate-native-failure.json', negative);
  assert.equal(negative.outcome, 'rejected', 'Native recognition failure became an app success.');
  assert(negative.error.endsWith(NATIVE_PIPELINE_RUN_FAILURE),
    'Expected the actual rebuilt Rust pipeline-run error; an earlier app rejection is insufficient.');

  const blank = await importBlank();
  const blankConfig = { ...negativeConfig, engine: 'tesseract', language: 'eng',
    stages: [{ engine: 'tesseract', source_type: 'auto', det_model: 'dbnet-det', rec_model: 'tesseract-eng',
      min_chars: 0, min_confidence: 0, cleanup: disabledCleanup }] };
  let result;
  try {
    result = await recognize({ pagePath: blank.image_path, config: blankConfig,
      engine: 'tesseract', language: 'eng', zone: null, allowDownloads: false });
  } catch (error) {
    await retain('candidate-valid-empty.json', { imported_blank: blank, config: blankConfig,
      outcome: 'rejected', error: String(error).slice(0, 16_384) });
    throw error;
  }
  await retain('candidate-valid-empty.json', { imported_blank: blank, config: blankConfig, result });
  assert.deepEqual(result.regions, [], 'A white raster returned unexpected text regions.');
  assert.equal(result.diagnostics?.full_text, '', 'A valid blank must retain the native empty text string.');
  assert.equal(result.diagnostics?.native_review?.schema, 'crispembed.ocr.review');
  assert.equal(result.diagnostics?.native_review?.schema_version, 1);
  assert.equal(result.diagnostics?.requested_config?.engine, 'tesseract');
  assert.equal(result.diagnostics?.requested_config?.stages?.[0]?.min_chars, 0);
  return { negative_outcome: negative.outcome, negative_error: negative.error,
    blank_regions: result.regions.length, blank_full_text: result.diagnostics.full_text,
    scope: 'Real app IPC -> rebuilt Rust wrapper -> native lazy unified initialization rejection; subsequent valid white-raster empty result through Tesseract. Reuses the installed baseline only. Does not qualify Ovis inference/patch bounds, same-context recovery, or multi-stage fallback.' };
}
