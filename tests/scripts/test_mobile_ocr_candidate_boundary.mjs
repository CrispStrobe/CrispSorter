// Evidence-validator controls only; these supplied responses are never reported
// as native or application execution. The CI caller uses actual Tauri IPC.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import { checkCandidateOcrBoundary } from '../../scripts/mobile_ocr_candidate_boundary.mjs';

const nativeError = 'Recognize page: OCR recognition failed: OCR pipeline recognition failed; see native diagnostics';
const blankResult = () => ({ regions: [], diagnostics: { full_text: '',
  native_review: { schema: 'crispembed.ocr.review', schema_version: 1 },
  requested_config: { engine: 'tesseract', stages: [{ min_chars: 0 }] } } });
function controls(first, second = blankResult()) {
  const retained = [], calls = [];
  return { retained, calls, options: { pagePath: '/app/page.png', baselineConfig: {},
    importBlank: async () => ({ image_path: '/app/blank.png' }),
    retain: async (name, value) => retained.push({ name, value }),
    recognize: async args => {
      calls.push(args);
      const result = calls.length === 1 ? first : second;
      if (result instanceof Error) throw result;
      return result;
    } } };
}

test('validator preserves actual observations and requests only installed baseline models with permissive gates', async () => {
  const value = controls(new Error(nativeError));
  const observed = await checkCandidateOcrBoundary(value.options);
  assert.equal(observed.negative_outcome, 'rejected');
  assert.equal(observed.blank_full_text, '');
  assert.equal(value.calls[0].engine, 'unified');
  assert.equal(value.calls[1].engine, 'tesseract');
  assert.equal(value.calls[0].config.stages[0].det_model, 'dbnet-det');
  assert.equal(value.calls[1].config.stages[0].rec_model, 'tesseract-eng');
  for (const call of value.calls) {
    assert.equal(call.allowDownloads, false);
    assert.equal(call.config.stages[0].min_chars, 0);
    assert.equal(call.config.stages[0].min_confidence, 0);
    assert.equal(call.config.layout, false);
  }
  assert.deepEqual(value.retained.map(item => item.name), ['candidate-native-failure.json', 'candidate-valid-empty.json']);
});

test('earlier model, policy, constructor and transport errors cannot masquerade as native run failure', async () => {
  for (const message of ['MODEL_DOWNLOAD_REQUIRED', 'Unsupported OCR engine',
    'CrispEmbed OCR per-stage pipeline initialization failed', 'IPC timeout']) {
    const value = controls(new Error(message));
    await assert.rejects(checkCandidateOcrBoundary(value.options), /actual rebuilt Rust pipeline-run error/);
    assert.equal(value.calls.length, 1);
    assert.match(value.retained[0].value.error, new RegExp(message));
  }
});

test('native false-success empty output is retained and fails the negative gate', async () => {
  const value = controls(blankResult());
  await assert.rejects(checkCandidateOcrBoundary(value.options), /failure became an app success/);
  assert.equal(value.retained[0].value.outcome, 'unexpected_success');
  assert.deepEqual(value.retained[0].value.result, blankResult());
});

test('nonempty, NULL-like, or missing native blank evidence is retained and rejected', async () => {
  for (const mutate of [
    result => { result.regions = [{ text: 'unexpected' }]; },
    result => { result.diagnostics.full_text = null; },
    result => { result.diagnostics.native_review = null; },
  ]) {
    const result = blankResult(); mutate(result);
    const value = controls(new Error(nativeError), result);
    await assert.rejects(checkCandidateOcrBoundary(value.options));
    assert.deepEqual(value.retained.at(-1).value.result, result);
  }
});

test('a rejected white-raster recognition is preserved and never reported as valid empty', async () => {
  const value = controls(new Error(nativeError), new Error('actual blank request failed'));
  await assert.rejects(checkCandidateOcrBoundary(value.options), /actual blank request failed/);
  assert.equal(value.retained.at(-1).value.outcome, 'rejected');
  assert.match(value.retained.at(-1).value.error, /actual blank request failed/);
});

test('real IPC caller limits the extra boundary check to candidate receipts and retains the white raster', () => {
  const source = fs.readFileSync(new URL('../../scripts/mobile_ocr_webview_smoke.mjs', import.meta.url), 'utf8');
  assert(source.includes('candidateRuntime: build.candidate_reuse != null'));
  const candidate = source.split('if (prepared.candidateRuntime) {')[1].split('    const recognized = [];')[0];
  assert(candidate.includes("recognize: args => invoke('ocr_workspace_recognize', args, 90_000)"));
  assert(candidate.includes("importBytes('ocr_workspace_import_image', bytes,"));
  assert(candidate.includes("'candidate-white-raster.png'"));
  assert(!candidate.includes('install_mobile_models'));
});
